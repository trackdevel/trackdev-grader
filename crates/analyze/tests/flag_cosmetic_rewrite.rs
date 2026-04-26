//! COSMETIC_REWRITE — T-P1.2 split into VICTIM (INFO, original author) and
//! ACTOR (WARNING, rewriter). Verify both fire and are correctly attributed.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn emits_victim_and_actor_flags_with_correct_attribution() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice"); // rewriter
    common::seed_student(&conn, "bob"); // victim
    conn.execute(
        "INSERT INTO cosmetic_rewrites
            (sprint_id, file_path, repo_full_name, original_author_id,
             rewriter_id, statements_affected, change_type)
         VALUES (?, 'src/X.java', ?, 'bob', 'alice', 12, 'rename')",
        params![common::SPRINT_ID, common::REPO_FULL_NAME],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "COSMETIC_REWRITE_VICTIM", "bob"),
        1
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "COSMETIC_REWRITE_ACTOR", "alice"),
        1
    );
    assert_eq!(
        common::flag_severity_for(&conn, common::SPRINT_ID, "COSMETIC_REWRITE_VICTIM", "bob")
            .as_deref(),
        Some("INFO"),
    );
    assert_eq!(
        common::flag_severity_for(&conn, common::SPRINT_ID, "COSMETIC_REWRITE_ACTOR", "alice")
            .as_deref(),
        Some("WARNING"),
    );
}

#[test]
fn victim_only_when_rewriter_unknown() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "bob");
    conn.execute(
        "INSERT INTO cosmetic_rewrites
            (sprint_id, file_path, repo_full_name, original_author_id,
             rewriter_id, statements_affected, change_type)
         VALUES (?, 'src/X.java', ?, 'bob', NULL, 5, 'rename')",
        params![common::SPRINT_ID, common::REPO_FULL_NAME],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "COSMETIC_REWRITE_VICTIM"),
        1
    );
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "COSMETIC_REWRITE_ACTOR"),
        0
    );
}

#[test]
fn silent_when_no_rewrites_recorded() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "COSMETIC_REWRITE_VICTIM"),
        0
    );
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "COSMETIC_REWRITE_ACTOR"),
        0
    );
}
