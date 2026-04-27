//! ZERO_TASKS — fires CRITICAL on every team member who completed no
//! non-USER_STORY tasks in DONE status this sprint.

mod common;

use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_for_member_with_no_done_tasks() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    // Alice has a DONE task, Bob has none.
    common::seed_task(
        &conn,
        100,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "TASK",
    );

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "ZERO_TASKS", "alice"),
        0
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "ZERO_TASKS", "bob"),
        1
    );
    assert_eq!(
        common::flag_severity_for(&conn, common::SPRINT_ID, "ZERO_TASKS", "bob").as_deref(),
        Some("CRITICAL"),
    );
}

#[test]
fn user_story_done_does_not_satisfy_the_check() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // Only a USER_STORY DONE — must not count.
    common::seed_task(
        &conn,
        200,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "USER_STORY",
    );

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "ZERO_TASKS", "alice"),
        1
    );
}
