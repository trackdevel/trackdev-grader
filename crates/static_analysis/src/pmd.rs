//! PMD 7.7.0 adapter. Source-only, no compile dependency. Invoked via
//! the official `pmd check` CLI; SARIF output flows through the shared
//! `sarif::parse` ingest path.
//!
//! ## Discovery order
//!
//! 1. `$PMD_HOME/bin/pmd` (env override)
//! 2. `/opt/pmd/bin/pmd`
//! 3. `crates/static_analysis/vendor/pmd-bin-7.7.0/bin/pmd` (vendored)
//! 4. `pmd` resolved against `$PATH`
//!
//! `discover()` returns the first hit; the smoke test (and `T5`'s
//! orchestration block) treat a `None` as `SKIPPED` rather than a hard
//! failure — the pipeline must keep running even on hosts without PMD
//! installed.
//!
//! ## Exit code 4 is success
//!
//! PMD 7 exits 4 when violations are found, regardless of `--no-fail-on-violation`
//! semantics in earlier major versions. We whitelist exit 0 and 4; any other
//! code is `Crashed` with the stderr tail in `diagnostics`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::adapter::{Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerOutput, AnalyzerStatus};
use crate::presets::resolve_pmd_ruleset;
use crate::sarif;

/// Pinned PMD version. Reproducibility anchor written to
/// `static_analysis_runs.diagnostics` and surfaced in reports. Keep in
/// sync with `scripts/install-analyzers.sh`.
pub const PMD_VERSION: &str = "7.7.0";

/// PMD analyzer. Holds a discovered launcher path so callers don't pay
/// the discovery cost per invocation.
pub struct Pmd {
    launcher: PathBuf,
}

impl Pmd {
    /// Run discovery and return a ready-to-use adapter. Returns `None`
    /// when no PMD launcher is reachable — the caller decides whether to
    /// log-and-skip or treat as fatal.
    pub fn discover() -> Option<Self> {
        let launcher = locate_pmd_launcher()?;
        Some(Self { launcher })
    }

    /// Construct from an explicit launcher path (escape hatch for tests).
    pub fn with_launcher(launcher: PathBuf) -> Self {
        Self { launcher }
    }
}

