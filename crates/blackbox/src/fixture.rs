//! Fixture grading.db + filesystem builder (T-T0.2).
//!
//! The default `Fixture` is a happy-path team-01 with two sprints,
//! five students, and a handful of tasks + PRs. Scenarios layer
//! deltas on top via `with_*` builder methods — each method documents
//! what it inserts. This keeps the per-scenario test files small and
//! the seed data discoverable in one place.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{params, Connection};

use sprint_grader_core::db::apply_schema;

/// Canonical IDs the default fixture uses. Scenarios should hard-code
/// these constants rather than rebuilding the strings — they are part
/// of the fixture's stable contract.
pub mod ids {
    pub const PROJECT_ID: i64 = 1;
    pub const PROJECT_SLUG: &str = "team-01";
    pub const PROJECT_NAME: &str = "Team 01";
    pub const ANDROID_REPO: &str = "udg-pds/android-team-01";
    pub const SPRING_REPO: &str = "udg-pds/spring-team-01";

    pub const PRIOR_SPRINT_ID: i64 = 100;
    pub const PRIOR_SPRINT_START: &str = "2026-01-15T08:00Z";
    pub const PRIOR_SPRINT_END: &str = "2026-01-31T20:00Z";

    pub const SPRINT_ID: i64 = 101;
    pub const SPRINT_START: &str = "2026-02-01T08:00Z";
    pub const SPRINT_END: &str = "2026-02-15T20:00Z";

    pub const STUDENTS: &[&str] = &["alice", "bob", "carol", "dan", "eve"];
}

/// On-disk paths the runner needs alongside the DB.
#[derive(Debug, Clone)]
pub struct FixturePaths {
    /// Absolute path to `grading.db`.
    pub db_path: PathBuf,
    /// Absolute path to `data/entregues/`.
    pub entregues_dir: PathBuf,
    /// Absolute path to `data/entregues/<team>/`.
    pub project_dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct Fixture {
    /// When true, append a third "future" sprint to test trajectory /
    /// CV scenarios that need multiple data points.
    extra_sprint: bool,
    /// When set, every default PR gets this `body` instead of the stock
    /// markdown — useful for the doc-evaluation scenarios.
    pr_body_override: Option<String>,
    /// When true, skip the default PR seeding entirely (the scenario
    /// will insert its own).
    skip_default_prs: bool,
}

impl Fixture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_extra_sprint(mut self) -> Self {
        self.extra_sprint = true;
        self
    }

    pub fn with_pr_body(mut self, body: impl Into<String>) -> Self {
        self.pr_body_override = Some(body.into());
        self
    }

    pub fn without_default_prs(mut self) -> Self {
        self.skip_default_prs = true;
        self
    }

    /// Create the temp-dir layout (`data/entregues/<project>/` plus
    /// the empty repo subdirs) and apply the schema. Returns the
    /// caller a `(Connection, FixturePaths)` pair so they can keep
    /// inserting deltas before invoking the runner.
    pub fn build(self, root: &Path) -> Result<(Connection, FixturePaths)> {
        let entregues = root.join("data").join("entregues");
        let project_dir = entregues.join(ids::PROJECT_SLUG);
        std::fs::create_dir_all(&project_dir)?;
        // Empty repo dirs so architecture/ownership scans have a target.
        std::fs::create_dir_all(project_dir.join("android-team-01"))?;
        std::fs::create_dir_all(project_dir.join("spring-team-01"))?;

        let db_path = entregues.join("grading.db");
        let conn = Connection::open(&db_path)?;
        apply_schema(&conn)?;
        self.seed(&conn)?;

        Ok((
            conn,
            FixturePaths {
                db_path,
                entregues_dir: entregues,
                project_dir,
            },
        ))
    }

