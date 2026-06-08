//! Load grading.db rows for the self-recalculating workbook.

use anyhow::Result;
use rusqlite::{params, Connection};
use sprint_grader_core::finding::{RuleKind, Severity};
use sprint_grader_core::rule_attribution::load_attributed_findings_for_repo;
use sprint_grader_core::Database;
use sprint_grader_quality_llm::list_all_flags;

use crate::aggregate::load_task_points;
use crate::config::GradingConfig;
use crate::grade::{grade_project, GradingResult};
use crate::labels::WorkbookLabels;
use crate::normalize::{
    code_quality_raw, documentation_raw, project_repos, score_architecture, score_code_quality,
    score_documentation, score_survival, survival_raw,
};

#[derive(Debug, Clone)]
pub struct WorkbookData {
    pub generated_at: String,
    pub weights_version: String,
    pub results: Vec<GradingResult>,
    /// Per-project axis inputs (includes cc/mutation columns for Quality).
    pub project_axes: Vec<ProjectAxisRaw>,
    pub tasks: Vec<TaskRow>,
    pub crit_flags: Vec<CritFlagRow>,
    pub flag_rows: Vec<FlagDiagRow>,
    pub ai_detect_rows: Vec<AiDetectRow>,
    pub llm_flag_rows: Vec<LlmFlagRow>,
    pub labels: WorkbookLabels,
}

#[derive(Debug, Clone)]
pub struct LlmFlagRow {
    pub project_id: i64,
    pub student_id: Option<String>,
    pub sprint_id: Option<i64>,
    pub scope: String,
    pub target_ref: Option<String>,
    pub category: String,
    pub severity: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub project_id: i64,
    pub project_name: String,
    pub task_id: i64,
    pub assignee_id: String,
    pub model: Option<String>,
    pub level: Option<String>,
    pub declared: bool,
    pub raw_points: f64,
}

#[derive(Debug, Clone)]
pub struct CritFlagRow {
    pub project_id: i64,
    pub repo_full_name: String,
    pub kind: String,
    pub rule_id: String,
    pub severity: String,
    pub category: Option<String>,
    pub penalty_points: f64,
}

