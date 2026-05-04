//! STATIC_ANALYSIS_HOTSPOT — per-student companion to the PMD/Checkstyle/
//! SpotBugs scan (T-SA). Sums each student's blame-attribution `weight`
//! across the sprint's `static_analysis_findings` rows; fires when the sum
//! is ≥ `detector_thresholds.static_analysis_hotspot_min_weighted` (default
//! `10.0`).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[allow(clippy::too_many_arguments)]
fn insert_finding(
    conn: &rusqlite::Connection,
    sprint_id: i64,
    file: &str,
    rule_id: &str,
    severity: &str,
    analyzer: &str,
    category: &str,
) -> i64 {
    let fingerprint = format!("fp-{analyzer}-{rule_id}-{file}");
    conn.execute(
        "INSERT INTO static_analysis_findings
            (repo_full_name, sprint_id, analyzer, rule_id, severity, category, file_path,
             start_line, end_line, message, fingerprint)
         VALUES ('udg/x', ?, ?, ?, ?, ?, ?, 1, 5, 'msg', ?)",
        params![
            sprint_id,
            analyzer,
            rule_id,
            severity,
            category,
            file,
            fingerprint
        ],
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
        "INSERT INTO static_analysis_finding_attribution
            (finding_id, student_id, lines_authored, total_lines, weight, sprint_id)
         VALUES (?, ?, 1, 5, ?, ?)",
        params![finding_id, student_id, weight, sprint_id],
    )
    .unwrap();
}

fn config_with_threshold(threshold: f64) -> Config {
    let mut c = Config::test_default();
    c.detector_thresholds.static_analysis_hotspot_min_weighted = threshold;
    c
}

#[test]
fn fires_when_weighted_sum_at_or_above_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let f1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "UnusedPrivateField",
        "WARNING",
        "pmd",
        "bug",
    );
    let f2 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "B.java",
        "EmptyCatchBlock",
        "WARNING",
        "pmd",
        "bug",
    );
    insert_attribution(&conn, f1, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, f2, "alice", 1.0, common::SPRINT_ID);
    // sum = 2.0; with a 2.0 threshold the flag fires.
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_threshold(2.0)).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "STATIC_ANALYSIS_HOTSPOT", "alice"),
        1
    );
    let details =
        common::flag_details_for(&conn, common::SPRINT_ID, "STATIC_ANALYSIS_HOTSPOT", "alice")
            .unwrap();
    assert_eq!(details["weighted"].as_f64(), Some(2.0));
    assert_eq!(details["min_weighted"].as_f64(), Some(2.0));
    let offenders = details["offenders"].as_array().unwrap();
    assert_eq!(offenders.len(), 2);
    assert_eq!(offenders[0]["analyzer"], "pmd");
    assert!(offenders
        .iter()
        .any(|o| o["rule_id"] == "UnusedPrivateField"));
    assert!(offenders.iter().any(|o| o["category"] == "bug"));
}

#[test]
fn default_threshold_keeps_flag_silent() {
    // Phase-1 sign-off: the default threshold (10.0) should keep the flag
    // effectively silent — feedback-only behaviour. With 5.0 weight,
    // Config::test_default() must not fire it.
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let f1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "R",
        "WARNING",
        "pmd",
        "style",
    );
    insert_attribution(&conn, f1, "alice", 5.0, common::SPRINT_ID);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "STATIC_ANALYSIS_HOTSPOT"),
        0,
        "5.0 << 10.0 (default threshold) — phase-1 feedback-only stance"
    );
}

#[test]
fn worst_severity_propagates_to_flag() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    let f1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "R1",
        "INFO",
        "checkstyle",
        "style",
    );
    let f2 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "B.java",
        "R2",
        "CRITICAL",
        "spotbugs",
        "security",
    );
    insert_attribution(&conn, f1, "alice", 1.5, common::SPRINT_ID);
    insert_attribution(&conn, f2, "alice", 0.6, common::SPRINT_ID);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_threshold(2.0)).unwrap();
    let sev =
        common::flag_severity_for(&conn, common::SPRINT_ID, "STATIC_ANALYSIS_HOTSPOT", "alice")
            .unwrap();
    assert_eq!(sev, "CRITICAL");
}

#[test]
fn each_student_evaluated_independently() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    let f1 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "R1",
        "WARNING",
        "pmd",
        "bug",
    );
    let f2 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "B.java",
        "R2",
        "WARNING",
        "pmd",
        "bug",
    );
    let f3 = insert_finding(
        &conn,
        common::SPRINT_ID,
        "C.java",
        "R3",
        "WARNING",
        "pmd",
        "bug",
    );
    // Alice owns 2.5 across three findings → fires under threshold 2.0.
    insert_attribution(&conn, f1, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, f2, "alice", 1.0, common::SPRINT_ID);
    insert_attribution(&conn, f3, "alice", 0.5, common::SPRINT_ID);
    // Bob owns 1.0 total → silent.
    insert_attribution(&conn, f3, "bob", 0.5, common::SPRINT_ID);
    insert_attribution(&conn, f2, "bob", 0.5, common::SPRINT_ID);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &config_with_threshold(2.0)).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "STATIC_ANALYSIS_HOTSPOT", "alice"),
        1
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "STATIC_ANALYSIS_HOTSPOT", "bob"),
        0
    );
}
