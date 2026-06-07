//! Track B: feedback-only LLM quality flags (advisory; never grade inputs).

mod backend;
mod context;
mod file_pass;
mod flag;
mod holistic_pass;
mod parse;
mod persist;
mod prefilter;
mod repo_path;
mod rubric;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::params;
use sprint_grader_core::{Config, Database};
use tracing::info;

pub use flag::LlmQualityFlagRow;
pub use persist::{
    delete_project_flags, file_flag_exists, holistic_flag_exists, insert_flag, list_all_flags,
    list_flagged_project_ids, list_flags_for_projects, persist_project_flags,
};
pub use context::{list_project_repos, load_file_flag_summaries};
pub use file_pass::{run_file_pass, FilePassStats};
pub use holistic_pass::{run_holistic_pass, HolisticPassStats};
pub use parse::{parse_quality_flags_json, ParsedFlag};
pub use prefilter::{list_file_candidates, FileCandidate};
pub use rubric::{load_rubric, QualityRubric};

#[derive(Debug, Clone, Default)]
pub struct QualityFlagsOpts {
    pub project_filter: Option<Vec<String>>,
    pub max_holistic: Option<usize>,
    pub resume: bool,
    pub today: String,
    pub entregues_dir: PathBuf,
}

/// Run the quality-flags pipeline (Track B). PA wires config + prefilter; PB
/// adds the LLM file pass.
///
/// Incremental: `--projects` scopes which teams are (re)processed; other
/// projects' `llm_quality_flag` rows are left untouched. `grading-sheet`
/// exports every flag in the DB on the `LLM_Flags` sheet.
pub fn run(db: &Database, cfg_dir: &Path, opts: &QualityFlagsOpts) -> Result<()> {
    let course = Config::load(cfg_dir).context("load course.toml for quality-flags")?;
    course.quality_llm.validate_for_run()?;
    let rubric = load_rubric(cfg_dir, &course.quality_llm)?;
    let holistic_cap = opts
        .max_holistic
        .unwrap_or(course.quality_llm.max_holistic);

    let project_ids = resolve_project_ids(db, opts.project_filter.as_deref())?;
    if project_ids.is_empty() {
        bail!("no projects matched quality-flags filter");
    }

    let mut total_file_judged = 0usize;
    let mut total_holistic_judged = 0usize;
    let mut total_flags = 0usize;

    for &project_id in &project_ids {
        let project_name: String = db.conn.query_row(
            "SELECT name FROM projects WHERE id = ?",
            params![project_id],
            |r| r.get(0),
        )?;
        let candidates = list_file_candidates(&db.conn, project_id, &course.quality_llm)?;
        info!(
            project_id,
            project = %project_name,
            files = candidates.len(),
            rubric = %rubric.path,
            resume = opts.resume,
            "quality-flags prefilter"
        );
        if !opts.resume {
            delete_project_flags(&db.conn, project_id)?;
        }
        let stats = file_pass::run_file_pass(
            &db.conn,
            project_id,
            &project_name,
            &opts.entregues_dir,
            &course.quality_llm,
            &rubric,
            &candidates,
            opts.resume,
        )?;
        total_file_judged += stats.judged;
        total_flags += stats.flags_written;
        info!(
            project_id,
            judged = stats.judged,
            flags = stats.flags_written,
            skipped_resume = stats.skipped_resume,
            skipped_missing = stats.skipped_missing,
            failures = stats.failures,
            "quality-flags file pass complete"
        );

        let hol_stats = holistic_pass::run_holistic_pass(
            &db.conn,
            project_id,
            &project_name,
            &course.quality_llm,
            &rubric,
            holistic_cap,
            opts.resume,
        )?;
        total_holistic_judged += hol_stats.judged;
        total_flags += hol_stats.flags_written;
        info!(
            project_id,
            judged = hol_stats.judged,
            flags = hol_stats.flags_written,
            skipped_resume = hol_stats.skipped_resume,
            failures = hol_stats.failures,
            max_holistic = holistic_cap,
            "quality-flags holistic pass complete"
        );
    }

    info!(
        projects = project_ids.len(),
        file_judged = total_file_judged,
        holistic_judged = total_holistic_judged,
        flags = total_flags,
        "quality-flags complete"
    );
    Ok(())
}

fn resolve_project_ids(db: &Database, filter: Option<&[String]>) -> Result<Vec<i64>> {
    match filter {
        None | Some([]) => {
            let mut stmt = db.conn.prepare("SELECT id FROM projects ORDER BY id")?;
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        }
        Some(names) => {
            let mut out = Vec::new();
            for name in names {
                let id: i64 = db.conn.query_row(
                    "SELECT id FROM projects WHERE name = ? OR slug = ?",
                    rusqlite::params![name, name],
                    |r| r.get(0),
                )
                .with_context(|| format!("project not found in grading.db: {name}"))?;
                out.push(id);
            }
            Ok(out)
        }
    }
}
