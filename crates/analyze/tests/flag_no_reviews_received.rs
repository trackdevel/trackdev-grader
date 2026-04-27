//! NO_REVIEWS_RECEIVED — INFO when a merged PR linked to a task in this
//! sprint has zero rows in `pr_reviews`.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_when_merged_pr_has_no_reviews() {
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
        Some(50),
        Some(5),
        None,
    );
    common::seed_task(
        &conn,
        10,
        common::SPRINT_ID,
        Some("alice"),
        Some(3),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 10, "pr-1");

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "NO_REVIEWS_RECEIVED", "alice"),
        1
    );
}

#[test]
fn silent_when_pr_has_at_least_one_review() {
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
        Some(50),
        Some(5),
        None,
    );
    common::seed_task(
        &conn,
        10,
        common::SPRINT_ID,
        Some("alice"),
        Some(3),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 10, "pr-1");
    conn.execute(
        "INSERT INTO pr_reviews (pr_id, reviewer_login, state, submitted_at)
         VALUES (?, 'bob', 'APPROVED', '2026-02-10T11:00Z')",
        params!["pr-1"],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "NO_REVIEWS_RECEIVED"),
        0
    );
}
