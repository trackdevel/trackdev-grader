//! Sprint planning quality. Mirrors `src/process/planning.py`.

use rusqlite::{params, Connection};
use tracing::info;

use sprint_grader_core::stats::coefficient_of_variation;

/// Computes velocity CV over sprints with non-zero delivered points.
/// Empty sprints (delivered = 0) are excluded — including them lets a
/// few legitimate-fail sprints dominate CV and gives consistent teams
/// a misleadingly noisy signal. The trade-off: a team that genuinely
/// failed a sprint with zero delivery is silently omitted from the CV
/// pool. Documenting the choice here so future readers don't "fix" it
/// back. Alternative (clamping each sprint to a small floor) papers
/// over the failure and was rejected.
pub fn compute_planning_quality(
    conn: &Connection,
    project_id: i64,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let planned: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
             WHERE sprint_id = ? AND type != 'USER_STORY'",
            [sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let completed: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
             WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'",
            [sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let reliability = if planned > 0 {
        completed as f64 / planned as f64
    } else {
        0.0
    };
    let sae = if planned > 0 {
        (planned - completed).abs() as f64 / planned as f64
    } else {
        0.0
    };

    let total_tasks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE sprint_id = ? AND type != 'USER_STORY'",
            [sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let unestimated: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE sprint_id = ? AND type != 'USER_STORY'
               AND (estimation_points IS NULL OR estimation_points = 0)",
            [sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let unestimated_pct = if total_tasks > 0 {
        unestimated as f64 / total_tasks as f64
    } else {
        0.0
    };

    // Velocity CV across sprints with non-zero delivered points (see fn doc).
    let mut stmt = conn.prepare(
        "SELECT COALESCE(SUM(t.estimation_points), 0) AS vel
         FROM tasks t
         JOIN sprints sp ON sp.id = t.sprint_id
         WHERE sp.project_id = ? AND t.status = 'DONE' AND t.type != 'USER_STORY'
         GROUP BY t.sprint_id
         HAVING vel > 0",
    )?;
    let velocities: Vec<f64> = stmt
        .query_map([project_id], |r| r.get::<_, Option<i64>>(0))?
        .filter_map(Result::ok)
        .map(|v| v.unwrap_or(0) as f64)
        .collect();
    drop(stmt);
    let velocity_cv = if velocities.len() >= 2 {
        coefficient_of_variation(&velocities)
    } else {
        0.0
    };

    conn.execute(
        "INSERT OR REPLACE INTO sprint_planning_quality
         (project_id, sprint_id, planned_points, completed_points,
          commitment_reliability, velocity, velocity_cv,
          sprint_accuracy_error, unestimated_task_pct)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            project_id,
            sprint_id,
            planned,
            completed,
            reliability,
            completed,
            velocity_cv,
            sae,
            unestimated_pct,
        ],
    )?;
    Ok(())
}

pub fn compute_all_planning(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("SELECT DISTINCT project_id FROM sprints WHERE id = ?")?;
    let project_ids: Vec<i64> = stmt
        .query_map([sprint_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for pid in &project_ids {
        compute_planning_quality(conn, *pid, sprint_id)?;
    }
    info!(
        count = project_ids.len(),
        sprint_id, "planning quality computed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sprints (id INTEGER PRIMARY KEY, project_id INTEGER);
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, sprint_id INTEGER,
                type TEXT, status TEXT, estimation_points INTEGER);
             CREATE TABLE sprint_planning_quality (
                project_id INTEGER, sprint_id INTEGER,
                planned_points INTEGER, completed_points INTEGER,
                commitment_reliability REAL, velocity INTEGER,
                velocity_cv REAL, sprint_accuracy_error REAL,
                unestimated_task_pct REAL,
                PRIMARY KEY (project_id, sprint_id));",
        )
        .unwrap();
        conn
    }

    fn insert_sprint(conn: &Connection, sid: i64, pid: i64, delivered: &[i64]) {
        conn.execute(
            "INSERT INTO sprints(id, project_id) VALUES (?, ?)",
            params![sid, pid],
        )
        .unwrap();
        for (i, pts) in delivered.iter().enumerate() {
            // status = DONE if pts > 0, else TODO so it's excluded from velocity.
            let status = if *pts > 0 { "DONE" } else { "TODO" };
            conn.execute(
                "INSERT INTO tasks(id, sprint_id, type, status, estimation_points)
                 VALUES (?, ?, 'TASK', ?, ?)",
                params![sid * 1000 + i as i64, sid, status, pts],
            )
            .unwrap();
        }
    }

    #[test]
    fn velocity_cv_excludes_zero_sprints() {
        let conn = mk_conn();
        // Project 1: velocities by sprint = [80, 75, 0, 0]. Filter must pick [80, 75].
        insert_sprint(&conn, 1, 1, &[80]);
        insert_sprint(&conn, 2, 1, &[75]);
        insert_sprint(&conn, 3, 1, &[]); // no delivered tasks at all
        insert_sprint(&conn, 4, 1, &[0]); // task exists but DONE only if > 0

        compute_planning_quality(&conn, 1, 2).unwrap();
        let cv: f64 = conn
            .query_row(
                "SELECT velocity_cv FROM sprint_planning_quality
                 WHERE project_id = 1 AND sprint_id = 2",
                [],
                |r| r.get(0),
            )
            .unwrap();

        let expected = coefficient_of_variation(&[80.0, 75.0]);
        assert!(
            (cv - expected).abs() < 1e-9,
            "cv={cv}, expected {expected} (CV of [80, 75], not [80, 75, 0, 0])"
        );
        let with_zeros = coefficient_of_variation(&[80.0, 75.0, 0.0, 0.0]);
        assert!(
            (cv - with_zeros).abs() > 1e-3,
            "must differ from CV-with-zeros ({with_zeros})"
        );
    }
}
