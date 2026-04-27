//! DB read/write for the estimation-bias fitter (T-P2.1).
//!
//! Read-side: pulls `(student_id, task_id, estimation_points)` triples
//! for tasks belonging to a project (across all sprints). Excludes
//! USER_STORY rows (parents, not gradeable units — see CLAUDE.md
//! gotcha #9), unassigned rows, and rows with NULL/0 points.
//!
//! Write-side: replaces all rows in `student_estimation_bias` for the
//! project (DELETE + INSERT pattern, matching the surrounding codebase
//! since the table has a composite PK and we re-fit on every run).

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};

use crate::em::{fit, Observation};

/// Pull the (student, task, points) triples for `project_id`. Filters
/// follow the codebase conventions: drop USER_STORY rows, drop
/// unassigned tasks, drop NULL/zero points.
pub fn load_observations(conn: &Connection, project_id: i64) -> Result<Vec<Observation>> {
    let mut stmt = conn.prepare(
        "SELECT t.assignee_id, t.id, t.estimation_points
         FROM tasks t
         JOIN sprints s ON s.id = t.sprint_id
         WHERE s.project_id = ?
           AND t.type != 'USER_STORY'
           AND t.assignee_id IS NOT NULL
           AND t.estimation_points IS NOT NULL
           AND t.estimation_points > 0",
    )?;
    let rows = stmt
        .query_map(params![project_id], |r| {
            Ok(Observation {
                student_id: r.get::<_, String>(0)?,
                task_id: r.get::<_, i64>(1)?,
                points: r.get::<_, i64>(2)? as f64,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Fit the model for one project and replace its rows in
/// `student_estimation_bias`. Returns the number of student rows
/// written. Skips silently when there are no observations.
pub fn fit_and_persist_for_project(conn: &Connection, project_id: i64) -> Result<usize> {
    let obs = load_observations(conn, project_id)
        .with_context(|| format!("loading observations for project {project_id}"))?;
    if obs.is_empty() {
        return Ok(0);
    }
    let res = fit(&obs);
    if !res.converged {
        tracing::warn!(
            project_id,
            iters = res.iterations,
            "estimation fit did not converge — persisting last iterate anyway"
        );
    }
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "DELETE FROM student_estimation_bias WHERE project_id = ?",
        params![project_id],
    )?;
    let mut ins = conn.prepare(
        "INSERT INTO student_estimation_bias
            (student_id, project_id, beta_mean, beta_lower95, beta_upper95,
             n_tasks, fitted_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?;
    let mut written = 0usize;
    for s in &res.students {
        ins.execute(params![
            s.student_id,
            project_id,
            s.beta_mean,
            s.beta_lower95,
            s.beta_upper95,
            s.n_tasks as i64,
            now,
        ])?;
        written += 1;
    }
    Ok(written)
}

/// Fit and persist for every project that has at least one task.
/// Returns the total number of student rows written.
pub fn fit_and_persist_for_all_projects(conn: &Connection) -> Result<usize> {
    fit_and_persist_for_projects(conn, None)
}

/// Project-scoped variant. When `project_ids` is `Some(&[…])`, only those
/// projects are fitted; pass `None` for the historical full-DB sweep.
pub fn fit_and_persist_for_projects(
    conn: &Connection,
    project_ids: Option<&[i64]>,
) -> Result<usize> {
    let pids: Vec<i64> = if let Some(ids) = project_ids {
        if ids.is_empty() {
            return Ok(0);
        }
        ids.to_vec()
    } else {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT s.project_id
             FROM tasks t JOIN sprints s ON s.id = t.sprint_id
             WHERE t.type != 'USER_STORY'
               AND t.assignee_id IS NOT NULL
               AND t.estimation_points IS NOT NULL
               AND t.estimation_points > 0",
        )?;
        let collected = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        collected
    };
    let mut total = 0usize;
    for pid in pids {
        total += fit_and_persist_for_project(conn, pid)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_grader_core::db::apply_schema;

    fn open_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        conn
    }

    fn seed_project(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO projects (id, slug, name) VALUES (?, ?, ?)",
            params![id, format!("p{id}"), format!("P{id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sprints (id, project_id, name, start_date, end_date)
             VALUES (?, ?, 'S1', '2026-01-01', '2026-01-15')",
            params![100 + id, id],
        )
        .unwrap();
    }

    fn seed_student(conn: &Connection, id: &str, project_id: i64) {
        conn.execute(
            "INSERT INTO students (id, username, github_login, full_name, team_project_id)
             VALUES (?, ?, ?, ?, ?)",
            params![id, id, id, id, project_id],
        )
        .unwrap();
    }

    fn seed_task(
        conn: &Connection,
        id: i64,
        sprint_id: i64,
        assignee: Option<&str>,
        points: Option<i64>,
        ttype: &str,
    ) {
        conn.execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                                assignee_id, sprint_id, parent_task_id)
             VALUES (?, ?, ?, ?, 'DONE', ?, ?, ?, NULL)",
            params![
                id,
                format!("T-{id}"),
                format!("Task {id}"),
                ttype,
                points,
                assignee,
                sprint_id
            ],
        )
        .unwrap();
    }

    #[test]
    fn load_filters_user_story_unassigned_and_zero_points() {
        let conn = open_db();
        seed_project(&conn, 1);
        seed_student(&conn, "alice", 1);
        seed_task(&conn, 1, 101, Some("alice"), Some(3), "TASK");
        seed_task(&conn, 2, 101, Some("alice"), Some(5), "USER_STORY"); // dropped
        seed_task(&conn, 3, 101, Some("alice"), Some(0), "TASK"); // dropped
        seed_task(&conn, 4, 101, Some("alice"), None, "TASK"); // dropped
        seed_task(&conn, 5, 101, None, Some(2), "TASK"); // dropped

        let obs = load_observations(&conn, 1).unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].student_id, "alice");
        assert_eq!(obs[0].points, 3.0);
    }

    #[test]
    fn fit_and_persist_replaces_rows_idempotently() {
        let conn = open_db();
        seed_project(&conn, 1);
        seed_student(&conn, "alice", 1);
        seed_student(&conn, "bob", 1);
        // Alice over-estimates (high points), Bob under-estimates.
        for i in 0..6 {
            seed_task(&conn, 100 + i, 101, Some("alice"), Some(8), "TASK");
            seed_task(&conn, 200 + i, 101, Some("bob"), Some(2), "TASK");
        }

        let n = fit_and_persist_for_project(&conn, 1).unwrap();
        assert_eq!(n, 2);

        // Re-running replaces, not duplicates.
        let n2 = fit_and_persist_for_project(&conn, 1).unwrap();
        assert_eq!(n2, 2);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM student_estimation_bias WHERE project_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Alice's β should be > Bob's (she estimates higher).
        let alice: f64 = conn
            .query_row(
                "SELECT beta_mean FROM student_estimation_bias WHERE student_id='alice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let bob: f64 = conn
            .query_row(
                "SELECT beta_mean FROM student_estimation_bias WHERE student_id='bob'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(alice > bob, "alice={alice}, bob={bob}");
    }

    #[test]
    fn empty_project_skipped_silently() {
        let conn = open_db();
        seed_project(&conn, 1);
        let n = fit_and_persist_for_project(&conn, 1).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn fit_all_iterates_over_every_project() {
        let conn = open_db();
        seed_project(&conn, 1);
        seed_project(&conn, 2);
        seed_student(&conn, "u1", 1);
        seed_student(&conn, "u2", 2);
        for i in 0..4 {
            seed_task(&conn, 10 + i, 101, Some("u1"), Some(3), "TASK");
            seed_task(&conn, 20 + i, 102, Some("u2"), Some(5), "TASK");
        }
        let n = fit_and_persist_for_all_projects(&conn).unwrap();
        assert_eq!(n, 2);
    }
}
