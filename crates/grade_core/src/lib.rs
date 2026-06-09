//! Pure grading engine: structural shaping and formula evaluation.
//!
//! No I/O, no SQLite — serde-only deps so the crate targets native and WASM.

mod formula;
mod grade;
mod modulation;
mod shape;
mod spec;
mod types;

pub use formula::{eval, free_vars, EvalError, Expr, Node, Scope};
pub use grade::{grade, round_grade};
pub use modulation::keep;
pub use shape::{aggregate, resolve_tasks, structural_scopes};
pub use spec::{
    AxisGrade, Formulas, GradeOutput, GradeSpec, GradeTrees, Meta, NamedNode, ProjectGrades,
    StructuralMeta, StructuralOutput, StructuralSpec, StudentGrades,
};
pub use types::{
    AggregateKnobs, AiMaps, AxisInputs, CritFinding, FindingKind, ProjectScopes, RawProject,
    RawStudent, RawTask, StudentFlag, StudentScope, TaskScope,
};
