//! Load `grade_core::RawProject` rows from a grading.db connection.

mod db_axis;
mod raw;

pub use db_axis::{
    architecture_counts, architecture_scan_present, code_quality_raw, documentation_raw,
    project_repos, survival_raw, AxisRaw,
};
pub use raw::{load_cohort_raw_projects, load_raw_project};
