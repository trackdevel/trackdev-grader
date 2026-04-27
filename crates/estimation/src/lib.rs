//! Per-student estimation-bias estimation (T-P2.1).
//!
//! This crate fits a Bayesian per-student bias `β_u` and per-task
//! difficulty `δ_i` against story-point estimates and persists the
//! per-student posterior to `student_estimation_bias`. See [`em`] for
//! the model and [`persist`] for the DB read/write layer.

pub mod em;
pub mod persist;

pub use em::{fit, FitResult, Observation, StudentBias};
pub use persist::{
    fit_and_persist_for_all_projects, fit_and_persist_for_project, fit_and_persist_for_projects,
};
