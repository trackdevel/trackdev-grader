//! Architecture conformance scanning (T-P2.2).
//!
//! Replaces the manual "did the team respect the layered architecture"
//! review pass with a course-config-driven scan. Reads the rules from
//! `architecture.toml`, walks every Java file in the cloned repo,
//! extracts (package, imports), and writes one `architecture_violations`
//! row per (file × broken rule × offending import).
//!
//! `analyze::flags::architecture_drift` (the ARCHITECTURE_DRIFT detector)
//! reads the resulting counts and fires WARNING when this sprint
//! strictly exceeds the previous sprint — i.e., the team is regressing
//! against the layered model.

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

use rusqlite::{params, Connection};
use tracing::{info, warn};

/// Scan one cloned repo for one sprint, persist violations, then run
/// blame attribution. Idempotent: pre-existing rows for
/// `(repo_full_name, sprint_id)` are deleted first (both the violation
/// table and the attribution table) so re-runs reflect the current state
/// of the working tree without duplicating.
pub fn scan_repo_to_db(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    rules: &ArchitectureRules,
) -> rusqlite::Result<usize> {
    // Clear attribution + violation rows that match this repo for this
    // sprint. The architecture stage used to write `repo_full_name` as the
    // BARE directory name (e.g. `spring-pds26_4c`); it now writes the
    // org-qualified `<org>/<repo>` form. The two forms don't share a key,
    // so a plain `WHERE repo_full_name = ?` would leave stale legacy rows
    // alive on every re-run after the qualifier-fix landed. Delete BOTH
    // the qualified form (current writes) and the bare basename
    // (legacy writes) before re-inserting.
    let bare = repo_full_name.rsplit('/').next().unwrap_or(repo_full_name);
    conn.execute(
        "DELETE FROM architecture_violation_attribution
         WHERE violation_rowid IN (
             SELECT rowid FROM architecture_violations
             WHERE sprint_id = ?
               AND (repo_full_name = ? OR repo_full_name = ?)
         )",
        params![sprint_id, repo_full_name, bare],
    )?;
    conn.execute(
        "DELETE FROM architecture_violations
         WHERE sprint_id = ?
           AND (repo_full_name = ? OR repo_full_name = ?)",
        params![sprint_id, repo_full_name, bare],
    )?;
    let files = scan_repo(repo_path);
    let mut written = 0usize;
    for file in &files {
        // Legacy package-glob and forbidden-import rules.
        for v in check_file(rules, file) {
            insert_violation(conn, repo_full_name, sprint_id, &rules.severity, &v)?;
            written += 1;
        }
        // AST rules (T-P3.1). Skip files we couldn't read for AST input —
        // most commonly those that scan_repo already filtered.
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
                    insert_violation(conn, repo_full_name, sprint_id, &rules.severity, &v)?;
                    written += 1;
                }
            }
        }
    }

    // Blame attribution runs once per (repo, sprint) — one git invocation
    // per file regardless of how many violations point into it.
    let attributed = match attribution::attribute_violations_for_repo(
        conn,
        repo_path,
        repo_full_name,
        sprint_id,
    ) {
        Ok(n) => n,
        Err(e) => {
            warn!(repo = repo_full_name, error = %e, "blame attribution failed; continuing without it");
            0
        }
    };

    info!(
        repo = repo_full_name,
        sprint_id,
        files = files.len(),
        violations = written,
        attribution_rows = attributed,
        "architecture scan complete"
    );
    Ok(written)
}

