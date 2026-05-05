//! COMPLEXITY_HOTSPOT — per-student companion to the testability scan
//! (T-CX). Sums each student's `weight × severity_rank` across the
//! sprint's `method_complexity_findings` rows; fires WARNING above
//! `detector_thresholds.complexity_hotspot_warn` and CRITICAL above
//! `detector_thresholds.complexity_hotspot_crit`.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[allow(clippy::too_many_arguments)]
fn insert_finding(
    conn: &rusqlite::Connection,
    sprint_id: i64,
    file: &str,
    method: &str,
    rule_key: &str,
    severity: &str,
    measured: Option<f64>,
    threshold: Option<f64>,
) -> i64 {
    conn.execute(
        "INSERT INTO method_complexity_findings
            (sprint_id, project_id, repo_full_name, file_path, class_name,
             method_name, start_line, end_line, rule_key, severity,
             measured_value, threshold, detail)
         VALUES (?, 1, 'udg/x', ?, 'A', ?, 10, 30, ?, ?, ?, ?, '')",
        params![sprint_id, file, method, rule_key, severity, measured, threshold,],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn insert_attribution(
    conn: &rusqlite::Connection,
    finding_id: i64,
    student_id: &str,
    weight: f64,
    sprint_id: i64,
) {
    conn.execute(
        "INSERT INTO method_complexity_attribution
            (finding_id, student_id, lines_attributed, weighted_lines, weight, sprint_id)
         VALUES (?, ?, 5, 10.0, ?, ?)",
        params![finding_id, student_id, weight, sprint_id],
    )
    .unwrap();
}

fn config_with_thresholds(warn: f64, crit: f64) -> Config {
    let mut c = Config::test_default();
    c.detector_thresholds.complexity_hotspot_warn = warn;
    c.detector_thresholds.complexity_hotspot_crit = crit;
    c
}

#[test]
fn silent_when_score_below_warn_band() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let f = insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "f",
        "broad-catch",
        "WARNING",
        None,
        None,
    );
    // weight 1.0 * rank 2 = 2.0 score; warn threshold is 4.
    insert_attribution(&conn, f, "alice", 1.0, common::SPRINT_ID);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_thresholds(4.0, 8.0))
        .unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT"),
        0,
        "score 2.0 < warn 4.0 must not fire"
    );
}

#[test]
fn warning_at_warn_band() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // Two WARNING-severity findings owned in full → score 2 + 2 = 4 → warn.
    let f1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "f",
        "cyclomatic",
        "WARNING",
        Some(12.0),
        Some(10.0),
    );
    let f2 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "B.java",
        "g",
        "broad-catch",
        "WARNING",
        None,
        None,
    );
    insert_attribution(&conn, f1, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, f2, "alice", 1.0, common::SPRINT_ID);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_thresholds(4.0, 8.0))
        .unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT", "alice"),
        1
    );
    let sev =
        common::flag_severity_for(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT", "alice").unwrap();
    assert_eq!(sev, "WARNING");
    let details =
        common::flag_details_for(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT", "alice").unwrap();
    assert!((details["score"].as_f64().unwrap() - 4.0).abs() < 1e-9);
    let offenders = details["offenders"].as_array().unwrap();
    assert_eq!(offenders.len(), 2);
    let rule_keys: Vec<&str> = offenders
        .iter()
        .map(|o| o["rule_key"].as_str().unwrap())
        .collect();
    assert!(rule_keys.contains(&"cyclomatic"));
    assert!(rule_keys.contains(&"broad-catch"));
}

#[test]
fn critical_at_crit_band() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // Four full-weight WARNING findings: 4 * 2 = 8 = crit threshold.
    for i in 0..4 {
        let f = insert_finding(
            &conn,
            common::SPRINT_ID,
            &format!("F{i}.java"),
            "f",
            "cognitive",
            "WARNING",
            Some(25.0),
            Some(15.0),
        );
        insert_attribution(&conn, f, "alice", 1.0, common::SPRINT_ID);
    }
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_thresholds(4.0, 8.0))
        .unwrap();
    let sev =
        common::flag_severity_for(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT", "alice").unwrap();
    assert_eq!(sev, "CRITICAL");
}

