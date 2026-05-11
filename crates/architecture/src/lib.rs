//! Architecture conformance scanning (T-P2.2 / T-P3.1 / T-P3.4).
//!
//! Replaces the manual "did the team respect the layered architecture"
//! review pass with a course-config-driven scan. Reads the rules from
//! `architecture.toml`, walks every Java file in the cloned repo,
//! extracts (package, imports), and writes one `architecture_violations`
//! row per (file × broken rule × offending import × line range).
//!
//! ## Artifact-shape (T-P3.4)
//!
//! The scan grades **the code as delivered on `main`**, not the
//! per-sprint trajectory. Each `(repo_full_name)` produces one set of
//! violation rows; sprint provenance for each violation lives on
//! `architecture_violations.introduced_sprint_id`, derived from blame.
//! Re-runs against an unchanged `head_sha` skip cleanly via
//! `architecture_runs`.

pub mod ast_rules;
pub mod attribution;
pub mod checker;
pub mod glob;
pub mod rubric;
pub mod rules;
pub mod scanner;

pub use ast_rules::{check_java_file as check_java_ast, AstRule, AstRuleKind};
pub use attribution::attribute_violations_for_repo;
pub use checker::{check_file, check_repo, Violation, ViolationKind};
pub use glob::PackagePattern;
pub use rubric::Rubric;
pub use rules::{ArchitectureRules, Forbidden, Layer};
pub use scanner::{parse_java, scan_repo, ImportLine, JavaFileFacts};

use std::path::Path;
use std::time::Instant;

use rusqlite::{params, Connection};
use sprint_grader_core::finding::{LineSpan, RuleFinding, RuleKind, Severity};
use tracing::{info, warn};

const STATUS_OK: &str = "OK";
const STATUS_SKIPPED_HEAD_UNCHANGED: &str = "SKIPPED_HEAD_UNCHANGED";
const STATUS_SKIPPED_NO_SOURCES: &str = "SKIPPED_NO_SOURCES";

