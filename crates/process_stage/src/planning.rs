//! Sprint planning quality. Mirrors `src/process/planning.py`.

use rusqlite::{params, Connection};
use tracing::info;

use sprint_grader_core::stats::coefficient_of_variation;

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

    // Velocity CV across all sprints for this project.
    let mut stmt = conn.prepare(
        "SELECT COALESCE(SUM(t.estimation_points), 0) AS vel
         FROM tasks t
         JOIN sprints sp ON sp.id = t.sprint_id
         WHERE sp.project_id = ? AND t.status = 'DONE' AND t.type != 'USER_STORY'
         GROUP BY t.sprint_id",
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
