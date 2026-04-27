//! Per-student attribution of architecture violations via `git blame` (T-P3.1).
//!
//! For every row in `architecture_violations` for a given (repo, sprint), we
//! blame the offending lines and tally how many lines each student authored.
//! Each student that contributed to the offending range gets one row in
//! `architecture_violation_attribution`:
//!
//! - `lines_authored` — count of lines blamed to the student.
//! - `total_lines`    — total blamed lines in the range (≤ end_line - start_line + 1).
//! - `weight`         — `lines_authored / total_lines` in `[0, 1]`.
//!
//! Weighting by lines (not by binary touched/not-touched) is intentional: a
//! one-line typo fix on a 30-line offending method gets ~3 % weight, not
//! 50 %. Whitespace-only edits and known cosmetic-rewrite revs are already
//! filtered by `survival::blame_file` (which passes `-w` and
//! `--ignore-revs-file`), so the same defences this codebase uses for
//! statement survival apply here too.
//!
//! ### Idempotency
//!
//! Pre-existing rows for `(repo, sprint)` are deleted before re-inserting,
//! mirroring the `architecture_violations` write path. The two deletes
//! happen in the same logical block (`scan_repo_to_db`) so partial state
//! from a previous run can't survive into a re-run.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection};
use sprint_grader_survival::blame::{blame_file, build_email_to_student_map, EmailStudentMap};
use tracing::warn;

