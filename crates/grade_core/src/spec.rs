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
    #[serde(default)]
    pub models: BTreeMap<String, f64>,
    #[serde(default)]
    pub levels: BTreeMap<String, f64>,
    #[serde(default)]
    pub formulas: Formulas,
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
    pub team_size: i64,
    pub axes: Vec<AxisGrade>,
    pub students: Vec<StudentGrades>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AxisGrade {
    pub key: String,
    pub raw: Option<f64>,
    pub score: Option<f64>,
    pub present: bool,
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
