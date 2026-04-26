//! LOW_CODE_HIGH_POINTS — student whose `points_delivered` is above team
//! median *and* `weighted_pr_lines` is below team p25.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_metric(conn: &rusqlite::Connection, sid: &str, points: i64, lines: f64) {
    conn.execute(
        "INSERT INTO student_sprint_metrics
            (student_id, sprint_id, points_delivered, weighted_pr_lines)
         VALUES (?, ?, ?, ?)",
        params![sid, common::SPRINT_ID, points, lines],
    )
    .unwrap();
}

#[test]
fn fires_when_high_points_with_low_lines() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    // Team with one outlier "a": top points, near-zero weighted lines.
    for sid in ["a", "b", "c", "d", "e"] {
        common::seed_student(&conn, sid);
    }
    insert_metric(&conn, "a", 50, 5.0); // outlier
    insert_metric(&conn, "b", 10, 800.0);
    insert_metric(&conn, "c", 8, 600.0);
    insert_metric(&conn, "d", 6, 500.0);
    insert_metric(&conn, "e", 4, 400.0);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "LOW_CODE_HIGH_POINTS", "a"),
        1
    );
}

#[test]
fn silent_when_points_track_lines() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d", "e"] {
        common::seed_student(&conn, sid);
    }
    insert_metric(&conn, "a", 10, 100.0);
    insert_metric(&conn, "b", 12, 120.0);
    insert_metric(&conn, "c", 14, 140.0);
    insert_metric(&conn, "d", 16, 160.0);
    insert_metric(&conn, "e", 18, 180.0);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "LOW_CODE_HIGH_POINTS"),
        0
    );
}
