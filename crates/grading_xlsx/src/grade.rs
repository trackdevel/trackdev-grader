//! Assemble project and student final grades with review gates.

use chrono::Utc;
use rusqlite::Connection;

use crate::aggregate::aggregate_team_points;
use crate::config::{GateConfig, GradingConfig, OutputConfig};
use crate::normalize::{clamp_0_10, load_quality_axes, quality_composite, AxisScore};
use crate::penalty::{project_penalty, student_penalty};

#[derive(Debug, Clone, PartialEq)]
pub struct ComponentScore {
    pub key: &'static str,
    pub raw_value: Option<f64>,
    pub score_0_10: Option<f64>,
    pub present: bool,
}

impl From<AxisScore> for ComponentScore {
    fn from(a: AxisScore) -> Self {
        Self {
            key: a.key,
            raw_value: a.raw_value,
            score_0_10: a.score_0_10,
            present: a.present,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectGradeRow {
    pub project_id: i64,
    pub name: String,
    pub components: Vec<ComponentScore>,
    pub quality_grade: f64,
    pub project_penalty: f64,
    pub quality_penalized: f64,
    pub ai_factor: f64,
    pub final_grade: f64,
    pub team_size: i64,
    pub review_gate: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StudentGradeRow {
    pub student_id: String,
    pub project_id: i64,
    pub full_name: String,
    pub raw_points: f64,
    pub effective_points: f64,
    pub ai_keep_factor: Option<f64>,
    pub contribution_ratio: Option<f64>,
    pub base_grade: f64,
    pub student_penalty: f64,
    pub final_grade: f64,
    pub review_gate: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GradingResult {
    pub project: ProjectGradeRow,
    pub students: Vec<StudentGradeRow>,
    pub ai_strength: f64,
    pub weights_version: String,
    pub generated_at: String,
}

pub fn grade_project(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    sprint_ids: &[i64],
    cfg: &GradingConfig,
) -> rusqlite::Result<GradingResult> {
    let components: Vec<ComponentScore> = load_quality_axes(conn, project_id, sprint_ids, cfg)?
        .into_iter()
        .map(ComponentScore::from)
        .collect();

    let q = quality_composite(
        &components
            .iter()
            .map(|c| AxisScore {
                key: c.key,
                raw_value: c.raw_value,
                score_0_10: c.score_0_10,
                present: c.present,
            })
            .collect::<Vec<_>>(),
        cfg,
    )
    .unwrap_or(0.0);

    let proj_pen = project_penalty(conn, project_id, &cfg.penalty)?;
    let q_pen = clamp_0_10(q - proj_pen);

    let team = aggregate_team_points(conn, project_id, sprint_ids, cfg)?;
    let ai_factor = if team.sum_raw > 0.0 {
        team.sum_effective / team.sum_raw
    } else {
        1.0
    };
    let project_final = round_grade(q_pen * ai_factor, &cfg.output);

    let plagiarism = project_has_plagiarism(conn, project_id, &cfg.gate)?;
    let project_gate = if plagiarism {
        Some("PLAGIARISM".to_string())
    } else {
        None
    };

    let weights_version = cfg.weights_version();
    let generated_at = Utc::now().to_rfc3339();

    let mut student_rows = Vec::new();
    for sp in &team.students {
        let full_name: String = conn.query_row(
            "SELECT full_name FROM students WHERE id = ?",
            rusqlite::params![sp.student_id],
            |r| r.get(0),
        )?;

        let contribution = if team.sum_effective > 0.0 {
            Some(sp.effective / team.sum_effective)
        } else {
            None
        };
        let ai_keep = if sp.raw > 0.0 {
            Some(sp.effective / sp.raw)
        } else {
            None
        };

        let base = if team.mean_raw > 0.0 {
            q_pen * sp.effective / team.mean_raw
        } else {
            0.0
        };

        let stu_pen = student_penalty(conn, &sp.student_id, project_id, sprint_ids, &cfg.penalty)?;

        let mut gate = project_gate.clone();
        if sp.effective <= 0.0 {
            gate = Some("NO_DELIVERY".to_string());
        } else if gate.is_none()
            && student_needs_ai_review(conn, &sp.student_id, project_id, sprint_ids, cfg)?
        {
            gate = Some("AI_REVIEW".to_string());
        }

        let final_g = round_grade(clamp_0_10(base - stu_pen), &cfg.output);

        student_rows.push(StudentGradeRow {
            student_id: sp.student_id.clone(),
            project_id,
            full_name,
            raw_points: sp.raw,
            effective_points: sp.effective,
            ai_keep_factor: ai_keep,
            contribution_ratio: contribution,
            base_grade: round_grade(base, &cfg.output),
            student_penalty: stu_pen,
            final_grade: if sp.effective <= 0.0 { 0.0 } else { final_g },
            review_gate: gate,
        });
    }

    let project_row = ProjectGradeRow {
        project_id,
        name: project_name.to_string(),
        components,
        quality_grade: round_grade(q, &cfg.output),
        project_penalty: proj_pen,
        quality_penalized: round_grade(q_pen, &cfg.output),
        ai_factor,
        final_grade: project_final,
        team_size: team.team_size,
        review_gate: project_gate,
    };

    Ok(GradingResult {
        project: project_row,
        students: student_rows,
        ai_strength: cfg.ai_usage.strength,
        weights_version,
        generated_at,
    })
}

fn round_grade(value: f64, output: &OutputConfig) -> f64 {
    let factor = 10f64.powi(output.decimals as i32);
    let rounded = (value * factor).round() / factor;
    if output.quantize_final > 0.0 {
        (rounded / output.quantize_final).round() * output.quantize_final
    } else {
        rounded
    }
}

fn project_has_plagiarism(
    conn: &Connection,
    project_id: i64,
    gate: &GateConfig,
) -> rusqlite::Result<bool> {
    let synthetic = format!("PROJECT_{project_id}");
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM flags f
         JOIN sprints sp ON sp.id = f.sprint_id
         WHERE sp.project_id = ?
           AND f.flag_type = ?
           AND f.student_id = ?",
        rusqlite::params![project_id, gate.plagiarism_flag, synthetic],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// HIGH detected AI + low/absent declared usage → AI_REVIEW gate.
fn student_needs_ai_review(
    conn: &Connection,
    student_id: &str,
    project_id: i64,
    sprint_ids: &[i64],
    cfg: &GradingConfig,
) -> rusqlite::Result<bool> {
    if sprint_ids.is_empty() {
        return Ok(false);
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT COUNT(*) FROM student_sprint_ai_usage
         WHERE student_id = ? AND project_id = ?
           AND sprint_id IN ({placeholders})
           AND risk_level = ?"
    );
    let mut params: Vec<rusqlite::types::Value> =
        vec![student_id.to_string().into(), project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    params.push(cfg.gate.ai_detect_risk_level.clone().into());
    let high_count: i64 = conn.query_row(&sql, rusqlite::params_from_iter(params), |r| r.get(0))?;
    if high_count == 0 {
        return Ok(false);
    }
    let declared_level = student_max_declared_level(conn, student_id, project_id, sprint_ids)?;
    match declared_level {
        None => Ok(true),
        Some(level) => Ok(cfg.gate.ai_detect_low_levels.iter().any(|l| l == &level)),
    }
}

fn student_max_declared_level(
    conn: &Connection,
    student_id: &str,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<Option<String>> {
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT tai.level_value
         FROM tasks t
         JOIN task_ai_usage tai ON tai.task_id = t.id
         JOIN students s ON s.id = t.assignee_id
         WHERE t.assignee_id = ?
           AND s.team_project_id = ?
           AND t.sprint_id IN ({placeholders})
           AND t.status = 'DONE'
           AND t.type != 'USER_STORY'
           AND tai.declared = 1
           AND tai.level_value IS NOT NULL"
    );
    let mut params: Vec<rusqlite::types::Value> =
        vec![student_id.to_string().into(), project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        r.get::<_, String>(0)
    })?;
    let order = ["A", "B", "C", "D", "E"];
    let mut best: Option<usize> = None;
    for row in rows {
        let level = row?;
        if let Some(idx) = order.iter().position(|&l| l == level) {
            best = Some(best.map_or(idx, |b| b.max(idx)));
        }
    }
    Ok(best.map(|i| order[i].to_string()))
}
