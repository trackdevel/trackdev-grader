//! `CursorCliClient` — invokes the Cursor Agent CLI (`agent --print …`) as
//! the LLM backend for PR-doc scoring. Uses the user's Cursor subscription
//! (or `CURSOR_API_KEY` when set) instead of Anthropic / DeepSeek API keys.
//!
//! Each `complete()` call spawns one `agent` process with:
//!   - `--print` (non-interactive: emit the final answer once, then exit),
//!   - `--mode ask` (read-only — no file edits or shell from tool use),
//!   - `--output-format text`,
//!   - `--model` carrying the configured id (e.g. `composer-2.5`),
//!   - `--trust` (skip workspace-trust prompts in headless runs),
//!   - `--workspace` pointing at an isolated temp directory so student repos
//!     are not indexed and PR bodies in the prompt are the only context.
//!
//! The rubric and user message are combined into one positional prompt
//! (the CLI has no `--append-system-prompt` analogue).
//!
//! **Concurrency:** each `agent` startup atomically renames
//! `~/.cursor/cli-config.json.tmp` → `cli-config.json`. Parallel spawns
//! race on that path and fail with `ENOENT` on the rename. All
//! `complete()` calls therefore take a process-wide lock and
//! [`max_parallel_invocations`] is `1` regardless of `judge_workers`.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CursorCliError {
    #[error("cursor agent CLI failed: {0}")]
    Cli(String),
}

#[derive(Debug, Clone)]
pub struct CursorCliClient {
    cli_path: String,
    model: String,
    timeout: Duration,
    workspace: PathBuf,
}

static WORKSPACE: OnceLock<PathBuf> = OnceLock::new();

/// Serialises `agent` subprocesses — see module docs.
static INVOCATION_LOCK: Mutex<()> = Mutex::new(());

fn isolated_workspace() -> PathBuf {
    WORKSPACE
        .get_or_init(|| {
            let dir = std::env::temp_dir().join("sprint-grader-cursor-judge");
            let _ = std::fs::create_dir_all(&dir);
            dir
        })
        .clone()
}

impl CursorCliClient {
    pub fn new(cli_path: String, model: String, timeout_seconds: u64) -> Self {
        Self {
            cli_path,
            model,
            timeout: Duration::from_secs(timeout_seconds.max(1)),
            workspace: isolated_workspace(),
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

    /// Upper bound for Rayon pools when this backend is selected.
    pub fn max_parallel_invocations() -> usize {
        1
    }

    /// Build argv after the binary name. Pulled out as a pure function so
    /// tests can assert `--model` and `--mode ask` are always present.
    fn build_argv(&self, combined_prompt: &str) -> Vec<String> {
        vec![
            "--print".to_string(),
            combined_prompt.to_string(),
            "--model".to_string(),
            self.model.clone(),
            "--mode".to_string(),
            "ask".to_string(),
            "--output-format".to_string(),
            "text".to_string(),
            "--trust".to_string(),
            "--workspace".to_string(),
            self.workspace.display().to_string(),
        ]
    }

    /// Run a single non-interactive request. `system` is the rubric;
    /// `user_prompt` is the PR/task payload. Returns raw stdout for JSON
    /// extraction by the caller.
    pub fn complete(&self, system: &str, user_prompt: &str) -> Result<String, CursorCliError> {
        let _guard = INVOCATION_LOCK.lock().map_err(|e| {
            CursorCliError::Cli(format!("cursor agent CLI invocation lock poisoned: {e}"))
        })?;

        // Rubric already ends with "Reply ONLY JSON"; user turn is minimal.
        let combined = format!("{system}\n\n---\n\n{user_prompt}");
        let mut child = Command::new(&self.cli_path)
            .args(self.build_argv(&combined))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                CursorCliError::Cli(format!("failed to spawn `{}`: {}", self.cli_path, e))
            })?;

        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(CursorCliError::Cli(format!(
                            "cursor agent CLI timed out after {}s",
                            self.timeout.as_secs()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(CursorCliError::Cli(format!(
                        "wait on cursor agent CLI: {e}"
                    )));
                }
            }
        }

        let output = child
            .wait_with_output()
            .map_err(|e| CursorCliError::Cli(format!("collect cursor agent CLI output: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(CursorCliError::Cli(format!(
                "cursor agent CLI exited with {} — stderr: {}",
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
    fn build_argv_always_passes_explicit_model_and_ask_mode() {
        let client = CursorCliClient::new(
            "agent".to_string(),
            "composer-2.5".to_string(),
            180,
        );
        let argv = client.build_argv("score this PR");
        let model_idx = argv
            .iter()
            .position(|a| a == "--model")
            .expect("--model must always be present in argv");
        assert_eq!(
            argv.get(model_idx + 1).map(String::as_str),
            Some("composer-2.5"),
            "--model must be followed by the configured model id"
        );
        let mode_idx = argv
            .iter()
            .position(|a| a == "--mode")
            .expect("--mode must always be present in argv");
        assert_eq!(argv.get(mode_idx + 1).map(String::as_str), Some("ask"));
        assert!(argv.iter().any(|a| a == "--print"));
        assert!(argv.iter().any(|a| a == "--trust"));
        assert_eq!(argv.first().map(String::as_str), Some("--print"));
        assert_eq!(argv.get(1).map(String::as_str), Some("score this PR"));
    }

    #[test]
    fn build_argv_propagates_caller_supplied_model_verbatim() {
        let client = CursorCliClient::new(
            "agent".to_string(),
            "composer-2.5".to_string(),
            180,
        );
        let argv = client.build_argv("");
        let i = argv.iter().position(|a| a == "--model").unwrap();
        assert_eq!(argv[i + 1], "composer-2.5");
    }

    #[test]
    fn is_available_returns_false_for_missing_binary() {
        assert!(!CursorCliClient::is_available(
            "/definitely/not/a/real/binary-xyz"
        ));
    }

    #[test]
    fn max_parallel_invocations_is_one() {
        assert_eq!(CursorCliClient::max_parallel_invocations(), 1);
    }
}
