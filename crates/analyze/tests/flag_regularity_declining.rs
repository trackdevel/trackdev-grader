//! REGULARITY_DECLINING — INFO when avg_regularity drops by more than
//! `regularity_declining_delta` between adjacent sprints AND both sprints
//! have ≥3 PRs (T-P0.8 added the min-PR gate).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_reg(conn: &rusqlite::Connection, sid: &str, sprint: i64, avg: f64, count: i64) {
    conn.execute(
        "INSERT INTO student_sprint_regularity
            (student_id, sprint_id, avg_regularity, pr_count)
         VALUES (?, ?, ?, ?)",
        params![sid, sprint, avg, count],
    )
    .unwrap();
}

#[test]
fn fires_when_avg_drops_with_enough_prs() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_reg(&conn, "alice", common::PRIOR_SPRINT_ID, 0.80, 5);
    insert_reg(&conn, "alice", common::SPRINT_ID, 0.20, 5);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "REGULARITY_DECLINING", "alice"),
        1
    );
}

#[test]
fn silent_when_pr_count_below_min() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_reg(&conn, "alice", common::PRIOR_SPRINT_ID, 0.80, 5);
    insert_reg(&conn, "alice", common::SPRINT_ID, 0.20, 2);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "REGULARITY_DECLINING"),
        0
    );
}

#[test]
fn silent_when_no_drop() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_reg(&conn, "alice", common::PRIOR_SPRINT_ID, 0.80, 5);
    insert_reg(&conn, "alice", common::SPRINT_ID, 0.85, 5);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "REGULARITY_DECLINING"),
        0
    );
}
