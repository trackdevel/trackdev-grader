//! Judge interface — what the orchestrator calls per cache miss.
//!
//! The trait is small on purpose so tests can swap in a deterministic
//! responder without an Anthropic key. The real impl lives in
//! [`LlmJudge`] and uses `sprint_grader_evaluate::AnthropicClient` for
//! transport.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sprint_grader_evaluate::llm_client::{AnthropicClient, AnthropicError};

/// One violation entry returned by the model. Schema-validated at parse
/// time; non-conforming entries are dropped at the call site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmViolation {
    pub rule_id: String,
    pub severity: String,
    pub start_line: u32,
    pub end_line: u32,
    pub explanation: String,
}

/// Wrapping object: the model is asked to return `{"violations": [...]}`.
/// Trailing prose / markdown fences are stripped at parse time
/// (`extract_json_object` already exists in `evaluate::llm_eval`; we
/// re-implement a tight version here to avoid pulling that dependency
/// for one helper).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    #[serde(default)]
    pub violations: Vec<LlmViolation>,
}

#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("anthropic call failed: {0}")]
    Anthropic(#[from] AnthropicError),
    #[error("model response is not valid JSON: {0}")]
    Parse(String),
    #[error("claude CLI failed: {0}")]
    Cli(String),
}

pub trait Judge {
    fn model_id(&self) -> &str;
    fn judge(
        &self,
        file_path: &str,
        rubric_section: &str,
        file_bytes: &[u8],
    ) -> Result<LlmResponse, JudgeError>;
}

pub struct LlmJudge {
    client: AnthropicClient,
    model: String,
    max_tokens: u32,
}

impl LlmJudge {
    pub fn new(
        api_key: &str,
        model: String,
        max_tokens: u32,
        timeout_seconds: u64,
    ) -> Result<Self, JudgeError> {
        let client = AnthropicClient::new(api_key, model.clone(), timeout_seconds)?;
        Ok(Self {
            client,
            model,
            max_tokens,
        })
    }
}

impl Judge for LlmJudge {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn judge(
        &self,
        file_path: &str,
        rubric_section: &str,
        file_bytes: &[u8],
    ) -> Result<LlmResponse, JudgeError> {
        let body = String::from_utf8_lossy(file_bytes);
        let system = format!(
            "You are an architecture reviewer. Evaluate the following Java file \
             against the rubric below. Return ONLY a JSON object with a single \
             key `violations` whose value is an array. Each entry has \
             `rule_id` (short SHOUTY_SNAKE_CASE id), `severity` \
             (`INFO` | `WARNING` | `CRITICAL`), `start_line` and `end_line` \
             (1-based, inclusive), and `explanation` (≤200 chars). Use \
             only line numbers that exist in the file. Do not include \
             prose outside the JSON object. If the file has no violations, \
             return `{{\"violations\": []}}`.\n\n\
             Rubric:\n{}",
            rubric_section
        );
        let user_msg = json!([
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": format!("Path: {file_path}\n\n```java\n{body}\n```")
                    }
                ]
            }
        ]);
        let messages = match user_msg.as_array() {
            Some(a) => a.clone(),
            None => Vec::new(),
        };
        let text = self.client.complete(&system, &messages, self.max_tokens)?;
        parse_response(&text)
    }
}

/// Strip optional ```json fences and trailing prose, then deserialize.
pub fn parse_response(text: &str) -> Result<LlmResponse, JudgeError> {
    let cleaned = extract_json_object(text);
    let resp: LlmResponse = serde_json::from_str(&cleaned)
        .map_err(|e| JudgeError::Parse(format!("{e} — text: {cleaned}")))?;
    Ok(resp)
}

fn extract_json_object(text: &str) -> String {
    // Drop fenced code blocks: ```json ... ``` (or unspecified language).
    let stripped = strip_code_fence(text);
    // Trim leading prose to the first '{' and trailing to the last '}'.
    if let (Some(s), Some(e)) = (stripped.find('{'), stripped.rfind('}')) {
        if e > s {
            return stripped[s..=e].to_string();
        }
    }
    stripped
}

fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        let rest = rest.trim_start_matches('\n');
        if let Some(end) = rest.rfind("```") {
            return rest[..end].to_string();
        }
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.trim_start_matches('\n');
        if let Some(end) = rest.rfind("```") {
            return rest[..end].to_string();
        }
    }
    text.to_string()
}

/// Stub judge for tests / offline runs. Always returns a fixed response,
/// so cache round-trip and violation insertion can be tested without a
/// network call. The orchestrator selects the real judge based on
/// config + key presence; this type is for tests only.
#[cfg(any(test, feature = "test-stub"))]
pub struct StubJudge {
    pub model: String,
    pub response: LlmResponse,
}

#[cfg(any(test, feature = "test-stub"))]
impl Judge for StubJudge {
    fn model_id(&self) -> &str {
        &self.model
    }
    fn judge(
        &self,
        _file_path: &str,
        _rubric_section: &str,
        _file_bytes: &[u8],
    ) -> Result<LlmResponse, JudgeError> {
        Ok(self.response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_naked_json_object() {
        let r = parse_response(r#"{"violations":[]}"#).unwrap();
        assert!(r.violations.is_empty());
    }

    #[test]
    fn parses_code_fenced_json() {
        let text = "```json\n{\"violations\":[{\"rule_id\":\"FAT_METHOD\",\"severity\":\"WARNING\",\"start_line\":10,\"end_line\":40,\"explanation\":\"too long\"}]}\n```";
        let r = parse_response(text).unwrap();
        assert_eq!(r.violations.len(), 1);
        assert_eq!(r.violations[0].rule_id, "FAT_METHOD");
        assert_eq!(r.violations[0].start_line, 10);
        assert_eq!(r.violations[0].end_line, 40);
    }

    #[test]
    fn parses_with_leading_and_trailing_prose() {
        let text =
            "Sure, here's the analysis:\n\n{\"violations\":[]}\n\nLet me know if you need more.";
        let r = parse_response(text).unwrap();
        assert!(r.violations.is_empty());
    }

    #[test]
    fn missing_violations_field_yields_empty() {
        // The response wrapper has `#[serde(default)]` so a partial
        // object still parses with an empty list.
        let r = parse_response("{}").unwrap();
        assert!(r.violations.is_empty());
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let err = parse_response("not json at all").unwrap_err();
        match err {
            JudgeError::Parse(_) => {}
            other => panic!("expected Parse, got {other:?}"),
        }
    }
}
