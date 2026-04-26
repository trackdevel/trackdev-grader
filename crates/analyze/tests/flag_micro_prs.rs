//! MICRO_PRS — INFO when ≥3 of a student's PRs are at or below
//! `micro_pr_max_lines` (default 10) AND that's >50% of their PRs.

mod common;

use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_micro_pr(conn: &rusqlite::Connection, pr_id: &str, num: i64, lines: i64, task_id: i64) {
    common::seed_pr(
        conn,
        pr_id,
        num,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "open",
        false,
        None,
        Some(lines),
        Some(0),
        None,
    );
    common::seed_task(
        conn,
        task_id,
        common::SPRINT_ID,
        Some("alice"),
        Some(1),
        "DONE",
        "TASK",
    );
    common::link_task_pr(conn, task_id, pr_id);
}

#[test]
fn fires_when_majority_of_prs_are_micro() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_micro_pr(&conn, "pr-1", 1, 3, 1);
    insert_micro_pr(&conn, "pr-2", 2, 4, 2);
    insert_micro_pr(&conn, "pr-3", 3, 5, 3);
    insert_micro_pr(&conn, "pr-4", 4, 200, 4); // not micro

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "MICRO_PRS", "alice"),
        1
    );
}

#[test]
fn silent_when_fewer_than_three_micro_prs() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_micro_pr(&conn, "pr-1", 1, 3, 1);
    insert_micro_pr(&conn, "pr-2", 2, 200, 2);
    insert_micro_pr(&conn, "pr-3", 3, 250, 3);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "MICRO_PRS"),
        0
    );
}
