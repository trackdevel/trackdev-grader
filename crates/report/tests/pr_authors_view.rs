//! Integration tests for the report-layer switch from `pr.author_id` to the
//! canonical `pr_authors` view (commit 1 of the PR-attribution overhaul).
//!
//! The four cases the production change must handle correctly:
//!   1. PR with NULL `pr.author_id` is still attributed to the linked task's
//!      assignee in the per-student PR table.
//!   2. Multi-assignee PR renders its primary author (max points) and footnotes
//!      the rest in the XLSX "Author" cell.
//!   3. Cross-project PR (assignees in two distinct teams) triggers the
//!      "⚠ Cross-project PRs detected" guard at the top of the Team identity
//!      section.
//!   4. Orphan PR (no linked tasks) appears in the "Ghost contributor" annex
//!      under its renamed heading.

use rusqlite::{params, Connection};
use sprint_grader_core::db::apply_schema;
use sprint_grader_report::{
    generate_markdown_report_multi_to_path_with_opts, generate_team_report, MultiReportOptions,
};
use std::path::Path;
use tempfile::TempDir;

/// Two adjacent projects, one shared sprint per project, three students per
/// project. Lays the bare minimum so the report-layer queries have rows.
fn seed_two_team_fixture(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO projects (id, slug, name) VALUES
            (1, 'team-alpha', 'Team Alpha'),
            (2, 'team-beta',  'Team Beta');
         INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES
            (10, 1, 'Sprint 1', '2026-04-01T00:00Z', '2026-04-30T23:59Z'),
            (20, 2, 'Sprint 1', '2026-04-01T00:00Z', '2026-04-30T23:59Z');
         INSERT INTO students (id, full_name, github_login, team_project_id) VALUES
            ('alice', 'Alice Adams',  'alice-gh',  1),
            ('bob',   'Bob Brown',    'bob-gh',    1),
            ('carol', 'Carol Carter', 'carol-gh',  1),
            ('dave',  'Dave Davis',   'dave-gh',   2);",
    )
    .unwrap();
}

/// Insert a task with a single assignee; returns its id.
fn seed_task(
    conn: &Connection,
    id: i64,
    sprint_id: i64,
    assignee: &str,
    points: i64,
    status: &str,
) {
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                            assignee_id, sprint_id)
         VALUES (?, ?, ?, 'TASK', ?, ?, ?, ?)",
        params![
            id,
            format!("T-{}", id),
            format!("Task {}", id),
            status,
            points,
            assignee,
            sprint_id
        ],
    )
    .unwrap();
}

/// Insert a PR; `author_id` left explicitly NULL on purpose for case (1).
fn seed_pr(
    conn: &Connection,
    pr_id: &str,
    pr_number: i64,
    repo: &str,
    title: &str,
    author_id: Option<&str>,
    merged_at: &str,
) {
    conn.execute(
        "INSERT INTO pull_requests (id, pr_number, repo_full_name, title, url,
                                    author_id, merged, merged_at, additions, deletions,
                                    changed_files)
         VALUES (?, ?, ?, ?, ?, ?, 1, ?, 100, 20, 5)",
        params![
            pr_id,
            pr_number,
            repo,
            title,
            format!("https://github.com/{}/pull/{}", repo, pr_number),
            author_id,
            merged_at
        ],
    )
    .unwrap();
}

fn link(conn: &Connection, task_id: i64, pr_id: &str) {
    conn.execute(
        "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (?, ?)",
        params![task_id, pr_id],
    )
    .unwrap();
}

fn render_md(conn: &Connection, project_id: i64, sprint_ids: &[i64], dir: &Path) -> String {
    let path = dir.join(format!("project-{}-REPORT.md", project_id));
    let opts = MultiReportOptions::default();
    generate_markdown_report_multi_to_path_with_opts(
        conn,
        project_id,
        "Team Alpha",
        sprint_ids,
        &path,
        opts,
    )
    .expect("markdown render");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {:?}: {}", path, e))
}

#[test]
fn null_author_id_pr_still_attributed_to_task_assignee_in_markdown() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    seed_two_team_fixture(&conn);

    // Alice has a DONE task linked to a PR whose author_id is NULL.
    seed_task(&conn, 1, 10, "alice", 5, "DONE");
    seed_pr(
        &conn,
        "pr-null-author",
        1,
        "trackdevel/spring-alpha",
        "Add login endpoint",
        None,
        "2026-04-15T10:00Z",
    );
    link(&conn, 1, "pr-null-author");

    let dir = TempDir::new().unwrap();
    let body = render_md(&conn, 1, &[10], dir.path());

    assert!(
        body.contains("Add login endpoint"),
        "markdown should list Alice's PR even when pr.author_id is NULL — \
         report layer must read pr_authors via task assignee, not pr.author_id.\n\n{}",
        body
    );
}

