use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, Row};

use crate::error::Result;

pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Apply the canonical schema to an open connection. Used by the integration
/// test harnesses in dependent crates so they can build an in-memory DB without
/// going through `Database::open` (which insists on a filesystem path).
pub fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)
}

pub struct Database {
    pub db_path: PathBuf,
    pub conn: Connection,
}

/// Apply the shared SQLite pragmas (WAL, foreign keys, busy timeout) to any
/// freshly opened connection. Call from every place that opens `grading.db`
/// so the settings stay in sync.
pub fn configure_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 10_000)?;
    Ok(())
}

impl Database {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        configure_pragmas(&conn)?;
        Ok(Self {
            db_path: db_path.to_path_buf(),
            conn,
        })
    }

    pub fn create_tables(&self) -> Result<()> {
        self.apply_additive_migrations()?;
        self.conn.execute_batch(SCHEMA_SQL)?;
        Ok(())
    }

    fn apply_additive_migrations(&self) -> Result<()> {
        // (table, column, column_type) — mirror of Python's _apply_additive_migrations.
        let migrations: &[(&str, &str, &str)] = &[
            ("pr_line_metrics", "merge_sha", "TEXT"),
            ("pr_line_metrics", "ld", "INTEGER"),
            ("task_similarity_groups", "stack", "TEXT"),
            ("task_similarity_groups", "layer", "TEXT"),
            ("task_similarity_groups", "action", "TEXT"),
            ("task_similarity_groups", "median_ls", "REAL"),
            ("task_similarity_groups", "median_ls_per_point", "REAL"),
            ("task_group_members", "ls_deviation", "REAL"),
            ("task_group_members", "ls_per_point_deviation", "REAL"),
            ("pr_submission_tiers", "pr_kind", "TEXT"),
            ("pull_requests", "last_github_fetch_updated_at", "TEXT"),
        ];
        for (table, column, coltype) in migrations {
            let existing: Vec<String> = self.column_names(table)?;
            if existing.is_empty() {
                continue; // table not created yet; schema will create it with column present
            }
            if !existing.iter().any(|c| c == column) {
                let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {coltype}");
                self.conn.execute(&sql, [])?;
            }
        }
        // Retired helper table — drop if present.
        self.conn
            .execute("DROP TABLE IF EXISTS task_similarity_scores", [])?;
        Ok(())
    }

    fn column_names(&self, table: &str) -> Result<Vec<String>> {
        let sql = format!("PRAGMA table_info({table})");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn count_table(&self, table: &str) -> Result<i64> {
        let sql = format!("SELECT count(*) FROM {table}");
        Ok(self.conn.query_row(&sql, [], |row| row.get(0))?)
    }

    pub fn commit(&self) -> Result<()> {
        // rusqlite auto-commits per statement in the default (non-transaction) mode.
        // Kept for parity with the Python API where callers explicitly commit().
        Ok(())
    }

    // ---- Upsert helpers (idempotent collection) ----

    pub fn upsert_project(&self, id: i64, slug: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO projects (id, slug, name) VALUES (?, ?, ?)",
            params![id, slug, name],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_student(
        &self,
        id: &str,
        username: &str,
        github_login: Option<&str>,
        full_name: &str,
        email: Option<&str>,
        team_project_id: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO students (id, username, github_login, full_name, email, team_project_id)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 username = excluded.username,
                 github_login = COALESCE(excluded.github_login, students.github_login),
                 full_name = excluded.full_name,
                 email = COALESCE(excluded.email, students.email),
                 team_project_id = COALESCE(excluded.team_project_id, students.team_project_id)",
            params![
                id,
                username,
                github_login,
                full_name,
                email,
                team_project_id
            ],
        )?;
        Ok(())
    }

    pub fn upsert_sprint(
        &self,
        id: i64,
        project_id: i64,
        name: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sprints (id, project_id, name, start_date, end_date)
             VALUES (?, ?, ?, ?, ?)",
            params![id, project_id, name, start_date, end_date],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_task(
        &self,
        id: i64,
        task_key: &str,
        name: &str,
        r#type: &str,
        status: &str,
        estimation_points: Option<i64>,
        assignee_id: Option<&str>,
        sprint_id: i64,
        parent_task_id: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tasks
             (id, task_key, name, type, status, estimation_points,
              assignee_id, sprint_id, parent_task_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                id,
                task_key,
                name,
                r#type,
                status,
                estimation_points,
                assignee_id,
                sprint_id,
                parent_task_id
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_pull_request(
        &self,
        id: &str,
        pr_number: i64,
        repo_full_name: &str,
        url: &str,
        title: &str,
        body: Option<&str>,
        state: &str,
        merged: bool,
        author_id: Option<&str>,
        additions: Option<i64>,
        deletions: Option<i64>,
        changed_files: Option<i64>,
        created_at: Option<&str>,
        updated_at: Option<&str>,
        merged_at: Option<&str>,
        github_author_login: Option<&str>,
        github_author_email: Option<&str>,
        merged_by_login: Option<&str>,
        merged_by_email: Option<&str>,
        attribution_errors: Option<&str>,
    ) -> Result<()> {
        // T-P1.5: attribution_errors accumulates across collect runs, so we
        // can't use INSERT OR REPLACE (which wipes the entire row, including
        // that column). Use INSERT ... ON CONFLICT DO UPDATE that lists every
        // column EXCEPT attribution_errors, so existing data-quality entries
        // survive a refresh of GitHub-side fields. Initial inserts still
        // accept the caller's attribution_errors value (always None today,
        // but kept on the surface for back-compat).
        self.conn.execute(
            "INSERT INTO pull_requests
             (id, pr_number, repo_full_name, url, title, body, state, merged,
              author_id, github_author_login, github_author_email,
              merged_by_login, merged_by_email,
              additions, deletions, changed_files,
              created_at, updated_at, merged_at, attribution_errors)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                pr_number = excluded.pr_number,
                repo_full_name = excluded.repo_full_name,
                url = excluded.url,
                title = excluded.title,
                body = excluded.body,
                state = excluded.state,
                merged = excluded.merged,
                author_id = excluded.author_id,
                github_author_login = excluded.github_author_login,
                github_author_email = excluded.github_author_email,
                merged_by_login = excluded.merged_by_login,
                merged_by_email = excluded.merged_by_email,
                additions = excluded.additions,
                deletions = excluded.deletions,
                changed_files = excluded.changed_files,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                merged_at = excluded.merged_at",
            params![
                id,
                pr_number,
                repo_full_name,
                url,
                title,
                body,
                state,
                merged,
                author_id,
                github_author_login,
                github_author_email,
                merged_by_login,
                merged_by_email,
                additions,
                deletions,
                changed_files,
                created_at,
                updated_at,
                merged_at,
                attribution_errors,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_github_user(
        &self,
        login: &str,
        name: Option<&str>,
        email: Option<&str>,
        student_id: Option<&str>,
        fetched_at: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO github_users (login, name, email, student_id, fetched_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(login) DO UPDATE SET
                 name = COALESCE(excluded.name, github_users.name),
                 email = COALESCE(excluded.email, github_users.email),
                 student_id = COALESCE(excluded.student_id, github_users.student_id),
                 fetched_at = COALESCE(excluded.fetched_at, github_users.fetched_at)",
            params![login, name, email, student_id, fetched_at],
        )?;
        Ok(())
    }

    pub fn link_task_pr(&self, task_id: i64, pr_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO task_pull_requests (task_id, pr_id) VALUES (?, ?)",
            params![task_id, pr_id],
        )?;
        Ok(())
    }

    pub fn replace_task_pr_links_for_sprint(
        &self,
        sprint_id: i64,
        links: &[(i64, String)],
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM task_pull_requests
             WHERE task_id IN (SELECT id FROM tasks WHERE sprint_id = ?)",
            [sprint_id],
        )?;
        for (task_id, pr_id) in links {
            self.link_task_pr(*task_id, pr_id)?;
        }
        Ok(())
    }

    pub fn remove_missing_tasks_for_sprint(&self, sprint_id: i64, task_ids: &[i64]) -> Result<()> {
        if task_ids.is_empty() {
            self.conn
                .execute("DELETE FROM tasks WHERE sprint_id = ?", [sprint_id])?;
            return Ok(());
        }

        let placeholders = std::iter::repeat("?")
            .take(task_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("DELETE FROM tasks WHERE sprint_id = ? AND id NOT IN ({placeholders})");
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(task_ids.len() + 1);
        params.push(rusqlite::types::Value::Integer(sprint_id));
        params.extend(
            task_ids
                .iter()
                .copied()
                .map(rusqlite::types::Value::Integer),
        );
        self.conn
            .execute(&sql, rusqlite::params_from_iter(params.iter()))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_pr_commit(
        &self,
        pr_id: &str,
        sha: &str,
        author_login: Option<&str>,
        author_email: Option<&str>,
        message: &str,
        timestamp: &str,
        additions: Option<i64>,
        deletions: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO pr_commits
             (pr_id, sha, author_login, author_email, message, timestamp, additions, deletions)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                pr_id,
                sha,
                author_login,
                author_email,
                message,
                timestamp,
                additions,
                deletions
            ],
        )?;
        Ok(())
    }

    /// Capture an original commit author for a merged PR (T-P1.4). Used by
    /// `author_mismatch` as the authoritative source — `pr_commits` is the
    /// fallback if no rows exist for a PR. Idempotent on (pr_id, sha).
    pub fn upsert_pr_pre_squash_author(
        &self,
        pr_id: &str,
        sha: &str,
        author_login: Option<&str>,
        author_email: Option<&str>,
        captured_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO pr_pre_squash_authors
             (pr_id, sha, author_login, author_email, captured_at)
             VALUES (?, ?, ?, ?, ?)",
            params![pr_id, sha, author_login, author_email, captured_at],
        )?;
        Ok(())
    }

    pub fn upsert_pr_review(
        &self,
        pr_id: &str,
        reviewer_login: &str,
        state: &str,
        submitted_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO pr_reviews (pr_id, reviewer_login, state, submitted_at)
             VALUES (?, ?, ?, ?)",
            params![pr_id, reviewer_login, state, submitted_at],
        )?;
        Ok(())
    }

    // ---- Query helpers ----

    /// Sprint ids belonging to `project_id` whose `start_date <= today`,
    /// returned in `start_date ASC` order. The sprint containing `today`
    /// (if any) is the last element. `today` is an ISO `YYYY-MM-DD` string.
    pub fn sprint_ids_up_to_current(&self, project_id: i64, today: &str) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM sprints
             WHERE project_id = ? AND start_date IS NOT NULL
                   AND start_date != '' AND start_date <= ?
             ORDER BY start_date ASC",
        )?;
        let rows = stmt.query_map(params![project_id, today], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Project-scoped variant of `get_pull_requests_for_sprint` — returns
    /// every PR linked to a non-USER_STORY task in any sprint of `project_id`.
    pub fn get_pull_requests_for_project(&self, project_id: i64) -> Result<Vec<PullRequestRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.state,
                             pr.merged, pr.merged_at, pr.author_id, pr.github_author_login,
                             pr.updated_at, pr.last_github_fetch_updated_at
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             JOIN sprints s ON s.id = t.sprint_id
             WHERE s.project_id = ? AND t.type != 'USER_STORY'",
        )?;
        let rows = stmt.query_map([project_id], PullRequestRow::from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// All PRs linked to non-USER_STORY tasks in a given sprint.
    pub fn get_pull_requests_for_sprint(&self, sprint_id: i64) -> Result<Vec<PullRequestRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.state,
                             pr.merged, pr.merged_at, pr.author_id, pr.github_author_login,
                             pr.updated_at, pr.last_github_fetch_updated_at
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
        )?;
        let rows = stmt.query_map([sprint_id], PullRequestRow::from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fetch the stored ETag for `(pr_id, endpoint)` if we have one.
    /// `endpoint` is one of: `"pr"`, `"commits"`, `"reviews"`.
    pub fn get_pr_github_etag(&self, pr_id: &str, endpoint: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row(
                "SELECT etag FROM pr_github_etags WHERE pr_id = ? AND endpoint = ?",
                params![pr_id, endpoint],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(v)
    }

    pub fn upsert_pr_github_etag(
        &self,
        pr_id: &str,
        endpoint: &str,
        etag: &str,
        fetched_at: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pr_github_etags (pr_id, endpoint, etag, fetched_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(pr_id, endpoint) DO UPDATE SET
                 etag = excluded.etag,
                 fetched_at = excluded.fetched_at",
            params![pr_id, endpoint, etag, fetched_at],
        )?;
        Ok(())
    }

    pub fn count_pr_commits(&self, pr_id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM pr_commits WHERE pr_id = ?",
            [pr_id],
            |row| row.get(0),
        )?)
    }
}

/// Minimal PR row used by the collector for idempotency checks.
#[derive(Debug, Clone)]
pub struct PullRequestRow {
    pub id: String,
    pub pr_number: i64,
    pub repo_full_name: Option<String>,
    pub state: Option<String>,
    pub merged: bool,
    pub merged_at: Option<String>,
    pub author_id: Option<String>,
    pub github_author_login: Option<String>,
    /// TrackDev-reported `updated_at` (ISO). Used as the high-water mark for
    /// deciding whether the GitHub-side data is still fresh.
    pub updated_at: Option<String>,
    /// Value of `updated_at` the last time we successfully pulled GitHub
    /// details for this PR. `NULL` means never fetched.
    pub last_github_fetch_updated_at: Option<String>,
}

impl PullRequestRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(PullRequestRow {
            id: row.get(0)?,
            pr_number: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            repo_full_name: row.get(2)?,
            state: row.get(3)?,
            merged: row.get::<_, Option<bool>>(4)?.unwrap_or(false),
            merged_at: row.get(5)?,
            author_id: row.get(6)?,
            github_author_login: row.get(7)?,
            updated_at: row.get(8)?,
            last_github_fetch_updated_at: row.get(9)?,
        })
    }
}

