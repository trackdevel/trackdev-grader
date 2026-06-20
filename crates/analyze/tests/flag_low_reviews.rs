//! LOW_REVIEWS — fires on team-normalized PR-review participation
//! (`student_sprint_contribution.review_signal`): WARNING below `review_warn`
//! (0.20), CRITICAL below `review_crit` (0.10). The CRITICAL band is a graded
//! behavioural penalty (review effort is not captured by effective points).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_contribution(conn: &rusqlite::Connection, sid: &str, review: f64) {
    conn.execute(
        "INSERT INTO student_sprint_contribution
            (student_id, sprint_id, code_signal, review_signal, task_signal,
             process_signal, composite_score)
         VALUES (?, ?, 0.50, ?, 0.50, 0.50, 0.50)",
        params![sid, common::SPRINT_ID, review],
    )
    .unwrap();
}

#[test]
fn critical_when_below_review_crit() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_contribution(&conn, "alice", 0.05);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::flag_severity_for(&conn, common::SPRINT_ID, "LOW_REVIEWS", "alice").as_deref(),
        Some("CRITICAL"),
    );
}

#[test]
fn warning_when_between_warn_and_crit() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_contribution(&conn, "alice", 0.15);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::flag_severity_for(&conn, common::SPRINT_ID, "LOW_REVIEWS", "alice").as_deref(),
        Some("WARNING"),
    );
}

#[test]
fn silent_when_above_warn() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_contribution(&conn, "alice", 0.50);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "LOW_REVIEWS"),
        0
    );
}
