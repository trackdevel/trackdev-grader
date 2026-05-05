//! Stage 5 (quality) — method-level AST metrics + SATD + sprint-over-sprint deltas.

pub mod complexity;
pub mod halstead;
pub mod i18n;
pub mod quality_delta;
pub mod satd;
pub mod testability;

pub use complexity::{analyze_file, analyze_method, MethodMetrics};
pub use halstead::{compute_halstead, maintainability_index, HalsteadMetrics};
pub use quality_delta::compute_all_quality;
pub use satd::{compute_satd_for_repo, satd_delta, scan_comments};
