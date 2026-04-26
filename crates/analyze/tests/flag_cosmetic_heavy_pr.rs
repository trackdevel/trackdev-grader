//! COSMETIC_HEAVY_PR — WARNING when (1 - lar/lat) exceeds the configured
//! `repo_analysis.cosmetic_share_threshold` (0.50 default).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_pr_and_metrics(conn: &rusqlite::Connection, lat: i64, lar: i64) {
    common::seed_pr(
        conn,
        "pr-1",
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "merged",
        true,
        Some("2026-02-10T10:00Z"),
        Some(lat),
        Some(0),
        None,
    );
    common::seed_task(
        conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(2),
        "DONE",
        "TASK",
    );
    common::link_task_pr(conn, 1, "pr-1");
    conn.execute(
        "INSERT INTO pr_line_metrics (pr_id, sprint_id, lat, lar)
         VALUES ('pr-1', ?, ?, ?)",
        params![common::SPRINT_ID, lat, lar],
    )
    .unwrap();
}

#[test]
fn fires_when_cosmetic_share_above_threshold() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_pr_and_metrics(&conn, 100, 10); // cosmetic_share = 0.90
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "COSMETIC_HEAVY_PR", "alice"),
        1
    );
}

#[test]
fn silent_when_lar_dominates_lat() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    insert_pr_and_metrics(&conn, 100, 95); // cosmetic_share = 0.05
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "COSMETIC_HEAVY_PR"),
        0
    );
}
