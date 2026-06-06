//! Shared trait for non-interactive CLI rubric backends (`claude-cli`,
//! `cursor-cli`).

use crate::claude_cli_client::{ClaudeCliClient, ClaudeCliError};
use crate::cursor_cli_client::{CursorCliClient, CursorCliError};

pub(crate) trait RubricCliBackend {
    fn complete_rubric(&self, rubric: &str, user_prompt: &str) -> Result<String, String>;
}

impl RubricCliBackend for ClaudeCliClient {
    fn complete_rubric(&self, rubric: &str, user_prompt: &str) -> Result<String, String> {
        self.complete(rubric, user_prompt)
            .map_err(|e: ClaudeCliError| e.to_string())
    }
}

impl RubricCliBackend for CursorCliClient {
    fn complete_rubric(&self, rubric: &str, user_prompt: &str) -> Result<String, String> {
        self.complete(rubric, user_prompt)
            .map_err(|e: CursorCliError| e.to_string())
    }
}
