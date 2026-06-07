//! Shared constants for the declared-AI-usage feature.
//!
//! The "Ús de IA" ENUM_PAIR ProfileAttribute is captured per task from the
//! TrackDev `/export/tasks` payload (in `collect`) and consumed by the
//! `grading_xlsx` grader. Both reference the same attribute-name literal
//! through this constant so a future instance rename touches one place.

/// Default display name of the TrackDev "Ús de IA" ENUM_PAIR ProfileAttribute
/// (slot 1 = model, slot 2 = level A–E). A TrackDev instance that renamed the
/// attribute overrides this via `CollectOpts.ai_attribute_name`; the grade
/// config's `[ai_usage].attribute_name` mirrors it on the grader side.
pub const DEFAULT_AI_ATTRIBUTE_NAME: &str = "Ús de IA";
