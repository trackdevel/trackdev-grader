//! Per-sprint PR documentation scoring.
//!
//! Backend selection driven by `[evaluate]` in `course.toml`:
//!
//! * `judge = "claude-cli"` (default) — shells out to the local Claude
//!   Code CLI per call. No API key. One subprocess per PR / per task;
//!   rate-limited by `judge_workers`. Stateless: each call is its own
//!   process. We deliberately do NOT reuse a process across PRs. The
//!   `--print` mode is non-interactive (no `/clear` slash command), and
//!   `--resume <session-id>` is the only way to share context — but it
//!   accumulates tokens linearly and the per-PR JSON contract is
//!   intrinsically independent, so the sharing buys nothing useful.
//!
//! * `judge = "anthropic-api"` — uses the Messages API directly.
//!   Requires `ANTHROPIC_API_KEY`. Keeps the existing `Conversation`
//!   accumulator so prompt-cache hits apply across PRs in the same team.
//!
//! Missing prerequisites for the selected backend silently fall back to
//! the deterministic heuristic; running without an LLM is a supported
//! mode.

use fancy_regex::Regex as FancyRegex;
use once_cell::sync::Lazy;
use rayon::prelude::*;
use regex::Regex;
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::Value;
use sprint_grader_core::Config;
use tracing::{info, warn};

use crate::claude_cli_client::ClaudeCliClient;
use crate::llm_client::{AnthropicClient, Conversation};

type LlmPrRow = (
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

// Same shape as LlmPrRow plus the parent_task_id, used by the CLI
// dispatcher which resolves parent-story names ahead of the parallel
// section (no shared Connection inside workers).
type CliPrRow = (
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// PR documentation rubric response shape. `total_doc_score` is optional
/// because the model sometimes omits it; we fall back to the sum.
#[derive(Debug, Deserialize)]
struct PrDocResponse {
    title_score: i64,
    description_score: i64,
    #[serde(default)]
    total_doc_score: Option<i64>,
    #[serde(default)]
    justification: String,
}

/// Task description rubric response shape.
#[derive(Debug, Deserialize)]
struct TaskEvalResponse {
    quality_score: f64,
    #[serde(default)]
    justification: String,
}

// ---- Rubric prompts ----
// Sourced from `assets/prompts/`. Baked in at build time via `include_str!`
// so the binary stays self-contained, but each rubric lives in its own .md
// file for easy per-semester tuning without touching Rust.

const RUBRIC_PR: &str = include_str!("../assets/prompts/rubric_pr.md");
const RUBRIC_TASK: &str = include_str!("../assets/prompts/rubric_task.md");

// ---- Heuristic scoring (used when no API key is set) ----

static GENERIC_TITLE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        ^.{0,9}$
        | ^fix\s*(bug)?$
        | ^update[sd]?$
        | ^change[sd]?$
        | ^wip$
        | ^test$
        | ^[A-Z]+-\d+$
        | ^(feature|bugfix|hotfix)/
        ",
    )
    .expect("generic title regex")
});

static TASK_ID_ONLY: Lazy<FancyRegex> =
    Lazy::new(|| FancyRegex::new(r"^(\s*[A-Za-z]+-\d+\s*[,;]?\s*)+$").expect("task id regex"));

// Markdown-linked counterpart of TASK_ID_ONLY: bodies whose entire content
// is one or more `[task-id](url)` links (e.g. `[p4d-194](https://...)`).
// Content-free but bypasses TASK_ID_ONLY because of the bracket/paren syntax.
// Anchor text allows alphanumeric prefixes (e.g. `p4d`) that real Trackdev
// project keys use, not just plain letters.
static TASK_MD_LINK_ONLY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(\s*\[[A-Za-z][A-Za-z0-9]*-\d+\]\([^)]+\)\s*[,;]?\s*)+$")
        .expect("task md-link regex")
});

fn heuristic_title_score(title: Option<&str>) -> i64 {
    let title = match title {
        Some(t) => t.trim(),
        None => return 0,
    };
    if title.len() < 5 {
        return 0;
    }
    if GENERIC_TITLE.is_match(title) {
        return 0;
    }
    if title.len() < 20 {
        1
    } else {
        2
    }
}

fn heuristic_description_score(body: Option<&str>) -> i64 {
    let body = match body {
        Some(b) => b.trim(),
        None => return 0,
    };
    if body.len() < 20 {
        return 0;
    }
    if TASK_ID_ONLY.is_match(body).unwrap_or(false) || TASK_MD_LINK_ONLY.is_match(body) {
        return 0;
    }
    if body.len() < 50 {
        return 1;
    }
    let lower = body.to_lowercase();
    let has_what = ["what", "change", "add", "implement", "fix"]
        .iter()
        .any(|k| lower.contains(k));
    let has_why = ["why", "because", "reason", "in order to"]
        .iter()
        .any(|k| lower.contains(k));
    let has_ref = ["task", "story", "issue", "ticket", "#"]
        .iter()
        .any(|k| lower.contains(k));
    let has_test = ["test", "verify", "check", "how to"]
        .iter()
        .any(|k| lower.contains(k));

    let mut score = 1;
    if has_what && has_ref {
        score = 2;
    }
    if has_what && has_why {
        score = 3;
    }
    if has_what && has_why && (has_test || has_ref) {
        score = 4;
    }
    score
}

