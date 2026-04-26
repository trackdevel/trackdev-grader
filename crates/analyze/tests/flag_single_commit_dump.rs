//! SINGLE_COMMIT_DUMP — WARNING when a PR has exactly one commit and
//! additions+deletions exceeds `single_commit_dump_lines` (default 200).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_commit(conn: &rusqlite::Connection, pr_id: &str, sha: &str) {
    conn.execute(
        "INSERT INTO pr_commits (pr_id, sha, author_login, message, timestamp)
         VALUES (?, ?, 'alice', 'work', '2026-02-10T10:00Z')",
        params![pr_id, sha],
    )
    .unwrap();
}

#[test]
fn fires_for_one_giant_commit() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_pr(
        &conn,
        "pr-1",
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-10T10:00Z"),
        Some(800),
        Some(0),
        None,
    );
    common::seed_task(
        &conn,
        10,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 10, "pr-1");
    insert_commit(&conn, "pr-1", "deadbeef");

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "SINGLE_COMMIT_DUMP"),
        1
    );
}

#[test]
fn silent_when_pr_has_multiple_commits() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_pr(
        &conn,
        "pr-1",
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-10T10:00Z"),
        Some(800),
        Some(0),
        None,
    );
    common::seed_task(
        &conn,
        10,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 10, "pr-1");
    insert_commit(&conn, "pr-1", "sha1");
    insert_commit(&conn, "pr-1", "sha2");

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "SINGLE_COMMIT_DUMP"),
        0
    );
}
