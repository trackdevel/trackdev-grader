//! `DeepseekJudge` — invokes the DeepSeek chat-completions API as the
//! LLM backend. Requires `DEEPSEEK_API_KEY`. Mirrors `LlmJudge` (the
//! Anthropic-API judge) one-for-one: same prompt construction, same
//! response parser. The only differences are the wire shape (handled
//! inside `DeepseekClient`) and that DeepSeek's automatic context caching
//! removes the need for `cache_control` opt-in headers.

use serde_json::json;
use sprint_grader_evaluate::DeepseekClient;

use crate::judge::{parse_response, Judge, JudgeError, LlmResponse};

pub struct DeepseekJudge {
    client: DeepseekClient,
    model: String,
    max_tokens: u32,
}

impl DeepseekJudge {
    pub fn new(api_key: &str, model: String, max_tokens: u32) -> Result<Self, JudgeError> {
        let client = DeepseekClient::new(api_key, model.clone())
            .map_err(|e| JudgeError::Cli(format!("deepseek client init: {e}")))?;
        Ok(Self {
            client,
            model,
            max_tokens,
        })
    }
}

impl Judge for DeepseekJudge {
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
        let messages = vec![json!({
            "role": "user",
            "content": format!("Path: {file_path}\n\n```java\n{body}\n```")
        })];
        let text = self
            .client
            .complete(&system, &messages, self.max_tokens)
            .map_err(|e| JudgeError::Cli(format!("deepseek call: {e}")))?;
        parse_response(&text)
    }
}
