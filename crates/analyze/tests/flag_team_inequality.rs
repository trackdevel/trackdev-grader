//! TEAM_INEQUALITY — fires per-member when the team's gini for a metric
//! crosses warn/crit AND the member's value deviates from the mean by more
//! than `team_inequality_outlier_deviation`.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_when_gini_high_and_member_is_outlier() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d"] {
        common::seed_student(&conn, sid);
    }
    // weighted_pr_lines metric — feeds team_inequality_evidence_for_member.
    let rows = [("a", 100.0), ("b", 100.0), ("c", 100.0), ("d", 1000.0)];
    for (sid, lines) in rows {
        conn.execute(
            "INSERT INTO student_sprint_metrics
                (student_id, sprint_id, weighted_pr_lines, points_share)
             VALUES (?, ?, ?, 0.25)",
            params![sid, common::SPRINT_ID, lines],
        )
        .unwrap();
    }
    // Crit-band gini for the lines metric.
    conn.execute(
        "INSERT INTO team_sprint_inequality
            (project_id, sprint_id, metric_name, gini, hoover, cv, member_count)
         VALUES (1, ?, 'pr_lines', 0.55, 0.40, 1.20, 4)",
        params![common::SPRINT_ID],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert!(
        common::count_flags(&conn, common::SPRINT_ID, "TEAM_INEQUALITY") >= 1,
        "expected at least one TEAM_INEQUALITY flag",
    );
}

#[test]
fn silent_when_gini_below_warn_band() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    conn.execute(
        "INSERT INTO student_sprint_metrics (student_id, sprint_id, weighted_pr_lines)
         VALUES ('alice', ?, 100.0)",
        params![common::SPRINT_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO team_sprint_inequality
            (project_id, sprint_id, metric_name, gini, hoover, cv, member_count)
         VALUES (1, ?, 'pr_lines', 0.10, 0.05, 0.20, 1)",
        params![common::SPRINT_ID],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "TEAM_INEQUALITY"),
        0
    );
}
