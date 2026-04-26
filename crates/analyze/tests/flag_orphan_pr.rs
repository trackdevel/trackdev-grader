//! ORPHAN_PR — INFO when a merged PR in a repo touched by this sprint has
//! no row in `task_pull_requests`.

mod common;

use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_on_merged_pr_with_no_task_link() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // pr-anchor links the repo to this sprint via a task.
    common::seed_pr(
        &conn,
        "pr-anchor",
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-10T10:00Z"),
        Some(20),
        Some(0),
        None,
    );
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(1),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 1, "pr-anchor");
    // Orphan: same repo, merged, but no task link.
    common::seed_pr(
        &conn,
        "pr-orphan",
        2,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-10T11:00Z"),
        Some(50),
        Some(2),
        None,
    );

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ORPHAN_PR"),
        1
    );
}

#[test]
fn silent_when_all_prs_linked() {
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
        Some(20),
        Some(0),
        None,
    );
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(1),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 1, "pr-1");

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ORPHAN_PR"),
        0
    );
}
