//! Per-project holistic synthesis tier (Track B PC).

use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;
use sprint_grader_core::QualityLlmConfig;
use tracing::{info, warn};

use crate::backend::QualityBackend;
use crate::context::{
    format_holistic_context, list_project_repos, load_file_flag_summaries, project_team_size,
};
use crate::flag::LlmQualityFlagRow;
use crate::parse::parse_quality_flags_json;
use crate::persist::{holistic_flag_exists, insert_flag};
use crate::rubric::QualityRubric;

pub const HOLISTIC_RUBRIC_APPENDIX: &str = r#"
## Holistic tier (this invocation)

You are synthesizing **team-level** instructor feedback from file-tier findings
and project context. Output is advisory only — never a numeric score.

- Emit `scope = "project"` flags (the pipeline sets this; you only return JSON).
- Identify cross-cutting themes: uneven contribution signals, recurring defect
  classes, testing gaps across modules, documentation/process issues visible in
  code, or team-wide maintainability risks.
- You may optionally set `student_id` on a flag when the issue clearly belongs
  to one member; omit it for team-wide observations.
- Do **not** re-list every file finding; synthesize patterns in 0–5 flags.
- If nothing meaningful beyond the file list, return `{"flags": []}`.
"#;

#[derive(Debug, Default, Clone, Copy)]
pub struct HolisticPassStats {
    pub judged: usize,
    pub skipped_resume: usize,
    pub skipped_cap: usize,
    pub flags_written: usize,
    pub failures: usize,
}

#[derive(Debug, Clone)]
enum HolisticTarget {
    TeamWide,
    Repo(String),
}

pub fn run_holistic_pass(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    ql: &QualityLlmConfig,
    rubric: &QualityRubric,
    max_holistic: usize,
    resume: bool,
) -> Result<HolisticPassStats> {
    let mut stats = HolisticPassStats::default();
    if max_holistic == 0 {
        stats.skipped_cap = 1;
        return Ok(stats);
    }

    let repos = list_project_repos(conn, project_id)?;
    let targets = holistic_targets(&repos, max_holistic);
    if targets.is_empty() {
        return Ok(stats);
    }

    let model_id = ql.resolved_model_id().to_string();
    let backend = ql.backend.clone();
    let prompt_version = ql.prompt_version.clone();

    let mut pending = Vec::new();
    for target in targets {
        let (target_ref, repo_focus) = match &target {
            HolisticTarget::TeamWide => (format!("project:{project_id}"), None),
            HolisticTarget::Repo(repo) => (format!("holistic:{repo}"), Some(repo.as_str())),
        };
        if resume
            && holistic_flag_exists(
                conn,
                project_id,
                &target_ref,
                &backend,
                &model_id,
                &prompt_version,
            )?
        {
            stats.skipped_resume += 1;
            continue;
        }
        let file_flags = load_file_flag_summaries(conn, project_id, repo_focus)?;
        pending.push((target_ref, repo_focus.map(str::to_string), file_flags));
    }

    if pending.is_empty() {
        return Ok(stats);
    }

    QualityBackend::ensure_available(ql)?;
    let client = QualityBackend::from_config(ql)?;
    let system = format!("{}\n{}", rubric.body, HOLISTIC_RUBRIC_APPENDIX);
    let team_size = project_team_size(conn, project_id)?;

    for (target_ref, repo_focus, file_flags) in pending {
        let repo_focus_ref = repo_focus.as_deref();
        let user_prompt = format_holistic_context(
            project_name,
            project_id,
            team_size,
            &repos,
            repo_focus_ref,
            &file_flags,
        );
        match judge_holistic(
            &client,
            &system,
            project_id,
            &target_ref,
            &user_prompt,
            &backend,
            &model_id,
            &prompt_version,
        ) {
            Ok(rows) => {
                stats.judged += 1;
                let n = rows.len();
                for row in rows {
                    insert_flag(conn, &row)?;
                    stats.flags_written += 1;
                }
                info!(
                    project_id,
                    target = %target_ref,
                    flags = n,
                    "quality-flags holistic pass complete"
                );
            }
            Err(e) => {
                stats.failures += 1;
                warn!(
                    project_id,
                    target = %target_ref,
                    error = %e,
                    "quality-flags holistic pass failed"
                );
            }
        }
    }

    Ok(stats)
}

fn holistic_targets(repos: &[String], max_holistic: usize) -> Vec<HolisticTarget> {
    if max_holistic == 0 {
        return Vec::new();
    }
    if max_holistic == 1 || repos.len() <= 1 {
        return vec![HolisticTarget::TeamWide];
    }
    let mut out: Vec<HolisticTarget> = repos
        .iter()
        .take(max_holistic)
        .map(|r| HolisticTarget::Repo(r.clone()))
        .collect();
    if out.is_empty() {
        out.push(HolisticTarget::TeamWide);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn judge_holistic(
    client: &QualityBackend,
    system: &str,
    project_id: i64,
    target_ref: &str,
    user_prompt: &str,
    backend: &str,
    model_id: &str,
    prompt_version: &str,
) -> Result<Vec<LlmQualityFlagRow>> {
    let raw = client.complete_rubric(system, user_prompt)?;
    let parsed = parse_quality_flags_json(&raw)?;
    let generated_at = Utc::now().to_rfc3339();
    Ok(parsed
        .into_iter()
        .map(|f| LlmQualityFlagRow {
            project_id,
            student_id: f.student_id,
            sprint_id: None,
            scope: "project".to_string(),
            target_ref: Some(target_ref.to_string()),
            category: f.category,
            severity: f.severity,
            summary: f.summary,
            detail: f.detail,
            backend: backend.to_string(),
            model_id: model_id.to_string(),
            prompt_version: prompt_version.to_string(),
            generated_at: generated_at.clone(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_one_is_team_wide_even_with_two_repos() {
        let repos = vec!["org/a".into(), "org/b".into()];
        let t = holistic_targets(&repos, 1);
        assert_eq!(t.len(), 1);
        assert!(matches!(t[0], HolisticTarget::TeamWide));
    }

    #[test]
    fn cap_two_yields_per_repo() {
        let repos = vec!["org/a".into(), "org/b".into()];
        let t = holistic_targets(&repos, 2);
        assert_eq!(t.len(), 2);
        assert!(matches!(&t[0], HolisticTarget::Repo(r) if r == "org/a"));
    }

    #[test]
    fn cap_zero_yields_none() {
        assert!(holistic_targets(&["org/a".into()], 0).is_empty());
    }
}
