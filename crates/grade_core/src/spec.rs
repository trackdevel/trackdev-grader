//! Grading spec (`grading.standard.json`) and grade output types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::formula::{Expr, Node};
use crate::types::{AggregateKnobs, AiMaps};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeSpec {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub weights: BTreeMap<String, f64>,
    /// Optional per-metric absolute anchors; missing keys fall back to legacy
    /// weight names (`doc_max`, `mi_floor`, …).
    #[serde(default)]
    pub anchors: BTreeMap<String, crate::anchor::MetricAnchor>,
    #[serde(default)]
    pub models: BTreeMap<String, f64>,
    #[serde(default)]
    pub levels: BTreeMap<String, f64>,
    #[serde(default)]
    pub formulas: Formulas,
    /// Professor-entered per-project inputs: global definitions plus
    /// per-project value overrides. Empty by default; absent in older specs.
    #[serde(default)]
    pub manual_fields: ManualFields,
    /// Named global constants injected into every formula scope (task, project,
    /// student). Empty by default; absent in older specs.
    #[serde(default)]
    pub constants: Vec<ConstantDef>,
}

/// A named global constant usable in any formula. `name` is the formula
/// identifier, `value` the number substituted, `description` a human label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstantDef {
    pub name: String,
    #[serde(default)]
    pub value: f64,
    #[serde(default)]
    pub description: String,
}

/// Manual per-project fields: shared definitions + per-project value overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ManualFields {
    #[serde(default)]
    pub defs: Vec<ManualFieldDef>,
    /// Keyed by `project_id` (as a string) → field name → value. A missing
    /// entry falls back to the field's default `value`.
    #[serde(default)]
    pub values: BTreeMap<String, BTreeMap<String, f64>>,
}

/// A single manual-field definition. `name` is the formula identifier,
/// `value` is the default applied when a project has no override, and
/// `description` is the human-facing label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualFieldDef {
    pub name: String,
    #[serde(default)]
    pub value: f64,
    #[serde(default)]
    pub description: String,
}

/// Phase 2 alias — the structural slice is a prefix of the full spec JSON.
pub type StructuralSpec = GradeSpec;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default = "default_decimals")]
    pub decimals: u32,
    #[serde(default)]
    pub quantize_final: f64,
    #[serde(default = "default_penalty_mode")]
    pub penalty_mode: String,
    #[serde(default = "default_final_outputs")]
    pub final_outputs: Vec<String>,
}

fn default_decimals() -> u32 {
    2
}
fn default_penalty_mode() -> String {
    "subtractive".to_string()
}
fn default_final_outputs() -> Vec<String> {
    vec!["project_final".into(), "student_final".into()]
}

