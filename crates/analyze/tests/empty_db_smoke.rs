//! T-P2.7 smoke test: every detector survives a freshly-applied schema with
//! zero data. Catches the class of bug where a detector references a column
//! that was renamed or removed without updating the SQL — those used to
//! surface only on full pipeline runs.

mod common;

use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn every_detector_runs_clean_against_empty_db() {
    let conn = common::make_db();
    common::seed_default_project(&conn);

    let cfg = Config::test_default();
    let total = detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &cfg)
        .expect("dispatcher must not error on empty db");

    assert_eq!(
        total,
        0,
        "no detector should fire against an empty fixture; fired: {:?}",
        common::fired_flag_types(&conn, common::SPRINT_ID),
    );
}

#[test]
fn dispatcher_does_not_panic_when_sprint_does_not_exist() {
    let conn = common::make_db();
    let cfg = Config::test_default();
    // No project, no sprint, no students. Detectors should each fail
    // independently (the run! macro logs a warn) but the dispatcher must
    // return Ok with zero flags.
    let total = detect_flags_for_sprint_id(&conn, 99_999, &cfg)
        .expect("dispatcher swallows per-detector errors");
    assert_eq!(total, 0);
}
