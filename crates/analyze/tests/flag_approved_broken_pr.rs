//! APPROVED_BROKEN_PR — INFO on every reviewer who approved a PR that
//! `pr_compilation` says is broken.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_for_reviewer_who_approved_broken_pr() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice"); // PR author
    common::seed_student(&conn, "bob"); // approving reviewer
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
        Some(40),
        Some(2),
        None,
    );
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(2),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 1, "pr-1");
    conn.execute(
        "INSERT INTO pr_reviews (pr_id, reviewer_login, state, submitted_at)
         VALUES ('pr-1', 'bob', 'APPROVED', '2026-02-10T10:30Z')",
        params![],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pr_compilation
            (pr_id, repo_name, sprint_id, author_id, reviewer_ids, pr_number,
             compiles, exit_code, tested_at)
         VALUES ('pr-1', ?, ?, 'alice', '[\"bob\"]', 1, 0, 1, '2026-02-10T11:00Z')",
        params![common::REPO_FULL_NAME, common::SPRINT_ID],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "APPROVED_BROKEN_PR", "bob"),
        1
    );
}

#[test]
fn silent_when_no_pr_compilation_row() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "APPROVED_BROKEN_PR"),
        0
    );
}
