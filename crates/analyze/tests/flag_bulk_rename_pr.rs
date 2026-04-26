//! BULK_RENAME_PR — INFO when adds≈dels, total > line floor, AND the
//! normalized survival rate exceeds the raw survival rate by >0.30 (i.e.,
//! the AST normaliser absorbed most of the diff, signalling churn).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_when_balanced_diff_normalises_away() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // 200 / 220 ≈ 0.91 ratio, total = 420 > 50 floor.
    common::seed_pr(
        &conn,
        "pr-1",
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-10T10:00Z"),
        Some(220),
        Some(200),
        None,
    );
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(2),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 1, "pr-1");
    // raw rate 0.10, normalised 0.95 → divergence > 0.30.
    conn.execute(
        "INSERT INTO pr_survival
            (pr_id, sprint_id, statements_added_raw, statements_surviving_raw,
             statements_added_normalized, statements_surviving_normalized)
         VALUES ('pr-1', ?, 100, 10, 100, 95)",
        params![common::SPRINT_ID],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "BULK_RENAME_PR", "alice"),
        1
    );
}

#[test]
fn silent_when_diff_is_substantive() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_pr(
        &conn,
        "pr-1",
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-10T10:00Z"),
        Some(220),
        Some(200),
        None,
    );
    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(2),
        "DONE",
        "TASK",
    );
    common::link_task_pr(&conn, 1, "pr-1");
    // raw and normalized agree → no divergence → no flag.
    conn.execute(
        "INSERT INTO pr_survival
            (pr_id, sprint_id, statements_added_raw, statements_surviving_raw,
             statements_added_normalized, statements_surviving_normalized)
         VALUES ('pr-1', ?, 100, 90, 100, 92)",
        params![common::SPRINT_ID],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "BULK_RENAME_PR"),
        0
    );
}