fn insert_violation(
    conn: &Connection,
    repo_full_name: &str,
    sprint_id: i64,
    default_severity: &str,
    v: &Violation,
) -> rusqlite::Result<()> {
    let rule_kind = v.kind.as_str();
    conn.execute(
        "INSERT OR REPLACE INTO architecture_violations
            (repo_full_name, sprint_id, file_path, rule_name,
             violation_kind, offending_import, severity,
             start_line, end_line, rule_kind)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            repo_full_name,
            sprint_id,
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

/// Convenience: scan every directory under `entregues_dir/<project_name>`
/// that looks like a cloned repo. Returns the total violation count
/// across all repos for the sprint. Skips silently when the project
/// directory or its repo subdirs are missing.
pub fn scan_project_to_db(
    conn: &Connection,
    project_root: &Path,
    sprint_id: i64,
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
        // Persist the org-qualified `<org>/<repo>` form when we can find one
        // in `pull_requests.repo_full_name` (collect already wrote it). Fall
        // back to the bare directory name only if no PR row references it,
        // so existing reports still build a GitHub URL whenever possible.
        let repo_full_name = resolve_qualified_repo_name(conn, &bare).unwrap_or(bare);
        total += scan_repo_to_db(conn, &repo_path, &repo_full_name, sprint_id, rules)?;
    }
    Ok(total)
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

/// Total violations recorded for one (project, sprint). Used by the
/// ARCHITECTURE_DRIFT detector and by report consumers.
pub fn count_for_project_sprint(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM architecture_violations av
         JOIN sprints s ON s.id = av.sprint_id
         WHERE s.project_id = ? AND av.sprint_id = ?",
        params![project_id, sprint_id],
        |r| r.get(0),
    )
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
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();

        let n = scan_repo_to_db(&conn, tmp.path(), "udg/spring-x", 10, &rules).unwrap();
        assert_eq!(n, 1, "controller→repository must produce one violation");

        let kind: String = conn
            .query_row(
                "SELECT violation_kind FROM architecture_violations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "layer_dependency");
    }

    #[test]
    fn rerun_purges_legacy_bare_name_rows() {
        // Regression: scan_project_to_db now writes the org-qualified
        // `<org>/<repo>` form, but pre-fix runs left bare-name rows in the
        // table. scan_repo_to_db must delete BOTH forms for the sprint
        // before re-inserting, otherwise stale bogus violations from a
        // pre-fix run linger forever.
        let tmp = TempDir::new().unwrap();
        write_java(
            tmp.path(),
            "src/main/java/com/x/controller/Bad.java",
            "package com.x.controller;\n\
             import com.x.repository.UserRepository;\n\
             public class Bad {}",
        );
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();

        // Seed legacy bare-name rows from a hypothetical old run.
        conn.execute(
            "INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name, violation_kind,
                 offending_import, severity, rule_kind)
             VALUES
                ('spring-x', 10, 'OldFile.java', 'domain->!infrastructure',
                 'layer_dependency', 'jakarta.persistence.Entity', 'WARNING', 'layer_dependency')",
            [],
        )
        .unwrap();

        // New code path writes under qualified name and must purge the legacy row.
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();
        scan_repo_to_db(&conn, tmp.path(), "udg-x/spring-x", 10, &rules).unwrap();
        let stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM architecture_violations
                 WHERE repo_full_name = 'spring-x' AND sprint_id = 10",
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
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();
        let first = scan_repo_to_db(&conn, tmp.path(), "udg/x", 10, &rules).unwrap();
        let second = scan_repo_to_db(&conn, tmp.path(), "udg/x", 10, &rules).unwrap();
        assert_eq!(first, second, "deterministic re-run");
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM architecture_violations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(total, 1, "row count must not duplicate");
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

        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        // Seed a PR row carrying the org-qualified repo name; this is what
        // collect normally writes during the GitHub fetch.
        conn.execute(
            "INSERT INTO pull_requests (id, repo_full_name) VALUES ('p1', 'udg-3c/spring-x')",
            [],
        )
        .unwrap();
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();
        scan_project_to_db(&conn, &project, 10, &rules).unwrap();

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
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        let rules = ArchitectureRules::from_toml_str(RULES).unwrap();
        scan_project_to_db(&conn, &project, 10, &rules).unwrap();
        let stored: String = conn
            .query_row(
                "SELECT repo_full_name FROM architecture_violations LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, "spring-y");
    }
}
