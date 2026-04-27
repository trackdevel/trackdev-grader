//! Longitudinal trajectory classification. Mirrors `src/analyze/trajectory.py`.

use rusqlite::{params, Connection};
use tracing::info;

use sprint_grader_core::config::DetectorThresholdsConfig;
use sprint_grader_core::stats::{coefficient_of_variation, linregress_index};

pub struct TrajectoryResult {
    pub class: &'static str,
    pub slope: Option<f64>,
    pub r_squared: Option<f64>,
    pub cv: Option<f64>,
}

pub fn classify_trajectory(scores: &[f64], dt: &DetectorThresholdsConfig) -> TrajectoryResult {
    if scores.len() < 2 {
        return TrajectoryResult {
            class: "insufficient_data",
            slope: None,
            r_squared: None,
            cv: None,
        };
    }
    let cv = coefficient_of_variation(scores);
    let lr = linregress_index(scores);

    let class = if cv < dt.trajectory_cv_low {
        "steady"
    } else if lr.slope > 0.0 && lr.p_value < dt.trajectory_slope_p_value {
        "growing"
    } else if lr.slope < 0.0 && lr.p_value < dt.trajectory_slope_p_value {
        "declining"
    } else if cv > dt.trajectory_cv_high {
        "sporadic"
    } else {
        "steady"
    };
    TrajectoryResult {
        class,
        slope: Some(lr.slope),
        r_squared: Some(lr.r_squared),
        cv: Some(cv),
    }
}

pub fn compute_all_trajectories(
    conn: &Connection,
    dt: &DetectorThresholdsConfig,
) -> rusqlite::Result<()> {
    compute_all_trajectories_filtered(conn, dt, None)
}

/// Project-scoped variant of `compute_all_trajectories`. Pass `Some(&[…])`
/// to delete + recompute only those projects' rows; pass `None` for the
/// historical full-table recompute.
pub fn compute_all_trajectories_filtered(
    conn: &Connection,
    dt: &DetectorThresholdsConfig,
    project_ids: Option<&[i64]>,
) -> rusqlite::Result<()> {
    let students: Vec<(String, i64)> = if let Some(ids) = project_ids {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = (0..ids.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        // Scoped delete: keep other projects' trajectory rows intact.
        let del_sql =
            format!("DELETE FROM student_trajectory WHERE project_id IN ({placeholders})");
        let params: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
        conn.execute(&del_sql, params.as_slice())?;
        let sel_sql = format!(
            "SELECT DISTINCT sc.student_id, s.team_project_id
             FROM student_sprint_contribution sc
             JOIN students s ON s.id = sc.student_id
             WHERE s.team_project_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sel_sql)?;
        let collected = stmt
            .query_map(params.as_slice(), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        collected
    } else {
        conn.execute("DELETE FROM student_trajectory", [])?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT sc.student_id, s.team_project_id
             FROM student_sprint_contribution sc
             JOIN students s ON s.id = sc.student_id
             WHERE s.team_project_id IS NOT NULL",
        )?;
        let collected = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        collected
    };

    for (sid, project_id) in &students {
        let mut stmt = conn.prepare(
            "SELECT sc.composite_score, sc.sprint_id
             FROM student_sprint_contribution sc
             JOIN sprints sp ON sp.id = sc.sprint_id
             WHERE sc.student_id = ?
             ORDER BY sp.start_date",
        )?;
        let rows: Vec<(Option<f64>, i64)> = stmt
            .query_map([sid], |r| {
                Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        let scores: Vec<f64> = rows.iter().filter_map(|(s, _)| *s).collect();
        let latest_sprint = rows.last().map(|(_, s)| *s);
        let traj = classify_trajectory(&scores, dt);
        conn.execute(
            "INSERT OR REPLACE INTO student_trajectory
             (student_id, project_id, trajectory_class, slope, r_squared,
              cv_across_sprints, sprint_count, latest_sprint_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                sid,
                project_id,
                traj.class,
                traj.slope,
                traj.r_squared,
                traj.cv,
                scores.len() as i64,
                latest_sprint,
            ],
        )?;
    }
    info!(count = students.len(), "Trajectory analysis classified");
    Ok(())
}
