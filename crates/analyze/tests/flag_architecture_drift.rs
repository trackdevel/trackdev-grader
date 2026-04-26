//! ARCHITECTURE_DRIFT — WARNING when this sprint's count of
//! `architecture_violations` rows is strictly higher than the most
//! recent prior sprint's count. Project-attributed (PROJECT_<id>).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_violation(conn: &rusqlite::Connection, sprint_id: i64, file: &str, rule: &str) {
    conn.execute(
        "INSERT INTO architecture_violations
            (repo_full_name, sprint_id, file_path, rule_name,
             violation_kind, offending_import, severity)
         VALUES ('udg/x', ?, ?, ?, 'layer_dependency', 'com.x.y', 'WARNING')",
        params![sprint_id, file, rule],
    )
    .unwrap();
}

#[test]
fn fires_when_count_strictly_increases() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    insert_violation(
        &conn,
        common::PRIOR_SPRINT_ID,
        "A.java",
        "presentation->!infrastructure",
    );
    insert_violation(
        &conn,
        common::SPRINT_ID,
        "A.java",
        "presentation->!infrastructure",
    );
    insert_violation(
        &conn,
        common::SPRINT_ID,
        "B.java",
        "presentation->!infrastructure",
    );
    insert_violation(
        &conn,
        common::SPRINT_ID,
        "C.java",
        "presentation->!infrastructure",
    );

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    let n = common::count_flags_for(&conn, common::SPRINT_ID, "ARCHITECTURE_DRIFT", "PROJECT_1");
    assert_eq!(n, 1);
    let details =
        common::flag_details_for(&conn, common::SPRINT_ID, "ARCHITECTURE_DRIFT", "PROJECT_1")
            .unwrap();
    assert_eq!(details["current"].as_i64(), Some(3));
    assert_eq!(details["previous"].as_i64(), Some(1));
    assert_eq!(details["delta"].as_i64(), Some(2));
}

#[test]
fn silent_when_count_unchanged() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    insert_violation(&conn, common::PRIOR_SPRINT_ID, "A.java", "rule");
    insert_violation(&conn, common::SPRINT_ID, "A.java", "rule");
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ARCHITECTURE_DRIFT"),
        0
    );
}

#[test]
fn silent_when_count_decreases() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    insert_violation(&conn, common::PRIOR_SPRINT_ID, "A.java", "rule");
    insert_violation(&conn, common::PRIOR_SPRINT_ID, "B.java", "rule");
    insert_violation(&conn, common::SPRINT_ID, "A.java", "rule");
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ARCHITECTURE_DRIFT"),
        0
    );
}

#[test]
fn silent_when_no_prior_sprint_exists() {
    // Only one sprint in the project — no baseline to compare against.
    let conn = common::make_db();
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (1, 'team-test', 'Test Project')",
        [],
    )
    .unwrap();
    common::seed_sprint(
        &conn,
        common::SPRINT_ID,
        common::PROJECT_ID,
        "Sprint 1",
        "2026-02-01",
        "2026-02-15",
    );
    insert_violation(&conn, common::SPRINT_ID, "A.java", "rule");
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ARCHITECTURE_DRIFT"),
        0
    );
}

#[test]
fn fires_when_prior_was_zero_and_current_is_nonzero() {
    // 0 → N is a strict increase and should fire — going from a clean
    // sprint to any drift is exactly the kind of regression the detector
    // is meant to surface.
    let conn = common::make_db();
    common::seed_default_project(&conn);
    insert_violation(&conn, common::SPRINT_ID, "A.java", "rule");
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "ARCHITECTURE_DRIFT"),
        1
    );
}
