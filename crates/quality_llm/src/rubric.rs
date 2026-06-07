//! Load `config/quality-llm-rubric.md`.

use std::path::Path;

use anyhow::{Context, Result};
use sprint_grader_core::QualityLlmConfig;

#[derive(Debug, Clone)]
pub struct QualityRubric {
    pub body: String,
    pub path: String,
}

pub fn load_rubric(cfg_dir: &Path, ql: &QualityLlmConfig) -> Result<QualityRubric> {
    let path = cfg_dir.join(&ql.rubric_path);
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("read quality LLM rubric {}", path.display()))?;
    Ok(QualityRubric {
        body,
        path: ql.rubric_path.clone(),
    })
}
