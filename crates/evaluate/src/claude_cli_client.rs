//! `ClaudeCliClient` — invokes the local Claude Code CLI (`claude --print …`)
//! as the LLM backend for PR-doc and task-description scoring. Backed by
//! the user's Claude.ai subscription, so no `ANTHROPIC_API_KEY` is required.
//!
//! Each `complete()` call spawns one `claude` process with:
//!   - `--print` (non-interactive: read stdin, print once, exit),
//!   - `--output-format text`,
//!   - `--allowedTools ""` (no filesystem / shell side effects — student
//!     PR bodies are untrusted; we don't want prompt injection to be able
//!     to trigger tool use),
//!   - `--append-system-prompt` carrying the rubric.
//!
//! The user prompt is piped on stdin. The CLI's stdout is the raw
//! assistant reply; the caller is responsible for JSON extraction
//! (`llm_eval::extract_json_object`).
//!
//! Each call is fully stateless — there is no `Conversation` analogue.
//! `claude --print` mode does not support REPL-style `/clear`; sharing
//! one process across multiple PRs would require `--resume <session-id>`
//! plumbing for negligible win, since the per-PR JSON contract is
//! intrinsically independent. See the comment in `llm_eval.rs` ahead of
//! `score_pr_via_cli`.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClaudeCliError {
    #[error("claude CLI failed: {0}")]
    Cli(String),
}

#[derive(Debug, Clone)]
pub struct ClaudeCliClient {
    cli_path: String,
    model: String,
    timeout: Duration,
}

impl ClaudeCliClient {
    pub fn new(cli_path: String, model: String, timeout_seconds: u64) -> Self {
        Self {
            cli_path,
            model,
            timeout: Duration::from_secs(timeout_seconds.max(1)),
        }
    }

    /// Lightweight presence check. Used by the dispatcher before it
    /// invokes the client so the missing-CLI case can be reported as a
    /// silent skip (matching the missing-API-key contract).
    pub fn is_available(cli_path: &str) -> bool {
        Command::new(cli_path)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn model_id(&self) -> &str {
        &self.model
    }

    /// Run a single non-interactive request: `system` is appended to
    /// the CLI's system prompt; `user_prompt` is fed on stdin. Returns
    /// the raw stdout (the caller parses JSON from it).
    pub fn complete(&self, system: &str, user_prompt: &str) -> Result<String, ClaudeCliError> {
        let mut child = Command::new(&self.cli_path)
            .arg("--print")
            .arg("--output-format")
            .arg("text")
            .arg("--append-system-prompt")
            .arg(system)
            .arg("--allowedTools")
            .arg("")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                ClaudeCliError::Cli(format!("failed to spawn `{}`: {}", self.cli_path, e))
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(user_prompt.as_bytes())
                .map_err(|e| ClaudeCliError::Cli(format!("write stdin: {e}")))?;
        }

        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(ClaudeCliError::Cli(format!(
                            "claude CLI timed out after {}s",
                            self.timeout.as_secs()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(ClaudeCliError::Cli(format!("wait on claude CLI: {e}")));
                }
            }
        }

        let output = child
            .wait_with_output()
            .map_err(|e| ClaudeCliError::Cli(format!("collect claude CLI output: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(ClaudeCliError::Cli(format!(
                "claude CLI exited with {} — stderr: {}",
                output.status,
                stderr.trim()
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_available_returns_false_for_missing_binary() {
        assert!(!ClaudeCliClient::is_available(
            "/definitely/not/a/real/binary-xyz"
        ));
    }
}