// ---- Task-description quantization ----

const TASK_LEVELS: [f64; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];

fn quantize_task_score(raw: f64) -> f64 {
    let c = raw.clamp(0.0, 1.0);
    *TASK_LEVELS
        .iter()
        .min_by(|a, b| (*a - c).abs().partial_cmp(&(*b - c).abs()).unwrap())
        .unwrap()
}

fn heuristic_task_description_score(name: Option<&str>) -> f64 {
    let name = match name {
        Some(n) => n.trim(),
        None => return 0.0,
    };
    if name.len() < 5 {
        return 0.0;
    }
    if name.len() < 15 {
        return 0.2;
    }
    let lower = name.to_lowercase();
    let has_verb = [
        "create",
        "implement",
        "add",
        "fix",
        "update",
        "remove",
        "configure",
        "design",
        "test",
        "integrate",
        "refactor",
        "set up",
        "build",
        "handle",
        "validate",
        "display",
        "show",
    ]
    .iter()
    .any(|k| lower.contains(k));

    static SPECIFICS: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)(endpoint|screen|page|button|field|table|model|service|controller|repository|view|fragment|activity|api|database|layout|menu|dialog)",
        )
        .unwrap()
    });
    let has_specifics = SPECIFICS.is_match(name);

    let mut score = 0.2;
    if has_verb {
        score += 0.2;
    }
    if has_specifics {
        score += 0.2;
    }
    if name.len() > 40 {
        score += 0.2;
    }
    if name.len() > 80 {
        score += 0.2;
    }
    quantize_task_score(score)
}

// ---- JSON-extraction helper for LLM responses ----

/// The model is instructed to reply with ONLY a JSON object, but models sometimes
/// wrap JSON in a markdown fence or add trailing prose. Extract the first `{...}`
/// balanced block and try to parse it; fall through to a plain parse if that
/// fails (which also works for pure JSON replies).
fn extract_json_object(s: &str) -> Option<Value> {
    // Strip common ```json fences.
    let trimmed = s.trim();
    let candidates = [
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim(),
        trimmed,
    ];
    for c in candidates {
        if let Ok(v) = serde_json::from_str::<Value>(c) {
            if v.is_object() {
                return Some(v);
            }
        }
    }
    // Last resort: find the first balanced {...} span.
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'{' {
            if depth == 0 {
                start = Some(i);
            }
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                if let Some(s0) = start {
                    if let Ok(v) = serde_json::from_str::<Value>(&s[s0..=i]) {
                        if v.is_object() {
                            return Some(v);
                        }
                    }
                }
            }
        }
    }
    None
}

// ---- Public entry points ----

/// PR doc evaluation for a single sprint. Selects the LLM backend per
/// `config.evaluate.judge`; falls back to the deterministic heuristic
/// when prerequisites are missing or `use_llm = false`. After scoring,
/// updates `avg_doc_score` on `student_sprint_metrics`.
pub fn run_pr_doc_evaluation_for_sprint_id(
    conn: &Connection,
    sprint_id: i64,
    config: &Config,
    use_llm: bool,
) -> rusqlite::Result<usize> {
    let mut count = 0usize;

    if !use_llm {
        info!("pr_doc_evaluation: heuristic-only path requested");
        count += evaluate_prs_heuristic(conn, sprint_id)?;
    } else {
        match config.evaluate.judge.as_str() {
            "claude-cli" => {
                if !ClaudeCliClient::is_available(&config.evaluate.claude_cli_path) {
                    info!(
                        cli = %config.evaluate.claude_cli_path,
                        "claude CLI not on PATH — heuristic PR doc scoring fallback"
                    );
                    count += evaluate_prs_heuristic(conn, sprint_id)?;
                } else {
                    let client = ClaudeCliClient::new(
                        config.evaluate.claude_cli_path.clone(),
                        config.evaluate.model_id.clone(),
                        config.evaluate.judge_timeout_seconds,
                    );
                    count += evaluate_prs_via_cli(
                        conn,
                        sprint_id,
                        &client,
                        config.evaluate.judge_workers,
                    )?;
                }
            }
            "anthropic-api" => {
                let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
                if api_key.is_empty() {
                    info!(
                        "judge=anthropic-api but ANTHROPIC_API_KEY not set — heuristic PR doc scoring fallback"
                    );
                    count += evaluate_prs_heuristic(conn, sprint_id)?;
                } else {
                    let model = std::env::var("ANTHROPIC_MODEL")
                        .unwrap_or_else(|_| config.evaluate.model_id.clone());
                    match AnthropicClient::new(&api_key, model) {
                        Ok(client) => count += evaluate_prs_llm(conn, sprint_id, &client)?,
                        Err(e) => {
                            warn!(error = %e, "Anthropic client init failed — heuristic fallback");
                            count += evaluate_prs_heuristic(conn, sprint_id)?;
                        }
                    }
                }
            }
            other => {
                warn!(
                    judge = %other,
                    "unknown [evaluate] judge — heuristic PR doc scoring fallback"
                );
                count += evaluate_prs_heuristic(conn, sprint_id)?;
            }
        }
    }

    update_avg_doc_score(conn, sprint_id)?;
    Ok(count)
}

