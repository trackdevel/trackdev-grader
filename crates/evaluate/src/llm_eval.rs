//! Per-sprint PR documentation scoring. Uses the Claude Messages API when
//! `ANTHROPIC_API_KEY` is set; otherwise falls back to the deterministic
//! heuristic used by `src/evaluate/llm_eval.py::_evaluate_team_heuristic`.

use fancy_regex::Regex as FancyRegex;
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::Value;
use sprint_grader_core::Config;
use tracing::{info, warn};

use crate::llm_client::{AnthropicClient, Conversation};

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
    if TASK_ID_ONLY.is_match(body).unwrap_or(false) {
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

/// PR doc evaluation for a single sprint. Uses the LLM if
/// `ANTHROPIC_API_KEY` is set; otherwise falls back to heuristics. After
/// scoring, updates `avg_doc_score` on `student_sprint_metrics`.
pub fn run_llm_evaluation_for_sprint_id(
    conn: &Connection,
    sprint_id: i64,
    config: &Config,
) -> rusqlite::Result<usize> {
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    let mut count = 0usize;

    if api_key.is_empty() {
        info!("ANTHROPIC_API_KEY not set — using heuristic PR doc scoring");
        count += evaluate_prs_heuristic(conn, sprint_id)?;
    } else {
        let model =
            std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-opus-4-7".to_string());
        match AnthropicClient::new(&api_key, model) {
            Ok(client) => count += evaluate_prs_llm(conn, sprint_id, &client)?,
            Err(e) => {
                warn!(error = %e, "Anthropic client init failed — heuristic fallback");
                count += evaluate_prs_heuristic(conn, sprint_id)?;
            }
        }
    }

    update_avg_doc_score(conn, sprint_id)?;
    let _ = config; // accepted for symmetry with the Python signature
    Ok(count)
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
        let prs: Vec<(
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = stmt
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
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return evaluate_tasks_heuristic(conn, sprint_id);
    }
    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-opus-4-7".to_string());
    match AnthropicClient::new(&api_key, model) {
        Ok(client) => evaluate_tasks_llm(conn, sprint_id, &client),
        Err(e) => {
            warn!(error = %e, "client init failed — heuristic task scoring");
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
}
