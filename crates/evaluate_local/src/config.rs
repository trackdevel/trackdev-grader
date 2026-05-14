//! Local-only helpers around `LocalEvaluateConfig`. The struct itself
//! lives in `sprint-grader-core` (re-exported here) so CLI and
//! orchestration can branch on `config.evaluate.judge` without importing
//! `sprint-grader-evaluate-local` for the type alone (Invariant C).

pub use sprint_grader_core::config::LocalEvaluateConfig;
