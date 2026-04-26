//! CRAMMING — reads `student_sprint_temporal.cramming_ratio`. T-P1.1 moved
//! the source from task-keyed to author-keyed.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_temporal(conn: &rusqlite::Connection, sid: &str, ratio: f64) {
    conn.execute(
        "INSERT INTO student_sprint_temporal (student_id, sprint_id, cramming_ratio)
         VALUES (?, ?, ?)",
        params![sid, common::SPRINT_ID, ratio],
    )
    .unwrap();
}

#[test]
fn fires_when_cramming_ratio_above_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_temporal(&conn, "alice", 0.95); // > 0.70 default
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "CRAMMING", "alice"),
        1
    );
}

#[test]
fn silent_when_ratio_is_below_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_temporal(&conn, "alice", 0.10);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(common::count_flags(&conn, common::SPRINT_ID, "CRAMMING"), 0);
}
