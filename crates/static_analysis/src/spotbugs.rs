//! SpotBugs 4.8.6 adapter (+ FindSecBugs 1.13.0). Class-file analyzer:
//! `requires_classes()` returns `true` and the orchestration layer
//! short-circuits to `SkippedNoClasses` when `compile_stage` didn't
//! produce a successful build for the sprint — surfacing that honestly
//! in the report rather than leaving a silent absence.
//!
//! ## Discovery order
//!
//! 1. `$SPOTBUGS_HOME/bin/spotbugs` (env override)
//! 2. `/opt/spotbugs/bin/spotbugs`
//! 3. `crates/static_analysis/vendor/spotbugs-4.8.6/bin/spotbugs`
//! 4. `spotbugs` resolved against `$PATH`
//!
//! `discover()` returns `None` when no launcher is reachable; the
//! caller logs and continues.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tracing::{debug, warn};

use crate::adapter::{Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerOutput, AnalyzerStatus};
use crate::presets::resolve_spotbugs_ruleset;
use crate::sarif;

pub const SPOTBUGS_VERSION: &str = "4.8.6";
pub const FINDSECBUGS_VERSION: &str = "1.13.0";

pub struct SpotBugs {
    launcher: PathBuf,
    /// Optional FindSecBugs plugin jar. None → don't pass `-pluginList`.
    findsecbugs_jar: Option<PathBuf>,
}

impl SpotBugs {
    pub fn discover(include_findsecbugs: bool) -> Option<Self> {
        let launcher = locate_spotbugs_launcher()?;
        let findsecbugs_jar = if include_findsecbugs {
            locate_findsecbugs_jar()
        } else {
            None
        };
        Some(Self {
            launcher,
            findsecbugs_jar,
        })
    }

    pub fn with_launcher(launcher: PathBuf, findsecbugs_jar: Option<PathBuf>) -> Self {
        Self {
            launcher,
            findsecbugs_jar,
        }
    }
}

impl Analyzer for SpotBugs {
    fn id(&self) -> &'static str {
        "spotbugs"
    }

    fn version(&self) -> &str {
        SPOTBUGS_VERSION
    }

    fn requires_classes(&self) -> bool {
        true
    }

    fn run(&self, input: &AnalyzerInput, cfg: &AnalyzerConfig) -> AnalyzerOutput {
        let started = Instant::now();
        if input.class_roots.is_empty() {
            return AnalyzerOutput {
                status: AnalyzerStatus::SkippedNoClasses,
                findings: vec![],
                raw_report: None,
                duration_ms: started.elapsed().as_millis() as u64,
                diagnostics: "no class roots — compile_stage didn't produce a successful build"
                    .to_string(),
            };
        }

        let resolved_ruleset = match resolve_ruleset_ref(&cfg.ruleset_ref) {
            Ok(r) => r,
            Err(e) => {
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    diagnostics: format!("ruleset resolution failed: {e}"),
                };
            }
        };

        let sarif_path = input.work_dir.join("spotbugs.sarif");

        let mut cmd = Command::new(&self.launcher);
        cmd.arg("-textui")
            .arg(format!("-sarif={}", sarif_path.display()))
            .arg("-include")
            .arg(resolved_ruleset.path())
            .arg("-low") // include the lowest-severity bugs; we filter via ruleset
            .arg("-quiet");
        if let Some(plugin) = &self.findsecbugs_jar {
            cmd.arg("-pluginList").arg(plugin);
        }
        for class_root in &input.class_roots {
            cmd.arg(class_root);
        }
        cmd.env(
            "JAVA_TOOL_OPTIONS",
            format!("-Xmx{}m", input.max_heap_mb.max(64)),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        debug!(launcher = %self.launcher.display(), "spawning SpotBugs");
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    diagnostics: format!(
                        "failed to spawn SpotBugs launcher `{}`: {}",
                        self.launcher.display(),
                        e
                    ),
                };
            }
        };

        let deadline = Instant::now() + input.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return AnalyzerOutput {
                            status: AnalyzerStatus::TimedOut,
                            findings: vec![],
                            raw_report: Some(sarif_path),
                            duration_ms: started.elapsed().as_millis() as u64,
                            diagnostics: format!(
                                "SpotBugs timed out after {}s",
                                input.timeout.as_secs()
                            ),
                        };
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return AnalyzerOutput {
                        status: AnalyzerStatus::Crashed,
                        findings: vec![],
                        raw_report: Some(sarif_path),
                        duration_ms: started.elapsed().as_millis() as u64,
                        diagnostics: format!("waiting on SpotBugs: {e}"),
                    };
                }
            }
        }

        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: Some(sarif_path),
                    duration_ms: started.elapsed().as_millis() as u64,
                    diagnostics: format!("collecting SpotBugs output: {e}"),
                };
            }
        };

        let exit = output.status.code().unwrap_or(-1);
        let stderr_tail = tail(&output.stderr, 4_000);
        let duration_ms = started.elapsed().as_millis() as u64;

        let findings = match sarif::parse(&sarif_path) {
            Ok(mut fs) => {
                fs.retain(|f| f.severity.at_least(cfg.severity_floor));
                if fs.len() > cfg.max_findings {
                    debug!(
                        kept = cfg.max_findings,
                        total = fs.len(),
                        "SpotBugs finding count exceeds max_findings; truncating"
                    );
                    fs.truncate(cfg.max_findings);
                }
                fs
            }
            Err(e) => {
                warn!(exit, "SpotBugs SARIF unreadable; treating as Crashed");
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: Some(sarif_path),
                    duration_ms,
                    diagnostics: format!(
                        "SpotBugs exit={}, SARIF parse failed: {}; stderr tail:\n{}",
                        exit, e, stderr_tail
                    ),
                };
            }
        };

        AnalyzerOutput {
            status: AnalyzerStatus::Ok,
            findings,
            raw_report: Some(sarif_path),
            duration_ms,
            diagnostics: format!("exit={exit}"),
        }
    }
}

