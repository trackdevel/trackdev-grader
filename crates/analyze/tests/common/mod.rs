//! Shared helpers for per-detector integration tests (T-P2.7).
//!
//! Each test file does `mod common;` and then composes builders. The
//! conventions:
//!
//! - `make_db()` returns an in-memory connection with the canonical schema
//!   applied.
//! - Default project_id = 1, default sprint_id = 10. Tests that need more than
//!   one sprint extend with [`seed_sprint`].
//! - Helpers insert *one row at a time* so test files can hand-craft anything
//!   the detector needs without reading helper internals.
//! - Counting helpers ([`count_flags`], [`flag_details_for`]) scope by
//!   `flag_type` so the assertions don't see incidental flags fired by other
//!   detectors on the same fixture.

#![allow(dead_code)] // not every helper is used by every test file

use rusqlite::{params, Connection};
use serde_json::Value;

pub const PROJECT_ID: i64 = 1;
pub const SPRINT_ID: i64 = 10;
pub const PRIOR_SPRINT_ID: i64 = 9;
pub const PROJECT_SLUG: &str = "team-test";
pub const REPO_FULL_NAME: &str = "udg-pds/spring-test";

/// In-memory connection with the canonical schema applied.
pub fn make_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    sprint_grader_core::db::apply_schema(&conn).expect("apply schema");
    conn
}

/// Insert a project row plus the default current sprint (id=SPRINT_ID,
/// 2026-02-01 → 2026-02-15) and prior sprint (id=PRIOR_SPRINT_ID,
/// 2026-01-15 → 2026-01-31).
pub fn seed_default_project(conn: &Connection) {
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, ?, ?)",
        params![PROJECT_ID, PROJECT_SLUG, "Test Project"],
    )
    .unwrap();
    seed_sprint(
        conn,
        PRIOR_SPRINT_ID,
        PROJECT_ID,
        "Sprint 1",
        "2026-01-15",
        "2026-01-31",
    );
    seed_sprint(
        conn,
        SPRINT_ID,
        PROJECT_ID,
        "Sprint 2",
        "2026-02-01",
        "2026-02-15",
    );
}

pub fn seed_sprint(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    name: &str,
    start: &str,
    end: &str,
) {
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES (?, ?, ?, ?, ?)",
        params![sprint_id, project_id, name, start, end],
    )
    .unwrap();
}

/// A team member rooted to the default project. `id` is also used as the
/// github_login for convenience.
pub fn seed_student(conn: &Connection, id: &str) {
    seed_student_in(conn, id, PROJECT_ID);
}

pub fn seed_student_in(conn: &Connection, id: &str, team_project_id: i64) {
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id)
         VALUES (?, ?, ?, ?, ?)",
        params![id, id, id, id, team_project_id],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
pub fn seed_task(
    conn: &Connection,
    id: i64,
    sprint_id: i64,
    assignee_id: Option<&str>,
    points: Option<i64>,
    status: &str,
    task_type: &str,
) {
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                            assignee_id, sprint_id, parent_task_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        params![
            id,
            format!("T-{id}"),
            format!("Task {id}"),
            task_type,
            status,
            points,
            assignee_id,
            sprint_id,
        ],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
pub fn seed_pr(
    conn: &Connection,
    id: &str,
    pr_number: i64,
    repo: &str,
    author_id: Option<&str>,
    github_author_login: Option<&str>,
    state: &str,
    merged: bool,
    merged_at: Option<&str>,
    additions: Option<i64>,
    deletions: Option<i64>,
    body: Option<&str>,
) {
    conn.execute(
        "INSERT INTO pull_requests
         (id, pr_number, repo_full_name, url, title, body, state, merged,
          author_id, github_author_login, additions, deletions, changed_files,
          created_at, merged_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            pr_number,
            repo,
            format!("https://example.test/{id}"),
            format!("PR {pr_number}"),
            body,
            state,
            merged,
            author_id,
            github_author_login,
            additions,
            deletions,
            1,
            merged_at,
            merged_at,
        ],
    )
    .unwrap();
}

pub fn link_task_pr(conn: &Connection, task_id: i64, pr_id: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO task_pull_requests (task_id, pr_id) VALUES (?, ?)",
        params![task_id, pr_id],
    )
    .unwrap();
}

pub fn count_flags(conn: &Connection, sprint_id: i64, flag_type: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM flags WHERE sprint_id = ? AND flag_type = ?",
        params![sprint_id, flag_type],
        |r| r.get(0),
    )
    .unwrap()
}

pub fn count_flags_for(conn: &Connection, sprint_id: i64, flag_type: &str, student: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM flags
         WHERE sprint_id = ? AND flag_type = ? AND student_id = ?",
        params![sprint_id, flag_type, student],
        |r| r.get(0),
    )
    .unwrap()
}

pub fn flag_details_for(
    conn: &Connection,
    sprint_id: i64,
    flag_type: &str,
    student: &str,
) -> Option<Value> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT details FROM flags
             WHERE sprint_id = ? AND flag_type = ? AND student_id = ? LIMIT 1",
            params![sprint_id, flag_type, student],
            |r| r.get(0),
        )
        .ok();
    raw.map(|s| serde_json::from_str(&s).expect("details is JSON"))
}

pub fn flag_severity_for(
    conn: &Connection,
    sprint_id: i64,
    flag_type: &str,
    student: &str,
) -> Option<String> {
    conn.query_row(
        "SELECT severity FROM flags
         WHERE sprint_id = ? AND flag_type = ? AND student_id = ? LIMIT 1",
        params![sprint_id, flag_type, student],
        |r| r.get(0),
    )
    .ok()
}

/// All flag types fired in the given sprint, deduped, sorted alphabetically.
/// Useful in negative tests that want to assert "none of these specific types"
/// without listing every irrelevant detector.
pub fn fired_flag_types(conn: &Connection, sprint_id: i64) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT flag_type FROM flags WHERE sprint_id = ? ORDER BY flag_type")
        .unwrap();
    let rows = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))
        .unwrap();
    rows.filter_map(|r| r.ok()).collect()
}