/// Back-compat wrapper: always tries LLM-then-heuristic. Used by the
/// `sprint-grader evaluate` CLI subcommand.
pub fn run_llm_evaluation_for_sprint_id(
    conn: &Connection,
    sprint_id: i64,
    config: &Config,
) -> rusqlite::Result<usize> {
    run_pr_doc_evaluation_for_sprint_id(conn, sprint_id, config, true)
}

fn evaluate_prs_llm(
    conn: &Connection,
    sprint_id: i64,
    client: &AnthropicClient,
) -> rusqlite::Result<usize> {
    // Group PRs by project — one conversation per team.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sp.project_id, p.name
         FROM sprints sp
         LEFT JOIN projects p ON p.id = sp.project_id
         WHERE sp.id = ?",
    )?;
    let teams: Vec<(i64, Option<String>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut count = 0usize;
    for (project_id, team_name) in teams {
        let team_label = team_name.unwrap_or_else(|| format!("project_{project_id}"));
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name,
                    pr.title, pr.body, t.name AS task_name
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             JOIN students s ON s.id = pr.author_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
               AND s.team_project_id = ?
             ORDER BY pr.pr_number",
        )?;
        let prs: Vec<LlmPrRow> = stmt
            .query_map(params![sprint_id, project_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        if prs.is_empty() {
            continue;
        }

        // Resume support — skip PRs that already have a scored row.
        let mut to_evaluate: Vec<_> = Vec::with_capacity(prs.len());
        let mut already = 0usize;
        for pr in &prs {
            let exists: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
                    params![pr.0, sprint_id],
                    |r| r.get(0),
                )
                .ok();
            if exists.is_some() {
                already += 1;
            } else {
                to_evaluate.push(pr);
            }
        }
        if to_evaluate.is_empty() {
            info!(team = %team_label, total = prs.len(), "all PRs already evaluated");
            continue;
        }
        info!(team = %team_label, evaluating = to_evaluate.len(), total = prs.len(), already);

        let mut conv = Conversation::new(client, RUBRIC_PR);
        for (pr_id, pr_number, repo, title, body, task_name) in to_evaluate {
            let parent_story: String = conn
                .query_row(
                    "SELECT t2.name FROM tasks t
                     JOIN tasks t2 ON t2.id = t.parent_task_id
                     JOIN task_pull_requests tpr ON tpr.task_id = t.id
                     WHERE tpr.pr_id = ? AND t.type != 'USER_STORY' LIMIT 1",
                    [pr_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .unwrap_or_else(|| "N/A".to_string());
            let task_name = task_name.as_deref().unwrap_or("");
            let repo_name = repo.as_deref().unwrap_or("");
            let title_str = title.as_deref().unwrap_or("");
            let body_str = body.as_deref().unwrap_or("(empty)");
            let pr_num_str = pr_number.map(|n| n.to_string()).unwrap_or_default();
            let msg = format!(
                "Task: {task_name}\nUser Story: {parent_story}\nPR #{pr_num_str} in {repo_name}\nTitle: {title_str}\nDescription:\n{body_str}"
            );

            let reply = match conv.ask(&msg) {
                Ok(r) => r,
                Err(e) => {
                    warn!(pr_id, error = %e, "LLM call failed — stopping team eval");
                    break;
                }
            };
            let parsed = extract_json_object(&reply).or_else(|| {
                // One retry — same contract as the Python side.
                match conv.ask("Please respond with ONLY a JSON object as specified.") {
                    Ok(r2) => extract_json_object(&r2),
                    Err(_) => None,
                }
            });
            let parsed = match parsed {
                Some(p) => p,
                None => {
                    warn!(pr_id, "could not parse LLM JSON — skipping");
                    continue;
                }
            };
            let resp: PrDocResponse = match serde_json::from_value(parsed.clone()) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        pr_id,
                        error = %e,
                        raw = %parsed,
                        "LLM response schema mismatch — skipping"
                    );
                    continue;
                }
            };
            let total = resp
                .total_doc_score
                .unwrap_or(resp.title_score + resp.description_score);

            conn.execute(
                "INSERT OR REPLACE INTO pr_doc_evaluation
                 (pr_id, sprint_id, title_score, description_score,
                  total_doc_score, justification)
                 VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    pr_id,
                    sprint_id,
                    resp.title_score,
                    resp.description_score,
                    total,
                    resp.justification
                ],
            )?;
            count += 1;
        }
    }
    Ok(count)
}

