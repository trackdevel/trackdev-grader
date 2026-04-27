//! LOW_MUTATION_SCORE — per-PR severity-tiered flag based on the
//! Pitest mutation score recorded in `pr_mutation` (T-P2.4).
//! Attributed to the PR author. Uses the configured info/warning
//! thresholds (defaults 0.50 / 0.30).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_mutation(
    conn: &rusqlite::Connection,
    pr_id: &str,
    sprint_id: i64,
    score: Option<f64>,
    total: i64,
    killed: i64,
) {
    conn.execute(
        "INSERT INTO pr_mutation
            (pr_id, repo_name, sprint_id, mutants_total, mutants_killed,
             mutation_score, duration_seconds)
         VALUES (?, 'android-test', ?, ?, ?, ?, 12.5)",
        params![pr_id, sprint_id, total, killed, score],
    )
    .unwrap();
}

fn seed_pr_for_alice(conn: &rusqlite::Connection, pr_id: &str, pr_number: i64) {
    common::seed_pr(
        conn,
        pr_id,
        pr_number,
        "udg/android-test",
        Some("alice"),
        Some("alice"),
        "MERGED",
        true,
        Some("2026-02-10T10:00:00Z"),
        Some(100),
        Some(20),
        Some("body"),
    );
}

#[test]
fn fires_warning_when_score_below_warning_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    seed_pr_for_alice(&conn, "pr-1", 1);
    insert_mutation(&conn, "pr-1", common::SPRINT_ID, Some(0.20), 30, 6);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    let n = common::count_flags_for(&conn, common::SPRINT_ID, "LOW_MUTATION_SCORE", "alice");
    assert_eq!(n, 1);
    let row: (String, String) = conn
        .query_row(
            "SELECT severity, details FROM flags
             WHERE flag_type='LOW_MUTATION_SCORE' AND student_id='alice'
             AND sprint_id = ?",
            [common::SPRINT_ID],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row.0, "WARNING");
    let details: serde_json::Value = serde_json::from_str(&row.1).unwrap();
    assert_eq!(details["mutation_score"].as_f64(), Some(0.20));
    assert_eq!(details["mutants_killed"].as_i64(), Some(6));
}

#[test]
fn fires_info_when_score_between_thresholds() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    seed_pr_for_alice(&conn, "pr-1", 1);
    insert_mutation(&conn, "pr-1", common::SPRINT_ID, Some(0.40), 20, 8);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    let row: String = conn
        .query_row(
            "SELECT severity FROM flags
             WHERE flag_type='LOW_MUTATION_SCORE' AND student_id='alice'
             AND sprint_id = ?",
            [common::SPRINT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(row, "INFO");
}

#[test]
fn silent_when_score_at_or_above_info_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    seed_pr_for_alice(&conn, "pr-1", 1);
    insert_mutation(&conn, "pr-1", common::SPRINT_ID, Some(0.50), 20, 10);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "LOW_MUTATION_SCORE"),
        0
    );
}

#[test]
fn silent_when_mutation_score_is_null() {
    // NULL score = report exists but every mutant non-viable, or the
    // run timed out. We don't grade what we couldn't measure.
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    seed_pr_for_alice(&conn, "pr-1", 1);
    insert_mutation(&conn, "pr-1", common::SPRINT_ID, None, 5, 0);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "LOW_MUTATION_SCORE"),
        0
    );
}

#[test]
fn silent_when_no_pr_mutation_row() {
    // No row in pr_mutation = mutation testing was opt-out (the
    // [mutation] enabled = false case, or the profile had no
    // mutation_command). Detector must not synthesise zero-scores.
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    seed_pr_for_alice(&conn, "pr-1", 1);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "LOW_MUTATION_SCORE"),
        0
    );
}
