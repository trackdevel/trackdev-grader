//! Java static-analysis stage (PMD / Checkstyle / SpotBugs) — T1 skeleton.
//!
//! Mirrors the shape of `sprint-grader-architecture` (T-P2.2/T-P3.1):
//! per-repo + per-sprint scan, blame-based per-student attribution, and a
//! `STATIC_ANALYSIS_HOTSPOT` flag in `analyze`. T1 ships only the type
//! surface and the SQL tables — no analyzer impls, no pipeline wiring.
//! Subsequent tasks (T2..T6) fill in PMD, Checkstyle, attribution,
//! pipeline+CLI, report rendering, and SpotBugs.

pub mod adapter;
pub mod attribution;
pub mod checkstyle;
pub mod config;
pub mod pmd;
pub mod presets;
pub mod sarif;

pub use adapter::{
    Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerOutput, AnalyzerStatus, Category, Finding,
    Severity,
};
pub use attribution::attribute_findings_for_repo;
pub use checkstyle::{Checkstyle, CHECKSTYLE_VERSION};
pub use config::Rules;
pub use pmd::{Pmd, PMD_VERSION};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rusqlite::{params, Connection};
use tracing::{info, warn};

/// Scan one cloned repo for one sprint: run enabled analyzers, persist
/// findings to `static_analysis_findings`, write outcome rows to
/// `static_analysis_runs`, then run blame-based per-student attribution.
///
/// Idempotent: pre-existing rows for `(repo_full_name, sprint_id)` in
/// all three tables are deleted up-front so re-runs reflect the current
/// working tree without duplicating.
///
/// Returns the number of `static_analysis_finding_attribution` rows
/// written. Any analyzer that crashes, times out, or isn't installed is
/// recorded in `static_analysis_runs` (or silently skipped, when the
/// analyzer's launcher isn't present at all) and the rest of the loop
/// continues.
pub fn scan_repo_to_db(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    rules: &Rules,
) -> rusqlite::Result<usize> {
    // Idempotency: clear all three tables for this (repo, sprint) before
    // we re-populate. The FK on `_attribution` has ON DELETE CASCADE so
    // dropping the findings rows would suffice, but explicit deletes
    // keep behaviour visible.
    conn.execute(
        "DELETE FROM static_analysis_finding_attribution
         WHERE finding_id IN (
             SELECT id FROM static_analysis_findings
             WHERE repo_full_name = ? AND sprint_id = ?
         )",
        params![repo_full_name, sprint_id],
    )?;
    conn.execute(
        "DELETE FROM static_analysis_findings
         WHERE repo_full_name = ? AND sprint_id = ?",
        params![repo_full_name, sprint_id],
    )?;
    conn.execute(
        "DELETE FROM static_analysis_runs
         WHERE repo_full_name = ? AND sprint_id = ?",
        params![repo_full_name, sprint_id],
    )?;

    let head_sha = git_head_sha(repo_path);
    let work_dir = match tempfile::Builder::new()
        .prefix("trackdev-static-analysis-")
        .tempdir()
    {
        Ok(d) => d,
        Err(e) => {
            warn!(repo = repo_full_name, error = %e, "cannot create scratch dir; skipping scan");
            return Ok(0);
        }
    };

    // PMD ----------------------------------------------------------------
    if rules.pmd.enabled {
        match Pmd::discover() {
            Some(analyzer) => {
                let cfg = AnalyzerConfig {
                    ruleset_ref: rules
                        .pmd
                        .ruleset_path
                        .clone()
                        .unwrap_or_else(|| rules.pmd.preset.clone()),
                    severity_floor: rules.severity_floor,
                    max_findings: rules.max_findings_per_analyzer,
                };
                run_analyzer_into_db(
                    conn,
                    &analyzer,
                    &cfg,
                    repo_path,
                    repo_full_name,
                    sprint_id,
                    head_sha.as_deref(),
                    rules,
                    work_dir.path(),
                    "pmd",
                    rules.pmd.heap_mb,
                )?;
            }
            None => {
                warn!(
                    repo = repo_full_name,
                    "PMD launcher not found; skipping (set PMD_HOME or run scripts/install-analyzers.sh)"
                );
            }
        }
    }

    // Checkstyle ---------------------------------------------------------
    if rules.checkstyle.enabled {
        match Checkstyle::discover() {
            Some(analyzer) => {
                let cfg = AnalyzerConfig {
                    ruleset_ref: rules
                        .checkstyle
                        .ruleset_path
                        .clone()
                        .unwrap_or_else(|| rules.checkstyle.preset.clone()),
                    severity_floor: rules.severity_floor,
                    max_findings: rules.max_findings_per_analyzer,
                };
                run_analyzer_into_db(
                    conn,
                    &analyzer,
                    &cfg,
                    repo_path,
                    repo_full_name,
                    sprint_id,
                    head_sha.as_deref(),
                    rules,
                    work_dir.path(),
                    "checkstyle",
                    rules.checkstyle.heap_mb,
                )?;
            }
            None => {
                warn!(
                    repo = repo_full_name,
                    "Checkstyle jar not found; skipping (set CHECKSTYLE_JAR or run scripts/install-analyzers.sh)"
                );
            }
        }
    }

    // SpotBugs lands in T6.

    // Attribution. Mirrors the architecture crate's lib.rs:100-111: log
    // and continue on error, so a single team's broken git repo can't
    // abort the wider pipeline.
    match attribute_findings_for_repo(conn, repo_path, repo_full_name, sprint_id) {
        Ok(n) => Ok(n),
        Err(e) => {
            warn!(
                repo = repo_full_name,
                sprint_id = sprint_id,
                error = %e,
                "static-analysis attribution failed; continuing"
            );
            Ok(0)
        }
    }
}

