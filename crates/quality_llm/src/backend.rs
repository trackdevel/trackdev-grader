//! LLM backend dispatch for quality-flags (`claude-cli`, `cursor-cli`, `ollama`).

use anyhow::{bail, Result};
use sprint_grader_core::QualityLlmConfig;
use sprint_grader_evaluate::{ClaudeCliClient, CursorCliClient};
use sprint_grader_evaluate_local::{LocalLlmBackend, OllamaClient};

/// Unified judge client for file and holistic tiers.
pub enum QualityBackend {
    Claude(ClaudeCliClient),
    Cursor(CursorCliClient),
    Ollama(OllamaClient),
}

impl QualityBackend {
    pub fn from_config(ql: &QualityLlmConfig) -> Result<Self> {
        Ok(match ql.backend.as_str() {
            "claude-cli" => Self::Claude(ClaudeCliClient::new(
                ql.claude_cli_path.clone(),
                ql.resolved_model_id().to_string(),
                ql.timeout_seconds,
            )),
            "cursor-cli" => Self::Cursor(CursorCliClient::new(
                ql.cursor_cli_path.clone(),
                ql.resolved_model_id().to_string(),
                ql.timeout_seconds,
            )),
            "ollama" => Self::Ollama(
                OllamaClient::from_quality_llm(ql).map_err(|e| anyhow::anyhow!("{e}"))?,
            ),
            other => bail!("unsupported quality-flags backend {other:?}"),
        })
    }

    pub fn ensure_available(ql: &QualityLlmConfig) -> Result<()> {
        match ql.backend.as_str() {
            "claude-cli" => {
                if !ClaudeCliClient::is_available(&ql.claude_cli_path) {
                    bail!(
                        "claude CLI not found at `{}` — install Claude Code or set \
                         [quality_llm] claude_cli_path",
                        ql.claude_cli_path
                    );
                }
            }
            "cursor-cli" => {
                if !CursorCliClient::is_available(&ql.cursor_cli_path) {
                    bail!(
                        "cursor agent CLI not found at `{}` — install Cursor Agent or set \
                         [quality_llm] cursor_cli_path",
                        ql.cursor_cli_path
                    );
                }
            }
            "ollama" => {
                let client = OllamaClient::from_quality_llm(ql).map_err(|e| anyhow::anyhow!("{e}"))?;
                if !client.is_available() {
                    bail!(
                        "ollama not reachable at `{}` — start the daemon or set \
                         [quality_llm] ollama_url",
                        ql.ollama_url
                    );
                }
            }
            other => bail!("unsupported quality-flags backend {other:?}"),
        }
        Ok(())
    }

    pub fn worker_count(ql: &QualityLlmConfig) -> usize {
        match ql.backend.as_str() {
            "cursor-cli" => CursorCliClient::max_parallel_invocations(),
            _ => ql.workers.max(1),
        }
    }

    pub fn complete_rubric(&self, system: &str, user_prompt: &str) -> Result<String> {
        match self {
            Self::Claude(c) => c
                .complete(system, user_prompt)
                .map_err(|e| anyhow::anyhow!("{e}")),
            Self::Cursor(c) => c
                .complete(system, user_prompt)
                .map_err(|e| anyhow::anyhow!("{e}")),
            Self::Ollama(c) => c
                .chat_json(system, user_prompt, None)
                .map_err(|e| anyhow::anyhow!("{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_backend_caps_workers_at_one() {
        let mut ql = QualityLlmConfig::default();
        ql.backend = "cursor-cli".into();
        ql.workers = 8;
        assert_eq!(QualityBackend::worker_count(&ql), 1);
    }

    #[test]
    fn claude_backend_uses_configured_workers() {
        let mut ql = QualityLlmConfig::default();
        ql.workers = 6;
        assert_eq!(QualityBackend::worker_count(&ql), 6);
    }
}