fn evaluate_prs_via_cli(
    conn: &Connection,
    sprint_id: i64,
    client: &ClaudeCliClient,
    workers: usize,
) -> rusqlite::Result<usize> {
    // Mirrors evaluate_prs_llm's iteration but issues one stateless
    // claude-cli call per PR. PRs across all teams in this sprint are
    // collected in one batch, then judged in parallel through a Rayon
    // worker pool. DB writes happen serially after the batch returns —
    // this connection is not shared with workers.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name,
                pr.title, pr.body, t.name AS task_name, t.parent_task_id
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
         ORDER BY pr.repo_full_name, pr.pr_number",
    )?;
    let prs: Vec<CliPrRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if prs.is_empty() {
        return Ok(0);
    }

    // Resume support — skip PRs that already have a scored row.
    let mut to_evaluate: Vec<_> = Vec::with_capacity(prs.len());
    let mut already = 0usize;
    for pr in prs {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
                params![pr.0, sprint_id],
                |r| r.get(0),
            )
            .ok();
        if exists.is_some() {
            already += 1;
        } else {
            to_evaluate.push(pr);
        }
    }
    if to_evaluate.is_empty() {
        info!(total = already, "all PRs already evaluated (CLI backend)");
        return Ok(0);
    }
    info!(
        evaluating = to_evaluate.len(),
        already, "claude-cli PR doc scoring"
    );

    // Resolve parent-story names ahead of the parallel section so each
    // worker has only the data it needs (no shared Connection).
    let prepared: Vec<(String, String)> = to_evaluate
        .into_iter()
        .map(
            |(pr_id, pr_number, repo, title, body, task_name, parent_id)| {
                let parent_story = match parent_id {
                    Some(pid) => conn
                        .query_row("SELECT name FROM tasks WHERE id = ?", [pid], |r| {
                            r.get::<_, Option<String>>(0)
                        })
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "N/A".to_string()),
                    None => "N/A".to_string(),
                };
                let pr_num_str = pr_number.map(|n| n.to_string()).unwrap_or_default();
                let repo_name = repo.as_deref().unwrap_or("");
                let title_str = title.as_deref().unwrap_or("");
                let body_str = body.as_deref().unwrap_or("(empty)");
                let task_str = task_name.as_deref().unwrap_or("");
                let user_msg = format!(
                    "Task: {task_str}\nUser Story: {parent_story}\nPR #{pr_num_str} in {repo_name}\nTitle: {title_str}\nDescription:\n{body_str}\n\nReturn ONLY the JSON object specified by the rubric — no prose, no fences."
                );
                (pr_id, user_msg)
            },
        )
        .collect();

    // Parallel CLI invocations through a bounded Rayon pool. Each worker
    // spawns its own subprocess; the OS schedules them.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers.max(1))
        .build()
        .map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(format!(
                "claude-cli rayon pool: {e}"
            ))))
        })?;

    let results: Vec<(String, Option<PrDocResponse>)> = pool.install(|| {
        prepared
            .par_iter()
            .map(|(pr_id, user_msg)| {
                let raw = match client.complete(RUBRIC_PR, user_msg) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(pr_id = %pr_id, error = %e, "claude-cli call failed");
                        return (pr_id.clone(), None);
                    }
                };
                let parsed = extract_json_object(&raw);
                let resp = parsed.and_then(|v| match serde_json::from_value::<PrDocResponse>(v) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        warn!(pr_id = %pr_id, error = %e, "schema mismatch from CLI");
                        None
                    }
                });
                (pr_id.clone(), resp)
            })
            .collect()
    });

    let mut count = 0usize;
    for (pr_id, resp) in results {
        let Some(resp) = resp else {
            continue;
        };
        let total = resp
            .total_doc_score
            .unwrap_or(resp.title_score + resp.description_score);
        conn.execute(
            "INSERT OR REPLACE INTO pr_doc_evaluation
             (pr_id, sprint_id, title_score, description_score,
              total_doc_score, justification)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                pr_id,
                sprint_id,
                resp.title_score,
                resp.description_score,
                total,
                resp.justification
            ],
        )?;
        count += 1;
    }
    Ok(count)
}

fn evaluate_prs_heuristic(conn: &Connection, sprint_id: i64) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.title, pr.body
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    let prs: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut count = 0usize;
    for (pr_id, title, body) in prs {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
                params![pr_id, sprint_id],
                |r| r.get(0),
            )
            .ok();
        if exists.is_some() {
            continue;
        }
        let title_score = heuristic_title_score(title.as_deref());
        let description_score = heuristic_description_score(body.as_deref());
        conn.execute(
            "INSERT OR REPLACE INTO pr_doc_evaluation
             (pr_id, sprint_id, title_score, description_score,
              total_doc_score, justification)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                pr_id,
                sprint_id,
                title_score,
                description_score,
                title_score + description_score,
                "Scored by deterministic heuristics (LLM unavailable)"
            ],
        )?;
        count += 1;
    }
    Ok(count)
}

