//! Thin blocking Anthropic Messages API client.
//!
//! Designed to match how the Python Claude-Code session library is used in
//! `llm_eval.py`: a *stateful* session with a single system prompt and
//! successive `send(message)` → response pairs. That maps cleanly onto the
//! HTTP `messages` array; we append each user message + assistant reply and
//! post the full history every turn.
//!
//! Prompt caching is enabled via `cache_control: {"type": "ephemeral"}` on the
//! system block. Because the system prompt and the conversation prefix stay
//! stable as PRs stream through, subsequent requests within the 5-minute TTL
//! are billed at the cache-hit rate.

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::llm_trait::{LlmClient, LlmError};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 1024;
const MAX_RETRIES: u32 = 3;
const BACKOFF_BASE_SECS: u64 = 2;

/// Whichever Claude model id the caller wants. The pipeline default is
/// `claude-haiku-4-5-20251001` (set in `EvaluateConfig::default()`); any
/// string the API accepts works. Do NOT default to Opus — the per-PR
/// rubric is small and Opus burns Max quota disproportionately.
pub type ModelId = String;

#[derive(Debug, thiserror::Error)]
pub enum AnthropicError {
    #[error("ANTHROPIC_API_KEY is empty — cannot call Claude API")]
    EmptyKey,

    #[error("Anthropic API error: {status} {body}")]
    Http { status: u16, body: String },

    #[error("Anthropic API failed after {retries} retries: {source}")]
    RequestFailed {
        retries: u32,
        #[source]
        source: reqwest::Error,
    },

    #[error("Anthropic response missing text content: {raw}")]
    NoContent { raw: String },

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct AnthropicClient {
    client: Client,
    model: ModelId,
}

impl AnthropicClient {
    pub fn new(api_key: &str, model: ModelId) -> Result<Self, AnthropicError> {
        if api_key.is_empty() {
            return Err(AnthropicError::EmptyKey);
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(api_key).map_err(|_| AnthropicError::EmptyKey)?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        // Enable ephemeral prompt caching — always safe to send; the API
        // ignores it on models that don't support caching.
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("prompt-caching-2024-07-31"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client build");
        Ok(Self { client, model })
    }

    /// Send a single Messages API request and return the assistant's text.
    pub fn complete(
        &self,
        system: &str,
        messages: &[Value],
        max_tokens: u32,
    ) -> Result<String, AnthropicError> {
        // Wrap the system text in the structured-block form so we can attach
        // `cache_control`. The API accepts either `system: "..."` or a Vec of
        // text blocks; the Vec form is required for cache hints.
        let system_blocks = json!([{
            "type": "text",
            "text": system,
            "cache_control": {"type": "ephemeral"},
        }]);
        let body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "temperature": 0,
            "system": system_blocks,
            "messages": messages,
        });

        let mut last_err: Option<reqwest::Error> = None;
        for attempt in 0..MAX_RETRIES {
            let resp = self.client.post(API_URL).json(&body).send();
            match resp {
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        let json: Value = match r.json() {
                            Ok(v) => v,
                            Err(e) => {
                                last_err = Some(e);
                                continue;
                            }
                        };
                        return extract_text(&json);
                    }
                    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                        let text = r.text().unwrap_or_default();
                        let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                        warn!(%status, wait_s = wait, body = %text, "Anthropic retry");
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    }
                    let text = r.text().unwrap_or_default();
                    return Err(AnthropicError::Http {
                        status: status.as_u16(),
                        body: text,
                    });
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < MAX_RETRIES {
                        let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                        warn!(wait_s = wait, "Anthropic request error — retrying");
                        std::thread::sleep(Duration::from_secs(wait));
                    }
                }
            }
        }
        Err(AnthropicError::RequestFailed {
            retries: MAX_RETRIES,
            source: last_err.expect("loop always populates last_err"),
        })
    }

    pub fn complete_default(
        &self,
        system: &str,
        messages: &[Value],
    ) -> Result<String, AnthropicError> {
        self.complete(system, messages, DEFAULT_MAX_TOKENS)
    }
}

/// Extract the first `content[].type == "text"` block from a Messages response.
fn extract_text(json: &Value) -> Result<String, AnthropicError> {
    let content = json.get("content").and_then(Value::as_array);
    if let Some(blocks) = content {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    return Ok(t.to_string());
                }
            }
        }
    }
    Err(AnthropicError::NoContent {
        raw: json.to_string(),
    })
}

// ---- LlmClient trait impl ----

impl LlmClient for AnthropicClient {
    fn complete(
        &self,
        system: &str,
        messages: &[Value],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        AnthropicClient::complete(self, system, messages, max_tokens).map_err(Into::into)
    }
}

impl From<AnthropicError> for LlmError {
    fn from(e: AnthropicError) -> Self {
        match e {
            AnthropicError::EmptyKey => LlmError::EmptyKey,
            AnthropicError::Http { status, body } => LlmError::Http { status, body },
            AnthropicError::RequestFailed { retries, source } => {
                LlmError::RequestFailed { retries, source }
            }
            AnthropicError::NoContent { raw } => LlmError::NoContent { raw },
            AnthropicError::Json(e) => LlmError::Json(e),
        }
    }
}

// ---- Conversational helper ----

/// A stateful conversation — models the `session.send(msg) -> response` pattern
/// used by the Python session library. The system prompt is fixed at construction;
/// each `ask()` call appends a user message, posts the full history, and appends
/// the assistant response so subsequent turns have full context.
///
/// Generic over the concrete client via the [`LlmClient`] trait so a single
/// driver in `llm_eval.rs` covers Anthropic, DeepSeek, and any future
/// backend.
pub struct Conversation<'a> {
    client: &'a dyn LlmClient,
    system: String,
    pub messages: Vec<Value>,
}

impl<'a> Conversation<'a> {
    pub fn new(client: &'a dyn LlmClient, system: impl Into<String>) -> Self {
        Self {
            client,
            system: system.into(),
            messages: Vec::new(),
        }
    }

    pub fn ask(&mut self, user_msg: &str) -> Result<String, LlmError> {
        self.messages.push(json!({
            "role": "user",
            "content": user_msg,
        }));
        let reply = self.client.complete_default(&self.system, &self.messages)?;
        self.messages.push(json!({
            "role": "assistant",
            "content": reply.clone(),
        }));
        debug!(turns = self.messages.len() / 2, "conversation turn");
        Ok(reply)
    }
}

#[derive(Serialize, Deserialize)]
struct _CacheHint {
    #[serde(rename = "type")]
    kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_picks_first_text_block() {
        let v = json!({
            "content": [
                {"type": "text", "text": "hello world"},
                {"type": "tool_use", "id": "t1"},
            ],
            "stop_reason": "end_turn"
        });
        assert_eq!(extract_text(&v).unwrap(), "hello world");
    }

    #[test]
    fn extract_text_errors_on_missing_content() {
        let v = json!({"content": []});
        assert!(matches!(
            extract_text(&v),
            Err(AnthropicError::NoContent { .. })
        ));
    }
}