#[derive(Debug, Clone)]
pub struct FlagDiagRow {
    pub project_id: i64,
    pub student_id: String,
    pub sprint_id: i64,
    pub flag_type: String,
    pub severity: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AiDetectRow {
    pub project_id: i64,
    pub student_id: String,
    pub sprint_id: i64,
    pub risk_level: Option<String>,
}

pub fn load_workbook_data(
    db: &Database,
    project_ids: &[i64],
    today: &str,
    cfg: &GradingConfig,
) -> Result<WorkbookData> {
    load_workbook_data_with_results(db, project_ids, today, cfg, None)
}

/// Build workbook rows. When `precomputed` is set it must align 1:1 with `project_ids`
/// (skips a second `grade_project` pass).
pub fn load_workbook_data_with_results(
    db: &Database,
    project_ids: &[i64],
    today: &str,
    cfg: &GradingConfig,
    precomputed: Option<&[GradingResult]>,
) -> Result<WorkbookData> {
    if let Some(rs) = precomputed {
        anyhow::ensure!(
            rs.len() == project_ids.len(),
            "precomputed results length {} != project_ids length {}",
            rs.len(),
            project_ids.len()
        );
        for (pid, r) in project_ids.iter().zip(rs.iter()) {
            anyhow::ensure!(
                r.project.project_id == *pid,
                "precomputed result project_id {} != expected {}",
                r.project.project_id,
                pid
            );
        }
    }

    let conn = &db.conn;
    let mut results = Vec::new();
    let mut project_axes = Vec::new();
    let mut tasks = Vec::new();
    let mut crit_flags = Vec::new();
    let mut flag_rows = Vec::new();
    let mut ai_detect_rows = Vec::new();

    for (i, &project_id) in project_ids.iter().enumerate() {
        let sprint_ids = db.sprint_ids_up_to_current(project_id, today)?;
        let name: String = conn.query_row(
            "SELECT name FROM projects WHERE id = ?",
            params![project_id],
            |r| r.get(0),
        )?;
        let result = match precomputed {
            Some(rs) => rs[i].clone(),
            None => grade_project(conn, project_id, &name, &sprint_ids, cfg)?,
        };
        project_axes.push(project_axis_raw(conn, project_id, &sprint_ids, cfg)?);

        for t in load_task_points(conn, project_id, &sprint_ids, cfg)? {
            let (model, level, declared): (Option<String>, Option<String>, i64) = conn
                .query_row(
                    "SELECT model_value, level_value, declared FROM task_ai_usage WHERE task_id = ?",
                    params![t.task_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap_or((None, None, 0));
            tasks.push(TaskRow {
                project_id,
                project_name: name.clone(),
                task_id: t.task_id,
                assignee_id: t.assignee_id,
                model,
                level,
                declared: declared == 1,
                raw_points: t.raw,
            });
        }

        crit_flags.extend(load_crit_flags(conn, project_id, cfg)?);
        flag_rows.extend(load_flag_diag(conn, project_id, &sprint_ids)?);
        ai_detect_rows.extend(load_ai_detect(conn, project_id, &sprint_ids)?);
        results.push(result);
    }

    let generated_at = results
        .first()
        .map(|r| r.generated_at.clone())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let weights_version = results
        .first()
        .map(|r| r.weights_version.clone())
        .unwrap_or_else(|| cfg.weights_version());

    let labels = WorkbookLabels::load(conn)?;
    let llm_flag_rows = list_all_flags(conn)?
        .into_iter()
        .map(|f| LlmFlagRow {
            project_id: f.project_id,
            student_id: f.student_id,
            sprint_id: f.sprint_id,
            scope: f.scope,
            target_ref: f.target_ref,
            category: f.category,
            severity: f.severity,
            summary: f.summary,
        })
        .collect();

    Ok(WorkbookData {
        generated_at,
        weights_version,
        results,
        project_axes,
        tasks,
        crit_flags,
        flag_rows,
        ai_detect_rows,
        llm_flag_rows,
        labels,
    })
}

/// Raw axis inputs per project (scores are computed as workbook formulas).
pub fn project_axis_raw(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
    cfg: &GradingConfig,
) -> rusqlite::Result<ProjectAxisRaw> {
    let doc = documentation_raw(conn, project_id, sprint_ids)?;
    let (cq, cc, mutation) = code_quality_raw(conn, project_id, sprint_ids)?;
    let surv = survival_raw(conn, project_id, sprint_ids)?;
    let arch = score_architecture(conn, project_id, &cfg.normalization)?;

    Ok(ProjectAxisRaw {
        documentation_raw: doc.raw_value,
        documentation_present: doc.present,
        code_quality_raw: cq.raw_value,
        code_quality_present: cq.present,
        cc_pct: cc,
        mutation_score: mutation,
        survival_raw: surv.raw_value,
        survival_present: surv.present,
        architecture_density: arch.raw_value,
        architecture_present: arch.present,
        // Rust-side scores for parity tests / cached formula results.
        documentation_score: score_documentation(&doc, &cfg.normalization).score_0_10,
        code_quality_score: score_code_quality(&cq, cc, mutation, &cfg.normalization).score_0_10,
        survival_score: score_survival(&surv, &cfg.normalization).score_0_10,
        architecture_score: arch.score_0_10,
    })
}

#[derive(Debug, Clone)]
pub struct ProjectAxisRaw {
    pub documentation_raw: Option<f64>,
    pub documentation_present: bool,
    pub code_quality_raw: Option<f64>,
    pub code_quality_present: bool,
    pub cc_pct: Option<f64>,
    pub mutation_score: Option<f64>,
    pub survival_raw: Option<f64>,
    pub survival_present: bool,
    pub architecture_density: Option<f64>,
    pub architecture_present: bool,
    pub documentation_score: Option<f64>,
    pub code_quality_score: Option<f64>,
    pub survival_score: Option<f64>,
    pub architecture_score: Option<f64>,
}

fn load_crit_flags(
    conn: &Connection,
    project_id: i64,
    cfg: &GradingConfig,
) -> rusqlite::Result<Vec<CritFlagRow>> {
    let mut out = Vec::new();
    for repo in project_repos(conn, project_id)? {
        let mut stmt = conn.prepare(
            "SELECT rule_id, category FROM static_analysis_findings
             WHERE repo_full_name = ? AND severity = 'CRITICAL'",
        )?;
        let rows = stmt.query_map(params![repo], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (rule_id, category) = row?;
            let mut pts = cfg.penalty.crit_sa_points;
            if category.as_deref() == Some("security") {
                pts += cfg.penalty.security_extra;
            }
            out.push(CritFlagRow {
                project_id,
                repo_full_name: repo.clone(),
                kind: "static_analysis".to_string(),
                rule_id,
                severity: "CRITICAL".to_string(),
                category,
                penalty_points: pts,
            });
        }

        let findings = load_attributed_findings_for_repo(conn, &repo, RuleKind::Complexity)?;
        for af in findings {
            if af.finding.severity == Severity::Critical {
                out.push(CritFlagRow {
                    project_id,
                    repo_full_name: repo.clone(),
                    kind: "complexity".to_string(),
                    rule_id: af.finding.rule_id.clone(),
                    severity: "CRITICAL".to_string(),
                    category: None,
                    penalty_points: cfg.penalty.crit_cx_points,
                });
            }
        }
    }
    Ok(out)
}

fn load_flag_diag(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<Vec<FlagDiagRow>> {
    if sprint_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT f.student_id, f.sprint_id, f.flag_type, f.severity, f.details
         FROM flags f
         WHERE f.sprint_id IN ({placeholders})
           AND f.student_id NOT LIKE 'PROJECT_%'"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (student_id, sprint_id, flag_type, severity, details) = row?;
        // Keep flags for students on this project.
        let on_project: i64 = conn.query_row(
            "SELECT COUNT(*) FROM students WHERE id = ? AND team_project_id = ?",
            params![student_id, project_id],
            |r| r.get(0),
        )?;
        if on_project > 0 {
            out.push(FlagDiagRow {
                project_id,
                student_id,
                sprint_id,
                flag_type,
                severity,
                details,
            });
        }
    }
    Ok(out)
}

fn load_ai_detect(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<Vec<AiDetectRow>> {
    if sprint_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT student_id, sprint_id, risk_level FROM student_sprint_ai_usage
         WHERE project_id = ? AND sprint_id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (student_id, sprint_id, risk_level) = row?;
        out.push(AiDetectRow {
            project_id,
            student_id,
            sprint_id,
            risk_level,
        });
    }
    Ok(out)
}
