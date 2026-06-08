//! `grading.html` emitter: a single, offline, SQL-queryable presentation of the
//! grade model computed by `sprint-grader-grading-xlsx`.
//!
//! Crate boundary (hard rule): this crate *presents* — it calls
//! `grading_xlsx`'s public functions and never re-queries the raw schema for
//! grading inputs, re-derives scores, or re-implements penalties in Rust. New
//! Rust is limited to: snapshot construction from `WorkbookData`, HTML
//! assembly, and CLI wiring.

mod render;
mod snapshot;

pub use render::render_html;
pub use snapshot::build_snapshot_bytes;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sprint_grader_core::Database;
use sprint_grader_grading_xlsx::{grade_persist_and_load, GradingConfig};

/// Options for a `grading-html` run. Mirrors the workbook-producing path of
/// `grading_xlsx::RunOpts` (no `--import-weights`; run `grading-sheet
/// --import-weights` first if knobs were edited in the XLSX).
#[derive(Debug, Clone, Default)]
pub struct HtmlOpts {
    pub project_filter: Option<Vec<String>>,
    pub out: Option<PathBuf>,
    pub today: String,
    /// Rebuild from all graded projects without a new `--projects` grade pass.
    pub workbook_only: bool,
}

/// Grade + persist the (filtered) projects via `grading_xlsx` (the SAME path as
/// `grading-sheet`, so persisted grade rows are identical), build the embedded
/// snapshot, render the single-file HTML, and write it to `out`. Returns the
/// path written.
pub fn run_html(db: &Database, cfg_dir: &Path, opts: &HtmlOpts) -> anyhow::Result<PathBuf> {
    let cfg = GradingConfig::load(cfg_dir)?;
    let data = grade_persist_and_load(
        db,
        &cfg,
        &opts.today,
        opts.project_filter.as_deref(),
        opts.workbook_only,
    )?;
    let snapshot = build_snapshot_bytes(&data, &cfg)?;
    let html = render_html(&snapshot, &cfg)?;
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("grading.html"));
    std::fs::write(&out, html).with_context(|| format!("write {}", out.display()))?;
    Ok(out)
}
