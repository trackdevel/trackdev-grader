//! FOREIGN_MERGE — INFO when a DONE task's merged PR was authored by
//! someone other than the task assignee.

mod common;

use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_when_task_owner_did_not_author_pr() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(3),
        "DONE",
        "TASK",
    );
    common::seed_pr(
        &conn,
        "pr-1",
        1,
        common::REPO_FULL_NAME,
        Some("bob"),
        Some("bob"),
        "merged",
        true,
        Some("2026-02-10T10:00Z"),
        Some(50),
        Some(5),
        None,
    );
    common::link_task_pr(&conn, 1, "pr-1");

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "FOREIGN_MERGE", "alice"),
        1
    );
}

#[test]
fn silent_when_task_owner_authored_pr() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(3),
        "DONE",
        "TASK",
    );
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
        Some(50),
        Some(5),
        None,
    );
    common::link_task_pr(&conn, 1, "pr-1");

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "FOREIGN_MERGE"),
        0
    );
}