#[test]
fn critical_severity_propagates_even_when_score_low() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // One CRITICAL contributing 3.0 + one WARNING contributing 2.0 = 5.0 in
    // the warn band; the worst-severity rule was CRITICAL, so the flag
    // escalates to CRITICAL even though the score is below the crit band.
    let f1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "f",
        "cyclomatic",
        "CRITICAL",
        Some(20.0),
        Some(15.0),
    );
    let f2 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "B.java",
        "g",
        "broad-catch",
        "WARNING",
        None,
        None,
    );
    insert_attribution(&conn, f1, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, f2, "alice", 1.0, common::SPRINT_ID);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_thresholds(4.0, 8.0))
        .unwrap();
    let sev =
        common::flag_severity_for(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT", "alice").unwrap();
    assert_eq!(sev, "CRITICAL", "CRITICAL contributing rule must escalate");
}

#[test]
fn each_student_evaluated_independently() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    // alice owns 100% of three WARNING findings → score 6 → fires.
    // bob owns 50% of one WARNING finding → score 1 → silent.
    let f1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "f",
        "cyclomatic",
        "WARNING",
        Some(12.0),
        Some(10.0),
    );
    let f2 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "B.java",
        "g",
        "broad-catch",
        "WARNING",
        None,
        None,
    );
    let f3 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "C.java",
        "h",
        "long-method",
        "WARNING",
        Some(80.0),
        Some(60.0),
    );
    insert_attribution(&conn, f1, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, f2, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, f3, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, f3, "bob", 0.5, common::SPRINT_ID);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_thresholds(4.0, 8.0))
        .unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT", "alice"),
        1
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT", "bob"),
        0
    );
}

#[test]
fn offenders_list_capped_at_top_three_by_contribution() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // Five contributing findings; the top-3 by contribution must be the
    // CRITICAL rows (3.0 each), then the WARNING (2.0).
    let crit1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "C1.java",
        "x",
        "cyclomatic",
        "CRITICAL",
        Some(20.0),
        Some(15.0),
    );
    let crit2 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "C2.java",
        "y",
        "cognitive",
        "CRITICAL",
        Some(25.0),
        Some(20.0),
    );
    let warn1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "W1.java",
        "p",
        "broad-catch",
        "WARNING",
        None,
        None,
    );
    let info1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "I1.java",
        "q",
        "static-singleton",
        "INFO",
        None,
        None,
    );
    let info2 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "I2.java",
        "r",
        "static-singleton",
        "INFO",
        None,
        None,
    );
    for f in [crit1, crit2, warn1, info1, info2] {
        insert_attribution(&conn, f, "alice", 1.0, common::SPRINT_ID);
    }
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_thresholds(4.0, 8.0))
        .unwrap();
    let details =
        common::flag_details_for(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT", "alice").unwrap();
    let offenders = details["offenders"].as_array().unwrap();
    assert_eq!(offenders.len(), 3, "top-3 cap");
    let severities: Vec<&str> = offenders
        .iter()
        .map(|o| o["severity"].as_str().unwrap())
        .collect();
    // The two CRITICALs and the WARNING must be the picks; both INFOs
    // (1.0 contribution each) are dropped.
    assert_eq!(severities.iter().filter(|s| **s == "CRITICAL").count(), 2);
    assert_eq!(severities.iter().filter(|s| **s == "WARNING").count(), 1);
    assert_eq!(severities.iter().filter(|s| **s == "INFO").count(), 0);
}

#[test]
fn silent_when_no_attribution_rows_exist() {
    // Findings without attribution must not fire — the score is 0.
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "f",
        "cyclomatic",
        "WARNING",
        Some(12.0),
        Some(10.0),
    );
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_thresholds(4.0, 8.0))
        .unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "COMPLEXITY_HOTSPOT"),
        0
    );
}
