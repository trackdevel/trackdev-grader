//! CROSS_TEAM_SIMILARITY — CRITICAL, attributed to a synthetic
//! `PROJECT_<id>` student id (not a real student).

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_for_each_team_when_match_recorded() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (2, 'team-other', 'Other')",
        params![],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cross_team_matches
            (sprint_id, team_a_project_id, team_b_project_id, file_path_a,
             file_path_b, method_name, fingerprint)
         VALUES (?, 1, 2, 'A.java', 'A.java', 'doX', 'aaaaaaaaaaaaaaaaXX')",
        params![common::SPRINT_ID],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(
            &conn,
            common::SPRINT_ID,
            "CROSS_TEAM_SIMILARITY",
            "PROJECT_1"
        ),
        1
    );
    assert_eq!(
        common::count_flags_for(
            &conn,
            common::SPRINT_ID,
            "CROSS_TEAM_SIMILARITY",
            "PROJECT_2"
        ),
        1
    );
}

#[test]
fn silent_when_no_matches() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "CROSS_TEAM_SIMILARITY"),
        0
    );
}
