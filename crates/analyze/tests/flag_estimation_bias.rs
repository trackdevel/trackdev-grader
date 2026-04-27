//! ESTIMATION_BIAS — WARNING when the per-student β_u credible
//! interval excludes 0 by more than 0.5 logits AND `n_tasks ≥ 5`.
//! Per-student attribution; the bias table is keyed by
//! `(student_id, project_id)`.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_bias(
    conn: &rusqlite::Connection,
    student: &str,
    project_id: i64,
    mean: f64,
    lower95: f64,
    upper95: f64,
    n_tasks: i64,
) {
    conn.execute(
        "INSERT INTO student_estimation_bias
            (student_id, project_id, beta_mean, beta_lower95, beta_upper95,
             n_tasks, fitted_at)
         VALUES (?, ?, ?, ?, ?, ?, '2026-04-26T00:00:00Z')",
        params![student, project_id, mean, lower95, upper95, n_tasks],
    )
    .unwrap();
}

#[test]
fn fires_when_cri_strictly_above_positive_margin_and_enough_tasks() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // CrI [0.6, 1.4] — strictly above +0.5; n_tasks ≥ 5.
    insert_bias(&conn, "alice", common::PROJECT_ID, 1.0, 0.6, 1.4, 8);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "ESTIMATION_BIAS", "alice"),
        1
    );
    let d = common::flag_details_for(&conn, common::SPRINT_ID, "ESTIMATION_BIAS", "alice").unwrap();
    assert_eq!(d["direction"].as_str(), Some("over"));
    assert_eq!(d["n_tasks"].as_i64(), Some(8));
}

#[test]
fn fires_when_cri_strictly_below_negative_margin() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "bob");
    // CrI [-1.4, -0.6] — strictly below −0.5.
    insert_bias(&conn, "bob", common::PROJECT_ID, -1.0, -1.4, -0.6, 6);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    let d = common::flag_details_for(&conn, common::SPRINT_ID, "ESTIMATION_BIAS", "bob").unwrap();
    assert_eq!(d["direction"].as_str(), Some("under"));
}

#[test]
fn silent_when_cri_straddles_zero() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "carol");
    insert_bias(&conn, "carol", common::PROJECT_ID, 0.1, -0.4, 0.6, 9);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ESTIMATION_BIAS"),
        0
    );
}

#[test]
fn silent_when_cri_excludes_zero_but_within_margin() {
    // CrI [0.05, 0.45] — excludes 0 but the lower bound is below the
    // 0.5-logit margin. Detector should not fire (it's the *margin*
    // that distinguishes ESTIMATION_BIAS from a generic non-zero test).
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "dan");
    insert_bias(&conn, "dan", common::PROJECT_ID, 0.25, 0.05, 0.45, 10);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ESTIMATION_BIAS"),
        0
    );
}

#[test]
fn silent_when_n_tasks_below_minimum() {
    // Same wide bias but n_tasks < 5 — small-sample mitigation: even
    // when the prior-shrunk posterior happens to clear the margin, we
    // require ≥5 observed tasks before flagging the student.
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "eve");
    insert_bias(&conn, "eve", common::PROJECT_ID, 1.0, 0.6, 1.4, 4);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ESTIMATION_BIAS"),
        0
    );
}

#[test]
fn silent_when_no_bias_row_for_student() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "frank");
    // No row in student_estimation_bias.
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ESTIMATION_BIAS"),
        0
    );
}
