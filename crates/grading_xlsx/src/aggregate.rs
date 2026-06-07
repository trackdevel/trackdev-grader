//! Per-task effective points and team totals.

use rusqlite::{params, Connection};

use crate::config::GradingConfig;
use crate::modulation::{keep_for_declared, keep_for_undeclared};

#[derive(Debug, Clone, PartialEq)]
pub struct TaskPoints {
    pub task_id: i64,
    pub assignee_id: String,
    pub raw: f64,
    pub effective: f64,
    pub keep: f64,
    pub declared: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StudentPoints {
    pub student_id: String,
    pub raw: f64,
    pub effective: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeamPoints {
    pub students: Vec<StudentPoints>,
    pub sum_raw: f64,
    pub sum_effective: f64,
    pub team_size: i64,
    pub mean_raw: f64,
}

/// Enrolled team members (`students.team_project_id = project_id`).
pub fn enrolled_team_size(conn: &Connection, project_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM students WHERE team_project_id = ?",
        params![project_id],
        |r| r.get(0),
    )
}

pub fn enrolled_student_ids(conn: &Connection, project_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ? ORDER BY id")?;
    let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Per-task keep + effective points for DONE non-USER_STORY tasks in scope.
pub fn load_task_points(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
    cfg: &GradingConfig,
) -> rusqlite::Result<Vec<TaskPoints>> {
    if sprint_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT t.id, t.assignee_id, t.estimation_points,
                tai.model_value, tai.level_value, tai.declared
         FROM tasks t
         JOIN students s ON s.id = t.assignee_id
         LEFT JOIN task_ai_usage tai ON tai.task_id = t.id
         WHERE s.team_project_id = ?
           AND t.sprint_id IN ({placeholders})
           AND t.status = 'DONE'
           AND t.type != 'USER_STORY'
           AND t.assignee_id IS NOT NULL
           AND t.estimation_points IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i64>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (task_id, assignee_id, raw_pts, model, level, declared_flag) = row?;
        let raw = raw_pts as f64;
        let keep = if declared_flag.unwrap_or(0) == 1 {
            if let (Some(m), Some(l)) = (model.as_deref(), level.as_deref()) {
                keep_for_declared(m, l, &cfg.ai_usage)
            } else {
                keep_for_undeclared(&cfg.ai_usage)
            }
        } else {
            keep_for_undeclared(&cfg.ai_usage)
        };
        out.push(TaskPoints {
            task_id,
            assignee_id,
            raw,
            effective: raw * keep,
            keep,
            declared: declared_flag.unwrap_or(0) == 1,
        });
    }
    Ok(out)
}

pub fn aggregate_team_points(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
    cfg: &GradingConfig,
) -> rusqlite::Result<TeamPoints> {
    let tasks = load_task_points(conn, project_id, sprint_ids, cfg)?;
    let team_size = enrolled_team_size(conn, project_id)?;
    let member_ids = enrolled_student_ids(conn, project_id)?;

    let mut per_student: Vec<StudentPoints> = member_ids
        .into_iter()
        .map(|id| StudentPoints {
            student_id: id,
            raw: 0.0,
            effective: 0.0,
        })
        .collect();

    for t in &tasks {
        if let Some(sp) = per_student
            .iter_mut()
            .find(|s| s.student_id == t.assignee_id)
        {
            sp.raw += t.raw;
            sp.effective += t.effective;
        }
    }

    let sum_raw: f64 = per_student.iter().map(|s| s.raw).sum();
    let sum_effective: f64 = per_student.iter().map(|s| s.effective).sum();
    let n = team_size.max(1) as f64;
    let mean_raw = if sum_raw > 0.0 { sum_raw / n } else { 0.0 };

    Ok(TeamPoints {
        students: per_student,
        sum_raw,
        sum_effective,
        team_size,
        mean_raw,
    })
}
