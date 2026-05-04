//! Checkstyle 10.x adapter. Source-only, no compile dependency. Reuses
//! the shared SARIF parser from `crates/static_analysis/src/sarif.rs` —
//! only the CLI invocation, exit-code interpretation, and preset XML
//! differ from PMD.
//!
//! ## Discovery order
//!
//! 1. `$CHECKSTYLE_JAR` (env override — full path to the `*-all.jar`)
//! 2. `/opt/checkstyle/checkstyle-*-all.jar`
//! 3. `crates/static_analysis/vendor/checkstyle-*-all.jar`
//!
//! Unlike PMD there is no `checkstyle` shim binary, so no `$PATH`
//! lookup. `discover()` returns `None` when no jar is reachable; the
//! caller logs and continues.
//!
//! ## Exit-code semantics
//!
//! Checkstyle returns non-zero on **either** error or violations and
//! doesn't easily distinguish the two. We disambiguate by checking
//! whether the SARIF file was produced and parses: yes → `Ok`
//! regardless of exit code, no → `Crashed`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::adapter::{Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerOutput, AnalyzerStatus};
use crate::presets::resolve_checkstyle_ruleset;
use crate::sarif;

/// Pinned Checkstyle version. Reproducibility anchor — kept in sync with
/// `scripts/install-analyzers.sh`. The host installation may differ;
/// `version()` reports this constant unconditionally so the report is
/// honest about what we *expect* even when discovery picked up a
/// different jar.
pub const CHECKSTYLE_VERSION: &str = "10.20.0";

pub struct Checkstyle {
    jar: PathBuf,
}

impl Checkstyle {
    pub fn discover() -> Option<Self> {
        Some(Self {
            jar: locate_checkstyle_jar()?,
        })
    }

    pub fn with_jar(jar: PathBuf) -> Self {
        Self { jar }
    }
}

