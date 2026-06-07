//! DB-backed context for holistic quality-flag synthesis.

use anyhow::Result;
use rusqlite::{params, Connection};

#[derive(Debug, Clone)]
pub struct FileFlagSummary {
    pub target_ref: String,
    pub category: String,
    pub severity: String,
    pub summary: String,
}

pub fn list_project_repos(conn: &Connection, project_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.repo_full_name
         FROM pull_requests pr
         JOIN pr_authors pa ON pa.pr_id = pr.id
         JOIN students s ON s.id = pa.student_id
         WHERE s.team_project_id = ?
           AND pr.repo_full_name IS NOT NULL AND pr.repo_full_name != ''
         ORDER BY pr.repo_full_name",
    )?;
    let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn load_file_flag_summaries(
    conn: &Connection,
    project_id: i64,
    repo_filter: Option<&str>,
) -> Result<Vec<FileFlagSummary>> {
    let mut stmt = conn.prepare(
        "SELECT target_ref, category, severity, summary
         FROM llm_quality_flag
         WHERE project_id = ? AND scope = 'file'
         ORDER BY
           CASE severity WHEN 'CRITICAL' THEN 0 WHEN 'WARNING' THEN 1 ELSE 2 END,
           target_ref, id
         LIMIT 80",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok(FileFlagSummary {
            target_ref: r.get(0)?,
            category: r.get(1)?,
            severity: r.get(2)?,
            summary: r.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        let s = row?;
        if let Some(repo) = repo_filter {
            let prefix = format!("{repo}:");
            if !s.target_ref.starts_with(&prefix) {
                continue;
            }
        }
        out.push(s);
    }
    Ok(out)
}

pub fn project_team_size(conn: &Connection, project_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM students WHERE team_project_id = ?",
        params![project_id],
        |r| r.get(0),
    )
}

pub fn format_holistic_context(
    project_name: &str,
    project_id: i64,
    team_size: i64,
    repos: &[String],
    repo_focus: Option<&str>,
    file_flags: &[FileFlagSummary],
) -> String {
    let mut lines = vec![
        format!("Project: {project_name} (id={project_id})"),
        format!("Enrolled students: {team_size}"),
        format!("Repositories: {}", repos.join(", ")),
    ];
    if let Some(repo) = repo_focus {
        lines.push(format!("Holistic focus: repository {repo}"));
    } else {
        lines.push("Holistic focus: whole team (all repositories)".to_string());
    }
    lines.push(String::new());
    lines.push("File-tier findings already collected (synthesize — do not repeat verbatim):".into());
    if file_flags.is_empty() {
        lines.push("  (none — note if the delivered codebase looks clean or under-reviewed.)".into());
    } else {
        for f in file_flags {
            lines.push(format!(
                "  - [{} / {}] {} — {}",
                f.severity, f.category, f.target_ref, f.summary
            ));
        }
    }
    lines.join("\n")
}