/// Project-level convenience: walk repo subdirectories under
/// `project_root` and call `scan_repo_to_db` for each. Mirrors
/// `architecture::scan_project_to_db`.
pub fn scan_project_to_db(
    conn: &Connection,
    project_root: &Path,
    sprint_id: i64,
    rules: &Rules,
) -> rusqlite::Result<usize> {
    if !project_root.is_dir() {
        warn!(
            path = %project_root.display(),
            "static-analysis: project dir missing"
        );
        return Ok(0);
    }
    let mut total = 0usize;
    let entries = match std::fs::read_dir(project_root) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let repo_path = entry.path();
        let bare = entry.file_name().to_string_lossy().into_owned();
        let repo_full_name = resolve_qualified_repo_name(conn, &bare).unwrap_or(bare);
        total += scan_repo_to_db(conn, &repo_path, &repo_full_name, sprint_id, rules)?;
    }
    Ok(total)
}

#[allow(clippy::too_many_arguments)]
fn run_analyzer_into_db(
    conn: &Connection,
    analyzer: &dyn Analyzer,
    cfg: &AnalyzerConfig,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    head_sha: Option<&str>,
    rules: &Rules,
    work_dir: &Path,
    analyzer_id: &str,
    heap_mb: u32,
) -> rusqlite::Result<()> {
    // Per-analyzer scratch dir so PMD's `pmd.sarif` and Checkstyle's
    // `checkstyle.sarif` can't collide.
    let analyzer_work = work_dir.join(analyzer_id);
    if let Err(e) = std::fs::create_dir_all(&analyzer_work) {
        warn!(analyzer = analyzer_id, error = %e, "cannot create analyzer scratch dir");
        return Ok(());
    }

    let input = AnalyzerInput {
        repo_path,
        repo_full_name,
        head_sha: head_sha.map(|s| s.to_string()),
        source_roots: discover_source_roots(repo_path),
        class_roots: vec![], // T6 fills this for SpotBugs.
        jdk_major: 21,
        work_dir: analyzer_work,
        timeout: Duration::from_secs(rules.timeout_seconds),
        max_heap_mb: heap_mb,
        locale: rules.locale.clone(),
    };

    info!(
        analyzer = analyzer_id,
        repo = repo_full_name,
        sprint_id,
        "running static analyzer"
    );
    let output = analyzer.run(&input, cfg);

    // Sort findings deterministically so re-runs INSERT in the same order
    // and `diff-db --derived-only` doesn't false-positive across runs.
    let mut findings = output.findings;
    findings.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });
    let findings_count = findings.len() as i64;

    if matches!(output.status, AnalyzerStatus::Ok) {
        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO static_analysis_findings
                (repo_full_name, sprint_id, analyzer, analyzer_version, rule_id,
                 category, severity, file_path, start_line, end_line, message,
                 help_uri, fingerprint, head_sha)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?;
        for f in &findings {
            stmt.execute(params![
                repo_full_name,
                sprint_id,
                f.analyzer,
                analyzer.version(),
                f.rule_id,
                f.category.as_str(),
                f.severity.as_str(),
                f.file_path,
                f.start_line.map(|n| n as i64),
                f.end_line.map(|n| n as i64),
                f.message,
                f.help_uri,
                f.fingerprint,
                head_sha,
            ])?;
        }
    }

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    conn.execute(
        "INSERT OR REPLACE INTO static_analysis_runs
            (repo_full_name, sprint_id, analyzer, status, findings_count,
             duration_ms, head_sha, diagnostics, ran_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            repo_full_name,
            sprint_id,
            analyzer_id,
            output.status.as_str(),
            findings_count,
            output.duration_ms as i64,
            head_sha,
            output.diagnostics,
            now,
        ],
    )?;

    if !matches!(output.status, AnalyzerStatus::Ok) {
        warn!(
            analyzer = analyzer_id,
            repo = repo_full_name,
            status = output.status.as_str(),
            duration_ms = output.duration_ms,
            "analyzer did not complete cleanly; diagnostics persisted to static_analysis_runs"
        );
    } else {
        info!(
            analyzer = analyzer_id,
            repo = repo_full_name,
            findings = findings_count,
            duration_ms = output.duration_ms,
            "analyzer ok"
        );
    }
    Ok(())
}