fn update_avg_doc_score(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT t.assignee_id FROM tasks t
         WHERE t.sprint_id = ? AND t.assignee_id IS NOT NULL AND t.type != 'USER_STORY'",
    )?;
    let ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for sid in ids {
        let avg: Option<f64> = conn
            .query_row(
                "SELECT AVG(pde.total_doc_score) FROM pr_doc_evaluation pde
                 JOIN pull_requests pr ON pr.id = pde.pr_id
                 JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                 JOIN tasks t ON t.id = tpr.task_id
                 WHERE t.sprint_id = ? AND t.assignee_id = ? AND t.type != 'USER_STORY'",
                params![sprint_id, sid],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten();
        if let Some(a) = avg {
            conn.execute(
                "UPDATE student_sprint_metrics SET avg_doc_score = ?
                 WHERE student_id = ? AND sprint_id = ?",
                params![a, sid, sprint_id],
            )?;
        }
    }
    Ok(())
}

// ---- Task descriptions ----

pub fn score_task_descriptions_for_sprint_id(
    conn: &Connection,
    sprint_id: i64,
    config: &Config,
) -> rusqlite::Result<usize> {
    if !config.repo_analysis.quality_eval_tasks {
        return Ok(0);
    }
    match config.evaluate.judge.as_str() {
        "claude-cli" => {
            if !ClaudeCliClient::is_available(&config.evaluate.claude_cli_path) {
                info!(
                    cli = %config.evaluate.claude_cli_path,
                    "claude CLI not on PATH — heuristic task scoring fallback"
                );
                return evaluate_tasks_heuristic(conn, sprint_id);
            }
            let client = ClaudeCliClient::new(
                config.evaluate.claude_cli_path.clone(),
                config.evaluate.model_id.clone(),
                config.evaluate.judge_timeout_seconds,
            );
            evaluate_tasks_via_cli(conn, sprint_id, &client, config.evaluate.judge_workers)
        }
        "anthropic-api" => {
            let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
            if api_key.is_empty() {
                return evaluate_tasks_heuristic(conn, sprint_id);
            }
            let model = std::env::var("ANTHROPIC_MODEL")
                .unwrap_or_else(|_| config.evaluate.model_id.clone());
            match AnthropicClient::new(&api_key, model) {
                Ok(client) => evaluate_tasks_llm(conn, sprint_id, &client),
                Err(e) => {
                    warn!(error = %e, "client init failed — heuristic task scoring");
                    evaluate_tasks_heuristic(conn, sprint_id)
                }
            }
        }
        other => {
            warn!(judge = %other, "unknown [evaluate] judge — heuristic task scoring");
            evaluate_tasks_heuristic(conn, sprint_id)
        }
    }
}

fn evaluate_tasks_llm(
    conn: &Connection,
    sprint_id: i64,
    client: &AnthropicClient,
) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, task_key, name, parent_task_id FROM tasks
         WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'
         ORDER BY task_key",
    )?;
    let tasks: Vec<(i64, String, Option<String>, Option<i64>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if tasks.is_empty() {
        return Ok(0);
    }
    let mut conv = Conversation::new(client, RUBRIC_TASK);
    let mut count = 0usize;
    for (task_id, task_key, name, parent_id) in tasks {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM task_description_evaluation WHERE task_id = ? AND sprint_id = ?",
                params![task_id, sprint_id],
                |r| r.get(0),
            )
            .ok();
        if exists.is_some() {
            continue;
        }
        let parent_name: String = match parent_id {
            Some(pid) => conn
                .query_row("SELECT name FROM tasks WHERE id = ?", [pid], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "N/A".to_string()),
            None => "N/A".to_string(),
        };
        let msg = format!(
            "Task key: {task_key}\nUser Story: {parent_name}\nTask description: {}",
            name.as_deref().unwrap_or("(empty)"),
        );
        let reply = match conv.ask(&msg) {
            Ok(r) => r,
            Err(e) => {
                warn!(task_key = %task_key, error = %e, "LLM task call failed");
                break;
            }
        };
        let parsed = extract_json_object(&reply).or_else(|| {
            conv.ask("Please respond with ONLY a JSON object as specified.")
                .ok()
                .and_then(|r2| extract_json_object(&r2))
        });
        let (score, justification) = match parsed {
            Some(p) => match serde_json::from_value::<TaskEvalResponse>(p.clone()) {
                Ok(r) => (quantize_task_score(r.quality_score), r.justification),
                Err(e) => {
                    warn!(
                        task_key = %task_key,
                        error = %e,
                        raw = %p,
                        "LLM task response schema mismatch — heuristic fallback"
                    );
                    let s = heuristic_task_description_score(name.as_deref());
                    (
                        s,
                        "Heuristic fallback (LLM response schema mismatch)".to_string(),
                    )
                }
            },
            None => {
                let s = heuristic_task_description_score(name.as_deref());
                (
                    s,
                    "Heuristic fallback (LLM response unparseable)".to_string(),
                )
            }
        };
        conn.execute(
            "INSERT OR REPLACE INTO task_description_evaluation
             (task_id, sprint_id, quality_score, justification)
             VALUES (?, ?, ?, ?)",
            params![task_id, sprint_id, score, justification],
        )?;
        count += 1;
    }
    Ok(count)
}

