//! LAST_MINUTE_PR — WARNING for any PR with `pr_regularity.regularity_band
//! = 'last_minute'`.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_regularity(conn: &rusqlite::Connection, pr_id: &str, band: &str) {
    common::seed_pr(
        conn,
        pr_id,
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-15T23:30Z"),
        Some(40),
        Some(2),
        None,
    );
    conn.execute(
        "INSERT INTO pr_regularity
            (pr_id, sprint_id, student_id, merged_at, sprint_end,
             hours_before_deadline, regularity_score, regularity_band)
         VALUES (?, ?, 'alice', '2026-02-15T23:30Z', '2026-02-15T23:59Z',
                 0.5, 0.05, ?)",
        params![pr_id, common::SPRINT_ID, band],
    )
    .unwrap();
}

#[test]
fn fires_for_last_minute_band() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_regularity(&conn, "pr-1", "last_minute");
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "LAST_MINUTE_PR", "alice"),
        1
    );
}

#[test]
fn silent_for_other_bands() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_regularity(&conn, "pr-1", "good");
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "LAST_MINUTE_PR"),
        0
    );
}
