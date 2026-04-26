//! ALL_PRS_LATE — WARNING when `student_sprint_regularity.avg_regularity`
//! is below `late_regularity` (0.20 default) AND `pr_count >= 2`.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_regularity(conn: &rusqlite::Connection, sid: &str, avg: f64, count: i64) {
    conn.execute(
        "INSERT INTO student_sprint_regularity
            (student_id, sprint_id, avg_regularity, pr_count,
             prs_in_last_24h, prs_in_last_3h)
         VALUES (?, ?, ?, ?, 0, 0)",
        params![sid, common::SPRINT_ID, avg, count],
    )
    .unwrap();
}

#[test]
fn fires_when_avg_low_and_pr_count_meets_min() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_regularity(&conn, "alice", 0.05, 4);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "ALL_PRS_LATE", "alice"),
        1
    );
}

#[test]
fn silent_when_pr_count_below_min() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_regularity(&conn, "alice", 0.05, 1);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ALL_PRS_LATE"),
        0
    );
}

#[test]
fn silent_when_regularity_above_late_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_regularity(&conn, "alice", 0.50, 4);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ALL_PRS_LATE"),
        0
    );
}