/// Helper exposed to the orchestration layer: did any PR for this
/// (repo, sprint) compile successfully? Used to short-circuit
/// `requires_classes()` analyzers without spawning the launcher when
/// `compile_stage` already reported failure.
///
/// Reads the canonical `pr_compilation.compiles` boolean. The plan
/// originally specified a `status = 'success'` column, but the actual
/// schema uses `compiles BOOLEAN` (T-P0.x); we honour the live schema.
pub fn latest_pr_compiled_ok(conn: &Connection, repo_full_name: &str) -> bool {
    // T-P3.4: artifact-shape — "did any PR for this repo ever compile
    // successfully?". `pr_compilation` retains its per-sprint shape
    // (governed by compile_stage), but the static-analysis gate only
    // needs to know that *some* successful build exists for the repo
    // so SpotBugs has class roots worth scanning.
    let row: rusqlite::Result<i64> = conn.query_row(
        "SELECT COUNT(*)
         FROM pr_compilation pc
         JOIN pull_requests pr ON pr.id = pc.pr_id
         WHERE pr.repo_full_name = ?
           AND pc.compiles = 1",
        rusqlite::params![repo_full_name],
        |r| r.get(0),
    );
    matches!(row, Ok(n) if n > 0)
}

/// Discover candidate class roots inside a built repo. Spring Boot
/// (Gradle) lands compiled classes at `build/classes/java/main`; Android
/// (AGP) lands them under `app/build/intermediates/javac/debug/classes`
/// (or one of several version-dependent paths). When none of the
/// conventions match, `find <repo>/build -name '*.class' | xargs dirname`
/// gives a usable fallback. Empty result → `SkippedNoClasses`.
pub fn discover_class_roots(repo_path: &Path) -> Vec<PathBuf> {
    let conventional = [
        "build/classes/java/main",
        "build/classes/java/test",
        "app/build/intermediates/javac/debug/classes",
        "app/build/intermediates/javac/release/classes",
    ];
    let mut roots: Vec<PathBuf> = conventional
        .iter()
        .map(|c| repo_path.join(c))
        .filter(|p| p.is_dir())
        .collect();
    if !roots.is_empty() {
        return roots;
    }
    // Fallback: walk `<repo>/build` for any directory that contains a
    // `.class` file. Pick unique parent dirs of the matches; this catches
    // older AGP layouts and unusual Gradle subprojects.
    let build = repo_path.join("build");
    if build.is_dir() {
        if let Ok(found) = walk_for_class_dirs(&build) {
            roots.extend(found);
        }
    }
    let app_build = repo_path.join("app").join("build");
    if app_build.is_dir() {
        if let Ok(found) = walk_for_class_dirs(&app_build) {
            roots.extend(found);
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn walk_for_class_dirs(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    use std::collections::BTreeSet;
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s == "class")
            {
                if let Some(parent) = path.parent() {
                    out.insert(parent.to_path_buf());
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}

fn resolve_ruleset_ref(reference: &str) -> anyhow::Result<RulesetHandle> {
    let path_form = Path::new(reference);
    if path_form.is_absolute() && path_form.is_file() {
        return Ok(RulesetHandle::Existing(path_form.to_path_buf()));
    }
    Ok(RulesetHandle::Temp(resolve_spotbugs_ruleset(reference)?))
}

enum RulesetHandle {
    Existing(PathBuf),
    Temp(tempfile::NamedTempFile),
}

impl RulesetHandle {
    fn path(&self) -> &Path {
        match self {
            RulesetHandle::Existing(p) => p,
            RulesetHandle::Temp(t) => t.path(),
        }
    }
}

fn tail(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max {
        return s.into_owned();
    }
    let start = s.len().saturating_sub(max);
    s[start..].to_string()
}

// --- Discovery --------------------------------------------------------------

fn locate_spotbugs_launcher() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("SPOTBUGS_HOME") {
        let candidate = PathBuf::from(home).join("bin").join("spotbugs");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let opt = PathBuf::from("/opt/spotbugs/bin/spotbugs");
    if opt.is_file() {
        return Some(opt);
    }
    if let Some(p) = locate_vendored_spotbugs() {
        return Some(p);
    }
    locate_on_path("spotbugs")
}

fn locate_vendored_spotbugs() -> Option<PathBuf> {
    let mut cwd = std::env::current_dir().ok()?;
    for _ in 0..6 {
        let candidate = cwd
            .join("crates")
            .join("static_analysis")
            .join("vendor")
            .join(format!("spotbugs-{SPOTBUGS_VERSION}"))
            .join("bin")
            .join("spotbugs");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !cwd.pop() {
            break;
        }
    }
    None
}

fn locate_findsecbugs_jar() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FINDSECBUGS_JAR") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut cwd = std::env::current_dir().ok()?;
    for _ in 0..6 {
        let candidate = cwd
            .join("crates")
            .join("static_analysis")
            .join("vendor")
            .join(format!("findsecbugs-plugin-{FINDSECBUGS_VERSION}.jar"));
        if candidate.is_file() {
            return Some(candidate);
        }
        if !cwd.pop() {
            break;
        }
    }
    None
}

fn locate_on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_grader_core::db::apply_schema;

    #[test]
    fn version_is_pinned() {
        assert_eq!(SPOTBUGS_VERSION, "4.8.6");
        assert_eq!(FINDSECBUGS_VERSION, "1.13.0");
    }

    #[test]
    fn requires_classes_is_true() {
        let s = SpotBugs::with_launcher(PathBuf::from("/nonexistent"), None);
        assert!(s.requires_classes());
        assert_eq!(s.id(), "spotbugs");
        assert_eq!(s.version(), "4.8.6");
    }

    #[test]
    fn empty_class_roots_yields_skipped_no_classes() {
        let s = SpotBugs::with_launcher(PathBuf::from("/nonexistent"), None);
        let tmp = tempfile::tempdir().unwrap();
        let input = AnalyzerInput {
            repo_path: tmp.path(),
            repo_full_name: "udg-pds/x",
            head_sha: None,
            source_roots: vec![],
            class_roots: vec![],
            jdk_major: 21,
            work_dir: tmp.path().to_path_buf(),
            timeout: Duration::from_secs(5),
            max_heap_mb: 256,
            locale: "en".into(),
        };
        let cfg = AnalyzerConfig {
            ruleset_ref: "beginner".into(),
            severity_floor: crate::adapter::Severity::Info,
            max_findings: 10,
        };
        let out = s.run(&input, &cfg);
        assert_eq!(out.status, AnalyzerStatus::SkippedNoClasses);
        assert!(out.diagnostics.contains("no class roots"));
    }

    #[test]
    fn latest_pr_compiled_ok_returns_true_only_with_success_row() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        // No PRs at all.
        assert!(!latest_pr_compiled_ok(&conn, "udg-pds/spring-x"));

        conn.execute(
            "INSERT INTO pull_requests (id, pr_number, repo_full_name, url, title, state, merged)
             VALUES ('p1', 1, 'udg-pds/spring-x', 'u', 't', 'closed', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pr_compilation
                (pr_id, repo_name, sprint_id, compiles, exit_code, tested_at)
             VALUES ('p1', 'spring-x', 1, 0, 1, '2026-02-15T00:00:00Z')",
            [],
        )
        .unwrap();
        assert!(!latest_pr_compiled_ok(&conn, "udg-pds/spring-x"));

        conn.execute(
            "INSERT INTO pull_requests (id, pr_number, repo_full_name, url, title, state, merged)
             VALUES ('p2', 2, 'udg-pds/spring-x', 'u', 't', 'closed', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pr_compilation
                (pr_id, repo_name, sprint_id, compiles, exit_code, tested_at)
             VALUES ('p2', 'spring-x', 1, 1, 0, '2026-02-15T00:00:00Z')",
            [],
        )
        .unwrap();
        assert!(latest_pr_compiled_ok(&conn, "udg-pds/spring-x"));
    }

    #[test]
    fn discover_class_roots_finds_spring_layout() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("build/classes/java/main/com/x")).unwrap();
        std::fs::write(
            tmp.path().join("build/classes/java/main/com/x/A.class"),
            b"x",
        )
        .unwrap();
        let roots = discover_class_roots(tmp.path());
        assert!(roots.iter().any(|p| p.ends_with("classes/java/main")));
    }

    #[test]
    fn discover_class_roots_falls_back_to_walk() {
        // Non-conventional layout: classes nested inside `build/somewhere`.
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("build/custom/module/A.class");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, b"x").unwrap();
        let roots = discover_class_roots(tmp.path());
        assert!(
            roots.iter().any(|p| p.ends_with("build/custom/module")),
            "fallback walker must surface the parent dir of orphan .class files: {:?}",
            roots
        );
    }
}
