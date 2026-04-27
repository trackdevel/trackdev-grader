//! HIDDEN_CONTRIBUTOR — INFO when code_signal ≥ 0.75 but task_signal ≤ 0.25
//! (lots of code, not reflected on the task board).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_contribution(conn: &rusqlite::Connection, sid: &str, code: f64, task: f64) {
    conn.execute(
        "INSERT INTO student_sprint_contribution
            (student_id, sprint_id, code_signal, task_signal,
             composite_score, review_signal, process_signal)
         VALUES (?, ?, ?, ?, 0.5, 0.5, 0.5)",
        params![sid, common::SPRINT_ID, code, task],
    )
    .unwrap();
}

#[test]
fn fires_when_code_high_but_task_low() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_contribution(&conn, "alice", 0.90, 0.10);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "HIDDEN_CONTRIBUTOR", "alice"),
        1
    );
}

#[test]
fn silent_when_task_signal_normal() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_contribution(&conn, "alice", 0.90, 0.50);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "HIDDEN_CONTRIBUTOR"),
        0
    );
}
