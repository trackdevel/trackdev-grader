//! Per-PR orchestration: short-circuit → embed → ridge → triage → LLM
//! borderline → persist.
//!
//! P2 wires the real embedding + ridge path. P3 adds the LLM fallback
//! between triage and persist for `Decision::NeedsLlm` borderline cases;
//! P2 falls back to the snapped regressor mean for those cases (no LLM).
//!
//! Justification prefixes are documented in plan §"Persist" so the CLI's
//! `reset-local-scores` invalidation discriminator
//! (`justification LIKE 'local:%'`) stays load-bearing (Invariant J).

use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::Value;
use sprint_grader_core::Config;
use sprint_grader_evaluate::{extract_json_object, RUBRIC_PR};
use tracing::{debug, info, warn};

use crate::flags::{detect, DetFlag};
use crate::ollama::{LocalLlmBackend, OllamaClient, OllamaError};
use crate::persist::{
    snap_description, snap_title, update_avg_doc_score, write_pr_row, PrPersistRow,
};
use crate::ridge::PrRidgeBundle;
use crate::triage::{Decision, PrPrediction, TriagePolicy};

/// Max inputs per `LocalLlmBackend::embed` call. Mirrors the trainer's
/// `EMBED_BATCH` (`tools/train_regressor/train.py`); kept in sync because
/// switching one without the other changes nothing on the determinism
/// front but the operator should keep both consistent for throughput.
pub(crate) const EMBED_BATCH: usize = 32;

/// Parsed shape of the LLM's PR-doc reply. `total_doc_score` is
/// regenerated from `title + description` when absent. `justification` is
/// accepted but ignored — the persist layer owns the `"local: …"` prefix.
#[derive(Debug, Deserialize)]
struct LlmPrResponse {
    title_score: f64,
    description_score: f64,
    #[allow(dead_code)]
    #[serde(default)]
    total_doc_score: Option<f64>,
    #[allow(dead_code)]
    #[serde(default)]
    justification: Option<String>,
}

/// JSON Schema sent to `LocalLlmBackend::chat_json` for schema-constrained
/// sampling. Matches the rubric: title ∈ [0,2], description ∈ [0,4],
/// total ∈ [0,6]. Salamandra-2B accepts this directly on ollama versions
/// that support `format`; older versions reject it with HTTP 400 (see
/// retry-without-format path in `llm_score_borderline`).
fn pr_response_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["title_score", "description_score"],
        "properties": {
            "title_score":       {"type": "number", "minimum": 0.0, "maximum": 2.0},
            "description_score": {"type": "number", "minimum": 0.0, "maximum": 4.0},
            "total_doc_score":   {"type": "number", "minimum": 0.0, "maximum": 6.0},
            "justification":     {"type": "string"}
        }
    })
}

/// Single-line inline schema sketch used when retrying the chat call
/// without `format` (because the operator's ollama version doesn't
/// support schema-constrained sampling). Mirrors the JSON Schema above —
/// keep both in sync if either side moves.
const PR_SCHEMA_SKETCH: &str = "{\"title_score\": number 0-2, \"description_score\": number 0-4, \
     \"total_doc_score\": number 0-6, \"justification\": string}";

/// Outcome of the optional LLM scoring step for a single PR.
#[derive(Debug, Clone, Copy)]
enum LlmOutcome {
    /// LLM was not invoked (Snap, ShortCircuit, or NeedsLlm with
    /// Disabled / DimMismatch).
    NotInvoked,
    /// LLM returned a parseable response (first attempt, retried parse,
    /// or retry-without-format). Snapped to the rubric grid before persist.
    Refined { title: f64, description: f64 },
    /// LLM call(s) all parse-failed. Fall back to the snapped regressor
    /// mean with justification `"local: llm-fallback-failed"`.
    FallbackFailed,
    /// HTTP 400 with `"format"` in the body, and the retry-without-format
    /// response also failed to parse. Fall back to the snapped regressor
    /// mean with justification `"local: llm-format-unsupported"`.
    FormatUnsupported,
}

