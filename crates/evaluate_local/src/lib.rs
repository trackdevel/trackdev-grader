//! Local-hybrid PR documentation evaluator.
//!
//! `judge = "local-hybrid"` in `[evaluate]` routes per-PR scoring through
//! this crate instead of the cloud-LLM dispatchers in
//! `crates/evaluate/src/llm_eval.rs`. The pipeline shape per PR is:
//!
//! ```text
//! short-circuit detectors ─► BGE-M3 embedding (ollama HTTP)
//!     │                  ─► ridge regression (Rust dot product, JSON weights)
//!     │                  ─► triage: Snap | NeedsLlm | ShortCircuit
//!     │                  ─► (NeedsLlm) Salamandra-2B-Instruct (ollama chat)
//!     └──────────────────► persist row + update avg_doc_score
//! ```
//!
//! Invariant J (load-bearing): every row this pipeline writes has
//! `pr_doc_evaluation.justification` beginning with the literal `"local:"`.
//! The CLI `reset-local-scores` command uses this prefix as the sole
//! invalidation discriminator — no schema column was added.
//!
//! Invariant O: ollama owns GPU memory. This crate never links `ort` /
//! `mistralrs` / `nvidia-smi`. The model dispatch surface is HTTP.
//!
//! Invariant C: `sprint-grader-evaluate` does NOT depend on this crate.
//! Dispatch goes the other direction — CLI and orchestration own the
//! routing into `run_local_hybrid_batch`; `evaluate::llm_eval`'s per-sprint
//! arm for `"local-hybrid"` is a defensive no-op early return.

pub mod config;
pub mod flags;
pub mod ollama;
pub mod persist;
pub mod pipeline;
pub mod ridge;
pub mod triage;

pub use flags::DetFlag;
pub use ollama::{LocalLlmBackend, OllamaClient, OllamaError};
pub use pipeline::{run_local_hybrid_batch, BatchStats};
pub use ridge::{PrRidgeBundle, RidgeHead};
pub use triage::{Decision, PrPrediction, TriagePolicy};