/// Run blame attribution for every violation row currently in
/// `architecture_violations` for `(repo_full_name, sprint_id)` that carries
/// a `start_line` / `end_line`. Rows without a line range are skipped (they
/// were produced before T-P3.1 added attribution and have no anchor to
/// blame).
///
/// Returns the number of `architecture_violation_attribution` rows written.
pub fn attribute_violations_for_repo(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
) -> rusqlite::Result<usize> {
    // Build the email/login → student map once per call.
    let email_map = build_email_to_student_map(conn)?;

    // Collect (rowid, file_path, start_line, end_line) for every violation
    // we care about.
    let mut stmt = conn.prepare(
        "SELECT rowid, file_path, start_line, end_line
         FROM architecture_violations
         WHERE repo_full_name = ? AND sprint_id = ?
           AND start_line IS NOT NULL AND end_line IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![repo_full_name, sprint_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut by_file: HashMap<String, Vec<(i64, u32, u32)>> = HashMap::new();
    for r in rows {
        let (rowid, file_path, start, end) = r?;
        let s = start.max(1) as u32;
        let e = end.max(start) as u32;
        by_file.entry(file_path).or_default().push((rowid, s, e));
    }
    drop(stmt);

    // Clear previous attribution for this (repo, sprint) before re-inserting.
    conn.execute(
        "DELETE FROM architecture_violation_attribution
         WHERE violation_rowid IN (
             SELECT rowid FROM architecture_violations
             WHERE repo_full_name = ? AND sprint_id = ?
         )",
        params![repo_full_name, sprint_id],
    )?;

    let mut written = 0usize;
    for (file_path, violations) in by_file {
        // One blame call per file, regardless of how many violations point
        // into that file.
        let blame = blame_file(repo_path, &file_path);
        if blame.is_empty() {
            warn!(
                repo = repo_full_name,
                file = %file_path,
                "blame returned no lines; skipping attribution for this file"
            );
            continue;
        }
        for (rowid, start, end) in violations {
            let mut per_student: HashMap<String, u32> = HashMap::new();
            let mut total: u32 = 0;
            for ln in start..=end {
                let bl = match blame.get(&ln) {
                    Some(b) => b,
                    None => continue,
                };
                total += 1;
                if let Some(student_id) = resolve_student(&email_map, &bl.author_email) {
                    *per_student.entry(student_id).or_default() += 1;
                }
            }
            if total == 0 || per_student.is_empty() {
                continue;
            }
            for (student_id, lines) in per_student {
                let weight = lines as f64 / total as f64;
                conn.execute(
                    "INSERT OR REPLACE INTO architecture_violation_attribution
                        (violation_rowid, student_id, lines_authored,
                         total_lines, weight, sprint_id)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![rowid, student_id, lines as i64, total as i64, weight, sprint_id],
                )?;
                written += 1;
            }
        }
    }
    Ok(written)
}

fn resolve_student(map: &EmailStudentMap, email: &str) -> Option<String> {
    let key = email.to_lowercase();
    if let Some((sid, _)) = map.get(&key) {
        return Some(sid.clone());
    }
    // Fallback: GitHub no-reply emails encode the login as the local part.
    if let Some(local) = key.split('@').next() {
        if let Some((sid, _)) = map.get(local) {
            return Some(sid.clone());
        }
        // Also try the no-reply form explicitly.
        if let Some((sid, _)) = map.get(&format!("{local}@users.noreply.github.com")) {
            return Some(sid.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use sprint_grader_core::db::apply_schema;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git invocation");
        assert!(status.success(), "git {:?} failed in {:?}", args, cwd);
    }

    fn init_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        run_git(&path, &["init", "-q", "-b", "main"]);
        run_git(&path, &["config", "user.email", "alice@example.com"]);
        run_git(&path, &["config", "user.name", "Alice"]);
        (tmp, path)
    }

    fn commit_file(repo: &Path, rel: &str, body: &str, email: &str, name: &str, msg: &str) {
        let target = repo.join(rel);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, body).unwrap();
        run_git(repo, &["config", "user.email", email]);
        run_git(repo, &["config", "user.name", name]);
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-q", "-m", msg]);
    }

    fn seed_db(conn: &Connection, sprint_id: i64) {
        // Minimal seed: a project, a sprint, and two students whose emails
        // match the test commits.
        conn.execute(
            "INSERT OR REPLACE INTO projects (id, slug, name) VALUES (1, 'p', 'P')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sprints (id, project_id, name, start_date, end_date)
             VALUES (?, 1, 's1', '2026-01-01T00:00:00Z', '2026-02-01T00:00:00Z')",
            [sprint_id],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO students (id, username, github_login, full_name, email, team_project_id)
             VALUES ('alice', 'alice', 'alice', 'Alice', 'alice@example.com', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO students (id, username, github_login, full_name, email, team_project_id)
             VALUES ('bob', 'bob', 'bob', 'Bob', 'bob@example.com', 1)",
            [],
        )
        .unwrap();
    }

    fn insert_violation(
        conn: &Connection,
        repo_full_name: &str,
        sprint_id: i64,
        file_path: &str,
        rule_name: &str,
        offending: &str,
        start: u32,
        end: u32,
    ) -> i64 {
        conn.execute(
            "INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name, violation_kind,
                 offending_import, severity, start_line, end_line, rule_kind)
             VALUES (?, ?, ?, ?, 'ast_forbidden_field_type', ?, 'WARNING', ?, ?, 'ast_forbidden_field_type')",
            params![
                repo_full_name,
                sprint_id,
                file_path,
                rule_name,
                offending,
                start as i64,
                end as i64
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn single_author_gets_full_weight() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn, 1);

        let (_g, repo) = init_repo();
        let body = (1..=10)
            .map(|i| format!("// line {i}\n"))
            .collect::<String>();
        commit_file(&repo, "Foo.java", &body, "alice@example.com", "Alice", "all alice");

        let vid = insert_violation(&conn, "udg/x", 1, "Foo.java", "rule", "anchor", 3, 7);
        let n = attribute_violations_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        assert!(n > 0, "expected at least one attribution row");

        let (sid, lines, total, weight): (String, i64, i64, f64) = conn
            .query_row(
                "SELECT student_id, lines_authored, total_lines, weight
                 FROM architecture_violation_attribution WHERE violation_rowid = ?",
                [vid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(sid, "alice");
        assert_eq!(lines, 5);
        assert_eq!(total, 5);
        assert!((weight - 1.0).abs() < 1e-9);
    }

    #[test]
    fn typo_fix_gets_proportional_weight() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn, 1);

        let (_g, repo) = init_repo();

        // Initial commit: alice writes 30 numbered lines.
        let mut body = String::new();
        for i in 1..=30 {
            body.push_str(&format!("// alice line {i}\n"));
        }
        commit_file(&repo, "Foo.java", &body, "alice@example.com", "Alice", "alice writes");

        // Bob fixes a typo on line 15 only. Use a non-trivial textual edit
        // so `git blame` reattributes the line (not just whitespace, which
        // would be elided by `-w`).
        let mut lines: Vec<String> = body.lines().map(|s| s.to_string()).collect();
        lines[14] = "// alice line 15 (fixed by bob)".to_string();
        let after = lines.join("\n") + "\n";
        commit_file(&repo, "Foo.java", &after, "bob@example.com", "Bob", "bob typo fix");

        // Violation spans lines 1..30 (the whole offending method).
        let vid = insert_violation(&conn, "udg/x", 1, "Foo.java", "rule", "anchor", 1, 30);
        let n = attribute_violations_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        assert_eq!(n, 2, "alice + bob");

        let mut rows: Vec<(String, i64, i64, f64)> = conn
            .prepare(
                "SELECT student_id, lines_authored, total_lines, weight
                 FROM architecture_violation_attribution WHERE violation_rowid = ?",
            )
            .unwrap()
            .query_map([vid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let alice = rows.iter().find(|(s, ..)| s == "alice").unwrap();
        let bob = rows.iter().find(|(s, ..)| s == "bob").unwrap();
        assert_eq!(alice.1, 29);
        assert_eq!(bob.1, 1);
        assert_eq!(alice.2, 30);
        assert!((alice.3 - 29.0 / 30.0).abs() < 1e-9);
        assert!((bob.3 - 1.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn rerun_replaces_attribution_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn, 1);
        let (_g, repo) = init_repo();
        let body = (1..=5).map(|i| format!("// l{i}\n")).collect::<String>();
        commit_file(&repo, "F.java", &body, "alice@example.com", "Alice", "init");
        let _vid = insert_violation(&conn, "udg/x", 1, "F.java", "r", "x", 1, 5);

        let n1 = attribute_violations_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        let n2 = attribute_violations_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        assert_eq!(n1, n2);
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM architecture_violation_attribution",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, n1 as i64, "duplicates must not accumulate");
    }
}
