//! Team-level inequality metrics. Mirrors `src/analyze/inequality.py`.

use rusqlite::{params, Connection};
use tracing::info;

use sprint_grader_core::stats::{coefficient_of_variation, gini, hoover, max_min_ratio};

/// SQL template + whether the query needs project_id as a second bind var.
struct Dim {
    name: &'static str,
    sql: &'static str,
    needs_project_id: bool,
}

const DIMENSIONS: &[Dim] = &[
    Dim {
        name: "points_delivered",
        sql: "SELECT t.assignee_id AS student_id,
                     COALESCE(SUM(t.estimation_points), 0) AS value
              FROM tasks t
              WHERE t.sprint_id = ? AND t.status = 'DONE' AND t.assignee_id IS NOT NULL
                AND t.type != 'USER_STORY'
              GROUP BY t.assignee_id",
        needs_project_id: false,
    },
    Dim {
        name: "commit_count",
        sql: "SELECT pr.author_id AS student_id,
                     COUNT(DISTINCT pc.sha) AS value
              FROM pull_requests pr
              JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
              JOIN tasks t ON t.id = tpr.task_id
              JOIN pr_commits pc ON pc.pr_id = pr.id
              WHERE t.sprint_id = ? AND t.type != 'USER_STORY' AND pr.author_id IS NOT NULL
              GROUP BY pr.author_id",
        needs_project_id: false,
    },
    Dim {
        name: "reviews_given",
        sql: "SELECT s.id AS student_id,
                     COUNT(DISTINCT rv.pr_id || rv.submitted_at) AS value
              FROM students s
              JOIN pr_reviews rv ON LOWER(rv.reviewer_login) = LOWER(s.github_login)
              JOIN pull_requests pr ON pr.id = rv.pr_id
              JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
              JOIN tasks t ON t.id = tpr.task_id
              WHERE t.sprint_id = ? AND t.type != 'USER_STORY' AND s.team_project_id = ?
              GROUP BY s.id",
        needs_project_id: true,
    },
    Dim {
        name: "pr_lines",
        sql: "SELECT student_id, COALESCE(weighted_pr_lines, 0) AS value
              FROM student_sprint_metrics
              WHERE sprint_id = ?",
        needs_project_id: false,
    },
];

fn compute_team_inequality(
    conn: &Connection,
    project_id: i64,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
    let member_ids: Vec<String> = stmt
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if member_ids.len() < 2 {
        return Ok(());
    }

    for dim in DIMENSIONS {
        let mut stmt = conn.prepare(dim.sql)?;
        let rows: Vec<(String, f64)> = if dim.needs_project_id {
            stmt.query_map(params![sprint_id, project_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        } else {
            stmt.query_map([sprint_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        drop(stmt);

        let mut per_student: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        for (sid, val) in rows {
            if member_ids.iter().any(|m| m == &sid) {
                per_student.insert(sid, val);
            }
        }
        let values: Vec<f64> = member_ids
            .iter()
            .map(|sid| per_student.get(sid).copied().unwrap_or(0.0))
            .collect();

        let g = gini(&values);
        let h = hoover(&values);
        let cv = coefficient_of_variation(&values);
        let mmr = max_min_ratio(&values);

        conn.execute(
            "INSERT OR REPLACE INTO team_sprint_inequality
             (project_id, sprint_id, metric_name, gini, hoover, cv, max_min_ratio, member_count)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                project_id,
                sprint_id,
                dim.name,
                g,
                h,
                cv,
                mmr,
                member_ids.len() as i64
            ],
        )?;
    }
    Ok(())
}

pub fn compute_all_inequality(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("SELECT DISTINCT project_id FROM sprints WHERE id = ?")?;
    let project_ids: Vec<i64> = stmt
        .query_map([sprint_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for pid in &project_ids {
        info!(project_id = *pid, sprint_id, "Computing inequality");
        compute_team_inequality(conn, *pid, sprint_id)?;
    }
    info!(sprint_id, "Inequality computation done");
    Ok(())
}
