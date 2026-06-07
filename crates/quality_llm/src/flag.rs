//! Row shape for `llm_quality_flag`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmQualityFlagRow {
    pub project_id: i64,
    pub student_id: Option<String>,
    pub sprint_id: Option<i64>,
    pub scope: String,
    pub target_ref: Option<String>,
    pub category: String,
    pub severity: String,
    pub summary: String,
    pub detail: Option<String>,
    pub backend: String,
    pub model_id: String,
    pub prompt_version: String,
    pub generated_at: String,
}
