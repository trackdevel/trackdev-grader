//! ARCHITECTURE_HOTSPOT — per-student companion to ARCHITECTURE_DRIFT.
//! Sums each student's blame-attribution `weight` across the sprint's
//! `architecture_violations` rows; fires when the sum is ≥ the configured
//! threshold (`detector_thresholds.architecture_hotspot_min_weighted`).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_violation(
    conn: &rusqlite::Connection,
    sprint_id: i64,
    file: &str,
    rule: &str,
    severity: &str,
) -> i64 {
    conn.execute(
        "INSERT INTO architecture_violations
            (repo_full_name, sprint_id, file_path, rule_name, violation_kind,
             offending_import, severity, start_line, end_line, rule_kind)
         VALUES ('udg/x', ?, ?, ?, 'ast_forbidden_field_type', 'anchor', ?, 1, 5, 'ast_forbidden_field_type')",
        params![sprint_id, file, rule, severity],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn insert_attribution(
    conn: &rusqlite::Connection,
    rowid: i64,
    student_id: &str,
    weight: f64,
    sprint_id: i64,
) {
    conn.execute(
        "INSERT INTO architecture_violation_attribution
            (violation_rowid, student_id, lines_authored, total_lines, weight, sprint_id)
         VALUES (?, ?, 1, 5, ?, ?)",
        params![rowid, student_id, weight, sprint_id],
    )
    .unwrap();
}

#[test]
fn fires_when_weighted_sum_at_or_above_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let v1 = insert_violation(&conn, common::SPRINT_ID, "A.java", "r1", "WARNING");
    let v2 = insert_violation(&conn, common::SPRINT_ID, "B.java", "r2", "WARNING");
    insert_attribution(&conn, v1, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, v2, "alice", 1.0, common::SPRINT_ID);
    // sum = 2.0; default threshold = 2.0, so >=
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "ARCHITECTURE_HOTSPOT", "alice"),
        1
    );
    let details =
        common::flag_details_for(&conn, common::SPRINT_ID, "ARCHITECTURE_HOTSPOT", "alice")
            .unwrap();
    assert_eq!(details["weighted"].as_f64(), Some(2.0));
    assert_eq!(details["min_weighted"].as_f64(), Some(2.0));
    assert_eq!(details["offenders"].as_array().unwrap().len(), 2);
}

#[test]
fn silent_when_weighted_sum_below_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let v1 = insert_violation(&conn, common::SPRINT_ID, "A.java", "r1", "WARNING");
    insert_attribution(&conn, v1, "alice", 0.30, common::SPRINT_ID);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ARCHITECTURE_HOTSPOT"),
        0,
        "0.30 << 2.0 (default threshold)"
    );
}

#[test]
fn each_student_evaluated_independently() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    let v1 = insert_violation(&conn, common::SPRINT_ID, "A.java", "r1", "WARNING");
    let v2 = insert_violation(&conn, common::SPRINT_ID, "B.java", "r2", "WARNING");
    let v3 = insert_violation(&conn, common::SPRINT_ID, "C.java", "r3", "WARNING");
    // Alice owns 2.5 across three violations → fires.
    insert_attribution(&conn, v1, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, v2, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, v3, "alice", 0.5, common::SPRINT_ID);
    // Bob owns 1.0 total → silent.
    insert_attribution(&conn, v3, "bob", 0.5, common::SPRINT_ID);
    insert_attribution(&conn, v2, "bob", 0.5, common::SPRINT_ID);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "ARCHITECTURE_HOTSPOT", "alice"),
        1
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "ARCHITECTURE_HOTSPOT", "bob"),
        0
    );
}

#[test]
fn worst_severity_propagates_to_flag() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let v1 = insert_violation(&conn, common::SPRINT_ID, "A.java", "r1", "INFO");
    let v2 = insert_violation(&conn, common::SPRINT_ID, "B.java", "r2", "CRITICAL");
    insert_attribution(&conn, v1, "alice", 1.5, common::SPRINT_ID);
    insert_attribution(&conn, v2, "alice", 0.6, common::SPRINT_ID);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    let sev = common::flag_severity_for(&conn, common::SPRINT_ID, "ARCHITECTURE_HOTSPOT", "alice")
        .unwrap();
    assert_eq!(sev, "CRITICAL");
}

#[test]
fn silent_when_no_attribution_rows() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // Violation exists but no attribution rows mean nobody has a weight to sum.
    let _v = insert_violation(&conn, common::SPRINT_ID, "A.java", "r1", "WARNING");
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ARCHITECTURE_HOTSPOT"),
        0
    );
}