/// Run the chat call for a single PR. The cascade is:
///
/// 1. `chat_json(RUBRIC_PR, user, Some(&schema))` — schema-constrained.
/// 2. On parse failure, retry once with the prefixed reminder.
/// 3. On HTTP 400 with `"format"` in the body (legacy ollama), retry once
///    without `format` and with an inlined schema sketch in the user
///    message. If that response parses, accept it.
/// 4. Any remaining failure → fall back to the snapped regressor mean.
///
/// `extract_json_object` is shared with the per-sprint dispatcher in
/// `sprint-grader-evaluate` so loose / fenced replies are handled
/// consistently across judge backends.
fn refined_from(resp: LlmPrResponse) -> LlmOutcome {
    LlmOutcome::Refined {
        title: snap_title(resp.title_score),
        description: snap_description(resp.description_score),
    }
}

fn llm_score_borderline(backend: &dyn LocalLlmBackend, row: &PrInputRow) -> LlmOutcome {
    let user = build_pr_user_message(row);
    let schema = pr_response_schema();

    match backend.chat_json(RUBRIC_PR, &user, Some(&schema)) {
        Ok(text) => {
            if let Some(resp) = parse_llm_pr_response(&text) {
                refined_from(resp)
            } else {
                // Parse failure on the schema-constrained call — retry once
                // with an explicit reminder prepended to the user message.
                let prefixed =
                    format!("Reply ONLY with a JSON object matching the schema.\n\n{user}");
                match backend.chat_json(RUBRIC_PR, &prefixed, Some(&schema)) {
                    Ok(text2) => parse_llm_pr_response(&text2)
                        .map(refined_from)
                        .unwrap_or(LlmOutcome::FallbackFailed),
                    Err(_) => LlmOutcome::FallbackFailed,
                }
            }
        }
        Err(e) => {
            // The 400+format retry-without-schema branch fires only on
            // this specific signal — not on other HTTP errors and not on
            // parse failures of the schema-constrained response.
            let is_format_400 = e
                .downcast_ref::<OllamaError>()
                .map(|oe| oe.is_unsupported_format_400())
                .unwrap_or(false);
            if is_format_400 {
                let prefixed = format!(
                    "Reply ONLY with a JSON object matching: {PR_SCHEMA_SKETCH}.\n\n{user}"
                );
                match backend.chat_json(RUBRIC_PR, &prefixed, None) {
                    Ok(text) => parse_llm_pr_response(&text)
                        .map(refined_from)
                        .unwrap_or(LlmOutcome::FormatUnsupported),
                    Err(_) => LlmOutcome::FormatUnsupported,
                }
            } else {
                LlmOutcome::FallbackFailed
            }
        }
    }
}

fn parse_llm_pr_response(text: &str) -> Option<LlmPrResponse> {
    let v = extract_json_object(text)?;
    serde_json::from_value::<LlmPrResponse>(v).ok()
}

/// Per-batch counters. Returned to the caller for logging; not persisted.
#[derive(Debug, Default, Clone, Copy)]
pub struct BatchStats {
    pub items_total: usize,
    pub items_already_scored: usize,
    pub short_circuited: usize,
    pub regressor_only: usize,
    pub llm_used: usize,
    pub failures: usize,
}

/// One PR's input data — selected once at the top of `run_local_hybrid_batch`
/// so the embedding and chat formatters see the same struct.
///
/// Marked `pub` + `#[doc(hidden)]` because the integration-test crate
/// needs to construct rows for the `embed_input_matches_trainer_shape`
/// test (a determinism gate against the trainer's `build_inputs`).
#[allow(dead_code)]
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct PrInputRow {
    pub pr_id: String,
    pub sprint_id: i64,
    pub title: Option<String>,
    pub body: Option<String>,
    pub task_name: Option<String>,
    pub parent_story: Option<String>,
}

/// Build the byte-for-byte input string that the trainer's `build_inputs`
/// must mirror. The determinism gate is the test
/// `embed_input_matches_trainer_shape`. `pub` + `#[doc(hidden)]` for the
/// same reason as [`PrInputRow`].
#[doc(hidden)]
pub fn build_pr_embedding_input(row: &PrInputRow) -> String {
    let task_name = row.task_name.as_deref().unwrap_or("");
    let parent_story = row.parent_story.as_deref().unwrap_or("N/A");
    let title = row.title.as_deref().unwrap_or("");
    let body = row.body.as_deref().unwrap_or("(empty)");
    format!("Task: {task_name}\nUser Story: {parent_story}\nTitle: {title}\nDescription:\n{body}")
}

