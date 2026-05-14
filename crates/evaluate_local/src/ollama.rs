//! HTTP backend for embedding and chat through a local ollama daemon.
//!
//! Invariant O (load-bearing): ollama owns GPU memory. This module never
//! links `ort`, `mistralrs`, or `nvidia-smi`. The dispatch surface is the
//! blocking HTTP client; the GPU stack stays a black box behind the daemon.
//!
//! The pipeline relies on the [`LocalLlmBackend`] trait — tests inject a
//! deterministic mock without spinning up ollama. `OllamaClient` is the
//! production implementation.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use sprint_grader_core::config::LocalEvaluateConfig;

/// Backend abstraction: embedding + chat. Implementations must be
/// `Send + Sync` so the (single-threaded P1) caller and any future
/// rayon-parallel callers share one instance.
pub trait LocalLlmBackend: Send + Sync {
    /// Cheap reachability probe. Implementations should swallow network
    /// errors and return `false` rather than panicking.
    fn is_available(&self) -> bool;

    /// Return embeddings for each input in `inputs`, in the same order.
    fn embed(&self, inputs: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Send a chat request with optional schema-constrained sampling.
    /// When `schema` is `Some`, the request body includes the `format`
    /// field; `None` omits it (used by the retry path when the operator's
    /// ollama version rejects schema-constrained sampling — see plan
    /// step 18). Returns the model's reply text.
    fn chat_json(&self, system: &str, user: &str, schema: Option<&Value>)
        -> anyhow::Result<String>;
}

#[derive(Debug, thiserror::Error)]
pub enum OllamaError {
    #[error("ollama HTTP error: {0}")]
    Http(String),
    #[error("ollama status {0}: {1}")]
    Status(u16, String),
    #[error("ollama shape: {0}")]
    Shape(String),
}

impl OllamaError {
    /// True iff this is the "schema-constrained sampling unsupported"
    /// signal — HTTP 400 with the literal `"format"` substring in the
    /// response body. The local-hybrid LLM fallback retries the same
    /// chat call without the `format` field exactly once when this
    /// matches (plan step 18).
    pub fn is_unsupported_format_400(&self) -> bool {
        matches!(self, OllamaError::Status(400, body) if body.contains("format"))
    }
}

/// Production [`LocalLlmBackend`] over the ollama REST API. Blocking
/// client; the pipeline is intentionally single-threaded at P1 to keep
/// the GPU contention story simple.
pub struct OllamaClient {
    base_url: String,
    http: reqwest::blocking::Client,
    embed_model: String,
    llm_model: String,
    llm_keep_alive: String,
}

impl OllamaClient {
    pub fn from_config(cfg: &LocalEvaluateConfig) -> Result<Self, OllamaError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(cfg.ollama_timeout_seconds))
            .build()
            .map_err(|e| OllamaError::Http(e.to_string()))?;
        Ok(Self {
            base_url: cfg.ollama_url.trim_end_matches('/').to_string(),
            http,
            embed_model: cfg.embed_model.clone(),
            llm_model: cfg.llm_model.clone(),
            llm_keep_alive: cfg.llm_keep_alive.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct EmbedItem {
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    #[serde(default)]
    data: Vec<EmbedItem>,
    /// Some ollama builds return `embeddings` instead of `data`.
    #[serde(default)]
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

impl LocalLlmBackend for OllamaClient {
    fn is_available(&self) -> bool {
        match self.http.get(format!("{}/api/tags", self.base_url)).send() {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    fn embed(&self, inputs: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let body = json!({
            "model": self.embed_model,
            "input": inputs,
        });
        let resp = self
            .http
            .post(format!("{}/api/embed", self.base_url))
            .json(&body)
            .send()
            .map_err(|e| OllamaError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(OllamaError::Status(status.as_u16(), text).into());
        }
        let parsed: EmbedResponse = resp
            .json()
            .map_err(|e| OllamaError::Shape(format!("embed response: {e}")))?;
        let vectors = if !parsed.embeddings.is_empty() {
            parsed.embeddings
        } else {
            parsed.data.into_iter().map(|i| i.embedding).collect()
        };
        if vectors.len() != inputs.len() {
            return Err(OllamaError::Shape(format!(
                "expected {} embedding vectors, got {}",
                inputs.len(),
                vectors.len()
            ))
            .into());
        }
        Ok(vectors)
    }

    fn chat_json(
        &self,
        system: &str,
        user: &str,
        schema: Option<&Value>,
    ) -> anyhow::Result<String> {
        let mut body = json!({
            "model": self.llm_model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user",   "content": user},
            ],
            "stream": false,
            "keep_alive": self.llm_keep_alive,
            "options": {
                "temperature": 0,
                "top_k": 1,
                "seed": 0,
            },
        });
        if let Some(s) = schema {
            body.as_object_mut()
                .expect("body is an object literal")
                .insert("format".to_string(), s.clone());
        }
        let resp = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .map_err(|e| OllamaError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(OllamaError::Status(status.as_u16(), text).into());
        }
        let parsed: ChatResponse = resp
            .json()
            .map_err(|e| OllamaError::Shape(format!("chat response: {e}")))?;
        Ok(parsed.message.content)
    }
}