/// `git rev-parse HEAD` for `repo_path`. Returns `None` when not a git
/// working tree or git is missing on `$PATH`.
fn git_head_sha(repo_path: &Path) -> Option<String> {
    let path = repo_path.to_str()?;
    let out = std::process::Command::new("git")
        .args(["-C", path, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Last successful run's recorded HEAD sha for this repo, or `None` when
/// no `OK` row exists. Compared against `git_head_sha` to short-circuit
/// the rescan.
fn cached_head_sha(conn: &Connection, repo_full_name: &str) -> Option<String> {
    conn.query_row(
        "SELECT head_sha FROM architecture_runs
         WHERE repo_full_name = ? AND status = ?",
        params![repo_full_name, STATUS_OK],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

#[allow(clippy::too_many_arguments)]
fn record_run(
    conn: &Connection,
    repo_full_name: &str,
    status: &str,
    findings_count: usize,
    duration_ms: i64,
    head_sha: Option<&str>,
    diagnostics: Option<&str>,
) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO architecture_runs
            (repo_full_name, status, findings_count,
             duration_ms, head_sha, diagnostics, ran_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            repo_full_name,
            status,
            findings_count as i64,
            duration_ms,
            head_sha,
            diagnostics,
            now,
        ],
    )?;
    Ok(())
}

/// Scan one cloned repo, persist violations + attribution. Idempotent at
/// `(repo_full_name)` granularity: pre-existing rows for the repo are
/// dropped before re-insert. Short-circuits when `architecture_runs`
/// records the same `head_sha` as the working tree currently has — the
/// prior `OK` row's findings remain valid and stay in the DB untouched.
pub fn scan_repo_to_db(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    rules: &ArchitectureRules,
) -> rusqlite::Result<usize> {
    let started = Instant::now();
    let head = git_head_sha(repo_path);

    // Head-sha skip: cached HEAD matches current working tree.
    if let (Some(current), Some(cached)) = (head.as_deref(), cached_head_sha(conn, repo_full_name))
    {
        if current == cached {
            let kept: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM architecture_violations
                     WHERE repo_full_name = ?",
                    params![repo_full_name],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            record_run(
                conn,
                repo_full_name,
                STATUS_SKIPPED_HEAD_UNCHANGED,
                kept as usize,
                started.elapsed().as_millis() as i64,
                Some(current),
                None,
            )?;
            info!(
                repo = repo_full_name,
                head = %current,
                cached = kept,
                "architecture scan skipped (head unchanged)"
            );
            return Ok(0);
        }
    }

    // The architecture stage used to write `repo_full_name` as the bare
    // directory name (e.g. `spring-pds26_4c`); it now writes the
    // org-qualified `<org>/<repo>` form. Delete BOTH the qualified form
    // (current writes) and the bare basename (legacy writes) before
    // re-inserting so stale rows can't linger.
    let bare = repo_full_name.rsplit('/').next().unwrap_or(repo_full_name);
    conn.execute(
        "DELETE FROM architecture_violation_attribution
         WHERE violation_rowid IN (
             SELECT rowid FROM architecture_violations
             WHERE repo_full_name = ? OR repo_full_name = ?
         )",
        params![repo_full_name, bare],
    )?;
    conn.execute(
        "DELETE FROM architecture_violations
         WHERE repo_full_name = ? OR repo_full_name = ?",
        params![repo_full_name, bare],
    )?;

    let files = scan_repo(repo_path);
    if files.is_empty() {
        record_run(
            conn,
            repo_full_name,
            STATUS_SKIPPED_NO_SOURCES,
            0,
            started.elapsed().as_millis() as i64,
            head.as_deref(),
            None,
        )?;
        info!(repo = repo_full_name, "architecture scan: no sources");
        return Ok(0);
    }

    let mut written = 0usize;
    for file in &files {
        // Legacy package-glob and forbidden-import rules.
        for v in check_file(rules, file) {
            insert_violation(conn, repo_full_name, &rules.severity, &v)?;
            written += 1;
        }
        // AST rules (T-P3.1).
        if !rules.ast_rules.is_empty() {
            let abs = repo_path.join(&file.rel_path);
            if let Ok(src) = std::fs::read(&abs) {
                let ast_violations = ast_rules::check_java_file(
                    &rules.ast_rules,
                    &file.rel_path,
                    &file.package,
                    &src,
                );
                for v in ast_violations {
                    insert_violation(conn, repo_full_name, &rules.severity, &v)?;
                    written += 1;
                }
            }
        }
    }

    // Blame attribution runs once per repo — one git invocation per file
    // regardless of how many violations point into it. Also fills
    // `introduced_sprint_id` on each violation row.
    let attributed =
        match attribution::attribute_violations_for_repo(conn, repo_path, repo_full_name) {
            Ok(n) => n,
            Err(e) => {
                warn!(
                    repo = repo_full_name,
                    error = %e,
                    "blame attribution failed; continuing without it"
                );
                0
            }
        };

    record_run(
        conn,
        repo_full_name,
        STATUS_OK,
        written,
        started.elapsed().as_millis() as i64,
        head.as_deref(),
        None,
    )?;

    info!(
        repo = repo_full_name,
        files = files.len(),
        violations = written,
        attribution_rows = attributed,
        head = head.as_deref().unwrap_or("(none)"),
        "architecture scan complete"
    );
    Ok(written)
}

fn insert_violation(
    conn: &Connection,
    repo_full_name: &str,
    default_severity: &str,
    v: &Violation,
) -> rusqlite::Result<()> {
    let rule_kind = v.kind.as_str();
    conn.execute(
        "INSERT OR REPLACE INTO architecture_violations
            (repo_full_name, file_path, rule_name,
             violation_kind, offending_import, severity,
             start_line, end_line, rule_kind)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            repo_full_name,
            v.file_path,
            v.rule_name,
            rule_kind,
            v.offending_import,
            default_severity,
            v.start_line.map(|n| n as i64),
            v.end_line.map(|n| n as i64),
            rule_kind,
        ],
    )?;
    Ok(())
}

/// Scan every directory under `project_root` that looks like a cloned
/// repo. Returns the total violation count across all repos. Skips
/// silently when the project directory or its repo subdirs are missing.
pub fn scan_project_to_db(
    conn: &Connection,
    project_root: &Path,
    rules: &ArchitectureRules,
) -> rusqlite::Result<usize> {
    if !project_root.is_dir() {
        warn!(
            path = %project_root.display(),
            "architecture scan: project dir missing"
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
        total += scan_repo_to_db(conn, &repo_path, &repo_full_name, rules)?;
    }
    Ok(total)
}

/// W2.T1: read every `architecture_violations` row for `repo_full_name`
/// and convert each into a shared `RuleFinding`. The renderer
/// unification in W2.T5 will consume this in place of the per-crate
/// `AttributedArchViolation` SELECT currently inlined in
/// `crates/report/src/markdown.rs`.
///
/// Path safety: the architecture stage writes repo-relative paths to
/// `file_path` (see `scanner::scan_repo`), so the value passes through
/// `RuleFinding::file_repo_relative` unchanged. If a malformed legacy
/// row contains an absolute path the renderer's debug-assert in
/// `report::url::github_blob_url` will catch it.
pub fn load_rule_findings_for_repo(
    conn: &Connection,
    repo_full_name: &str,
) -> rusqlite::Result<Vec<RuleFinding>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, rule_name, offending_import, severity,
                start_line, end_line, COALESCE(explanation, '')
         FROM architecture_violations
         WHERE repo_full_name = ?
         ORDER BY file_path, start_line, rule_name, offending_import",
    )?;
    let rows = stmt.query_map(params![repo_full_name], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, String>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (file, rule_name, offending, severity_s, s_line, e_line, explanation) = row?;
        let span = build_span(s_line, e_line);
        out.push(RuleFinding {
            rule_id: rule_name,
            kind: RuleKind::Architecture,
            severity: parse_severity(&severity_s),
            repo_full_name: repo_full_name.to_string(),
            file_repo_relative: file,
            span,
            evidence: explanation,
            extra: Some(offending),
        });
    }
    Ok(out)
}

fn build_span(start: Option<i64>, end: Option<i64>) -> LineSpan {
    match (start, end) {
        (Some(s), Some(e)) if e > s && s >= 1 && e >= 1 => {
            // start ≥ 1 / end ≥ 1 guarded above; the casts are safe.
            LineSpan::range(s as u32, e as u32)
        }
        (Some(s), _) if s >= 1 => LineSpan::single(s as u32),
        _ => LineSpan::single(0),
    }
}

fn parse_severity(s: &str) -> Severity {
    match s.to_ascii_uppercase().as_str() {
        "CRITICAL" | "ERROR" => Severity::Critical,
        "INFO" | "INFORMATIONAL" | "NOTICE" => Severity::Info,
        _ => Severity::Warning,
    }
}

/// Look up the `<org>/<repo>` form for a bare repo directory name by
/// matching against `pull_requests.repo_full_name`. Returns `None` if no
/// PR row references this repo (e.g. fresh project with no PRs yet).
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
    use std::fs;
    use tempfile::TempDir;

    fn write_java(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    /// Initialise `dir` as a git repo with a single commit so the
    /// architecture scan's head_sha lookup succeeds.
    fn git_init(dir: &Path) {
        use std::process::Command;
        let run = |args: &[&str]| {
            let s = Command::new("git")
                .args(args)
                .current_dir(dir)
                .status()
                .expect("git invocation");
            assert!(s.success(), "git {:?} failed", args);
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
    }

    const RULES: &str = r#"
[[layers]]
name = "controller"
packages = ["**/controller/**"]
may_depend_on = ["service"]

[[layers]]
name = "service"
packages = ["**/service/**"]
may_depend_on = []

[[layers]]
name = "repository"
packages = ["**/repository/**"]
may_depend_on = []
"#;

    #[test]
    fn end_to_end_controller_skipping_service_is_flagged() {
        let tmp = TempDir::new().unwrap();
        write_java(
            tmp.path(),
            "src/main/java/com/x/controller/UserController.java",
            "package com.x.controller;\n\
             import com.x.repository.UserRepository;\n\
             public class UserController {}",
        );
        git_init(tmp.path());
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();

        let n = scan_repo_to_db(&conn, tmp.path(), "udg/spring-x", &rules).unwrap();
        assert_eq!(n, 1, "controller→repository must produce one violation");

        let kind: String = conn
            .query_row(
                "SELECT violation_kind FROM architecture_violations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "layer_dependency");

        // architecture_runs must record the OK row with the current head_sha.
        let (status, findings, head): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT status, findings_count, head_sha FROM architecture_runs
                 WHERE repo_full_name = ?",
                ["udg/spring-x"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "OK");
        assert_eq!(findings, 1);
        assert!(head.is_some(), "head_sha must be recorded");
    }

    #[test]
    fn rerun_purges_legacy_bare_name_rows() {
        let tmp = TempDir::new().unwrap();
        write_java(
            tmp.path(),
            "src/main/java/com/x/controller/Bad.java",
            "package com.x.controller;\n\
             import com.x.repository.UserRepository;\n\
             public class Bad {}",
        );
        git_init(tmp.path());
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();

        // Seed a bare-name row from a hypothetical pre-qualifier-fix run.
        conn.execute(
            "INSERT INTO architecture_violations
                (repo_full_name, file_path, rule_name, violation_kind,
                 offending_import, severity, start_line, end_line, rule_kind)
             VALUES
                ('spring-x', 'OldFile.java', 'domain->!infrastructure',
                 'layer_dependency', 'jakarta.persistence.Entity', 'WARNING',
                 1, 1, 'layer_dependency')",
            [],
        )
        .unwrap();

        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();
        scan_repo_to_db(&conn, tmp.path(), "udg-x/spring-x", &rules).unwrap();
        let stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM architecture_violations
                 WHERE repo_full_name = 'spring-x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0, "legacy bare-name rows must be purged on re-scan");
    }

    #[test]
    fn rerun_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        write_java(
            tmp.path(),
            "src/main/java/com/x/controller/Bad.java",
            "package com.x.controller;\n\
             import com.x.repository.UserRepository;\n\
             public class Bad {}",
        );
        git_init(tmp.path());
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();
        let first = scan_repo_to_db(&conn, tmp.path(), "udg/x", &rules).unwrap();
        // Second run: head_sha unchanged, returns 0 (skipped) but findings preserved.
        let second = scan_repo_to_db(&conn, tmp.path(), "udg/x", &rules).unwrap();
        assert_eq!(first, 1, "first scan writes the violation");
        assert_eq!(second, 0, "second scan skips on head_sha match");
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM architecture_violations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(total, 1, "head-sha skip preserves the prior row");

        // architecture_runs should now have a SKIPPED_HEAD_UNCHANGED row
        // (the OK row is overwritten by INSERT OR REPLACE on the same key).
        let status: String = conn
            .query_row(
                "SELECT status FROM architecture_runs WHERE repo_full_name = 'udg/x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "SKIPPED_HEAD_UNCHANGED");
    }

    #[test]
    fn scan_project_qualifies_repo_name_from_pull_requests() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("team-x");
        let repo = project.join("spring-x");
        write_java(
            &repo,
            "src/main/java/com/x/controller/Bad.java",
            "package com.x.controller;\n\
             import com.x.repository.UserRepository;\n\
             public class Bad {}",
        );
        git_init(&repo);

        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO pull_requests (id, repo_full_name) VALUES ('p1', 'udg-3c/spring-x')",
            [],
        )
        .unwrap();
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();
        scan_project_to_db(&conn, &project, &rules).unwrap();

        let stored: String = conn
            .query_row(
                "SELECT repo_full_name FROM architecture_violations LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored, "udg-3c/spring-x",
            "scan_project_to_db must persist <org>/<repo> when collect knows it"
        );
    }

    #[test]
    fn scan_project_falls_back_to_bare_name_when_no_pr_row() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("team-y");
        let repo = project.join("spring-y");
        write_java(
            &repo,
            "src/main/java/com/x/controller/Bad.java",
            "package com.x.controller;\n\
             import com.x.repository.UserRepository;\n\
             public class Bad {}",
        );
        git_init(&repo);
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();
        scan_project_to_db(&conn, &project, &rules).unwrap();
        let stored: String = conn
            .query_row(
                "SELECT repo_full_name FROM architecture_violations LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, "spring-y");
    }

    #[test]
    fn into_rule_finding_carries_span_severity_and_extra() {
        // W2.T1: in-memory Violation → RuleFinding conversion is lossless.
        // Range-shaped span with explicit severity and an LLM evidence string.
        let v = Violation {
            file_path: "src/main/java/Login.java".to_string(),
            rule_name: "VALIDATION_IN_UI".to_string(),
            kind: ViolationKind::AstRule("ast_forbidden_method_call".to_string()),
            offending_import: "LoginController::validate".to_string(),
            start_line: Some(42),
            end_line: Some(99),
        };
        let f = v.into_rule_finding(
            "udg-pds/spring-mini",
            Severity::Warning,
            "Validation belongs in the service layer.".to_string(),
        );
        assert_eq!(f.kind, RuleKind::Architecture);
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.rule_id, "VALIDATION_IN_UI");
        assert_eq!(f.repo_full_name, "udg-pds/spring-mini");
        assert_eq!(f.file_repo_relative, "src/main/java/Login.java");
        assert_eq!(f.span, LineSpan::range(42, 99));
        assert_eq!(f.evidence, "Validation belongs in the service layer.");
        assert_eq!(f.extra.as_deref(), Some("LoginController::validate"));
    }

    #[test]
    fn into_rule_finding_collapses_single_line_span() {
        let v = Violation {
            file_path: "Foo.java".to_string(),
            rule_name: "x".to_string(),
            kind: ViolationKind::LayerDependency,
            offending_import: "com.x.Y".to_string(),
            start_line: Some(13),
            end_line: Some(13),
        };
        let f = v.into_rule_finding("o/r", Severity::Critical, String::new());
        assert_eq!(f.span, LineSpan::single(13));
        assert_eq!(f.evidence, "");
    }

    #[test]
    fn load_rule_findings_for_repo_round_trips_through_db() {
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, file_path, rule_name, violation_kind,
                 offending_import, severity, start_line, end_line, rule_kind, explanation)
             VALUES
                ('udg/spring-x', 'src/main/java/A.java', 'VALIDATION_IN_UI',
                 'ast_forbidden_method_call', 'A::validate', 'CRITICAL', 42, 99,
                 'ast_forbidden_method_call', 'Validation belongs in service.'),
                ('udg/spring-x', 'src/main/java/B.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repo.UserRepo', 'WARNING', 7, 7,
                 'layer_dependency', NULL),
                ('udg/spring-other', 'src/main/java/Z.java', 'r', 'layer_dependency',
                 'com.z.X', 'INFO', 1, 1, 'layer_dependency', NULL);",
        )
        .unwrap();
        let findings = load_rule_findings_for_repo(&conn, "udg/spring-x").unwrap();
        assert_eq!(findings.len(), 2, "must scope by repo_full_name");
        // Sorted by file_path → A first, then B.
        let a = &findings[0];
        assert_eq!(a.file_repo_relative, "src/main/java/A.java");
        assert_eq!(a.severity, Severity::Critical);
        assert_eq!(a.rule_id, "VALIDATION_IN_UI");
        assert_eq!(a.span, LineSpan::range(42, 99));
        assert_eq!(a.evidence, "Validation belongs in service.");
        assert_eq!(a.extra.as_deref(), Some("A::validate"));

        let b = &findings[1];
        assert_eq!(b.severity, Severity::Warning);
        assert_eq!(b.span, LineSpan::single(7));
        assert_eq!(
            b.evidence, "",
            "NULL explanation must surface as empty string"
        );
    }
}
