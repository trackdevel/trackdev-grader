//! GHOST_CONTRIBUTOR — WARNING when a student has ≥1 assigned task this
//! sprint, composite < 0.15, AND code_signal < 0.10.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_contribution(conn: &rusqlite::Connection, sid: &str, composite: f64, code: f64) {
    conn.execute(
        "INSERT INTO student_sprint_contribution
            (student_id, sprint_id, code_signal, composite_score, task_signal,
             review_signal, process_signal)
         VALUES (?, ?, ?, ?, 0.5, 0.5, 0.5)",
        params![sid, common::SPRINT_ID, code, composite],
    )
    .unwrap();
}

#[test]
fn fires_when_assigned_but_invisible() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "TASK",
    );
    insert_contribution(&conn, "alice", 0.05, 0.02);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "GHOST_CONTRIBUTOR", "alice"),
        1
    );
}

#[test]
fn silent_when_no_tasks_assigned() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_contribution(&conn, "alice", 0.05, 0.02);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "GHOST_CONTRIBUTOR"),
        0
    );
}

#[test]
fn silent_when_code_signal_above_floor() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "TASK",
    );
    insert_contribution(&conn, "alice", 0.05, 0.20);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "GHOST_CONTRIBUTOR"),
        0
    );
}
