//! LOW_DOC_SCORE — INFO when `student_sprint_metrics.avg_doc_score` falls
//! below `thresholds.low_doc_score` (default 2).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_metric(conn: &rusqlite::Connection, sid: &str, doc_score: f64) {
    conn.execute(
        "INSERT INTO student_sprint_metrics (student_id, sprint_id, avg_doc_score)
         VALUES (?, ?, ?)",
        params![sid, common::SPRINT_ID, doc_score],
    )
    .unwrap();
}

#[test]
fn fires_when_avg_doc_score_below_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_metric(&conn, "alice", 1.0);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "LOW_DOC_SCORE", "alice"),
        1
    );
}

#[test]
fn silent_when_at_or_above_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_metric(&conn, "alice", 3.0);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "LOW_DOC_SCORE"),
        0
    );
}
