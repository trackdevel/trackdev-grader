//! LOW_SURVIVAL_RATE — WARNING when a member's z-below-mean exceeds
//! `low_survival_rate_stddev` AND survival rate is below the absolute floor
//! (T-P0.3 added the floor gate).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_survival(conn: &rusqlite::Connection, sid: &str, rate: f64) {
    conn.execute(
        "INSERT INTO student_sprint_survival (student_id, sprint_id, survival_rate_normalized)
         VALUES (?, ?, ?)",
        params![sid, common::SPRINT_ID, rate],
    )
    .unwrap();
}

#[test]
fn fires_when_relative_low_and_absolute_low() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d"] {
        common::seed_student(&conn, sid);
    }
    insert_survival(&conn, "a", 0.99);
    insert_survival(&conn, "b", 0.99);
    insert_survival(&conn, "c", 0.99);
    insert_survival(&conn, "d", 0.50); // below floor + clearly z-low.

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "LOW_SURVIVAL_RATE", "d"),
        1
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "LOW_SURVIVAL_RATE", "a"),
        0
    );
}

#[test]
fn silent_when_team_uniformly_high() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d"] {
        common::seed_student(&conn, sid);
    }
    insert_survival(&conn, "a", 0.99);
    insert_survival(&conn, "b", 0.99);
    insert_survival(&conn, "c", 0.99);
    insert_survival(&conn, "d", 0.95);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "LOW_SURVIVAL_RATE"),
        0
    );
}
