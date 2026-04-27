//! AUTHOR_MISMATCH — WARNING when commit authors differ from the PR author.
//! T-P1.4 made `pr_pre_squash_authors` authoritative when present, so this
//! test exercises both code paths.

mod common;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

fn seed_pr_and_task(conn: &rusqlite::Connection) {
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
        Some(50),
        Some(5),
        None,
    );
    common::seed_task(
        conn,
        10,
        common::SPRINT_ID,
        Some("alice"),
        Some(3),
        "DONE",
        "TASK",
    );
    common::link_task_pr(conn, 10, "pr-1");
}

#[test]
fn fires_when_pr_commits_have_a_foreign_author() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    seed_pr_and_task(&conn);
    conn.execute(
        "INSERT INTO pr_commits (pr_id, sha, author_login, message, timestamp)
         VALUES ('pr-1', 's1', 'bob', 'msg', '2026-02-10T09:00Z')",
        params![],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "AUTHOR_MISMATCH", "alice"),
        1
    );
}

#[test]
fn pre_squash_table_is_preferred_over_pr_commits() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    seed_pr_and_task(&conn);
    // pr_commits agrees with PR author (post-squash). Pre-squash table
    // remembers the original "bob" commit — detector should use that.
    conn.execute(
        "INSERT INTO pr_commits (pr_id, sha, author_login, message, timestamp)
         VALUES ('pr-1', 'squash', 'alice', 'sq', '2026-02-10T09:00Z')",
        params![],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pr_pre_squash_authors (pr_id, sha, author_login, captured_at)
         VALUES ('pr-1', 's1', 'bob', '2026-02-10T09:30Z')",
        params![],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "AUTHOR_MISMATCH"),
        1
    );
}

#[test]
fn silent_when_authors_align() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    seed_pr_and_task(&conn);
    conn.execute(
        "INSERT INTO pr_commits (pr_id, sha, author_login, message, timestamp)
         VALUES ('pr-1', 's1', 'alice', 'msg', '2026-02-10T09:00Z')",
        params![],
    )
    .unwrap();

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(
        common::count_flags(&conn, common::SPRINT_ID, "AUTHOR_MISMATCH"),
        0
    );
}
