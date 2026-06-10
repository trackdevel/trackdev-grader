//! Plain input/output structs for the grading engine (serde-serializable, no I/O).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Per-repo structural inventory metrics (`repo_structural_metrics`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RepoMetrics {
    pub repo_full_name: String,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawProject {
    pub project_id: i64,
    pub name: String,
    pub team_size: i64,
    pub axis: AxisInputs,
    #[serde(default)]
    pub inventory: Vec<RepoMetrics>,
    pub tasks: Vec<RawTask>,
    pub students: Vec<RawStudent>,
    pub crit_findings: Vec<CritFinding>,
    pub student_flags: Vec<StudentFlag>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AxisInputs {
    pub documentation_raw: f64,
    pub doc_present: bool,
    pub code_quality_raw: f64,
    pub cc_pct: f64,
    pub mutation_score: f64,
    pub cq_present: bool,
    pub survival_raw: f64,
    pub surv_present: bool,
    pub arch_crit_count: f64,
    pub arch_warn_count: f64,
    pub arch_present: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawTask {
    pub assignee_id: String,
    pub raw_points: f64,
    pub ai_model: Option<String>,
    pub ai_level: Option<String>,
    pub declared: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawStudent {
    pub student_id: String,
    pub full_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    StaticAnalysis,
    Complexity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CritFinding {
    pub kind: FindingKind,
    pub category: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudentFlag {
    pub student_id: String,
    pub severity: String,
    pub source: String,
}

/// Per-task resolved scalars before the keep formula runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskScope {
    pub assignee_id: String,
    pub raw_points: f64,
    pub model_m: f64,
    pub level_l: f64,
    /// True only when declared AND both model and level strings are present.
    pub declared: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudentScope {
    pub student_id: String,
    pub student_eff: f64,
    pub ai_keep: Option<f64>,
    pub contribution: Option<f64>,
    pub student_critical_count: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectScopes {
    pub sum_raw: f64,
    pub sum_eff: f64,
    pub mean_raw: f64,
    pub ai_factor: f64,
    pub crit_sa_count: f64,
    pub crit_security_count: f64,
    pub crit_cx_count: f64,
    pub penalty_on: f64,
    pub students: Vec<StudentScope>,
}

/// AI model/level maps plus undeclared fallbacks (from the grading spec weights).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiMaps {
    pub models: BTreeMap<String, f64>,
    pub levels: BTreeMap<String, f64>,
    pub undeclared_model_m: f64,
    pub undeclared_level_l: f64,
}

/// Structural knobs needed by `aggregate` (subset of the full spec).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregateKnobs {
    pub penalty_mode: String,
}