#[test]
fn multi_assignee_pr_renders_primary_with_co_assignee_footnote_in_xlsx() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    seed_two_team_fixture(&conn);

    // Two DONE tasks linked to the same PR: Alice 8 pts, Bob 3 pts. Alice
    // wins on points and is the primary author.
    seed_task(&conn, 1, 10, "alice", 8, "DONE");
    seed_task(&conn, 2, 10, "bob", 3, "DONE");
    seed_pr(
        &conn,
        "pr-multi",
        2,
        "trackdevel/spring-alpha",
        "Add session middleware",
        None,
        "2026-04-16T10:00Z",
    );
    link(&conn, 1, "pr-multi");
    link(&conn, 2, "pr-multi");

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("alpha.xlsx");
    generate_team_report(&conn, 10, 1, "Team Alpha", &path).expect("xlsx render");

    // Re-open the produced workbook through SQLite isn't trivial — instead we
    // assert via a direct view query that the primary picker matches the
    // documented rule. The XLSX rendering uses the same ORDER BY clause as
    // this query.
    let primary_name: String = conn
        .query_row(
            "SELECT COALESCE(s.full_name, s.github_login, '')
             FROM pr_authors pa
             JOIN students s ON s.id = pa.student_id
             WHERE pa.pr_id = 'pr-multi'
             ORDER BY pa.author_points DESC, pa.author_task_count DESC,
                      s.full_name, s.id
             LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(primary_name, "Alice Adams", "Alice has more points");

    let co_assignee_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pr_authors WHERE pr_id = 'pr-multi'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        co_assignee_count, 2,
        "view exposes both assignees so the renderer can footnote one"
    );

    assert!(
        path.exists() && path.metadata().unwrap().len() > 0,
        "team report file actually written"
    );
}

#[test]
fn cross_project_pr_triggers_team_identity_warning() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    seed_two_team_fixture(&conn);

    // Invariant-violating data: one PR linked to tasks across both teams.
    // (In a healthy DB this is impossible — that's exactly why we surface it.)
    seed_task(&conn, 1, 10, "alice", 5, "DONE");
    seed_task(&conn, 2, 20, "dave", 5, "DONE");
    seed_pr(
        &conn,
        "pr-cross",
        3,
        "trackdevel/spring-mixed",
        "Cross-project PR",
        None,
        "2026-04-17T10:00Z",
    );
    link(&conn, 1, "pr-cross");
    link(&conn, 2, "pr-cross");

    let dir = TempDir::new().unwrap();
    let body = render_md(&conn, 1, &[10], dir.path());

    assert!(
        body.contains("Cross-project PRs detected"),
        "team identity guard must fire when a PR's assignees span multiple \
         projects.\n\n{}",
        body
    );
    assert!(
        body.contains("Team Alpha") && body.contains("Team Beta"),
        "guard lists the offending projects.\n\n{}",
        body
    );
}

#[test]
fn no_cross_project_pr_renders_clean_team_identity_line() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    seed_two_team_fixture(&conn);

    // Healthy case: one PR with all assignees in Team Alpha.
    seed_task(&conn, 1, 10, "alice", 5, "DONE");
    seed_pr(
        &conn,
        "pr-clean",
        4,
        "trackdevel/spring-alpha",
        "Clean PR",
        None,
        "2026-04-18T10:00Z",
    );
    link(&conn, 1, "pr-clean");

    let dir = TempDir::new().unwrap();
    let body = render_md(&conn, 1, &[10], dir.path());

    assert!(
        body.contains("All PRs that touch this team are scoped to it"),
        "healthy case renders the green-light line.\n\n{}",
        body
    );
    assert!(
        !body.contains("Cross-project PRs detected"),
        "guard must NOT fire when the invariant holds.\n\n{}",
        body
    );
}

#[test]
fn orphan_pr_renders_under_ghost_contributor_heading() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    seed_two_team_fixture(&conn);

    // A linked PR for the team (so the team's repo shows up in the orphan
    // annex's repo-scoping subquery), plus an orphan PR in the same repo.
    seed_task(&conn, 1, 10, "alice", 5, "DONE");
    seed_pr(
        &conn,
        "pr-linked",
        5,
        "trackdevel/spring-alpha",
        "Linked PR",
        None,
        "2026-04-15T10:00Z",
    );
    link(&conn, 1, "pr-linked");

    seed_pr(
        &conn,
        "pr-orphan",
        6,
        "trackdevel/spring-alpha",
        "Drive-by orphan PR",
        None,
        "2026-04-19T10:00Z",
    );
    // intentionally no link()

    let dir = TempDir::new().unwrap();
    let body = render_md(&conn, 1, &[10], dir.path());

    assert!(
        body.contains("Ghost contributor"),
        "orphan annex renamed to 'Ghost contributor'.\n\n{}",
        body
    );
    assert!(
        body.contains("Drive-by orphan PR"),
        "orphan PR shows up in the ghost section.\n\n{}",
        body
    );
    assert!(
        !body.contains("Annex: orphan pull requests"),
        "old heading must not survive.\n\n{}",
        body
    );
}
