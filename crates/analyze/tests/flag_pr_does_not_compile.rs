//! PR_DOES_NOT_COMPILE — WARNING when a merged PR has a `pr_compilation`
//! row with `compiles = 0`.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn seed_compilation(conn: &rusqlite::Connection, compiles: bool) {
    common::seed_pr(
        conn,
        "pr-1",
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-10T10:00Z"),
        Some(40),
        Some(2),
        None,
    );
    common::seed_task(
        conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(2),
        "DONE",
        "TASK",
    );
    common::link_task_pr(conn, 1, "pr-1");
    conn.execute(
        "INSERT INTO pr_compilation
            (pr_id, repo_name, sprint_id, author_id, pr_number, compiles,
             exit_code, tested_at)
         VALUES ('pr-1', ?, ?, 'alice', 1, ?, 0, '2026-02-10T11:00Z')",
        params![common::REPO_FULL_NAME, common::SPRINT_ID, compiles],
    )
    .unwrap();
}

#[test]
fn fires_when_merged_pr_does_not_compile() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    seed_compilation(&conn, false);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "PR_DOES_NOT_COMPILE", "alice"),
        1
    );
}

#[test]
fn silent_when_pr_compiles() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    seed_compilation(&conn, true);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "PR_DOES_NOT_COMPILE"),
        0
    );
}
