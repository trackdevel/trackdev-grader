//! CONTRIBUTION_IMBALANCE — z-score on `student_sprint_metrics.points_share`
//! against the team mean (1/n).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn insert_metric(conn: &rusqlite::Connection, sid: &str, share: f64) {
    conn.execute(
        "INSERT INTO student_sprint_metrics (student_id, sprint_id, points_share) VALUES (?, ?, ?)",
        params![sid, common::SPRINT_ID, share],
    )
    .unwrap();
}

#[test]
fn fires_when_share_is_far_from_mean() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d"] {
        common::seed_student(&conn, sid);
    }
    // Three at ~0.27 share, one outlier at 0.85 — z-score >> 1.5.
    insert_metric(&conn, "a", 0.05);
    insert_metric(&conn, "b", 0.05);
    insert_metric(&conn, "c", 0.05);
    insert_metric(&conn, "d", 0.85);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert!(
        common::count_flags(&conn, common::SPRINT_ID, "CONTRIBUTION_IMBALANCE") >= 1,
        "expected at least one outlier",
    );
}

#[test]
fn silent_when_abs_deviation_below_min_even_if_z_high() {
    // Six students nearly equal: five at 0.159 and one at 0.205. The absolute
    // gap is 3.8pp from fair share (1/6 ≈ 0.167), yet stddev is so tight that
    // z >> 1.5. The min-abs-deviation gate must suppress the flag.
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d", "e", "f"] {
        common::seed_student(&conn, sid);
    }
    insert_metric(&conn, "a", 0.159);
    insert_metric(&conn, "b", 0.159);
    insert_metric(&conn, "c", 0.159);
    insert_metric(&conn, "d", 0.159);
    insert_metric(&conn, "e", 0.159);
    insert_metric(&conn, "f", 0.205);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "CONTRIBUTION_IMBALANCE"),
        0,
        "tight team with <5pp gap from equal share must not trip the flag",
    );
}

#[test]
fn silent_when_shares_are_uniform() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    for sid in ["a", "b", "c", "d"] {
        common::seed_student(&conn, sid);
        insert_metric(&conn, sid, 0.25);
    }
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "CONTRIBUTION_IMBALANCE"),
        0
    );
}
