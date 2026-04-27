//! UNKNOWN_CONTRIBUTOR — WARNING when a commit author / PR author / merger
//! login is not in `students.github_login` or `github_users.student_id`.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

#[test]
fn fires_for_unknown_commit_author() {
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
        Some(20),
        Some(0),
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
    conn.execute(
        "INSERT INTO pr_commits (pr_id, sha, author_login, message, timestamp)
         VALUES ('pr-1', 's1', 'random-bot', 'msg', '2026-02-10T09:00Z')",
        params![],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    let n = common::count_flags(&conn, common::SPRINT_ID, "UNKNOWN_CONTRIBUTOR");
    assert!(n >= 1, "expected at least one UNKNOWN_CONTRIBUTOR; got {n}");
}

#[test]
fn silent_when_all_contributors_known() {
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
        Some(20),
        Some(0),
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
    conn.execute(
        "INSERT INTO pr_commits (pr_id, sha, author_login, message, timestamp)
         VALUES ('pr-1', 's1', 'alice', 'msg', '2026-02-10T09:00Z')",
        params![],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "UNKNOWN_CONTRIBUTOR"),
        0
    );
}
