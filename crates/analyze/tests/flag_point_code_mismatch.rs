//! POINT_CODE_MISMATCH — INFO when |points_share - code_share| > 0.25.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_metric(conn: &rusqlite::Connection, sid: &str, share: f64, lines: f64) {
    conn.execute(
        "INSERT INTO student_sprint_metrics
            (student_id, sprint_id, points_share, weighted_pr_lines)
         VALUES (?, ?, ?, ?)",
        params![sid, common::SPRINT_ID, share, lines],
    )
    .unwrap();
}

#[test]
fn fires_when_share_and_code_diverge() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b"] {
        common::seed_student(&conn, sid);
    }
    // a: 80% of points but 10% of lines (gap = 0.70). b balances the totals.
    insert_metric(&conn, "a", 0.80, 100.0);
    insert_metric(&conn, "b", 0.20, 900.0);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "POINT_CODE_MISMATCH", "a"),
        1
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "POINT_CODE_MISMATCH", "b"),
        1
    );
}

#[test]
fn silent_when_share_tracks_code() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b"] {
        common::seed_student(&conn, sid);
    }
    insert_metric(&conn, "a", 0.50, 500.0);
    insert_metric(&conn, "b", 0.50, 500.0);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "POINT_CODE_MISMATCH"),
        0
    );
}
