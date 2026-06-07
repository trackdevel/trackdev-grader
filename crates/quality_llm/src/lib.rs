//! Track B: feedback-only LLM quality flags (advisory; never grade inputs).

mod flag;
mod persist;
mod prefilter;
mod rubric;

use std::path::Path;

use anyhow::{bail, Context, Result};
use sprint_grader_core::{Config, Database};
use tracing::info;

pub use flag::LlmQualityFlagRow;
pub use persist::{
    delete_project_flags, file_flag_exists, insert_flag, list_all_flags,
    list_flagged_project_ids, list_flags_for_projects, persist_project_flags,
};
pub use prefilter::{list_file_candidates, FileCandidate};
pub use rubric::{load_rubric, QualityRubric};

#[derive(Debug, Clone, Default)]
pub struct QualityFlagsOpts {
    pub project_filter: Option<Vec<String>>,
    pub max_holistic: Option<usize>,
    pub resume: bool,
    pub today: String,
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
    let _holistic_cap = opts
        .max_holistic
        .unwrap_or(course.quality_llm.max_holistic);

    let project_ids = resolve_project_ids(db, opts.project_filter.as_deref())?;
    if project_ids.is_empty() {
        bail!("no projects matched quality-flags filter");
    }

    for &project_id in &project_ids {
        let candidates =
            list_file_candidates(&db.conn, project_id, &course.quality_llm)?;
        info!(
            project_id,
            files = candidates.len(),
            rubric = %rubric.path,
            resume = opts.resume,
            "quality-flags prefilter"
        );
        if !opts.resume {
            delete_project_flags(&db.conn, project_id)?;
        }
    }

    bail!(
        "quality-flags LLM pass not implemented yet (Track B PB); \
         config, rubric, and prefilter are ready — {} project(s) scanned",
        project_ids.len()
    )
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
