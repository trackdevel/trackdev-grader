//! AI detection pipeline: behavioral + stylometry + curriculum check +
//! text consistency + fusion. Mirrors `src/ai_detect/` minus the
//! GPU-perplexity and LLM-as-judge signals (dropped by the migration plan;
//! fusion weights redistributed across the surviving signals).

pub mod behavioral;
pub mod curriculum_check;
pub mod fusion;
pub mod stylometry;
pub mod text_consistency;

pub use behavioral::{compute_all_behavioral, compute_pr_behavioral};
pub use curriculum_check::{extract_file_concepts, scan_repo_curriculum};
pub use fusion::{
    attribute_to_students, bayesian_fuse, compute_all_ai_probability, fuse_all_signals,
    run_full_fusion, DEFAULT_FILE_WEIGHTS,
};
pub use stylometry::{
    analyze_repo_stylometry, build_student_baselines, compute_ai_style_score,
    extract_style_features, StyleFeatureVector,
};
pub use text_consistency::{
    build_text_profile, compute_all_text_consistency, compute_sprint_consistency,
};
