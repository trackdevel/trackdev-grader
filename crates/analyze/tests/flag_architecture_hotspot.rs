//! ARCHITECTURE_HOTSPOT — per-student artifact flag (T-P3.4). Sums each
//! student's blame-attribution `weight` across the project's
//! `architecture_violations` rows and fires when the sum is ≥ the
//! configured threshold (`detector_thresholds.architecture_hotspot_min_weighted`).
//! Sprint-free: the flag lives in `student_artifact_flags`, not
//! `flags`. The retired ARCHITECTURE_DRIFT (per-sprint trajectory)
//! stayed retired in PR 1.

mod common;

use rusqlite::params;
use sprint_grader_analyze::detect_artifact_flags_for_project_id;
use sprint_grader_core::Config;

fn insert_violation(conn: &rusqlite::Connection, file: &str, rule: &str, severity: &str) -> i64 {
    conn.execute(
        "INSERT INTO architecture_violations
            (repo_full_name, file_path, rule_name,
             offending_import, severity, start_line, end_line, rule_kind)
         VALUES ('udg/x', ?, ?, 'anchor', ?, 1, 5, 'ast_forbidden_field_type')",
        params![file, rule, severity],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn insert_attribution(conn: &rusqlite::Connection, rowid: i64, student_id: &str, weight: f64) {
    conn.execute(
        "INSERT INTO architecture_violation_attribution
            (violation_rowid, student_id, lines_authored, total_lines, weight)
         VALUES (?, ?, 1, 5, ?)",
        params![rowid, student_id, weight],
    )
    .unwrap();
}

#[test]
fn fires_when_weighted_sum_at_or_above_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let v1 = insert_violation(&conn, "A.java", "r1", "WARNING");
    let v2 = insert_violation(&conn, "B.java", "r2", "WARNING");
    insert_attribution(&conn, v1, "alice", 1.0);
    insert_attribution(&conn, v2, "alice", 1.0);
    // sum = 2.0; default threshold = 2.0, so >=
    detect_artifact_flags_for_project_id(&conn, common::PROJECT_ID, &Config::test_default())
        .unwrap();

    assert_eq!(
        common::count_artifact_flags_for(
            &conn,
            common::PROJECT_ID,
            "ARCHITECTURE_HOTSPOT",
            "alice"
        ),
        1
    );
    let details = common::artifact_flag_details_for(
        &conn,
        common::PROJECT_ID,
        "ARCHITECTURE_HOTSPOT",
        "alice",
    )
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
    let v1 = insert_violation(&conn, "A.java", "r1", "WARNING");
    insert_attribution(&conn, v1, "alice", 0.30);
    detect_artifact_flags_for_project_id(&conn, common::PROJECT_ID, &Config::test_default())
        .unwrap();

    assert_eq!(
        common::count_artifact_flags(&conn, common::PROJECT_ID, "ARCHITECTURE_HOTSPOT"),
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
    let v1 = insert_violation(&conn, "A.java", "r1", "WARNING");
    let v2 = insert_violation(&conn, "B.java", "r2", "WARNING");
    let v3 = insert_violation(&conn, "C.java", "r3", "WARNING");
    // Alice owns 2.5 across three violations → fires.
    insert_attribution(&conn, v1, "alice", 1.0);
    insert_attribution(&conn, v2, "alice", 1.0);
    insert_attribution(&conn, v3, "alice", 0.5);
    // Bob owns 1.0 total → silent.
    insert_attribution(&conn, v3, "bob", 0.5);
    insert_attribution(&conn, v2, "bob", 0.5);

    detect_artifact_flags_for_project_id(&conn, common::PROJECT_ID, &Config::test_default())
        .unwrap();
    assert_eq!(
        common::count_artifact_flags_for(
            &conn,
            common::PROJECT_ID,
            "ARCHITECTURE_HOTSPOT",
            "alice"
        ),
        1
    );
    assert_eq!(
        common::count_artifact_flags_for(&conn, common::PROJECT_ID, "ARCHITECTURE_HOTSPOT", "bob"),
        0
    );
}

#[test]
fn worst_severity_propagates_to_flag() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let v1 = insert_violation(&conn, "A.java", "r1", "INFO");
    let v2 = insert_violation(&conn, "B.java", "r2", "CRITICAL");
    insert_attribution(&conn, v1, "alice", 1.5);
    insert_attribution(&conn, v2, "alice", 0.6);

    detect_artifact_flags_for_project_id(&conn, common::PROJECT_ID, &Config::test_default())
        .unwrap();
    let sev = common::artifact_flag_severity_for(
        &conn,
        common::PROJECT_ID,
        "ARCHITECTURE_HOTSPOT",
        "alice",
    )
    .unwrap();
    assert_eq!(sev, "CRITICAL");
}

#[test]
fn silent_when_no_attribution_rows() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let _v = insert_violation(&conn, "A.java", "r1", "WARNING");
    detect_artifact_flags_for_project_id(&conn, common::PROJECT_ID, &Config::test_default())
        .unwrap();
    assert_eq!(
        common::count_artifact_flags(&conn, common::PROJECT_ID, "ARCHITECTURE_HOTSPOT"),
        0
    );
}

#[test]
fn dispatcher_idempotently_replaces_prior_flag_rows() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let v1 = insert_violation(&conn, "A.java", "r1", "WARNING");
    insert_attribution(&conn, v1, "alice", 2.5);

    for _ in 0..3 {
        detect_artifact_flags_for_project_id(&conn, common::PROJECT_ID, &Config::test_default())
            .unwrap();
    }
    assert_eq!(
        common::count_artifact_flags_for(
            &conn,
            common::PROJECT_ID,
            "ARCHITECTURE_HOTSPOT",
            "alice"
        ),
        1,
        "re-running the dispatcher must not duplicate flag rows"
    );
}
