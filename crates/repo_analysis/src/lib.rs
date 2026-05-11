//! Repo-level analysis: keyword-based stack/layer inference, task similarity
//! grouping, and PR-timing tier classification.
//! Mirrors `src/repo_analysis/` in the Python reference.

pub mod keywords;
pub mod layer_path;
pub mod ownership;
pub mod task_similarity;
pub mod temporal_analysis;

pub use keywords::{action_tag, is_fix_title, layer_tags, tokenize};
pub use layer_path::layer_for_path;
pub use ownership::{compute_team_ownership, file_ownership_for_project, FileOwnership};
pub use task_similarity::compute_task_similarity;
pub use temporal_analysis::{classify_pr_kind, classify_tier, compute_temporal_analysis};
