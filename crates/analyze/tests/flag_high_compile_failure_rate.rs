//! HIGH_COMPILE_FAILURE_RATE — WARNING when an author has ≥3 builds and
//! ≥50% failed.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_build(conn: &rusqlite::Connection, pr_id: &str, author: &str, compiles: bool) {
    conn.execute(
        "INSERT INTO pr_compilation
            (pr_id, repo_name, sprint_id, author_id, pr_number, compiles,
             exit_code, tested_at)
         VALUES (?, ?, ?, ?, 1, ?, 0, '2026-02-10T11:00Z')",
        params![
            pr_id,
            common::REPO_FULL_NAME,
            common::SPRINT_ID,
            author,
            compiles
        ],
    )
    .unwrap();
}

#[test]
fn fires_when_author_failure_rate_high() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_build(&conn, "pr-1", "alice", false);
    insert_build(&conn, "pr-2", "alice", false);
    insert_build(&conn, "pr-3", "alice", true);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(
            &conn,
            common::SPRINT_ID,
            "HIGH_COMPILE_FAILURE_RATE",
            "alice"
        ),
        1
    );
}

#[test]
fn silent_below_min_build_count() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_build(&conn, "pr-1", "alice", false);
    insert_build(&conn, "pr-2", "alice", false);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "HIGH_COMPILE_FAILURE_RATE"),
        0
    );
}

#[test]
fn silent_when_failure_rate_low() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_build(&conn, "pr-1", "alice", false);
    insert_build(&conn, "pr-2", "alice", true);
    insert_build(&conn, "pr-3", "alice", true);
    insert_build(&conn, "pr-4", "alice", true);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "HIGH_COMPILE_FAILURE_RATE"),
        0
    );
}