impl Analyzer for Pmd {
    fn id(&self) -> &'static str {
        "pmd"
    }

    fn version(&self) -> &str {
        PMD_VERSION
    }

    fn requires_classes(&self) -> bool {
        false
    }

    fn run(&self, input: &AnalyzerInput, cfg: &AnalyzerConfig) -> AnalyzerOutput {
        // Resolve the ruleset XML to a concrete file path, either from a
        // preset or directly from a user-supplied absolute path.
        let resolved_ruleset = match resolve_ruleset_ref(&cfg.ruleset_ref) {
            Ok(r) => r,
            Err(e) => {
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: None,
                    duration_ms: 0,
                    diagnostics: format!("ruleset resolution failed: {e}"),
                };
            }
        };

        let sarif_path = input.work_dir.join("pmd.sarif");

        // Source roots are joined with `,` per PMD CLI convention. Falling
        // back to the repo path means we still run usefully even when the
        // caller didn't pre-compute roots.
        let sources_arg = if input.source_roots.is_empty() {
            input.repo_path.to_string_lossy().to_string()
        } else {
            input
                .source_roots
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(",")
        };

        let mut cmd = Command::new(&self.launcher);
        cmd.arg("check")
            .arg("-d")
            .arg(&sources_arg)
            .arg("-R")
            .arg(resolved_ruleset.path())
            .arg("-f")
            .arg("sarif")
            .arg("-r")
            .arg(&sarif_path)
            .arg("--no-cache")
            .arg("--no-fail-on-violation")
            .arg("--no-progress")
            .arg("--threads")
            .arg("1")
            .env(
                "JAVA_TOOL_OPTIONS",
                format!("-Xmx{}m", input.max_heap_mb.max(64)),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!(launcher = %self.launcher.display(), sources = %sources_arg, "spawning PMD");
        let started = Instant::now();
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    diagnostics: format!(
                        "failed to spawn PMD launcher `{}`: {}",
                        self.launcher.display(),
                        e
                    ),
                };
            }
        };

        // Poll-based timeout — same shape as `architecture_llm::cli_judge`.
        // Avoids pulling in tokio for one process.
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
                                "PMD timed out after {}s",
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
                        diagnostics: format!("waiting on PMD: {e}"),
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
                    diagnostics: format!("collecting PMD output: {e}"),
                };
            }
        };

        let exit = output.status.code().unwrap_or(-1);
        let stderr_tail = tail(&output.stderr, 4_000);
        let duration_ms = started.elapsed().as_millis() as u64;

        // Whitelist 0 (no violations) and 4 (violations found).
        if exit != 0 && exit != 4 {
            warn!(exit, "PMD exited with non-success code");
            return AnalyzerOutput {
                status: AnalyzerStatus::Crashed,
                findings: vec![],
                raw_report: Some(sarif_path),
                duration_ms,
                diagnostics: format!("PMD exited {}; stderr tail:\n{}", exit, stderr_tail),
            };
        }

        let findings = match sarif::parse(&sarif_path, Some(input.repo_path)) {
            Ok(mut fs) => {
                // Drop sub-floor severity findings before INSERT.
                fs.retain(|f| f.severity.at_least(cfg.severity_floor));
                if fs.len() > cfg.max_findings {
                    debug!(
                        kept = cfg.max_findings,
                        total = fs.len(),
                        "PMD finding count exceeds max_findings; truncating"
                    );
                    fs.truncate(cfg.max_findings);
                }
                fs
            }
            Err(e) => {
                return AnalyzerOutput {
                    status: AnalyzerStatus::Crashed,
                    findings: vec![],
                    raw_report: Some(sarif_path),
                    duration_ms,
                    diagnostics: format!("parsing PMD SARIF: {e}; stderr:\n{stderr_tail}"),
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

/// Resolve a `ruleset_ref` (preset name or absolute path) to a concrete
/// XML path on disk. Returns a `RulesetHandle` that owns any temp file it
/// materialised, so the caller's stack pins it for PMD's duration.
fn resolve_ruleset_ref(reference: &str) -> anyhow::Result<RulesetHandle> {
    let path_form = Path::new(reference);
    if path_form.is_absolute() && path_form.is_file() {
        return Ok(RulesetHandle::Existing(path_form.to_path_buf()));
    }
    Ok(RulesetHandle::Temp(resolve_pmd_ruleset(reference)?))
}

/// Owns either an existing on-disk ruleset or a temp file that must
/// outlive the PMD process.
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

fn locate_pmd_launcher() -> Option<PathBuf> {
    // 1. PMD_HOME env override.
    if let Some(home) = std::env::var_os("PMD_HOME") {
        let candidate = PathBuf::from(home).join("bin").join("pmd");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // 2. Conventional system location.
    let opt = PathBuf::from("/opt/pmd/bin/pmd");
    if opt.is_file() {
        return Some(opt);
    }
    // 3. Vendored under the workspace. We can't rely on CARGO_MANIFEST_DIR
    //    at runtime (the orchestration binary is built from a different
    //    crate), so walk up from CWD looking for the workspace marker.
    if let Some(p) = locate_vendored_pmd() {
        return Some(p);
    }
    // 4. PATH lookup.
    locate_on_path("pmd")
}

fn locate_vendored_pmd() -> Option<PathBuf> {
    let mut cwd = std::env::current_dir().ok()?;
    for _ in 0..6 {
        let candidate = cwd
            .join("crates")
            .join("static_analysis")
            .join("vendor")
            .join(format!("pmd-bin-{PMD_VERSION}"))
            .join("bin")
            .join("pmd");
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

    #[test]
    fn version_is_pinned() {
        assert_eq!(PMD_VERSION, "7.7.0");
    }

    #[test]
    fn requires_classes_is_false() {
        let p = Pmd::with_launcher(PathBuf::from("/nonexistent"));
        assert!(!p.requires_classes());
        assert_eq!(p.id(), "pmd");
        assert_eq!(p.version(), "7.7.0");
    }

    #[test]
    fn missing_launcher_returns_crashed() {
        let p = Pmd::with_launcher(PathBuf::from("/definitely/not/a/real/pmd"));
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
        let out = p.run(&input, &cfg);
        assert_eq!(out.status, AnalyzerStatus::Crashed);
        assert!(out.diagnostics.contains("spawn"));
    }

    #[test]
    fn unknown_ruleset_is_rejected() {
        // Don't actually need PMD to test ruleset resolution; the run
        // bails before spawning.
        match resolve_ruleset_ref("not-a-preset") {
            Ok(_) => panic!("expected unknown preset to fail"),
            Err(e) => assert!(e.to_string().contains("unknown PMD preset")),
        }
    }

    /// Smoke test for tail truncation — bounds the diagnostics field so
    /// we don't dump megabytes of stderr into the DB.
    #[test]
    fn tail_truncates_long_buffers() {
        let body = b"a".repeat(10_000);
        let t = tail(&body, 100);
        assert_eq!(t.len(), 100);
    }
}