/// Mirrors `evaluate_prs_via_cli`'s per-PR user message — the body sent
/// to the LLM in `llm_score_borderline`. Currently identical to the
/// embedding input so the regressor and the LLM see the same prompt
/// context; the two could diverge later without affecting determinism.
pub(crate) fn build_pr_user_message(row: &PrInputRow) -> String {
    build_pr_embedding_input(row)
}

/// Embed every input through `backend.embed`, batched in chunks of
/// [`EMBED_BATCH`]. Returns one vector per input in the original order.
#[doc(hidden)]
pub fn embed_for_prs(
    backend: &dyn LocalLlmBackend,
    inputs: &[&str],
) -> anyhow::Result<Vec<Vec<f32>>> {
    let mut out = Vec::with_capacity(inputs.len());
    for chunk in inputs.chunks(EMBED_BATCH) {
        let vectors = backend.embed(chunk)?;
        out.extend(vectors);
    }
    Ok(out)
}

/// Public entry point. Loads PRs across every sprint in `sprint_ids`, runs
/// the local-hybrid pipeline, and updates `student_sprint_metrics.avg_doc_score`
/// per sprint. Returns aggregate counters.
pub fn run_local_hybrid_batch(
    conn: &Connection,
    sprint_ids: &[i64],
    config: &Config,
) -> anyhow::Result<BatchStats> {
    let bundle = PrRidgeBundle::load_optional(&config.evaluate.local.regressor_dir)?;
    let client = OllamaClient::from_config(&config.evaluate.local)?;
    debug!(
        "ollama client constructed; is_available={}",
        client.is_available()
    );
    run_local_hybrid_batch_with_backend(conn, sprint_ids, config, &client, bundle.as_ref())
}

