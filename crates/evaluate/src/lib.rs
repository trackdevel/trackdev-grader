//! Stage 4 — PR documentation quality evaluation.
//!
//! * `heuristics` — deterministic EMPTY_DESCRIPTION / GENERIC_TITLE flag writers
//!   (mirror of `src/evaluate/heuristics.py`).
//! * `llm_client` — thin blocking Anthropic Messages API client with
//!   prompt-cache support via `cache_control`.
//! * `llm_eval` — team-level evaluation driver. Uses the LLM when
//!   `ANTHROPIC_API_KEY` is set; falls back to deterministic heuristic scoring
//!   otherwise (mirror of `src/evaluate/llm_eval.py`).

pub mod heuristics;
pub mod llm_client;
pub mod llm_eval;

pub use heuristics::run_heuristics_for_sprint_id;
pub use llm_client::{AnthropicClient, AnthropicError, ModelId};
pub use llm_eval::{run_llm_evaluation_for_sprint_id, score_task_descriptions_for_sprint_id};
