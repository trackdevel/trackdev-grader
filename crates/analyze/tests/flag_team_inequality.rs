//! TEAM_INEQUALITY detector removed — flag must not be emitted.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn team_inequality_detector_disabled_even_when_gini_high() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d"] {
        common::seed_student(&conn, sid);
    }
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
    conn.execute(
        "INSERT INTO team_sprint_inequality
            (project_id, sprint_id, metric_name, gini, hoover, cv, member_count)
         VALUES (1, ?, 'pr_lines', 0.55, 0.40, 1.20, 4)",
        params![common::SPRINT_ID],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "TEAM_INEQUALITY"),
        0
    );
}
