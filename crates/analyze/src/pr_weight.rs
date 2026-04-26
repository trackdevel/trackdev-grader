//! Proportional PR weight distribution across linked tasks.
//! Mirrors `src/analyze/pr_weight.py`.

use std::collections::HashMap;

use rusqlite::Connection;
use tracing::info;

#[derive(Debug, Clone)]
pub struct WeightedPRMetrics {
    pub task_id: i64,
    pub pr_id: String,
    pub weight: f64,
    pub additions: f64,
    pub deletions: f64,
    pub changed_files: f64,
    pub lat: f64,
    pub ls: f64,
}

pub fn distribute_pr_weights_for_sprint(
    conn: &Connection,
    sprint_id: i64,
) -> rusqlite::Result<Vec<WeightedPRMetrics>> {
    let mut stmt = conn.prepare(
        "SELECT tpr.task_id, tpr.pr_id,
                t.estimation_points,
                pr.additions, pr.deletions, pr.changed_files,
                plm.lat AS plm_lat, plm.ls AS plm_ls
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         JOIN pull_requests pr ON pr.id = tpr.pr_id
         LEFT JOIN pr_line_metrics plm
             ON plm.pr_id = tpr.pr_id AND plm.sprint_id = t.sprint_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;

    #[derive(Clone)]
    struct Row {
        task_id: i64,
        pr_id: String,
        points: i64,
        additions: i64,
        deletions: i64,
        changed_files: i64,
        lat: i64,
        ls: i64,
    }

    let rows: Vec<Row> = stmt
        .query_map([sprint_id], |r| {
            Ok(Row {
                task_id: r.get(0)?,
                pr_id: r.get(1)?,
                points: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                additions: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                deletions: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                changed_files: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                lat: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                ls: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut pr_total_points: HashMap<String, i64> = HashMap::new();
    let mut pr_tasks: HashMap<String, Vec<Row>> = HashMap::new();
    for r in &rows {
        *pr_total_points.entry(r.pr_id.clone()).or_insert(0) += r.points;
        pr_tasks.entry(r.pr_id.clone()).or_default().push(r.clone());
    }

    let mut out: Vec<WeightedPRMetrics> = Vec::new();
    for (pr_id, tasks) in pr_tasks {
        let total_pts = pr_total_points.get(&pr_id).copied().unwrap_or(0);
        let task_count = tasks.len().max(1) as f64;
        for task in tasks {
            let weight = if total_pts > 0 {
                task.points as f64 / total_pts as f64
            } else {
                1.0 / task_count
            };
            out.push(WeightedPRMetrics {
                task_id: task.task_id,
                pr_id: pr_id.clone(),
                weight,
                additions: task.additions as f64 * weight,
                deletions: task.deletions as f64 * weight,
                changed_files: task.changed_files as f64 * weight,
                lat: task.lat as f64 * weight,
                ls: task.ls as f64 * weight,
            });
        }
    }
    info!(pairs = out.len(), "Distributed PR weights");
    Ok(out)
}
