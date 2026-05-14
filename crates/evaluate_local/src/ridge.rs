//! Three-head ridge-regression predictor (title / description / total).
//!
//! Weights are produced by `tools/train_regressor/train.py` and live as
//! three sidecar JSONs in `[evaluate.local] regressor_dir`. The Rust
//! pipeline only does the dot product — training stays in Python so the
//! GPU stack (ollama + bge-m3) is the only heavy artifact in the
//! operator's environment (Invariant O).
//!
//! Dimension mismatch returns `NaN`, not a panic; triage routes NaN to
//! the LLM fallback path. The plan's `dim-mismatch` justification surfaces
//! the operator action item.

use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// One ridge head (title / description / total). Mirrors the JSON shape
/// emitted by `tools/train_regressor/train.py` so de/serialisation needs
/// no custom logic.
#[derive(Debug, Clone, Deserialize)]
pub struct RidgeHead {
    pub embedding_model: String,
    pub embedding_dim: usize,
    pub intercept: f64,
    pub coefficients: Vec<f64>,
    pub residual_stddev: f64,
    pub n_train: usize,
    pub trained_at: String,
}

impl RidgeHead {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read ridge head {}", path.display()))?;
        let head: RidgeHead = serde_json::from_str(&body)
            .with_context(|| format!("parse ridge head {}", path.display()))?;
        if head.coefficients.len() != head.embedding_dim {
            anyhow::bail!(
                "{}: coefficients length {} does not match embedding_dim {}",
                path.display(),
                head.coefficients.len(),
                head.embedding_dim
            );
        }
        Ok(head)
    }

    /// Returns `intercept + Σ coef[i]·embedding[i]`. Returns `NaN` when the
    /// embedding length differs from `embedding_dim` so the caller routes
    /// to the LLM fallback without panicking; do NOT pad/truncate.
    pub fn predict(&self, embedding: &[f32]) -> f64 {
        if embedding.len() != self.embedding_dim {
            return f64::NAN;
        }
        let mut acc = self.intercept;
        for (c, x) in self.coefficients.iter().zip(embedding.iter()) {
            acc += c * (*x as f64);
        }
        acc
    }
}

/// All three heads loaded as a single bundle. `load_optional` returns
/// `Ok(None)` when the directory or any of the three sidecar files is
/// missing — that's the regressor-disabled state, expected on a fresh
/// checkout where the trainer hasn't run yet.
#[derive(Debug, Clone)]
pub struct PrRidgeBundle {
    pub title: RidgeHead,
    pub description: RidgeHead,
    pub total: RidgeHead,
}

impl PrRidgeBundle {
    /// `Ok(None)` when `dir` does not exist or any of
    /// `pr_{title,description,total}.json` is missing. `Ok(Some(..))`
    /// when all three load cleanly. `Err` only on parse failure (i.e.
    /// the file exists but is corrupt or has wrong types).
    pub fn load_optional(dir: &Path) -> anyhow::Result<Option<Self>> {
        if !dir.exists() {
            return Ok(None);
        }
        let title_path = dir.join("pr_title.json");
        let description_path = dir.join("pr_description.json");
        let total_path = dir.join("pr_total.json");
        if !title_path.exists() || !description_path.exists() || !total_path.exists() {
            return Ok(None);
        }
        Ok(Some(Self {
            title: RidgeHead::load(&title_path)?,
            description: RidgeHead::load(&description_path)?,
            total: RidgeHead::load(&total_path)?,
        }))
    }
}
