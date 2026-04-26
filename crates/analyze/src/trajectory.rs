//! Longitudinal trajectory classification. Mirrors `src/analyze/trajectory.py`.

use rusqlite::{params, Connection};
use tracing::info;

use sprint_grader_core::stats::{coefficient_of_variation, linregress_index};

pub struct TrajectoryResult {
    pub class: &'static str,
    pub slope: Option<f64>,
    pub r_squared: Option<f64>,
    pub cv: Option<f64>,
}

pub fn classify_trajectory(scores: &[f64]) -> TrajectoryResult {
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

    let class = if cv < 0.20 {
        "steady"
    } else if lr.slope > 0.0 && lr.p_value < 0.15 {
        "growing"
    } else if lr.slope < 0.0 && lr.p_value < 0.15 {
        "declining"
    } else if cv > 0.40 {
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

pub fn compute_all_trajectories(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM student_trajectory", [])?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sc.student_id, s.team_project_id
         FROM student_sprint_contribution sc
         JOIN students s ON s.id = sc.student_id
         WHERE s.team_project_id IS NOT NULL",
    )?;
    let students: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

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
        let traj = classify_trajectory(&scores);
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
