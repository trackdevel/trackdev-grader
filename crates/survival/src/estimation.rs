//! Estimation density — logs per-team averages and per-student deviations.
//! Mirrors `src/survival/estimation.py`.
//!
//! The actual `estimation_density` column is populated by `survival::compute`
//! (this module only reads + logs).

use rusqlite::Connection;
use tracing::{debug, info};

pub fn compute_estimation_density(conn: &Connection, sprint_ids: &[i64]) -> rusqlite::Result<()> {
    for sprint_id in sprint_ids {
        let project_id: Option<i64> = conn
            .query_row(
                "SELECT project_id FROM sprints WHERE id = ?",
                [sprint_id],
                |r| r.get(0),
            )
            .ok();
        let project_id = match project_id {
            Some(p) => p,
            None => continue,
        };

        let mut stmt = conn.prepare(
            "SELECT sss.student_id, sss.estimation_density
             FROM student_sprint_survival sss
             JOIN students s ON s.id = sss.student_id
             WHERE sss.sprint_id = ? AND s.team_project_id = ?",
        )?;
        let rows = stmt.query_map([*sprint_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            ))
        })?;
        let pairs: Vec<(String, f64)> = rows.collect::<rusqlite::Result<_>>()?;
        if pairs.is_empty() {
            continue;
        }
        let densities: Vec<f64> = pairs.iter().map(|(_, d)| *d).collect();
        let team_avg = densities.iter().sum::<f64>() / densities.len() as f64;
        info!(
            project_id,
            sprint_id = *sprint_id,
            team_avg,
            students = pairs.len(),
            "team avg estimation density"
        );
        for (sid, density) in &pairs {
            let deviation = density - team_avg;
            debug!(
                student_id = %sid,
                density,
                deviation,
                "  per-student density"
            );
        }
    }
    info!("Estimation density computation complete");
    Ok(())
}
