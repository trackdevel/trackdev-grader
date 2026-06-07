//! Track B: feedback-only LLM quality flags (advisory; never grade inputs).
//!
//! Wave 5 wires the CLI; implementation lands in Track B (plan PA–PE).

use std::path::Path;

use anyhow::{bail, Result};
use sprint_grader_core::Database;

#[derive(Debug, Clone, Default)]
pub struct QualityFlagsOpts {
    pub project_filter: Option<Vec<String>>,
    pub max_holistic: Option<usize>,
    pub resume: bool,
    pub today: String,
}

pub fn run(_db: &Database, _cfg_dir: &Path, _opts: &QualityFlagsOpts) -> Result<()> {
    bail!(
        "quality-flags is not implemented yet (Track B — see \
         plans/total_grading/claude-refine-plan-v2.md §Track B)"
    )
}