/// Test seam — accepts an injected backend + optional regressor bundle
/// so integration tests in `tests/` can drive the pipeline without a
/// running ollama daemon. Marked `pub` to be reachable from the
/// integration-test crate (which cannot see `pub(crate)` items); the
/// `#[doc(hidden)]` marker keeps it off `cargo doc` output so callers
/// aren't tempted to invoke it from production code paths. Production
/// callers should go through [`run_local_hybrid_batch`].
#[doc(hidden)]
pub fn run_local_hybrid_batch_with_backend(
    conn: &Connection,
    sprint_ids: &[i64],
    config: &Config,
    backend: &dyn LocalLlmBackend,
    bundle: Option<&PrRidgeBundle>,
) -> anyhow::Result<BatchStats> {
    let mut stats = BatchStats::default();
    let policy = TriagePolicy::from_config(&config.evaluate.local);
    // Count format-unsupported events across the whole batch so we emit
    // exactly one operator-facing `warn!` per pipeline run instead of one
    // per offending PR (plan step 18).
    let mut format_unsupported_count: usize = 0;

    if bundle.is_none() {
        warn!(
            regressor_dir = %config.evaluate.local.regressor_dir.display(),
            "ridge regressor weights absent — every borderline PR will persist with 'local: regressor-disabled' (P2 has no LLM fallback)"
        );
    }

    for &sprint_id in sprint_ids {
        let rows = select_prs_for_sprint(conn, sprint_id)?;
        stats.items_total += rows.len();
        if rows.is_empty() {
            continue;
        }
        info!(sprint_id, prs = rows.len(), "local-hybrid batch starting");

        // Resume guard: drop PRs already scored for this sprint before
        // paying the embedding cost.
        let mut unscored: Vec<&PrInputRow> = Vec::with_capacity(rows.len());
        for row in &rows {
            if pr_already_scored(conn, &row.pr_id, sprint_id)? {
                stats.items_already_scored += 1;
            } else {
                unscored.push(row);
            }
        }
        if unscored.is_empty() {
            update_avg_doc_score(conn, sprint_id)?;
            continue;
        }

        // Predictions are keyed parallel to `unscored`. None marks
        // "regressor disabled" (no bundle); NaN total marks "dim mismatch"
        // (bundle present but BGE-M3 returned wrong shape).
        let predictions: Vec<Option<PrPrediction>> = match bundle {
            None => vec![None; unscored.len()],
            Some(b) => {
                let inputs: Vec<String> = unscored
                    .iter()
                    .map(|r| build_pr_embedding_input(r))
                    .collect();
                let input_refs: Vec<&str> = inputs.iter().map(|s| s.as_str()).collect();
                let embeddings = embed_for_prs(backend, &input_refs)?;
                embeddings
                    .into_iter()
                    .map(|emb| {
                        Some(PrPrediction {
                            title: b.title.predict(&emb),
                            description: b.description.predict(&emb),
                            total: b.total.predict(&emb),
                        })
                    })
                    .collect()
            }
        };

        for (row, pred) in unscored.iter().zip(predictions.iter()) {
            let flags = detect(row.title.as_deref(), row.body.as_deref());
            let decision = policy.decide(&flags, pred.as_ref());
            let context = RegressorContext::resolve(pred.as_ref());
            // Only invoke the LLM when the regressor's prediction is
            // healthy (Ok) and triage said `NeedsLlm`. Disabled /
            // DimMismatch keep their P2 fallback semantics.
            let llm_outcome = match (&decision, context) {
                (Decision::NeedsLlm { .. }, RegressorContext::Ok) => {
                    llm_score_borderline(backend, row)
                }
                _ => LlmOutcome::NotInvoked,
            };
            if matches!(llm_outcome, LlmOutcome::FormatUnsupported) {
                format_unsupported_count += 1;
            }
            match write_decision(conn, &row.pr_id, sprint_id, &decision, context, llm_outcome) {
                Ok(kind) => match kind {
                    DecisionKind::ShortCircuit => stats.short_circuited += 1,
                    DecisionKind::Snap => stats.regressor_only += 1,
                    DecisionKind::NeedsLlm => stats.llm_used += 1,
                },
                Err(e) => {
                    warn!(pr_id = %row.pr_id, sprint_id, error = %e, "local-hybrid persist failed");
                    stats.failures += 1;
                }
            }
        }
        update_avg_doc_score(conn, sprint_id)?;
    }
    if format_unsupported_count > 0 {
        warn!(
            count = format_unsupported_count,
            "ollama returned HTTP 400 on schema-constrained sampling — upgrade ollama to a version that supports the `format` field on /api/chat. \
             Affected PRs persisted with 'local: llm-format-unsupported' (snapped regressor mean)."
        );
    }
    info!(
        total = stats.items_total,
        already = stats.items_already_scored,
        short = stats.short_circuited,
        regressor = stats.regressor_only,
        llm = stats.llm_used,
        failures = stats.failures,
        format_unsupported = format_unsupported_count,
        "local-hybrid batch done"
    );
    Ok(stats)
}

enum DecisionKind {
    Snap,
    ShortCircuit,
    NeedsLlm,
}

/// Why did the regressor produce the prediction we have (or not)? Used to
/// pick the right justification on `Decision::NeedsLlm` rows. The triage
/// layer alone can't distinguish these cases — both yield `NeedsLlm`
/// with a zero / NaN regressor mean.
#[derive(Debug, Clone, Copy)]
enum RegressorContext {
    /// Bundle loaded and the prediction's `total` is finite. P2 still
    /// falls back to the snapped regressor mean for borderline `NeedsLlm`
    /// (no LLM until P3); the justification reads `local: regressor`.
    Ok,
    /// `PrRidgeBundle::load_optional` returned `None`. Persist
    /// `(0, 0, 0)` with `local: regressor-disabled`.
    Disabled,
    /// Bundle present but `RidgeHead::predict` returned NaN (embedding
    /// length ≠ `embedding_dim`). Persist `(0, 0, 0)` with
    /// `local: dim-mismatch`.
    DimMismatch,
}

impl RegressorContext {
    fn resolve(pred: Option<&PrPrediction>) -> Self {
        match pred {
            None => Self::Disabled,
            Some(p) if p.total.is_nan() => Self::DimMismatch,
            Some(_) => Self::Ok,
        }
    }
}

