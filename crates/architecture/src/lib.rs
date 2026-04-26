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

pub mod checker;
pub mod glob;
pub mod rules;
pub mod scanner;

pub use checker::{check_file, check_repo, Violation, ViolationKind};
pub use glob::PackagePattern;
pub use rules::{ArchitectureRules, Forbidden, Layer};
pub use scanner::{parse_java, scan_repo, JavaFileFacts};

use std::path::Path;

use rusqlite::{params, Connection};
use tracing::{info, warn};

/// Scan one cloned repo for one sprint and persist violations.
/// Idempotent: pre-existing rows for `(repo_full_name, sprint_id)` are
/// deleted first so re-runs reflect the current state of the working
/// tree without duplicating.
pub fn scan_repo_to_db(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    rules: &ArchitectureRules,
) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM architecture_violations WHERE repo_full_name = ? AND sprint_id = ?",
        params![repo_full_name, sprint_id],
    )?;
    let files = scan_repo(repo_path);
    let mut written = 0usize;
    for file in &files {
        for v in check_file(rules, file) {
            conn.execute(
                "INSERT OR REPLACE INTO architecture_violations
                    (repo_full_name, sprint_id, file_path, rule_name,
                     violation_kind, offending_import, severity)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    repo_full_name,
                    sprint_id,
                    v.file_path,
                    v.rule_name,
                    v.kind.as_str(),
                    v.offending_import,
                    rules.severity,
                ],
            )?;
            written += 1;
        }
    }
    info!(
        repo = repo_full_name,
        sprint_id,
        files = files.len(),
        violations = written,
        "architecture scan complete"
    );
    Ok(written)
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
        let repo_full_name = entry.file_name().to_string_lossy().into_owned();
        total += scan_repo_to_db(conn, &repo_path, &repo_full_name, sprint_id, rules)?;
    }
    Ok(total)
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
}
