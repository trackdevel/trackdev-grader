//! Per-file LLM quality-flag pass (Track B PB/PD).

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use rayon::prelude::*;
use rusqlite::Connection;
use sprint_grader_core::QualityLlmConfig;
use tracing::{info, warn};

use crate::backend::QualityBackend;
use crate::flag::LlmQualityFlagRow;
use crate::parse::parse_quality_flags_json;
use crate::persist::{file_flag_exists, insert_flag};
use crate::prefilter::FileCandidate;
use crate::repo_path::local_file_path;
use crate::rubric::QualityRubric;

/// Max source bytes sent to the LLM per file (truncated with a notice).
const MAX_FILE_BYTES: usize = 120_000;

#[derive(Debug, Default, Clone, Copy)]
pub struct FilePassStats {
    pub judged: usize,
    pub skipped_resume: usize,
    pub skipped_missing: usize,
    pub skipped_empty: usize,
    pub flags_written: usize,
    pub failures: usize,
}

struct FileJob {
    candidate: FileCandidate,
    target_ref: String,
    source: String,
}

#[allow(clippy::too_many_arguments)]
pub fn run_file_pass(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    entregues_dir: &Path,
    ql: &QualityLlmConfig,
    rubric: &QualityRubric,
    candidates: &[FileCandidate],
    resume: bool,
) -> Result<FilePassStats> {
    let model_id = ql.resolved_model_id().to_string();
    let backend = ql.backend.clone();
    let prompt_version = ql.prompt_version.clone();

    let mut stats = FilePassStats::default();
    let mut jobs = Vec::new();

    for cand in candidates {
        let target_ref = format!("{}:{}", cand.repo_full_name, cand.file_path);
        if resume
            && file_flag_exists(
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

        let path = local_file_path(
            entregues_dir,
            project_name,
            &cand.repo_full_name,
            &cand.file_path,
        );
        if !path.is_file() {
            warn!(
                project_id,
                path = %path.display(),
                "quality-flags skipping missing file on disk"
            );
            stats.skipped_missing += 1;
            continue;
        }

        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        if bytes.is_empty() {
            stats.skipped_empty += 1;
            continue;
        }
        let truncated = bytes.len() > MAX_FILE_BYTES;
        let mut source =
            String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_FILE_BYTES)]).into_owned();
        if truncated {
            source.push_str("\n\n[... truncated for LLM context limit ...]");
        }

        jobs.push(FileJob {
            candidate: cand.clone(),
            target_ref,
            source,
        });
    }

    if jobs.is_empty() {
        return Ok(stats);
    }

    QualityBackend::ensure_available(ql)?;
    let client = QualityBackend::from_config(ql)?;
    let rubric_body = rubric.body.clone();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(QualityBackend::worker_count(ql))
        .build()
        .context("build quality-flags worker pool")?;
    let judged: Vec<(FileJob, Result<Vec<LlmQualityFlagRow>>)> = pool.install(|| {
        jobs.into_par_iter()
            .map(|job| {
                let rows = judge_one_file(
                    &client,
                    &rubric_body,
                    project_id,
                    &job,
                    &backend,
                    &model_id,
                    &prompt_version,
                );
                (job, rows)
            })
            .collect()
    });

    // SQLite writes stay on the caller thread.
    for (job, result) in judged {
        match result {
            Ok(rows) => {
                stats.judged += 1;
                for row in rows {
                    insert_flag(conn, &row)?;
                    stats.flags_written += 1;
                }
                info!(
                    project_id,
                    target = %job.target_ref,
                    "quality-flags file pass complete"
                );
            }
            Err(e) => {
                stats.failures += 1;
                warn!(
                    project_id,
                    target = %job.target_ref,
                    error = %e,
                    "quality-flags file pass failed"
                );
            }
        }
    }

    Ok(stats)
}

fn judge_one_file(
    client: &QualityBackend,
    rubric_body: &str,
    project_id: i64,
    job: &FileJob,
    backend: &str,
    model_id: &str,
    prompt_version: &str,
) -> Result<Vec<LlmQualityFlagRow>> {
    let user_prompt = format!(
        "Review this delivered Java file for instructor feedback only (no score).\n\n\
         Repo: {}\n\
         Path: {}\n\
         Fingerprinted statements: {}\n\n\
         --- FILE ---\n{}\n--- END FILE ---",
        job.candidate.repo_full_name,
        job.candidate.file_path,
        job.candidate.statement_count,
        job.source,
    );
    let raw = client.complete_rubric(rubric_body, &user_prompt)?;
    let parsed = parse_quality_flags_json(&raw)?;
    let generated_at = Utc::now().to_rfc3339();
    Ok(parsed
        .into_iter()
        .map(|f| LlmQualityFlagRow {
            project_id,
            student_id: f.student_id,
            sprint_id: None,
            scope: "file".to_string(),
            target_ref: Some(job.target_ref.clone()),
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
