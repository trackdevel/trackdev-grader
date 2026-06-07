//! Grading-sheet computation: project quality × AI-discounted contribution.

mod aggregate;
mod config;
mod data;
mod grade;
mod import_weights;
mod modulation;
mod normalize;
mod penalty;
mod persist;
mod weights_layout;
mod workbook;

pub use aggregate::{
    aggregate_team_points, enrolled_student_ids, enrolled_team_size, load_task_points,
    StudentPoints, TaskPoints, TeamPoints,
};
pub use config::{
    AiUsageConfig, GateConfig, GradingConfig, NormalizationConfig, OutputConfig, PenaltyConfig,
    WeightsProject,
};
pub use data::{load_workbook_data, load_workbook_data_with_results, WorkbookData};
pub use grade::{grade_project, ComponentScore, GradingResult, ProjectGradeRow, StudentGradeRow};
pub use import_weights::{import_weights, WEIGHTS_SHEET_NAME};
pub use modulation::{keep, keep_for_declared, keep_for_undeclared};
pub use normalize::{load_quality_axes, quality_composite, AxisScore};
pub use persist::{list_graded_project_ids, load_persisted_project, persist_project_grades};
pub use workbook::{write_workbook, write_workbook_buffer, DEFINED_NAMES};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sprint_grader_core::Database;

/// Options for the full grading-sheet run (Wave 5 wires the CLI).
#[derive(Debug, Clone, Default)]
pub struct RunOpts {
    pub project_filter: Option<Vec<String>>,
    pub out: Option<PathBuf>,
    pub import_weights: Option<PathBuf>,
    pub today: String,
    /// Rebuild the workbook from all graded projects without a new `--projects` pass.
    pub workbook_only: bool,
    /// Persist grades only; skip merged workbook export.
    pub no_workbook: bool,
}

/// Compute grades for all (or filtered) projects and persist to `grading.db`.
/// The workbook includes every project with a `project_final_grade` row; on export,
/// all of them are recomputed and re-persisted so DB and xlsx stay aligned.
pub fn run(db: &Database, cfg_dir: &Path, opts: &RunOpts) -> Result<PathBuf> {
    if let Some(xlsx) = &opts.import_weights {
        let imported = import_weights(xlsx)?;
        imported.write_to_dir(cfg_dir)?;
        return Ok(xlsx.clone());
    }

    let cfg = GradingConfig::load(cfg_dir)?;
    let today = &opts.today;
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("grading_sheet.xlsx"));

    let filter_ids = if opts.workbook_only {
        Vec::new()
    } else {
        resolve_project_ids(db, opts.project_filter.as_deref())?
    };

    let mut graded: HashMap<i64, GradingResult> = HashMap::new();
    for &project_id in &filter_ids {
        let result = grade_and_persist(db, project_id, today, &cfg)?;
        graded.insert(project_id, result);
    }

    if opts.no_workbook {
        return Ok(out);
    }

    let workbook_ids = list_graded_project_ids(&db.conn)?;
    if workbook_ids.is_empty() {
        bail!(
            "no graded projects in grading.db; run grading-sheet for at least one project first"
        );
    }

    let mut workbook_results = Vec::with_capacity(workbook_ids.len());
    for &project_id in &workbook_ids {
        let result = if let Some(r) = graded.get(&project_id) {
            r.clone()
        } else {
            grade_and_persist(db, project_id, today, &cfg)?
        };
        workbook_results.push(result);
    }

    let wb_data =
        load_workbook_data_with_results(db, &workbook_ids, today, &cfg, Some(&workbook_results))?;
    write_workbook(&wb_data, &cfg, &out)?;
    Ok(out)
}

fn grade_and_persist(
    db: &Database,
    project_id: i64,
    today: &str,
    cfg: &GradingConfig,
) -> Result<GradingResult> {
    let sprint_ids = db.sprint_ids_up_to_current(project_id, today)?;
    let name: String = db.conn.query_row(
        "SELECT name FROM projects WHERE id = ?",
        rusqlite::params![project_id],
        |r| r.get(0),
    )?;
    let result = grade_project(&db.conn, project_id, &name, &sprint_ids, cfg)?;
    persist_project_grades(&db.conn, &result)?;
    Ok(result)
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