    fn seed(&self, conn: &Connection) -> Result<()> {
        seed_project(conn)?;
        seed_sprint(
            conn,
            ids::PRIOR_SPRINT_ID,
            "Sprint 1",
            ids::PRIOR_SPRINT_START,
            ids::PRIOR_SPRINT_END,
        )?;
        seed_sprint(
            conn,
            ids::SPRINT_ID,
            "Sprint 2",
            ids::SPRINT_START,
            ids::SPRINT_END,
        )?;
        if self.extra_sprint {
            seed_sprint(
                conn,
                ids::SPRINT_ID + 1,
                "Sprint 3",
                "2026-02-16T08:00Z",
                "2026-03-01T20:00Z",
            )?;
        }
        for s in ids::STUDENTS {
            seed_student(conn, s)?;
        }
        // A few realistic per-sprint tasks: USER_STORY parents +
        // assignable subtasks. The default seeding aims to be the
        // smallest set that lets `analyze` / `inequality` /
        // `contribution` produce non-empty derived rows.
        for (sprint_id, base_task) in [(ids::PRIOR_SPRINT_ID, 1_000), (ids::SPRINT_ID, 2_000)] {
            for (i, student) in ids::STUDENTS.iter().enumerate() {
                let task_id = base_task + i as i64;
                seed_task(
                    conn,
                    task_id,
                    sprint_id,
                    Some(student),
                    Some(3),
                    "DONE",
                    "TASK",
                )?;
            }
        }
        if !self.skip_default_prs {
            let body = self
                .pr_body_override
                .clone()
                .unwrap_or_else(default_pr_body);
            for (i, student) in ids::STUDENTS.iter().enumerate() {
                let pr_id = format!("pr-default-{i}");
                seed_pr(
                    conn,
                    &pr_id,
                    (i + 1) as i64,
                    ids::ANDROID_REPO,
                    Some(student),
                    Some(student),
                    "MERGED",
                    true,
                    Some(&format!("2026-02-1{}T15:00Z", i % 5 + 1)),
                    Some(40),
                    Some(10),
                    Some(&body),
                )?;
                link_task_pr(conn, 2_000 + i as i64, &pr_id)?;
            }
        }
        Ok(())
    }
}

fn default_pr_body() -> String {
    "## Summary\n\n- Implements feature X\n- Adds tests for the new branch\n\n\
     ## How to test\n\n1. Run the unit test suite\n2. Smoke-test the new screen"
        .to_string()
}

// ---- low-level seeders (re-exported so scenarios can extend) ----

pub fn seed_project(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, ?, ?)",
        params![ids::PROJECT_ID, ids::PROJECT_SLUG, ids::PROJECT_NAME],
    )?;
    Ok(())
}

pub fn seed_sprint(
    conn: &Connection,
    sprint_id: i64,
    name: &str,
    start: &str,
    end: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (?, ?, ?, ?, ?)",
        params![sprint_id, ids::PROJECT_ID, name, start, end],
    )?;
    Ok(())
}

pub fn seed_student(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id)
         VALUES (?, ?, ?, ?, ?)",
        params![id, id, id, id, ids::PROJECT_ID],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn seed_task(
    conn: &Connection,
    id: i64,
    sprint_id: i64,
    assignee: Option<&str>,
    points: Option<i64>,
    status: &str,
    ttype: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                            assignee_id, sprint_id, parent_task_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        params![
            id,
            format!("T-{id}"),
            format!("Task {id}"),
            ttype,
            status,
            points,
            assignee,
            sprint_id,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn seed_pr(
    conn: &Connection,
    id: &str,
    pr_number: i64,
    repo: &str,
    author_id: Option<&str>,
    github_author_login: Option<&str>,
    state: &str,
    merged: bool,
    merged_at: Option<&str>,
    additions: Option<i64>,
    deletions: Option<i64>,
    body: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pull_requests
         (id, pr_number, repo_full_name, url, title, body, state, merged,
          author_id, github_author_login, additions, deletions, changed_files,
          created_at, merged_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            pr_number,
            repo,
            format!("https://github.com/{repo}/pull/{pr_number}"),
            format!("PR #{pr_number}"),
            body,
            state,
            merged,
            author_id,
            github_author_login,
            additions,
            deletions,
            5_i64,
            "2026-02-01T10:00Z",
            merged_at,
        ],
    )?;
    Ok(())
}

pub fn link_task_pr(conn: &Connection, task_id: i64, pr_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (?, ?)",
        params![task_id, pr_id],
    )?;
    Ok(())
}