fn discover_source_roots(repo_path: &Path) -> Vec<PathBuf> {
    // Conventional Java source roots for Spring Boot + Android Gradle.
    // Falls back to the repo root when no convention matches — the
    // analyzer walks the whole tree but caps duplicate work via PMD's
    // own caching (we disabled it) or Checkstyle's recursion.
    let candidates = [
        "src/main/java",
        "src/test/java",
        "app/src/main/java",
        "app/src/test/java",
    ];
    let mut roots: Vec<PathBuf> = candidates
        .iter()
        .map(|c| repo_path.join(c))
        .filter(|p| p.is_dir())
        .collect();
    if roots.is_empty() {
        roots.push(repo_path.to_path_buf());
    }
    roots
}

fn git_head_sha(repo_path: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.trim().to_string())
}

/// Look up the `<org>/<repo>` form for a bare repo directory name by
/// matching against `pull_requests.repo_full_name`. Returns `None` if no
/// PR row references this repo (e.g. fresh project with no PRs yet).
/// Mirrors `architecture::resolve_qualified_repo_name`.
fn resolve_qualified_repo_name(conn: &Connection, bare: &str) -> Option<String> {
    let like = format!("%/{}", bare);
    conn.query_row(
        "SELECT repo_full_name FROM pull_requests
         WHERE repo_full_name = ? OR repo_full_name LIKE ?
         ORDER BY (repo_full_name = ?) DESC, length(repo_full_name) DESC
         LIMIT 1",
        params![bare, like, bare],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .filter(|s| s.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_grader_core::db::apply_schema;

    #[test]
    fn scan_repo_to_db_is_idempotent_with_disabled_analyzers() {
        // Empty repo + all analyzers disabled → no findings, no runs.
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut rules = Rules::default();
        rules.pmd.enabled = false;
        rules.checkstyle.enabled = false;
        rules.spotbugs.enabled = false;

        let n1 = scan_repo_to_db(&conn, tmp.path(), "udg/x", 1, &rules).unwrap();
        let n2 = scan_repo_to_db(&conn, tmp.path(), "udg/x", 1, &rules).unwrap();
        assert_eq!(n1, 0);
        assert_eq!(n2, 0);

        let runs: i64 = conn
            .query_row("SELECT COUNT(*) FROM static_analysis_runs", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(runs, 0);
    }

    #[test]
    fn discover_source_roots_falls_back_to_repo_root() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = discover_source_roots(tmp.path());
        assert_eq!(roots, vec![tmp.path().to_path_buf()]);
    }

    #[test]
    fn discover_source_roots_picks_conventional_paths() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/main/java/com/x")).unwrap();
        std::fs::create_dir_all(tmp.path().join("app/src/main/java/com/y")).unwrap();
        let roots = discover_source_roots(tmp.path());
        assert!(roots.iter().any(|p| p.ends_with("src/main/java")));
        assert!(roots.iter().any(|p| p.ends_with("app/src/main/java")));
    }

    #[test]
    fn scan_project_to_db_returns_zero_for_missing_dir() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        let n = scan_project_to_db(&conn, Path::new("/no/such/dir"), 1, &Rules::default()).unwrap();
        assert_eq!(n, 0);
    }
}
