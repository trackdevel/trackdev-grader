//! Vendor-neutral LLM client interface.
//!
//! Both `AnthropicClient` and `DeepseekClient` implement [`LlmClient`] so the
//! per-sprint evaluation drivers in `llm_eval.rs` and the `Conversation`
//! accumulator can dispatch the slow API call uniformly. The trait method
//! returns a single `LlmError` enum so callers don't need to know which
//! backend they're holding.
//!
//! Caller-visible determinism: implementations MUST send `temperature = 0`.
//! The architecture-LLM cache key only includes `(file_sha, rubric_key,
//! model_id)`; without temperature pinning, repeat calls on a cache miss
//! could write divergent rows.

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("API key is empty")]
    EmptyKey,
    #[error("HTTP error: {status} {body}")]
    Http { status: u16, body: String },
    #[error("request failed after {retries} retries: {source}")]
    RequestFailed {
        retries: u32,
        #[source]
        source: reqwest::Error,
    },
    #[error(
        "body parse failed after {retries} retries: status={status} content_type={content_type} preview={preview}: {error}"
    )]
    BodyParseFailed {
        retries: u32,
        status: u16,
        content_type: String,
        preview: String,
        error: String,
    },
    #[error("response missing text content: {raw}")]
    NoContent { raw: String },
    #[error(
        "model output truncated by max_tokens cap (finish_reason={finish_reason}); raise [architecture] / [evaluate] max_tokens or set thinking=\"disabled\""
    )]
    Truncated { finish_reason: String },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// One blocking call returning the assistant's text reply.
///
/// `messages` is in OpenAI / Anthropic conversational shape:
/// `[{"role": "user" | "assistant", "content": "..."}, ...]`. The system
/// prompt is passed separately because Anthropic carries it in a top-level
/// `system` field and the DeepSeek client folds it into a leading
/// `{"role": "system"}` message.
pub trait LlmClient: Send + Sync {
    fn complete(
        &self,
        system: &str,
        messages: &[Value],
        max_tokens: u32,
    ) -> Result<String, LlmError>;

    fn complete_default(&self, system: &str, messages: &[Value]) -> Result<String, LlmError> {
        self.complete(system, messages, 1024)
    }
}
