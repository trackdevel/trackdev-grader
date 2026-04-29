//! Stage 3 — per-student metrics, team inequality, weighted contribution,
//! longitudinal trajectory, and flag detection.

pub mod contribution;
pub mod flags;
pub mod inequality;
pub mod metrics;
pub mod pr_weight;
pub mod trajectory;

pub use contribution::{compute_all_contributions, ContributionWeights};
pub use flags::{detect_flags_for_sprint_id, redetect_compile_flags_for_sprint_id, Flag};
pub use inequality::compute_all_inequality;
pub use metrics::compute_metrics_for_sprint_id;
pub use pr_weight::{distribute_pr_weights_for_sprint, WeightedPRMetrics};
pub use trajectory::{compute_all_trajectories, compute_all_trajectories_filtered};
