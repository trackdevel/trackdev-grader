//! `config/grading.toml` loader for the grading-sheet model.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradingConfig {
    #[serde(default = "default_weights_project")]
    pub weights_project: WeightsProject,
    #[serde(default)]
    pub ai_usage: AiUsageConfig,
    #[serde(default)]
    pub penalty: PenaltyConfig,
    #[serde(default)]
    pub gate: GateConfig,
    #[serde(default)]
    pub normalization: NormalizationConfig,
    #[serde(default)]
    pub output: OutputConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightsProject {
    pub documentation: f64,
    pub code_quality: f64,
    pub survival: f64,
    pub architecture: f64,
}

fn default_weights_project() -> WeightsProject {
    WeightsProject {
        documentation: 0.25,
        code_quality: 0.30,
        survival: 0.20,
        architecture: 0.25,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiUsageConfig {
    #[serde(default = "default_strength")]
    pub strength: f64,
    #[serde(default = "default_floor_keep")]
    pub floor_keep: f64,
    #[serde(default = "default_attribute_name")]
    pub attribute_name: String,
    #[serde(default = "default_undeclared_model_m")]
    pub undeclared_model_m: f64,
    #[serde(default = "default_undeclared_level_l")]
    pub undeclared_level_l: f64,
    #[serde(default)]
    pub models: BTreeMap<String, f64>,
    #[serde(default)]
    pub levels: BTreeMap<String, f64>,
}

fn default_strength() -> f64 {
    1.0
}
fn default_floor_keep() -> f64 {
    0.20
}
fn default_attribute_name() -> String {
    sprint_grader_core::DEFAULT_AI_ATTRIBUTE_NAME.to_string()
}
fn default_undeclared_model_m() -> f64 {
    1.0
}
fn default_undeclared_level_l() -> f64 {
    0.50
}

impl Default for AiUsageConfig {
    fn default() -> Self {
        Self {
            strength: default_strength(),
            floor_keep: default_floor_keep(),
            attribute_name: default_attribute_name(),
            undeclared_model_m: default_undeclared_model_m(),
            undeclared_level_l: default_undeclared_level_l(),
            models: default_model_map(),
            levels: default_level_map(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PenaltyConfig {
    #[serde(default = "default_penalty_mode")]
    pub mode: String,
    #[serde(default = "default_max_penalty")]
    pub max_penalty_points: f64,
    #[serde(default = "default_student_penalty_cap")]
    pub student_penalty_cap: f64,
    #[serde(default = "default_crit_sa")]
    pub crit_sa_points: f64,
    #[serde(default = "default_crit_cx")]
    pub crit_cx_points: f64,
    #[serde(default = "default_crit_flag")]
    pub crit_flag_points: f64,
    #[serde(default = "default_security_extra")]
    pub security_extra: f64,
}

fn default_penalty_mode() -> String {
    "subtractive".to_string()
}
fn default_max_penalty() -> f64 {
    2.0
}
fn default_student_penalty_cap() -> f64 {
    1.0
}
fn default_crit_sa() -> f64 {
    0.50
}
fn default_crit_cx() -> f64 {
    0.50
}
fn default_crit_flag() -> f64 {
    0.75
}
fn default_security_extra() -> f64 {
    0.50
}

impl Default for PenaltyConfig {
    fn default() -> Self {
        Self {
            mode: default_penalty_mode(),
            max_penalty_points: default_max_penalty(),
            student_penalty_cap: default_student_penalty_cap(),
            crit_sa_points: default_crit_sa(),
            crit_cx_points: default_crit_cx(),
            crit_flag_points: default_crit_flag(),
            security_extra: default_security_extra(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateConfig {
    #[serde(default = "default_plagiarism_flag")]
    pub plagiarism_flag: String,
    #[serde(default = "default_ai_detect_risk")]
    pub ai_detect_risk_level: String,
    #[serde(default = "default_ai_detect_low_levels")]
    pub ai_detect_low_levels: Vec<String>,
    #[serde(default)]
    pub ai_mismatch_auto_apply_worstcase: bool,
}

fn default_plagiarism_flag() -> String {
    "CROSS_TEAM_SIMILARITY".to_string()
}
fn default_ai_detect_risk() -> String {
    "HIGH".to_string()
}
fn default_ai_detect_low_levels() -> Vec<String> {
    vec!["A".to_string(), "B".to_string()]
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            plagiarism_flag: default_plagiarism_flag(),
            ai_detect_risk_level: default_ai_detect_risk(),
            ai_detect_low_levels: default_ai_detect_low_levels(),
            ai_mismatch_auto_apply_worstcase: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationConfig {
    #[serde(default = "default_doc_max")]
    pub doc_max: f64,
    #[serde(default = "default_mi_floor")]
    pub mi_floor: f64,
    #[serde(default = "default_mi_ceiling")]
    pub mi_ceiling: f64,
    #[serde(default = "default_cc_penalty")]
    pub cc_penalty: f64,
    #[serde(default = "default_test_bonus")]
    pub test_bonus: f64,
    #[serde(default = "default_test_cap")]
    pub test_cap: f64,
    #[serde(default = "default_surv_floor")]
    pub surv_floor: f64,
    #[serde(default = "default_surv_ceiling")]
    pub surv_ceiling: f64,
    #[serde(default = "default_k_crit")]
    pub k_crit: f64,
    #[serde(default = "default_k_warn")]
    pub k_warn: f64,
    #[serde(default = "default_arch_norm")]
    pub arch_norm: f64,
}

fn default_doc_max() -> f64 {
    6.0
}
fn default_mi_floor() -> f64 {
    50.0
}
fn default_mi_ceiling() -> f64 {
    85.0
}
fn default_cc_penalty() -> f64 {
    2.0
}
fn default_test_bonus() -> f64 {
    1.0
}
fn default_test_cap() -> f64 {
    0.5
}
fn default_surv_floor() -> f64 {
    0.50
}
fn default_surv_ceiling() -> f64 {
    0.95
}
fn default_k_crit() -> f64 {
    2.0
}
fn default_k_warn() -> f64 {
    0.5
}
fn default_arch_norm() -> f64 {
    4.0
}

impl Default for NormalizationConfig {
    fn default() -> Self {
        Self {
            doc_max: default_doc_max(),
            mi_floor: default_mi_floor(),
            mi_ceiling: default_mi_ceiling(),
            cc_penalty: default_cc_penalty(),
            test_bonus: default_test_bonus(),
            test_cap: default_test_cap(),
            surv_floor: default_surv_floor(),
            surv_ceiling: default_surv_ceiling(),
            k_crit: default_k_crit(),
            k_warn: default_k_warn(),
            arch_norm: default_arch_norm(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Grid snap for displayed finals (`MROUND` in the workbook when > 0).
    /// Operator policy: keep at **0.0** — scoring stays continuous; only
    /// `decimals` rounding applies.
    #[serde(default)]
    pub quantize_final: f64,
    #[serde(default = "default_decimals")]
    pub decimals: u32,
}

fn default_decimals() -> u32 {
    2
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            quantize_final: 0.0,
            decimals: default_decimals(),
        }
    }
}

impl Default for GradingConfig {
    fn default() -> Self {
        Self {
            weights_project: default_weights_project(),
            ai_usage: AiUsageConfig::default(),
            penalty: PenaltyConfig::default(),
            gate: GateConfig::default(),
            normalization: NormalizationConfig::default(),
            output: OutputConfig::default(),
        }
    }
}

impl GradingConfig {
    pub fn load(cfg_dir: &Path) -> Result<Self> {
        let path = cfg_dir.join("grading.toml");
        if path.is_file() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("read grading config {}", path.display()))?;
            let cfg: GradingConfig = toml::from_str(&raw)
                .with_context(|| format!("parse grading config {}", path.display()))?;
            Ok(cfg)
        } else {
            Ok(Self::default())
        }
    }

    /// SHA-256 of the canonically re-serialized TOML (stable weight fingerprint).
    pub fn weights_version(&self) -> String {
        let canonical = toml::to_string(self).unwrap_or_default();
        let digest = Sha256::digest(canonical.as_bytes());
        format!("{:x}", digest)
    }

    /// Write `config/grading.toml` (used by `--import-weights`).
    pub fn write_to_dir(&self, cfg_dir: &Path) -> Result<()> {
        let path = cfg_dir.join("grading.toml");
        let body = toml::to_string_pretty(self)
            .with_context(|| format!("serialize grading config for {}", path.display()))?;
        fs::write(&path, body)
            .with_context(|| format!("write grading config {}", path.display()))?;
        Ok(())
    }
}

fn default_model_map() -> BTreeMap<String, f64> {
    let mut m = BTreeMap::new();
    m.insert("Cap".to_string(), 0.0);
    m.insert("Copilot-Auto".to_string(), 0.70);
    m.insert("Cursor".to_string(), 0.90);
    m.insert("Kimi-2.6".to_string(), 0.85);
    m.insert("DeepSeek-v4".to_string(), 0.85);
    m.insert("Sonnet-4.6".to_string(), 0.90);
    m.insert("Gemini-3.1".to_string(), 1.0);
    m.insert("Opus-4.6-4.7".to_string(), 1.0);
    m.insert("GPT-5.5".to_string(), 1.0);
    m.insert("GPT-5.4".to_string(), 1.0);
    m.insert("GPT-5.3-codex".to_string(), 1.0);
    m.insert("GPT-5.2-codex".to_string(), 1.0);
    m
}

fn default_level_map() -> BTreeMap<String, f64> {
    let mut m = BTreeMap::new();
    m.insert("A".to_string(), 0.0);
    m.insert("B".to_string(), 0.25);
    m.insert("C".to_string(), 0.50);
    m.insert("D".to_string(), 0.75);
    m.insert("E".to_string(), 1.0);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_is_continuous_scoring() {
        assert_eq!(GradingConfig::default().output.quantize_final, 0.0);
    }

    #[test]
    fn default_config_round_trips_through_toml() {
        let cfg = GradingConfig::default();
        let s = toml::to_string(&cfg).unwrap();
        let back: GradingConfig = toml::from_str(&s).unwrap();
        assert!((back.weights_project.documentation - 0.25).abs() < 1e-9);
        assert!(!cfg.weights_version().is_empty());
    }
}