fn evaluate_tasks_via_cli(
    conn: &Connection,
    sprint_id: i64,
    client: &ClaudeCliClient,
    workers: usize,
) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, task_key, name, parent_task_id FROM tasks
         WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'
         ORDER BY task_key",
    )?;
    let tasks: Vec<(i64, String, Option<String>, Option<i64>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if tasks.is_empty() {
        return Ok(0);
    }

    let mut to_evaluate: Vec<(i64, String, Option<String>, Option<i64>)> =
        Vec::with_capacity(tasks.len());
    for t in tasks {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM task_description_evaluation WHERE task_id = ? AND sprint_id = ?",
                params![t.0, sprint_id],
                |r| r.get(0),
            )
            .ok();
        if exists.is_none() {
            to_evaluate.push(t);
        }
    }
    if to_evaluate.is_empty() {
        return Ok(0);
    }

    let prepared: Vec<(i64, Option<String>, String)> = to_evaluate
        .into_iter()
        .map(|(task_id, task_key, name, parent_id)| {
            let parent_name: String = match parent_id {
                Some(pid) => conn
                    .query_row("SELECT name FROM tasks WHERE id = ?", [pid], |r| {
                        r.get::<_, Option<String>>(0)
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "N/A".to_string()),
                None => "N/A".to_string(),
            };
            let user_msg = format!(
                "Task key: {task_key}\nUser Story: {parent_name}\nTask description: {}\n\nReturn ONLY the JSON object specified by the rubric.",
                name.as_deref().unwrap_or("(empty)"),
            );
            (task_id, name, user_msg)
        })
        .collect();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers.max(1))
        .build()
        .map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(format!(
                "claude-cli rayon pool: {e}"
            ))))
        })?;

    let results: Vec<(i64, f64, String)> = pool.install(|| {
        prepared
            .par_iter()
            .map(|(task_id, name, user_msg)| {
                let raw = match client.complete(RUBRIC_TASK, user_msg) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(task_id, error = %e, "claude-cli task call failed");
                        let s = heuristic_task_description_score(name.as_deref());
                        return (
                            *task_id,
                            s,
                            "Heuristic fallback (CLI call failed)".to_string(),
                        );
                    }
                };
                match extract_json_object(&raw)
                    .and_then(|v| serde_json::from_value::<TaskEvalResponse>(v).ok())
                {
                    Some(r) => (
                        *task_id,
                        quantize_task_score(r.quality_score),
                        r.justification,
                    ),
                    None => {
                        let s = heuristic_task_description_score(name.as_deref());
                        (
                            *task_id,
                            s,
                            "Heuristic fallback (CLI response unparseable)".to_string(),
                        )
                    }
                }
            })
            .collect()
    });

    let mut count = 0usize;
    for (task_id, score, justification) in results {
        conn.execute(
            "INSERT OR REPLACE INTO task_description_evaluation
             (task_id, sprint_id, quality_score, justification)
             VALUES (?, ?, ?, ?)",
            params![task_id, sprint_id, score, justification],
        )?;
        count += 1;
    }
    Ok(count)
}