/// Return all user-defined table names in the DB. Useful for verification.
pub fn list_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::Database;

    #[test]
    fn sprint_task_reconciliation_replaces_links_and_removes_missing_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("grading.db")).unwrap();
        db.create_tables().unwrap();

        db.upsert_task(1, "t1", "Task 1", "TASK", "DONE", Some(5), None, 10, None)
            .unwrap();
        db.upsert_task(2, "t2", "Task 2", "TASK", "DONE", Some(3), None, 10, None)
            .unwrap();
        db.upsert_pull_request(
            "pr-1",
            1,
            "udg-pds/android-demo",
            "https://example.test/pr-1",
            "PR 1",
            None,
            "open",
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        db.upsert_pull_request(
            "pr-2",
            2,
            "udg-pds/android-demo",
            "https://example.test/pr-2",
            "PR 2",
            None,
            "open",
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        db.link_task_pr(1, "pr-1").unwrap();
        db.link_task_pr(2, "pr-2").unwrap();

        db.replace_task_pr_links_for_sprint(10, &[(1, "pr-2".to_string())])
            .unwrap();
        db.remove_missing_tasks_for_sprint(10, &[1]).unwrap();

        let links: Vec<(i64, String)> = {
            let mut stmt = db
                .conn
                .prepare("SELECT task_id, pr_id FROM task_pull_requests ORDER BY task_id, pr_id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert_eq!(links, vec![(1, "pr-2".to_string())]);

        let tasks: Vec<i64> = {
            let mut stmt = db
                .conn
                .prepare("SELECT id FROM tasks WHERE sprint_id = 10 ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert_eq!(tasks, vec![1]);
    }
}
