//! CARRYING_TEAM — fires WARNING on a member whose share of DONE points
//! exceeds `thresholds.carrying_team_pct` (default 0.40).

mod common;

use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_when_one_member_dominates_done_points() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    // Alice = 70 / 100 points DONE, well above the 0.40 default.
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(70),
        "DONE",
        "TASK",
    );
    common::seed_task(
        &conn,
        2,
        common::SPRINT_ID,
        Some("bob"),
        Some(30),
        "DONE",
        "TASK",
    );

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "CARRYING_TEAM", "alice"),
        1
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "CARRYING_TEAM", "bob"),
        0
    );
}

#[test]
fn silent_when_load_is_balanced() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d"] {
        common::seed_student(&conn, sid);
    }
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("a"),
        Some(25),
        "DONE",
        "TASK",
    );
    common::seed_task(
        &conn,
        2,
        common::SPRINT_ID,
        Some("b"),
        Some(25),
        "DONE",
        "TASK",
    );
    common::seed_task(
        &conn,
        3,
        common::SPRINT_ID,
        Some("c"),
        Some(25),
        "DONE",
        "TASK",
    );
    common::seed_task(
        &conn,
        4,
        common::SPRINT_ID,
        Some("d"),
        Some(25),
        "DONE",
        "TASK",
    );

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "CARRYING_TEAM"),
        0
    );
}
