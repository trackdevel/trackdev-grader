//! Pure grading engine: structural shaping and formula evaluation.
//!
//! No I/O, no SQLite — serde-only deps so the crate targets native and WASM.

mod anchor;
mod axes;
mod cohort;
mod formula;
mod grade;
mod modulation;
mod shape;
mod spec;
mod types;

pub use anchor::MetricAnchor;
pub use axes::{collect_cohort_samples, compute_project_axes, normalize_project_all, ProjectAxisScores};
pub use cohort::{
    collect_raw_samples, compute_cohort_bounds, hybrid_normalize, normalize_project_metrics,
    percentile_linear, CohortBounds, CohortGradeOutput, CohortProjectGrade, MetricBounds,
};
pub use formula::{eval, free_vars, EvalError, Expr, Node, Scope};
pub use grade::{grade, grade_cohort, round_grade};
pub use modulation::keep;
pub use shape::{aggregate, resolve_tasks, structural_scopes};
pub use spec::{
    AxisGrade, Formulas, GradeOutput, GradeSpec, GradeTrees, ManualFieldDef, ManualFields, Meta,
    NamedNode, ProjectGrades, StructuralMeta, StructuralOutput, StructuralSpec, StudentGrades,
};
pub use types::{
    AggregateKnobs, AiMaps, AxisInputs, CritFinding, FindingKind, ProjectScopes, RawProject,
    RawStudent, RawTask, RepoMetrics, StudentFlag, StudentScope, TaskScope,
};
