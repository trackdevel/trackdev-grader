//! Stage 4 — PR documentation quality evaluation.
//!
//! * `heuristics` — deterministic EMPTY_DESCRIPTION / GENERIC_TITLE flag writers
//!   (mirror of `src/evaluate/heuristics.py`).
//! * `llm_client` — thin blocking Anthropic Messages API client with
//!   prompt-cache support via `cache_control`.
//! * `llm_eval` — team-level evaluation driver. Uses the LLM when
//!   `ANTHROPIC_API_KEY` is set; falls back to deterministic heuristic scoring
//!   otherwise (mirror of `src/evaluate/llm_eval.py`).

pub mod claude_cli_client;
pub mod deepseek_client;
pub mod heuristics;
pub mod llm_client;
pub mod llm_eval;
pub mod llm_trait;

pub use claude_cli_client::{ClaudeCliClient, ClaudeCliError};
pub use deepseek_client::DeepseekClient;
pub use heuristics::{is_empty_description, is_generic_title, run_heuristics_for_sprint_id};
pub use llm_client::{AnthropicClient, AnthropicError, ModelId};
pub use llm_eval::{
    evaluate_prs_heuristic, extract_json_object, run_heuristics_for_all_sprint_ids,
    run_llm_evaluation_for_sprint_id, run_pr_doc_evaluation_for_sprint_id,
    score_task_descriptions_for_sprint_id, update_avg_doc_score_pub, RUBRIC_PR,
};
pub use llm_trait::{LlmClient, LlmError};
