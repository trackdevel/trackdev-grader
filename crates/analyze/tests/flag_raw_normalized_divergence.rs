//! RAW_NORMALIZED_DIVERGENCE — INFO when normalized survival exceeds raw
//! survival by more than `raw_normalized_divergence_threshold` (default 0.20).
//! Signals heavy reformatting that the AST normaliser absorbed.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_survival(conn: &rusqlite::Connection, sid: &str, raw: f64, norm: f64) {
    conn.execute(
        "INSERT INTO student_sprint_survival
            (student_id, sprint_id, survival_rate_raw, survival_rate_normalized)
         VALUES (?, ?, ?, ?)",
        params![sid, common::SPRINT_ID, raw, norm],
    )
    .unwrap();
}

#[test]
fn fires_when_normalized_exceeds_raw_by_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_survival(&conn, "alice", 0.40, 0.85); // delta = 0.45

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(
            &conn,
            common::SPRINT_ID,
            "RAW_NORMALIZED_DIVERGENCE",
            "alice"
        ),
        1
    );
}

#[test]
fn silent_when_within_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_survival(&conn, "alice", 0.80, 0.85);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "RAW_NORMALIZED_DIVERGENCE"),
        0
    );
}