fn evaluate_tasks_heuristic(conn: &Connection, sprint_id: i64) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, name FROM tasks
         WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'",
    )?;
    let tasks: Vec<(i64, Option<String>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut count = 0usize;
    for (task_id, name) in tasks {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM task_description_evaluation WHERE task_id = ? AND sprint_id = ?",
                params![task_id, sprint_id],
                |r| r.get(0),
            )
            .ok();
        if exists.is_some() {
            continue;
        }
        let score = heuristic_task_description_score(name.as_deref());
        conn.execute(
            "INSERT OR REPLACE INTO task_description_evaluation
             (task_id, sprint_id, quality_score, justification)
             VALUES (?, ?, ?, ?)",
            params![
                task_id,
                sprint_id,
                score,
                "Scored by deterministic heuristics (LLM unavailable)"
            ],
        )?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_title_matches_python() {
        assert_eq!(heuristic_title_score(None), 0);
        assert_eq!(heuristic_title_score(Some("fix")), 0);
        assert_eq!(heuristic_title_score(Some("PROJ-42")), 0);
        assert_eq!(heuristic_title_score(Some("login changes")), 1);
        assert_eq!(
            heuristic_title_score(Some("Implement login controller with JWT")),
            2
        );
    }

    #[test]
    fn heuristic_description_matches_python() {
        assert_eq!(heuristic_description_score(None), 0);
        assert_eq!(heuristic_description_score(Some("short")), 0);
        assert_eq!(
            heuristic_description_score(Some("PDS-123 pds-44")),
            0,
            "task-id only must score 0"
        );
        // Markdown-linked task-id only must also score 0 (T-P0.5).
        assert_eq!(
            heuristic_description_score(Some(
                "[p4d-194](https://trackdev.org/dashboard/tasks/5075)"
            )),
            0,
            "single md-link to task must score 0"
        );
        assert_eq!(
            heuristic_description_score(Some(
                "[p4d-194](https://example.com), [p4d-195](https://example.com)"
            )),
            0,
            "comma-separated md-links must score 0"
        );
        assert!(
            heuristic_description_score(Some(
                "[p4d-194](https://example.com) Adds the user endpoint."
            )) > 0,
            "md-link followed by real prose must score > 0"
        );
        // 20..50 chars, no structure → 1
        assert_eq!(
            heuristic_description_score(Some("Added login form with basic auth")),
            1
        );
        // what + ref → 2
        assert_eq!(
            heuristic_description_score(Some(
                "Implement login controller and wire it to the existing auth service task."
            )),
            2
        );
        // what + why + ref + test → 4
        assert_eq!(
            heuristic_description_score(Some(
                "Implement the login controller because users could not sign in. \
                 Linked to task PDS-42; verify by running the auth test suite."
            )),
            4
        );
    }

    #[test]
    fn task_score_quantization_snaps_to_grid() {
        for (raw, want) in [
            (0.0, 0.0),
            (0.1, 0.0),
            (0.19, 0.2),
            (0.45, 0.4),
            (0.51, 0.6),
            (1.2, 1.0),
        ] {
            let got = quantize_task_score(raw);
            assert!(
                (got - want).abs() < 1e-9,
                "raw={raw}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn extract_json_handles_fenced_and_extra_prose() {
        let fenced = "```json\n{\"pr_number\": 7, \"title_score\": 2}\n```";
        let v = extract_json_object(fenced).unwrap();
        assert_eq!(v["pr_number"].as_i64(), Some(7));

        let prefixed =
            "Here is my evaluation:\n{\"title_score\": 1, \"description_score\": 2}\nThanks.";
        let v2 = extract_json_object(prefixed).unwrap();
        assert_eq!(v2["description_score"].as_i64(), Some(2));
    }

    fn build_minimal_config() -> Config {
        // Config::default() is not implemented; round-trip a tiny course.toml
        // through Config::load. Use a process-unique dir under the OS temp
        // root to avoid pulling tempfile into evaluate's dev-deps.
        let dir = std::env::temp_dir().join(format!(
            "sprint_grader_evaluate_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("course.toml"),
            r#"
[course]
name = "test"
num_sprints = 1
pm_base_url = "https://example.invalid"
github_org = "udg-pds"
course_id = 5

[thresholds]
carrying_team_pct = 0.40
cramming_hours = 48
cramming_commit_pct = 0.70
single_commit_dump_lines = 200
micro_pr_max_lines = 10
low_doc_score = 2
contribution_imbalance_stddev = 1.5

[build]
max_parallel_builds = 1
stderr_max_chars = 2000
skip_already_tested = true

[regularity]

[repo_analysis]

[architecture]
model_id = "claude-haiku-4-5-20251001"

[evaluate]
model_id = "claude-haiku-4-5-20251001"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("user_mapping.csv"),
            "trackdev_username,github_username,enrollment_id,team_id\n",
        )
        .unwrap();
        Config::load(&dir).unwrap()
    }

    #[test]
    fn pr_doc_use_llm_false_uses_heuristic() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE students (id TEXT PRIMARY KEY, full_name TEXT,
                github_login TEXT, team_project_id INTEGER);
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, task_key TEXT, name TEXT,
                type TEXT, status TEXT, estimation_points INTEGER,
                assignee_id TEXT, sprint_id INTEGER, parent_task_id INTEGER);
             CREATE TABLE pull_requests (id TEXT PRIMARY KEY, pr_number INTEGER,
                repo_full_name TEXT, title TEXT, body TEXT, author_id TEXT);
             CREATE TABLE task_pull_requests (task_id INTEGER, pr_id TEXT,
                PRIMARY KEY (task_id, pr_id));
             CREATE TABLE pr_doc_evaluation (pr_id TEXT, sprint_id INTEGER,
                title_score INTEGER, description_score INTEGER,
                total_doc_score INTEGER, justification TEXT,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE student_sprint_metrics (student_id TEXT, sprint_id INTEGER,
                avg_doc_score REAL, PRIMARY KEY (student_id, sprint_id));
             INSERT INTO students(id, full_name, team_project_id)
                VALUES ('alice', 'Alice', 1);
             INSERT INTO tasks(id, task_key, name, type, status, sprint_id, assignee_id)
                VALUES (1, 'PDS-1', 'login', 'TASK', 'DONE', 10, 'alice');
             INSERT INTO pull_requests(id, pr_number, repo_full_name, title, body, author_id)
                VALUES ('pr-1', 7, 'org/repo',
                        'Implement login controller with JWT',
                        'Implement the login controller because users could not sign in. \
                         Linked to task PDS-42; verify by running the auth test suite.',
                        'alice');
             INSERT INTO task_pull_requests(task_id, pr_id) VALUES (1, 'pr-1');
             INSERT INTO student_sprint_metrics(student_id, sprint_id, avg_doc_score)
                VALUES ('alice', 10, NULL);",
        )
        .unwrap();

        let cfg = build_minimal_config();
        // Set ANTHROPIC_API_KEY so we can prove use_llm=false really skips it.
        // SAFETY: tests run in a single process; we restore afterward.
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-should-not-be-used") };
        let count = run_pr_doc_evaluation_for_sprint_id(&conn, 10, &cfg, false).unwrap();
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };

        assert_eq!(count, 1);

        let (pr_id, total, justification): (String, i64, String) = conn
            .query_row(
                "SELECT pr_id, total_doc_score, justification FROM pr_doc_evaluation
                 WHERE sprint_id = 10",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(pr_id, "pr-1");
        assert!(
            total > 0,
            "heuristic should award positive score for rich PR"
        );
        assert!(
            justification.contains("heuristics"),
            "justification must mark heuristic origin"
        );

        let avg: Option<f64> = conn
            .query_row(
                "SELECT avg_doc_score FROM student_sprint_metrics
                 WHERE student_id = 'alice' AND sprint_id = 10",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(avg.is_some(), "avg_doc_score must be populated");
        assert!((avg.unwrap() - total as f64).abs() < 1e-9);
    }

    #[test]
    fn pr_doc_cli_judge_falls_back_to_heuristic_when_binary_missing() {
        // Default config has judge = "claude-cli" with claude_cli_path =
        // "claude". We override claude_cli_path to a non-existent binary
        // and assert the dispatcher does not hard-fail and instead writes
        // a heuristic-marked row.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE students (id TEXT PRIMARY KEY, full_name TEXT,
                github_login TEXT, team_project_id INTEGER);
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, task_key TEXT, name TEXT,
                type TEXT, status TEXT, estimation_points INTEGER,
                assignee_id TEXT, sprint_id INTEGER, parent_task_id INTEGER);
             CREATE TABLE pull_requests (id TEXT PRIMARY KEY, pr_number INTEGER,
                repo_full_name TEXT, title TEXT, body TEXT, author_id TEXT);
             CREATE TABLE task_pull_requests (task_id INTEGER, pr_id TEXT,
                PRIMARY KEY (task_id, pr_id));
             CREATE TABLE pr_doc_evaluation (pr_id TEXT, sprint_id INTEGER,
                title_score INTEGER, description_score INTEGER,
                total_doc_score INTEGER, justification TEXT,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE student_sprint_metrics (student_id TEXT, sprint_id INTEGER,
                avg_doc_score REAL, PRIMARY KEY (student_id, sprint_id));
             INSERT INTO students(id, full_name, team_project_id)
                VALUES ('bob', 'Bob', 1);
             INSERT INTO tasks(id, task_key, name, type, status, sprint_id, assignee_id)
                VALUES (1, 'PDS-1', 'login', 'TASK', 'DONE', 10, 'bob');
             INSERT INTO pull_requests(id, pr_number, repo_full_name, title, body, author_id)
                VALUES ('pr-1', 7, 'org/repo',
                        'Implement login controller with JWT',
                        'Implement the login controller because users could not sign in. \
                         Linked to task PDS-42; verify by running the auth test suite.',
                        'bob');
             INSERT INTO task_pull_requests(task_id, pr_id) VALUES (1, 'pr-1');
             INSERT INTO student_sprint_metrics(student_id, sprint_id, avg_doc_score)
                VALUES ('bob', 10, NULL);",
        )
        .unwrap();

        let mut cfg = build_minimal_config();
        cfg.evaluate.judge = "claude-cli".to_string();
        cfg.evaluate.claude_cli_path = "/definitely/not/a/real/binary-xyz".to_string();
        let count = run_pr_doc_evaluation_for_sprint_id(&conn, 10, &cfg, true).unwrap();
        assert_eq!(count, 1);

        let justification: String = conn
            .query_row(
                "SELECT justification FROM pr_doc_evaluation WHERE sprint_id = 10",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            justification.contains("heuristics"),
            "missing CLI binary must fall back to heuristic, got: {justification}"
        );
    }
}