fn write_decision(
    conn: &Connection,
    pr_id: &str,
    sprint_id: i64,
    decision: &Decision,
    context: RegressorContext,
    llm_outcome: LlmOutcome,
) -> rusqlite::Result<DecisionKind> {
    match decision {
        Decision::Snap {
            title,
            description,
            total,
        } => {
            write_pr_row(
                conn,
                &PrPersistRow {
                    pr_id,
                    sprint_id,
                    title_score: *title,
                    description_score: *description,
                    total_doc_score: *total,
                    justification: "local: regressor".to_string(),
                },
            )?;
            Ok(DecisionKind::Snap)
        }
        Decision::ShortCircuit { kind, regressor } => {
            let (title_score, description_score, justification) = match kind {
                DetFlag::EmptyBody => (snap_title(regressor.title), 0.0, "local: empty body"),
                DetFlag::TaskIdOnlyBody => {
                    (snap_title(regressor.title), 0.0, "local: task-id-only body")
                }
                DetFlag::GenericTitle => (
                    0.0,
                    snap_description(regressor.description),
                    "local: generic title",
                ),
            };
            let total = title_score + description_score;
            write_pr_row(
                conn,
                &PrPersistRow {
                    pr_id,
                    sprint_id,
                    title_score,
                    description_score,
                    total_doc_score: total,
                    justification: justification.to_string(),
                },
            )?;
            Ok(DecisionKind::ShortCircuit)
        }
        Decision::NeedsLlm { regressor } => {
            let snapped_t = snap_title(regressor.title);
            let snapped_d = snap_description(regressor.description);
            let (title_score, description_score, justification) = match context {
                RegressorContext::Disabled => (0.0, 0.0, "local: regressor-disabled"),
                RegressorContext::DimMismatch => (0.0, 0.0, "local: dim-mismatch"),
                RegressorContext::Ok => match llm_outcome {
                    LlmOutcome::Refined { title, description } => {
                        (title, description, "local: regressor+llm")
                    }
                    LlmOutcome::FallbackFailed => {
                        (snapped_t, snapped_d, "local: llm-fallback-failed")
                    }
                    LlmOutcome::FormatUnsupported => {
                        (snapped_t, snapped_d, "local: llm-format-unsupported")
                    }
                    // Defensive: NotInvoked + Ok shouldn't happen — the caller
                    // always invokes the LLM for NeedsLlm/Ok. Persist the
                    // snapped regressor mean to keep the user contract
                    // (we never write zero scores for a healthy regressor).
                    LlmOutcome::NotInvoked => (snapped_t, snapped_d, "local: regressor"),
                },
            };
            let total = title_score + description_score;
            write_pr_row(
                conn,
                &PrPersistRow {
                    pr_id,
                    sprint_id,
                    title_score,
                    description_score,
                    total_doc_score: total,
                    justification: justification.to_string(),
                },
            )?;
            Ok(DecisionKind::NeedsLlm)
        }
    }
}

fn pr_already_scored(conn: &Connection, pr_id: &str, sprint_id: i64) -> rusqlite::Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
            params![pr_id, sprint_id],
            |r| r.get(0),
        )
        .ok();
    Ok(exists.is_some())
}

fn select_prs_for_sprint(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<PrInputRow>> {
    // Mirrors the SELECT in `evaluate_prs_via_cli` (crates/evaluate/src/llm_eval.rs:596)
    // with the resume-guard filter inlined.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT p.id, p.title, p.body, t.name, t2.name
         FROM pull_requests p
         JOIN task_pull_requests tpr ON tpr.pr_id = p.id
         JOIN tasks t ON t.id = tpr.task_id
         LEFT JOIN tasks t2 ON t2.id = t.parent_task_id
         WHERE t.sprint_id = ?
           AND t.type != 'USER_STORY'
           AND p.id NOT IN (
               SELECT pr_id FROM pr_doc_evaluation WHERE sprint_id = ?
           )
         ORDER BY p.id",
    )?;
    let rows: Vec<PrInputRow> = stmt
        .query_map(params![sprint_id, sprint_id], |r| {
            Ok(PrInputRow {
                pr_id: r.get::<_, String>(0)?,
                sprint_id,
                title: r.get::<_, Option<String>>(1)?,
                body: r.get::<_, Option<String>>(2)?,
                task_name: r.get::<_, Option<String>>(3)?,
                parent_story: r.get::<_, Option<String>>(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
