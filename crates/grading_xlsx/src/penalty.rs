//! Subtractive penalties at project and student scope.

use rusqlite::{params, Connection};

use sprint_grader_core::finding::RuleKind;
use sprint_grader_core::finding::Severity;
use sprint_grader_core::rule_attribution::load_attributed_findings_for_repo;

use crate::config::PenaltyConfig;
use crate::normalize::project_repos;

pub fn project_penalty(
    conn: &Connection,
    project_id: i64,
    cfg: &PenaltyConfig,
) -> rusqlite::Result<f64> {
    if cfg.mode != "subtractive" {
        return Ok(0.0);
    }
    let repos = project_repos(conn, project_id)?;
    let mut total = 0.0;
    for repo in &repos {
        total += static_analysis_penalty(conn, repo, cfg)?;
        total += complexity_penalty(conn, repo, cfg)?;
    }
    Ok(total.min(cfg.max_penalty_points))
}

fn static_analysis_penalty(
    conn: &Connection,
    repo: &str,
    cfg: &PenaltyConfig,
) -> rusqlite::Result<f64> {
    let mut stmt = conn.prepare(
        "SELECT id, category FROM static_analysis_findings
         WHERE repo_full_name = ? AND severity = 'CRITICAL'",
    )?;
    let rows = stmt.query_map(params![repo], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut sum = 0.0;
    for row in rows {
        let (_id, category) = row?;
        sum += cfg.crit_sa_points;
        if category.as_deref() == Some("security") {
            sum += cfg.security_extra;
        }
    }
    Ok(sum)
}

fn complexity_penalty(conn: &Connection, repo: &str, cfg: &PenaltyConfig) -> rusqlite::Result<f64> {
    let findings = load_attributed_findings_for_repo(conn, repo, RuleKind::Complexity)?;
    let crit = findings
        .iter()
        .filter(|af| af.finding.severity == Severity::Critical)
        .count();
    Ok(crit as f64 * cfg.crit_cx_points)
}

pub fn student_penalty(
    conn: &Connection,
    student_id: &str,
    project_id: i64,
    sprint_ids: &[i64],
    cfg: &PenaltyConfig,
) -> rusqlite::Result<f64> {
    if cfg.mode != "subtractive" {
        return Ok(0.0);
    }
    let mut total = 0.0;
    total += sprint_flags_penalty(conn, student_id, sprint_ids, cfg)?;
    total += artifact_flags_penalty(conn, student_id, project_id, cfg)?;
    Ok(total.min(cfg.student_penalty_cap))
}

fn sprint_flags_penalty(
    conn: &Connection,
    student_id: &str,
    sprint_ids: &[i64],
    cfg: &PenaltyConfig,
) -> rusqlite::Result<f64> {
    if sprint_ids.is_empty() {
        return Ok(0.0);
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT COUNT(*) FROM flags
         WHERE student_id = ?
           AND sprint_id IN ({placeholders})
           AND severity = 'CRITICAL'
           AND student_id NOT LIKE 'PROJECT_%'"
    );
    let mut params: Vec<rusqlite::types::Value> = vec![student_id.to_string().into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let count: i64 = conn.query_row(&sql, rusqlite::params_from_iter(params), |r| r.get(0))?;
    Ok(count as f64 * cfg.crit_flag_points)
}

fn artifact_flags_penalty(
    conn: &Connection,
    student_id: &str,
    project_id: i64,
    cfg: &PenaltyConfig,
) -> rusqlite::Result<f64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM student_artifact_flags
         WHERE student_id = ? AND project_id = ? AND severity = 'CRITICAL'",
        params![student_id, project_id],
        |r| r.get(0),
    )?;
    Ok(count as f64 * cfg.crit_flag_points)
}