impl Default for Meta {
    fn default() -> Self {
        Self {
            decimals: default_decimals(),
            quantize_final: 0.0,
            penalty_mode: default_penalty_mode(),
            final_outputs: default_final_outputs(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Formulas {
    #[serde(default)]
    pub task: Vec<FormulaDef>,
    #[serde(default)]
    pub project: Vec<FormulaDef>,
    #[serde(default)]
    pub student: Vec<FormulaDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaDef {
    pub name: String,
    pub infix: String,
    pub expr: Expr,
}

impl GradeSpec {
    pub fn ai_maps(&self) -> AiMaps {
        AiMaps {
            models: self.models.clone(),
            levels: self.levels.clone(),
            undeclared_model_m: self
                .weights
                .get("undeclared_model_m")
                .copied()
                .unwrap_or(1.0),
            undeclared_level_l: self
                .weights
                .get("undeclared_level_l")
                .copied()
                .unwrap_or(0.5),
        }
    }

    pub fn aggregate_knobs(&self) -> AggregateKnobs {
        AggregateKnobs {
            penalty_mode: self.meta.penalty_mode.clone(),
        }
    }

    /// Constant `name → value` map for injection into every formula scope.
    pub fn constant_values(&self) -> BTreeMap<String, f64> {
        self.constants
            .iter()
            .map(|c| (c.name.clone(), c.value))
            .collect()
    }

    /// Resolve manual-field `name → value` for one project: each defined
    /// field takes the project's override if present, else its default.
    /// Returns an empty map when no fields are defined.
    pub fn manual_field_values(&self, project_id: i64) -> BTreeMap<String, f64> {
        let overrides = self.manual_fields.values.get(&project_id.to_string());
        self.manual_fields
            .defs
            .iter()
            .map(|d| {
                let value = overrides
                    .and_then(|m| m.get(&d.name))
                    .copied()
                    .unwrap_or(d.value);
                (d.name.clone(), value)
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeOutput {
    pub grades: ProjectGrades,
    pub trees: GradeTrees,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectGrades {
    pub project_id: i64,
    pub quality_grade: f64,
    pub quality_penalized: f64,
    pub project_penalty: f64,
    pub ai_factor: f64,
    pub project_final: f64,
    /// Collective code-quality penalty subtracted from `project_final`: the 20%
    /// team share of every architecture/complexity/static-analysis finding,
    /// capped at `qpen_team_cap`. The 80% author share lands on each student's
    /// `codequality_penalty`. See `plans/quality_penalty_8020/PLAN.md`.
    #[serde(default)]
    pub team_quality_penalty: f64,
    pub team_size: i64,
    pub axes: Vec<AxisGrade>,
    /// Size/complexity `work_base` before `extra_tech` is merged in (diagnostics).
    #[serde(default)]
    pub work_base_structural: f64,
    /// EXTRA_TECH aggregate: weighted "extra technologies vs. baseline" units.
    /// Folded into `work_base` before project formulas run (`work_base_structural
    /// × work_scale` is already applied in the axis score).
    #[serde(default)]
    pub extra_tech: f64,
    /// Per-signal breakdown of `extra_tech` (only signals with raw > 0).
    #[serde(default)]
    pub extra_tech_components: Vec<ExtraTechComponent>,
    pub students: Vec<StudentGrades>,
}

/// One contribution to a project's `extra_tech` aggregate: the signal key, the
/// raw value summed across the project's repos, the spec weight applied, and the
/// resulting points (`contribution`). The sum of `contribution` is `extra_tech`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraTechComponent {
    pub key: String,
    pub raw: f64,
    pub weight: f64,
    pub contribution: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AxisGrade {
    pub key: String,
    pub raw: Option<f64>,
    pub score: Option<f64>,
    pub present: bool,
}

/// One negative contribution to a student's code-quality penalty: which signal
/// fired, the per-student blame, and the penalty points it adds. The capped sum
/// of `points` over a student's components equals `codequality_penalty`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeQualityComponent {
    /// Signal: "architecture", "complexity", or "static_analysis".
    pub dimension: String,
    /// Raw per-student blame magnitude summed from that signal's hotspot flags.
    pub blame: f64,
    /// Blame per effective point — the quantity ranked across the cohort.
    pub blame_per_point: f64,
    /// Cohort band the student landed in: "critical" or "warning".
    pub tier: String,
    /// Penalty points this signal contributes (before the overall cap).
    pub points: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudentGrades {
    pub student_id: String,
    pub raw_points: f64,
    pub effective_points: f64,
    pub ai_keep: Option<f64>,
    pub contribution: Option<f64>,
    pub base_grade: f64,
    pub student_penalty: f64,
    #[serde(default)]
    pub codequality_penalty: f64,
    /// Per-signal breakdown of `codequality_penalty` (empty when no penalty).
    #[serde(default)]
    pub codequality_components: Vec<CodeQualityComponent>,
    /// Number of the student's gradable (DONE, point-bearing) tasks whose "Ús de
    /// IA" attribute is set on neither the task nor its parent USER_STORY,
    /// excluding AI-exempt early sprints (1–2). Equals the count of `RawTask`
    /// with `!declared && !ai_exempt`; the same set the `MISSING_AI_DECLARATION`
    /// flag reports. Informational — never a grade input.
    #[serde(default)]
    pub ai_undeclared_count: i64,
    pub student_final: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeTrees {
    pub project: Vec<NamedNode>,
    pub students: Vec<StudentTree>,
    pub tasks: Vec<TaskTree>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedNode {
    pub name: String,
    pub node: Node,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudentTree {
    pub student_id: String,
    pub formulas: Vec<NamedNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskTree {
    pub assignee_id: String,
    pub raw_points: f64,
    pub keep: f64,
    pub node: Node,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuralOutput {
    pub scopes: crate::types::ProjectScopes,
}

pub type StructuralMeta = Meta;
