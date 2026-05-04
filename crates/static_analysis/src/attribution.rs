//! Per-student attribution of static-analysis findings via `git blame`.
//!
//! Mirrors `architecture::attribution::attribute_violations_for_repo`
//! exactly — the algorithm is unchanged, only the table names differ:
//!
//! - reads `static_analysis_findings` rows that carry a line range
//! - blames the offending lines via `survival::blame::blame_file`
//! - tallies lines per student over `[start_line..=end_line]`
//! - writes one `static_analysis_finding_attribution` row per student
//!   with `weight = lines_authored / total_lines` in `[0, 1]`
//!
//! The blame call applies `-w` (whitespace-insensitive) and
//! `--ignore-revs-file`, so a 1-line typo fix on a 30-line offending
//! method earns ~3 % weight, not 50 %. Same defence the architecture
//! attribution and the survival stage already rely on.
//!
//! ### Idempotency
//!
//! Pre-existing rows for `(repo, sprint)` are deleted before re-inserting,
//! mirroring the `static_analysis_findings` write path. The two deletes
//! happen in the same logical block (`scan_repo_to_db`) so partial state
//! from a previous run can't survive into a re-run.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection};
use sprint_grader_survival::blame::{blame_file, build_email_to_student_map, EmailStudentMap};
use tracing::warn;

/// Run blame attribution for every finding row currently in
/// `static_analysis_findings` for `(repo_full_name, sprint_id)` that
/// carries a `start_line` / `end_line`. Findings without a line range
/// (rare — typically file-level analyzer outputs) are skipped: they have
/// no anchor to blame.
///
/// Returns the number of `static_analysis_finding_attribution` rows
/// written.
pub fn attribute_findings_for_repo(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
) -> rusqlite::Result<usize> {
    let email_map = build_email_to_student_map(conn)?;

    let mut stmt = conn.prepare(
        "SELECT id, file_path, start_line, end_line
         FROM static_analysis_findings
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
        let (id, file_path, start, end) = r?;
        let s = start.max(1) as u32;
        let e = end.max(start) as u32;
        by_file.entry(file_path).or_default().push((id, s, e));
    }
    drop(stmt);

    // Clear previous attribution for this (repo, sprint) before re-inserting.
    // The `ON DELETE CASCADE` on the FK would also clear these when the
    // findings rows are deleted, but explicit deletes keep behaviour visible
    // to operators reading the SQL.
    conn.execute(
        "DELETE FROM static_analysis_finding_attribution
         WHERE finding_id IN (
             SELECT id FROM static_analysis_findings
             WHERE repo_full_name = ? AND sprint_id = ?
         )",
        params![repo_full_name, sprint_id],
    )?;

    let mut written = 0usize;
    for (file_path, findings) in by_file {
        // One blame call per file, regardless of how many findings point
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
        for (id, start, end) in findings {
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
                    "INSERT OR REPLACE INTO static_analysis_finding_attribution
                        (finding_id, student_id, lines_authored,
                         total_lines, weight, sprint_id)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![
                        id,
                        student_id,
                        lines as i64,
                        total as i64,
                        weight,
                        sprint_id
                    ],
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
    if let Some(local) = key.split('@').next() {
        if let Some((sid, _)) = map.get(local) {
            return Some(sid.clone());
        }
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

    #[allow(clippy::too_many_arguments)]
    fn insert_finding(
        conn: &Connection,
        repo_full_name: &str,
        sprint_id: i64,
        file_path: &str,
        rule_id: &str,
        start: Option<u32>,
        end: Option<u32>,
    ) -> i64 {
        let fingerprint = format!("fp-{rule_id}-{}-{}", file_path, start.unwrap_or(0));
        conn.execute(
            "INSERT INTO static_analysis_findings
                (repo_full_name, sprint_id, analyzer, rule_id, severity, file_path,
                 start_line, end_line, message, fingerprint)
             VALUES (?, ?, 'pmd', ?, 'WARNING', ?, ?, ?, 'msg', ?)",
            params![
                repo_full_name,
                sprint_id,
                rule_id,
                file_path,
                start.map(|n| n as i64),
                end.map(|n| n as i64),
                fingerprint,
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
        commit_file(
            &repo,
            "Foo.java",
            &body,
            "alice@example.com",
            "Alice",
            "all alice",
        );

        let fid = insert_finding(&conn, "udg/x", 1, "Foo.java", "R1", Some(3), Some(7));
        let n = attribute_findings_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        assert!(n > 0, "expected at least one attribution row");

        let (sid, lines, total, weight): (String, i64, i64, f64) = conn
            .query_row(
                "SELECT student_id, lines_authored, total_lines, weight
                 FROM static_analysis_finding_attribution WHERE finding_id = ?",
                [fid],
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

        let mut body = String::new();
        for i in 1..=30 {
            body.push_str(&format!("// alice line {i}\n"));
        }
        commit_file(
            &repo,
            "Foo.java",
            &body,
            "alice@example.com",
            "Alice",
            "alice writes",
        );

        // Bob fixes a typo on line 15 only. Non-trivial textual edit so
        // `git blame -w` reattributes the line.
        let mut lines: Vec<String> = body.lines().map(|s| s.to_string()).collect();
        lines[14] = "// alice line 15 (fixed by bob)".to_string();
        let after = lines.join("\n") + "\n";
        commit_file(
            &repo,
            "Foo.java",
            &after,
            "bob@example.com",
            "Bob",
            "bob typo fix",
        );

        let fid = insert_finding(&conn, "udg/x", 1, "Foo.java", "R1", Some(1), Some(30));
        let n = attribute_findings_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        assert_eq!(n, 2, "alice + bob");

        let mut rows: Vec<(String, i64, i64, f64)> = conn
            .prepare(
                "SELECT student_id, lines_authored, total_lines, weight
                 FROM static_analysis_finding_attribution WHERE finding_id = ?",
            )
            .unwrap()
            .query_map([fid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
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
        let _fid = insert_finding(&conn, "udg/x", 1, "F.java", "R", Some(1), Some(5));

        let n1 = attribute_findings_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        let n2 = attribute_findings_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        assert_eq!(n1, n2);
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM static_analysis_finding_attribution",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, n1 as i64, "duplicates must not accumulate");
    }

    /// New T-SA test: file-level analyzer findings have no line range
    /// (e.g., SpotBugs file-scope warnings, or a malformed SARIF result).
    /// Attribution should silently skip them rather than blame the whole
    /// file or crash.
    #[test]
    fn findings_without_line_range_are_skipped() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn, 1);
        let (_g, repo) = init_repo();
        commit_file(
            &repo,
            "F.java",
            "// only line\n",
            "alice@example.com",
            "Alice",
            "init",
        );

        // Two findings: one with a real range, one with start_line = NULL.
        let _real = insert_finding(&conn, "udg/x", 1, "F.java", "R1", Some(1), Some(1));
        let _file_level = insert_finding(&conn, "udg/x", 1, "F.java", "R2", None, None);

        let n = attribute_findings_for_repo(&conn, &repo, "udg/x", 1).unwrap();
        assert_eq!(
            n, 1,
            "only the finding with a line range should produce attribution rows"
        );

        // Specifically, the NULL-range finding has zero attribution rows.
        let null_attr_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM static_analysis_finding_attribution a
                 JOIN static_analysis_findings f ON f.id = a.finding_id
                 WHERE f.start_line IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(null_attr_count, 0);
    }
}