impl Analyzer for Checkstyle {
    fn id(&self) -> &'static str {
        "checkstyle"
    }

    fn version(&self) -> &str {
        CHECKSTYLE_VERSION
    }

    fn requires_classes(&self) -> bool {
        false
    }

    fn run(&self, input: &AnalyzerInput, cfg: &AnalyzerConfig) -> AnalyzerOutput {
        let started = Instant::now();
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

        let sarif_path = input.work_dir.join("checkstyle.sarif");

        // Source roots: pass each as a positional path. Checkstyle walks
        // directories recursively. Falling back to the repo path keeps
        // the adapter usable when callers haven't pre-computed roots.
        let source_roots: Vec<PathBuf> = if input.source_roots.is_empty() {
            vec![input.repo_path.to_path_buf()]
        } else {
            input.source_roots.clone()
        };

        // Locale: forward "es"/"ca" as `-Duser.language=...` so localised
        // bundled messages, where available, surface in Spanish/Catalan.
        let mut java_opts = format!("-Xmx{}m", input.max_heap_mb.max(64));
        if matches!(input.locale.as_str(), "es" | "ca") {
            java_opts.push_str(" -Duser.language=");
            java_opts.push_str(&input.locale);
        }

        let mut cmd = Command::new("java");
        cmd.arg("-jar")
            .arg(&self.jar)
            .arg("-c")
            .arg(resolved_ruleset.path())
            .arg("-f")
            .arg("sarif")
            .arg("-o")
            .arg(&sarif_path);
        for src in &source_roots {
            cmd.arg(src);
        }
        cmd.env("JAVA_TOOL_OPTIONS", &java_opts)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!(jar = %self.jar.display(), "spawning Checkstyle");
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    diagnostics: format!(
                        "failed to spawn java for Checkstyle (jar={}): {}",
                        self.jar.display(),
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
                                "Checkstyle timed out after {}s",
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
                        diagnostics: format!("waiting on Checkstyle: {e}"),
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
                    diagnostics: format!("collecting Checkstyle output: {e}"),
                };
            }
        };

        let exit = output.status.code().unwrap_or(-1);
        let stderr_tail = tail(&output.stderr, 4_000);
        let duration_ms = started.elapsed().as_millis() as u64;

        // Disambiguate: if the SARIF file parses, we got real findings
        // regardless of exit code. Only when the file is missing or
        // unparseable do we treat as Crashed.
        let findings = match sarif::parse(&sarif_path) {
            Ok(mut fs) => {
                fs.retain(|f| f.severity.at_least(cfg.severity_floor));
                if fs.len() > cfg.max_findings {
                    debug!(
                        kept = cfg.max_findings,
                        total = fs.len(),
                        "Checkstyle finding count exceeds max_findings; truncating"
                    );
                    fs.truncate(cfg.max_findings);
                }
                fs
            }
            Err(e) => {
                warn!(exit, "Checkstyle SARIF unreadable; treating as Crashed");
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: Some(sarif_path),
                    duration_ms,
                    diagnostics: format!(
                        "Checkstyle exit={}, SARIF parse failed: {}; stderr tail:\n{}",
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

fn resolve_ruleset_ref(reference: &str) -> anyhow::Result<RulesetHandle> {
    let path_form = Path::new(reference);
    if path_form.is_absolute() && path_form.is_file() {
        return Ok(RulesetHandle::Existing(path_form.to_path_buf()));
    }
    Ok(RulesetHandle::Temp(resolve_checkstyle_ruleset(reference)?))
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

fn locate_checkstyle_jar() -> Option<PathBuf> {
    // 1. Env override.
    if let Some(path) = std::env::var_os("CHECKSTYLE_JAR") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    // 2. /opt/checkstyle/*all.jar
    if let Some(p) = first_matching_jar(Path::new("/opt/checkstyle"), "checkstyle-", "-all.jar") {
        return Some(p);
    }
    // 3. Vendored alongside the workspace.
    locate_vendored_checkstyle()
}

fn locate_vendored_checkstyle() -> Option<PathBuf> {
    let mut cwd = std::env::current_dir().ok()?;
    for _ in 0..6 {
        let vendor = cwd.join("crates").join("static_analysis").join("vendor");
        if let Some(p) = first_matching_jar(&vendor, "checkstyle-", "-all.jar") {
            return Some(p);
        }
        if !cwd.pop() {
            break;
        }
    }
    None
}

fn first_matching_jar(dir: &Path, prefix: &str, suffix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut matches: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix))
        })
        .collect();
    // Sort so the highest-versioned match wins on lexicographic order
    // (works fine for `checkstyle-10.x.y-all.jar` while we stay on 10.x).
    matches.sort();
    matches.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_pinned() {
        assert_eq!(CHECKSTYLE_VERSION, "10.20.0");
    }

    #[test]
    fn requires_classes_is_false() {
        let cs = Checkstyle::with_jar(PathBuf::from("/nonexistent"));
        assert!(!cs.requires_classes());
        assert_eq!(cs.id(), "checkstyle");
        assert_eq!(cs.version(), "10.20.0");
    }

    #[test]
    fn missing_jar_returns_crashed() {
        let cs = Checkstyle::with_jar(PathBuf::from("/definitely/not/a/real/checkstyle.jar"));
        let tmp = tempfile::tempdir().unwrap();
        let input = AnalyzerInput {
            repo_path: tmp.path(),
            repo_full_name: "udg-pds/empty",
            head_sha: None,
            source_roots: vec![tmp.path().to_path_buf()],
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
        let out = cs.run(&input, &cfg);
        // Java will start but fail to load the missing jar; SARIF won't
        // parse → Crashed.
        assert_eq!(out.status, AnalyzerStatus::Crashed);
    }

    #[test]
    fn unknown_ruleset_is_rejected() {
        match resolve_ruleset_ref("not-a-preset") {
            Ok(_) => panic!("expected unknown preset to fail"),
            Err(e) => assert!(e.to_string().contains("unknown Checkstyle preset")),
        }
    }

    #[test]
    fn first_matching_jar_picks_highest_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("checkstyle-10.10.0-all.jar"), b"x").unwrap();
        std::fs::write(dir.path().join("checkstyle-10.20.0-all.jar"), b"x").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();
        let pick = first_matching_jar(dir.path(), "checkstyle-", "-all.jar").unwrap();
        assert!(pick.to_string_lossy().contains("10.20.0"));
    }
}
