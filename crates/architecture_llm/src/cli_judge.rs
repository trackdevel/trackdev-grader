//! `ClaudeCliJudge` — invokes the local Claude Code CLI (`claude --print …`)
//! as the LLM backend instead of the direct Anthropic API. Backed by the
//! user's Claude.ai subscription, so no `ANTHROPIC_API_KEY` is required.
//!
//! Each `judge` call spawns one `claude` process with:
//!   - `--print` (non-interactive mode, prints once and exits),
//!   - `--output-format text` (we parse the JSON ourselves; the json
//!     envelope wrapping varies across CLI versions and we want robust
//!     fallback to the existing `parse_response` extractor that already
//!     handles fenced/prose-wrapped JSON),
//!   - `--allowedTools` empty (no filesystem / shell side effects),
//!   - `--append-system-prompt` carrying the rubric + response shape
//!     contract.
//!
//! The user prompt is the file contents wrapped in a fenced block plus
//! the path. The CLI's stdout is piped back; non-zero exits or timeouts
//! produce `JudgeError::Cli`.
//!
//! ### Why `--print` and not the SDK
//! - No API key. The user has only a Claude.ai subscription.
//! - Reproducibility — `--print` is deterministic (no streaming UI).
//! - Permission-isolated — `--allowedTools=` blocks tool use, so prompt
//!   injection in student source can't trigger filesystem / shell.
//!
//! ### Concurrency
//! The orchestrator decides `judge_workers`; this module is per-call
//! synchronous. Each call is a self-contained subprocess; running N
//! concurrent calls = N concurrent subprocesses.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::judge::{parse_response, Judge, JudgeError, LlmResponse};

/// CLI judge configuration. The orchestrator builds one of these once
/// per pipeline run and shares it across all `judge` calls.
#[derive(Debug, Clone)]
pub struct ClaudeCliJudge {
    /// Path to the `claude` binary. Resolved against `$PATH` if it
    /// contains no `/`. Default `"claude"`.
    cli_path: String,
    /// Model id passed to the CLI via `--model`, e.g.
    /// `claude-haiku-4-5-20251001`. Also forms part of the cache key, so
    /// changing it forces re-judge.
    model: String,
    /// Subprocess timeout. The judge errors with `JudgeError::Cli` if
    /// the CLI doesn't return within this window.
    timeout: Duration,
}

impl ClaudeCliJudge {
    pub fn new(cli_path: String, model: String, timeout_seconds: u64) -> Self {
        Self {
            cli_path,
            model,
            timeout: Duration::from_secs(timeout_seconds.max(1)),
        }
    }

    /// Lightweight presence check. Used by the orchestrator before it
    /// invokes the judge so the missing-CLI case can be reported as a
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
}

impl ClaudeCliJudge {
    /// Build the argv vector for the `claude` subprocess. Pulled out as
    /// a pure function so we can assert in tests that `--model` is
    /// always passed, locking down the contract that the configured
    /// model id (e.g. Haiku) cannot silently fall back to the user's
    /// default Claude session model (e.g. Opus on Max plans).
    fn build_argv(&self, system_prompt: &str) -> Vec<String> {
        vec![
            "--print".to_string(),
            "--output-format".to_string(),
            "text".to_string(),
            "--model".to_string(),
            self.model.clone(),
            "--append-system-prompt".to_string(),
            system_prompt.to_string(),
            "--allowedTools".to_string(),
            String::new(),
        ]
    }
}

impl Judge for ClaudeCliJudge {
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
        let system_prompt = build_system_prompt(rubric_section);
        let user_prompt = format!("Path: {file_path}\n\n```java\n{body}\n```");

        let mut child = Command::new(&self.cli_path)
            .args(self.build_argv(&system_prompt))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| JudgeError::Cli(format!("failed to spawn `{}`: {}", self.cli_path, e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(user_prompt.as_bytes())
                .map_err(|e| JudgeError::Cli(format!("write stdin: {e}")))?;
            // explicit drop closes stdin so the child can finish.
        }

        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(JudgeError::Cli(format!(
                            "claude CLI timed out after {}s",
                            self.timeout.as_secs()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(JudgeError::Cli(format!("wait on claude CLI: {e}")));
                }
            }
        }

        let output = child
            .wait_with_output()
            .map_err(|e| JudgeError::Cli(format!("collect claude CLI output: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(JudgeError::Cli(format!(
                "claude CLI exited with {} — stderr: {}",
                output.status,
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_response(&stdout)
    }
}

fn build_system_prompt(rubric_section: &str) -> String {
    format!(
        "You are an architecture reviewer. Evaluate the user-provided Java \
         file against the rubric below. Return ONLY a JSON object with a \
         single key `violations` whose value is an array. Each entry has \
         `rule_id` (short SHOUTY_SNAKE_CASE id), `severity` \
         (`INFO` | `WARNING` | `CRITICAL`), `start_line` and `end_line` \
         (1-based, inclusive), and `explanation` (≤200 chars). Use only \
         line numbers that exist in the file. Do not include prose \
         outside the JSON object. If the file has no violations, return \
         `{{\"violations\": []}}`.\n\nRubric:\n{}",
        rubric_section
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_system_prompt_includes_rubric_and_response_contract() {
        let s = build_system_prompt("RUBRIC_BODY");
        assert!(s.contains("RUBRIC_BODY"));
        assert!(s.contains("violations"));
        assert!(s.contains("rule_id"));
        assert!(s.contains("severity"));
        assert!(s.contains("start_line"));
        assert!(s.contains("end_line"));
    }

    #[test]
    fn is_available_returns_false_for_missing_binary() {
        // A nonsense path won't be found on $PATH and the call fails
        // silently — we never panic and never return true on absent.
        assert!(!ClaudeCliJudge::is_available(
            "/definitely/not/a/real/binary-xyz"
        ));
    }

    #[test]
    fn build_argv_always_passes_explicit_model() {
        // Locks the contract that the architecture LLM judge cannot
        // silently fall back to the user's default Claude session model
        // (Opus on Max plans). If `--model <model_id>` ever drops out
        // of the argv, architecture review would silently start
        // consuming Opus quota again.
        let judge = ClaudeCliJudge::new(
            "claude".to_string(),
            "claude-haiku-4-5-20251001".to_string(),
            180,
        );
        let argv = judge.build_argv("system prompt body");
        let model_idx = argv
            .iter()
            .position(|a| a == "--model")
            .expect("--model must always be present in argv");
        assert_eq!(
            argv.get(model_idx + 1).map(String::as_str),
            Some("claude-haiku-4-5-20251001"),
            "--model must be followed by the configured model id"
        );
        assert!(
            argv.iter().any(|a| a == "--print"),
            "--print mode is required for non-interactive runs"
        );
        assert!(
            argv.iter().any(|a| a == "--allowedTools"),
            "--allowedTools must be set so prompt injection cannot trigger tool use"
        );
    }

    #[test]
    fn build_argv_propagates_caller_supplied_model_verbatim() {
        // Use a distinct, pinned id (not Haiku) to prove the propagation
        // contract is independent of the production default — any string
        // the caller hands us must reach the CLI unchanged.
        let judge = ClaudeCliJudge::new(
            "claude".to_string(),
            "claude-sonnet-4-6-20250101".to_string(),
            180,
        );
        let argv = judge.build_argv("");
        let i = argv.iter().position(|a| a == "--model").unwrap();
        assert_eq!(argv[i + 1], "claude-sonnet-4-6-20250101");
    }
}
