//! CONTRIBUTION_IMBALANCE detector removed — flag must not be emitted.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn contribution_imbalance_detector_disabled() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d"] {
        common::seed_student(&conn, sid);
    }
    let shares = [("a", 0.70), ("b", 0.10), ("c", 0.10), ("d", 0.10)];
    for (sid, share) in shares {
        conn.execute(
            "INSERT INTO student_sprint_metrics (student_id, sprint_id, points_share)
             VALUES (?, ?, ?)",
            params![sid, common::SPRINT_ID, share],
        )
        .unwrap();
    }

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "CONTRIBUTION_IMBALANCE"),
        0
    );
}
