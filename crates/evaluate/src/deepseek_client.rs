//! Thin blocking DeepSeek chat-completions client.
//!
//! DeepSeek's HTTP surface is OpenAI-compatible: `POST /chat/completions`,
//! body `{model, messages, max_tokens, temperature, response_format}`, reply
//! at `choices[0].message.content`. The `messages` array is the same shape
//! the Anthropic client already builds, with one twist: DeepSeek expects
//! the system prompt as a leading `{"role": "system"}` message rather than
//! a top-level `system` field, so we prepend it on the way out.
//!
//! Server-side context caching is automatic — identical prompt prefixes hit
//! the cache without any `cache_control`-style opt-in. Reusing
//! `Conversation` to accumulate the rubric prefix is therefore free.
//!
//! Determinism: `temperature = 0`. Strict JSON shape: `response_format =
//! json_object`. Both belt-and-braces guards because the cache key for
//! architecture LLM violations omits temperature.

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tracing::warn;

use crate::llm_trait::{LlmClient, LlmError};

const API_URL: &str = "https://api.deepseek.com/chat/completions";
const DEFAULT_MAX_TOKENS: u32 = 1024;
const MAX_RETRIES: u32 = 3;
const BACKOFF_BASE_SECS: u64 = 2;

#[derive(Debug, Clone)]
pub struct DeepseekClient {
    client: Client,
    model: String,
    /// V4 thinking mode. When `Some`, emitted in the request body as
    /// `{"thinking": {"type": <value>}}`. When `None`, the field is
    /// omitted and the server applies its own default (currently
    /// `enabled` for V4 models). Validated upstream in `Config::load`
    /// to one of `"enabled"` or `"disabled"`.
    thinking: Option<String>,
}

impl DeepseekClient {
    pub fn new(api_key: &str, model: String) -> Result<Self, LlmError> {
        if api_key.is_empty() {
            return Err(LlmError::EmptyKey);
        }
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {api_key}");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&bearer).map_err(|_| LlmError::EmptyKey)?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client build");
        Ok(Self {
            client,
            model,
            thinking: None,
        })
    }

    /// Builder: pin V4 thinking mode (`"enabled"` or `"disabled"`).
    /// Pass `None` to leave it server-default.
    pub fn with_thinking(mut self, thinking: Option<String>) -> Self {
        self.thinking = thinking;
        self
    }

    fn build_body(&self, system: &str, messages: &[Value], max_tokens: u32) -> Value {
        let mut full = Vec::with_capacity(messages.len() + 1);
        full.push(json!({"role": "system", "content": system}));
        full.extend(messages.iter().cloned());
        let mut body = json!({
            "model": self.model,
            "messages": full,
            "max_tokens": max_tokens,
            "temperature": 0,
            "response_format": {"type": "json_object"},
            "stream": false,
        });
        if let Some(mode) = &self.thinking {
            body["thinking"] = json!({"type": mode});
        }
        body
    }

    pub fn complete(
        &self,
        system: &str,
        messages: &[Value],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        let body = self.build_body(system, messages, max_tokens);

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
                        warn!(%status, wait_s = wait, body = %text, "DeepSeek retry");
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    }
                    let text = r.text().unwrap_or_default();
                    return Err(LlmError::Http {
                        status: status.as_u16(),
                        body: text,
                    });
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < MAX_RETRIES {
                        let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                        warn!(wait_s = wait, "DeepSeek request error — retrying");
                        std::thread::sleep(Duration::from_secs(wait));
                    }
                }
            }
        }
        Err(LlmError::RequestFailed {
            retries: MAX_RETRIES,
            source: last_err.expect("loop always populates last_err"),
        })
    }

    pub fn complete_default(&self, system: &str, messages: &[Value]) -> Result<String, LlmError> {
        self.complete(system, messages, DEFAULT_MAX_TOKENS)
    }
}

impl LlmClient for DeepseekClient {
    fn complete(
        &self,
        system: &str,
        messages: &[Value],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        DeepseekClient::complete(self, system, messages, max_tokens)
    }
}

fn extract_text(json: &Value) -> Result<String, LlmError> {
    let choices = json.get("choices").and_then(Value::as_array);
    if let Some(arr) = choices {
        if let Some(first) = arr.first() {
            // DeepSeek/OpenAI surface the truncation reason in
            // `finish_reason`. "length" means the model hit the
            // `max_tokens` cap before emitting `</s>`; on V4 with
            // thinking enabled, reasoning tokens count against this
            // cap, so a too-low budget silently yields half-written
            // JSON that downstream parsers reject. Convert to a typed
            // error so the call site logs an actionable message
            // instead of "EOF while parsing a list".
            let finish_reason = first
                .get("finish_reason")
                .and_then(Value::as_str)
                .unwrap_or("");
            if finish_reason == "length" {
                return Err(LlmError::Truncated {
                    finish_reason: finish_reason.to_string(),
                });
            }
            if let Some(text) = first
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
            {
                return Ok(text.to_string());
            }
        }
    }
    Err(LlmError::NoContent {
        raw: json.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_client() -> DeepseekClient {
        // We build with a non-empty key so the constructor succeeds; no
        // network calls are issued in these tests.
        DeepseekClient::new("sk-test", "deepseek-chat".to_string()).unwrap()
    }

    #[test]
    fn empty_key_rejected() {
        let err = DeepseekClient::new("", "deepseek-chat".to_string()).unwrap_err();
        assert!(matches!(err, LlmError::EmptyKey));
    }

    #[test]
    fn build_body_prepends_system_message_and_pins_temperature_zero() {
        let c = fixture_client();
        let messages = vec![json!({"role": "user", "content": "hello"})];
        let body = c.build_body("You are a reviewer.", &messages, 256);

        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["response_format"]["type"], "json_object");
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are a reviewer.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hello");
    }

    #[test]
    fn build_body_omits_thinking_field_by_default() {
        let body =
            fixture_client().build_body("sys", &[json!({"role": "user", "content": "x"})], 16);
        assert!(body.get("thinking").is_none(), "thinking must default-omit");
    }

    #[test]
    fn build_body_emits_thinking_disabled_when_pinned() {
        let c = fixture_client().with_thinking(Some("disabled".to_string()));
        let body = c.build_body("sys", &[json!({"role": "user", "content": "x"})], 16);
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    #[test]
    fn build_body_emits_thinking_enabled_when_pinned() {
        let c = fixture_client().with_thinking(Some("enabled".to_string()));
        let body = c.build_body("sys", &[json!({"role": "user", "content": "x"})], 16);
        assert_eq!(body["thinking"]["type"], "enabled");
    }

    #[test]
    fn extract_text_picks_first_choice_message_content() {
        let v = json!({
            "id": "xyz",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello world"},
                "finish_reason": "stop"
            }]
        });
        assert_eq!(extract_text(&v).unwrap(), "hello world");
    }

    #[test]
    fn extract_text_errors_on_missing_choices() {
        let v = json!({"choices": []});
        assert!(matches!(extract_text(&v), Err(LlmError::NoContent { .. })));
    }

    #[test]
    fn extract_text_surfaces_finish_reason_length_as_truncated() {
        let v = json!({
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "{\"violations\":[{"},
                "finish_reason": "length"
            }]
        });
        match extract_text(&v) {
            Err(LlmError::Truncated { finish_reason }) => {
                assert_eq!(finish_reason, "length");
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }
}
