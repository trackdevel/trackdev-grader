//! Multi-signal AI probability fusion.
//!
//! Mirrors `src/ai_detect/fusion.py`, with one intentional change from the
//! migration plan: the `perplexity` and `llm_judge` signals are dropped
//! entirely, and the `DEFAULT_FILE_WEIGHTS` are redistributed across the
//! remaining signals. Python defaults were:
//!
//! ```text
//! curriculum: 0.20, stylometry: 0.20, perplexity: 0.15,
//! llm_judge: 0.25,  text_consistency: 0.05, behavioral: 0.15
//! ```
//!
//! We fold the 0.40 dropped weight (perplexity + llm_judge) back into the
//! four remaining signals proportionally so each keeps its *relative*
//! contribution. New defaults (sum = 1.00):
//!
//! ```text
//! curriculum: 0.333, stylometry: 0.333, text_consistency: 0.083, behavioral: 0.251
//! ```

use std::collections::{BTreeSet, HashMap};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sprint_grader_core::stats::round_half_even;
use tracing::info;

type BehavioralRow = (
    Option<bool>,
    Option<i64>,
    Option<bool>,
    Option<bool>,
    Option<bool>,
    Option<bool>,
);

type FileStyleRow = (
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    bool,
    bool,
    bool,
    bool,
);

type FileAiProbRow = (
    String,
    String,
    f64,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
);

pub struct SignalWeights {
    pub behavioral: f64,
    pub stylometric: f64,
    pub coherence: f64,
    pub heuristic: f64,
}

impl Default for SignalWeights {
    fn default() -> Self {
        Self {
            behavioral: 0.35,
            stylometric: 0.25,
            coherence: 0.20,
            heuristic: 0.20,
        }
    }
}

pub fn default_file_weights() -> HashMap<&'static str, f64> {
    // Perplexity (0.15) + llm_judge (0.25) = 0.40, folded proportionally into
    // the four surviving signals (curriculum 0.20, stylometry 0.20,
    // text_consistency 0.05, behavioral 0.15 → sum 0.60). Scale factor 1/0.60.
    let mut m = HashMap::new();
    m.insert("curriculum", 0.20 / 0.60);
    m.insert("stylometry", 0.20 / 0.60);
    m.insert("text_consistency", 0.05 / 0.60);
    m.insert("behavioral", 0.15 / 0.60);
    m
}

pub static DEFAULT_FILE_WEIGHTS: once_cell::sync::Lazy<HashMap<&'static str, f64>> =
    once_cell::sync::Lazy::new(default_file_weights);

pub const DEFAULT_ELEVATED_THRESHOLD: f64 = 0.40;
pub const DEFAULT_HIGH_THRESHOLD: f64 = 0.65;

/// PR-level risk thresholds used by `fuse_signals_pr`. Looser than the
/// file-level pair because per-PR signal availability is noisier.
pub const PR_RISK_ELEVATED_THRESHOLD: f64 = 0.40;
pub const PR_RISK_HIGH_THRESHOLD: f64 = 0.70;

// ── PR-level fusion (legacy) ────────────────────────────────────────────────

pub fn compute_behavioral_score(conn: &Connection, pr_id: &str) -> Option<f64> {
    let row: Option<BehavioralRow> = conn
        .query_row(
            "SELECT single_commit_pr, max_lines_per_commit, has_fixup_pattern,
                    productivity_anomaly, has_test_adjustments, has_intermediate_changes
             FROM pr_behavioral_signals WHERE pr_id = ?",
            [pr_id],
            |r| {
                Ok((
                    r.get::<_, Option<bool>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<bool>>(2)?,
                    r.get::<_, Option<bool>>(3)?,
                    r.get::<_, Option<bool>>(4)?,
                    r.get::<_, Option<bool>>(5)?,
                ))
            },
        )
        .ok();
    let row = row?;
    let mut score: f64 = 0.0;
    if row.0.unwrap_or(false) && row.1.unwrap_or(0) > 200 {
        score += 0.30;
    }
    if row.2.unwrap_or(false) {
        score += 0.25;
    }
    if row.3.unwrap_or(false) {
        score += 0.25;
    }
    if !row.4.unwrap_or(false) && row.1.unwrap_or(0) > 100 {
        score += 0.10;
    }
    if !row.5.unwrap_or(false) {
        score += 0.10;
    }
    Some(score.min(1.0))
}

pub fn compute_heuristic_score(conn: &Connection, pr_id: &str) -> f64 {
    let author: Option<String> = conn
        .query_row(
            "SELECT author_id FROM pull_requests WHERE id = ?",
            [pr_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let Some(author) = author else { return 0.0 };

    let sprint_id: Option<i64> = conn
        .query_row(
            "SELECT t.sprint_id FROM tasks t
             JOIN task_pull_requests tpr ON tpr.task_id = t.id
             WHERE tpr.pr_id = ? AND t.type != 'USER_STORY' LIMIT 1",
            [pr_id],
            |r| r.get::<_, i64>(0),
        )
        .ok();
    let Some(sprint_id) = sprint_id else {
        return 0.0;
    };

    let mut stmt =
        match conn.prepare("SELECT flag_type FROM flags WHERE student_id = ? AND sprint_id = ?") {
            Ok(s) => s,
            Err(_) => return 0.0,
        };
    let types: Vec<String> = stmt
        .query_map(params![author, sprint_id], |r| r.get::<_, String>(0))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    drop(stmt);
    let set: BTreeSet<String> = types.into_iter().collect();

    let mut score: f64 = 0.0;
    if set.contains("SINGLE_COMMIT_DUMP") {
        score += 0.30;
    }
    if set.contains("NO_REVIEWS_RECEIVED") {
        score += 0.10;
    }
    if set.contains("EMPTY_DESCRIPTION") {
        score += 0.20;
    }
    if set.contains("LOW_SURVIVAL_RATE") {
        score += 0.20;
    }
    // Post-T-P1.2 the detector emits COSMETIC_REWRITE_ACTOR (rewriter) and
    // COSMETIC_REWRITE_VICTIM (original author). Only the actor variant
    // signals AI-likely behaviour. Match the legacy type too for old DB rows.
    if set.contains("COSMETIC_REWRITE_ACTOR") || set.contains("COSMETIC_REWRITE") {
        score += 0.20;
    }
    score.min(1.0)
}

pub fn fuse_signals_pr(
    scores: &HashMap<&'static str, Option<f64>>,
    weights: &SignalWeights,
) -> (f64, String, String, Vec<Value>) {
    let weight_map: HashMap<&'static str, f64> = HashMap::from([
        ("behavioral", weights.behavioral),
        ("stylometric", weights.stylometric),
        ("coherence", weights.coherence),
        ("heuristic", weights.heuristic),
    ]);

    let available: HashMap<&'static str, f64> = scores
        .iter()
        .filter_map(|(k, v)| v.map(|x| (*k, x)))
        .collect();
    if available.is_empty() {
        return (0.0, "low".into(), "normal".into(), Vec::new());
    }
    let total_weight: f64 = available
        .keys()
        .map(|k| weight_map.get(k).copied().unwrap_or(0.0))
        .sum();
    if total_weight == 0.0 {
        return (0.0, "low".into(), "normal".into(), Vec::new());
    }
    let probability: f64 = available
        .iter()
        .map(|(k, v)| (weight_map.get(k).copied().unwrap_or(0.0) / total_weight) * v)
        .sum();

    let n = available.len();
    let confidence = if n >= 3 {
        "high"
    } else if n >= 2 {
        "medium"
    } else {
        "low"
    };
    // PR-level thresholds are intentionally stricter than file-level
    // (`DEFAULT_HIGH_THRESHOLD` = 0.65): per-PR fusion has noisier inputs.
    let risk = if probability > PR_RISK_HIGH_THRESHOLD {
        "high"
    } else if probability > PR_RISK_ELEVATED_THRESHOLD {
        "elevated"
    } else {
        "normal"
    };

    let mut contributions: Vec<(String, f64, f64)> = available
        .iter()
        .map(|(k, v)| {
            let w = weight_map.get(k).copied().unwrap_or(0.0);
            ((*k).to_string(), *v, (w / total_weight) * v)
        })
        .collect();
    contributions.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let top: Vec<Value> = contributions
        .into_iter()
        .take(3)
        .map(|(signal, score, weighted)| {
            json!({
                "signal": signal,
                "score": round_half_even(score, 3),
                "weighted": round_half_even(weighted, 3),
            })
        })
        .collect();

    (probability, confidence.into(), risk.into(), top)
}

pub fn compute_all_ai_probability(
    conn: &Connection,
    sprint_id: i64,
    weights: Option<SignalWeights>,
) -> rusqlite::Result<()> {
    let weights = weights.unwrap_or_default();

    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.author_id FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    for (pr_id, student_id) in &rows {
        let mut scores: HashMap<&'static str, Option<f64>> = HashMap::new();
        scores.insert("behavioral", compute_behavioral_score(conn, pr_id));
        scores.insert("stylometric", None);
        scores.insert("coherence", None);
        scores.insert("heuristic", Some(compute_heuristic_score(conn, pr_id)));

        let (probability, confidence, risk, top_signals) = fuse_signals_pr(&scores, &weights);

        conn.execute(
            "INSERT OR REPLACE INTO pr_ai_probability
             (pr_id, student_id, sprint_id, stylometric_score, behavioral_score,
              coherence_score, heuristic_score, ai_probability, confidence,
              risk_level, top_signals)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                pr_id,
                student_id,
                sprint_id,
                scores.get("stylometric").copied().flatten(),
                scores.get("behavioral").copied().flatten(),
                scores.get("coherence").copied().flatten(),
                scores.get("heuristic").copied().flatten(),
                probability,
                confidence,
                risk,
                Value::Array(top_signals).to_string(),
            ],
        )?;
    }
    info!(
        count = rows.len(),
        sprint_id, "PR-level AI probability computed"
    );
    Ok(())
}

// ── File-level Bayesian fusion ──────────────────────────────────────────────

pub fn bayesian_fuse(
    scores: &HashMap<&'static str, Option<f64>>,
    weights: &HashMap<&'static str, f64>,
    elevated_threshold: f64,
    high_threshold: f64,
) -> (f64, String, String) {
    let available: HashMap<&'static str, f64> = scores
        .iter()
        .filter_map(|(k, v)| v.map(|x| (*k, x)))
        .collect();
    if available.is_empty() {
        return (0.0, "low".into(), "normal".into());
    }
    let total_weight: f64 = available
        .keys()
        .map(|k| weights.get(k).copied().unwrap_or(0.0))
        .sum();
    if total_weight == 0.0 {
        return (0.0, "low".into(), "normal".into());
    }
    let probability: f64 = available
        .iter()
        .map(|(k, v)| (weights.get(k).copied().unwrap_or(0.0) / total_weight) * v)
        .sum();
    let probability = probability.clamp(0.0, 1.0);
    let n = available.len();
    let confidence = if n >= 4 {
        "high"
    } else if n == 3 {
        "medium"
    } else {
        "low"
    };
    let risk = if probability > high_threshold {
        "high"
    } else if probability > elevated_threshold {
        "elevated"
    } else {
        "normal"
    };
    (probability, confidence.into(), risk.into())
}

fn get_file_curriculum_score(
    conn: &Connection,
    file_path: &str,
    repo_name: &str,
    sprint_id: i64,
) -> Option<f64> {
    let mut stmt = conn
        .prepare(
            "SELECT severity FROM curriculum_violations
             WHERE file_path = ? AND repo_name = ? AND sprint_id = ?",
        )
        .ok()?;
    let sevs: Vec<String> = stmt
        .query_map(params![file_path, repo_name, sprint_id], |r| {
            r.get::<_, String>(0)
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    if sevs.is_empty() {
        return None;
    }
    let mut score: f64 = 0.0;
    for s in sevs {
        let delta = match s.as_str() {
            "HIGH" | "high" => 0.25,
            "MEDIUM" | "medium" => 0.10,
            _ => 0.05,
        };
        score += delta;
    }
    Some(score.min(1.0))
}

fn get_file_stylometry_score(
    conn: &Connection,
    file_path: &str,
    repo_name: &str,
    sprint_id: i64,
) -> Option<f64> {
    use crate::stylometry::{compute_ai_style_score, StyleFeatureVector};
    let row: Option<FileStyleRow> = conn
        .query_row(
            "SELECT avg_identifier_length, identifier_length_stddev,
                    camelcase_ratio, comment_density,
                    avg_method_length, method_length_stddev,
                    avg_catch_body_lines, empty_catch_ratio,
                    import_alphabetized, has_comprehensive_javadoc,
                    has_null_checks_everywhere, uniform_formatting
             FROM file_style_features WHERE file_path = ? AND repo_name = ? AND sprint_id = ?",
            params![file_path, repo_name, sprint_id],
            |r| {
                Ok((
                    r.get::<_, Option<f64>>(0)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                    r.get::<_, Option<bool>>(8)?.unwrap_or(false),
                    r.get::<_, Option<bool>>(9)?.unwrap_or(false),
                    r.get::<_, Option<bool>>(10)?.unwrap_or(false),
                    r.get::<_, Option<bool>>(11)?.unwrap_or(false),
                ))
            },
        )
        .ok();
    let row = row?;
    let fv = StyleFeatureVector {
        avg_identifier_length: row.0,
        identifier_length_stddev: row.1,
        camelcase_ratio: row.2,
        comment_density: row.3,
        avg_method_length: row.4,
        method_length_stddev: row.5,
        avg_catch_body_lines: row.6,
        empty_catch_ratio: row.7,
        import_alphabetized: row.8,
        has_comprehensive_javadoc: row.9,
        has_null_checks_everywhere: row.10,
        uniform_formatting: row.11,
        ..Default::default()
    };
    Some(compute_ai_style_score(&fv))
}

pub struct FusionConfig {
    pub weights: HashMap<&'static str, f64>,
    pub elevated_threshold: f64,
    pub high_threshold: f64,
    pub min_lines_for_attribution: i64,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            weights: default_file_weights(),
            elevated_threshold: DEFAULT_ELEVATED_THRESHOLD,
            high_threshold: DEFAULT_HIGH_THRESHOLD,
            min_lines_for_attribution: 10,
        }
    }
}

pub fn fuse_all_signals(
    conn: &Connection,
    repo_name: &str,
    project_id: i64,
    sprint_id: i64,
    cfg: &FusionConfig,
) -> rusqlite::Result<usize> {
    let mut file_set: BTreeSet<String> = BTreeSet::new();
    for (table, col) in [
        ("curriculum_violations", "file_path"),
        ("file_style_features", "file_path"),
    ] {
        let sql = format!(
            "SELECT DISTINCT {} FROM {} WHERE repo_name = ? AND sprint_id = ?",
            col, table
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            continue;
        };
        let Ok(rows) = stmt.query_map(params![repo_name, sprint_id], |r| r.get::<_, String>(0))
        else {
            continue;
        };
        for r in rows.filter_map(|r| r.ok()) {
            file_set.insert(r);
        }
    }
    if file_set.is_empty() {
        info!(repo_name, sprint_id, "no AI detection signals found");
        return Ok(0);
    }

    let mut count = 0usize;
    for file_path in &file_set {
        let mut scores: HashMap<&'static str, Option<f64>> = HashMap::new();
        scores.insert(
            "curriculum",
            get_file_curriculum_score(conn, file_path, repo_name, sprint_id),
        );
        scores.insert(
            "stylometry",
            get_file_stylometry_score(conn, file_path, repo_name, sprint_id),
        );
        // perplexity and llm_judge dropped; text_consistency and behavioral are
        // not file-scoped in the student-level attribution step below.

        if !scores.values().any(|v| v.is_some()) {
            continue;
        }

        let (probability, confidence, risk) = bayesian_fuse(
            &scores,
            &cfg.weights,
            cfg.elevated_threshold,
            cfg.high_threshold,
        );

        let mut avail: Vec<(String, f64)> = scores
            .iter()
            .filter_map(|(k, v)| v.map(|x| ((*k).to_string(), x)))
            .collect();
        avail.sort_by(|a, b| {
            let wa = cfg.weights.get(a.0.as_str()).copied().unwrap_or(0.0) * a.1;
            let wb = cfg.weights.get(b.0.as_str()).copied().unwrap_or(0.0) * b.1;
            wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
        });
        let top: Vec<Value> = avail
            .into_iter()
            .take(3)
            .map(|(k, v)| {
                json!({
                    "signal": k,
                    "score": round_half_even(v, 3),
                })
            })
            .collect();

        conn.execute(
            "INSERT OR REPLACE INTO file_ai_probability
             (file_path, repo_name, project_id, sprint_id,
              curriculum_score, stylometry_score, perplexity_score,
              llm_judge_score, text_consistency_score, behavioral_score,
              ai_probability, confidence, risk_level, top_signals)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                file_path,
                repo_name,
                project_id,
                sprint_id,
                scores.get("curriculum").copied().flatten(),
                scores.get("stylometry").copied().flatten(),
                Option::<f64>::None,
                Option::<f64>::None,
                scores.get("text_consistency").copied().flatten(),
                scores.get("behavioral").copied().flatten(),
                probability,
                confidence,
                risk,
                Value::Array(top).to_string(),
            ],
        )?;
        count += 1;
    }

    info!(count, repo_name, sprint_id, "file AI signals fused");
    Ok(count)
}

pub fn attribute_to_students(
    conn: &Connection,
    project_id: i64,
    sprint_id: i64,
    cfg: &FusionConfig,
) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT file_path, repo_name, ai_probability,
                curriculum_score, stylometry_score, perplexity_score,
                llm_judge_score, text_consistency_score, behavioral_score
         FROM file_ai_probability
         WHERE project_id = ? AND sprint_id = ?",
    )?;
    let files: Vec<FileAiProbRow> = stmt
        .query_map(params![project_id, sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, Option<f64>>(6)?,
                r.get::<_, Option<f64>>(7)?,
                r.get::<_, Option<f64>>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if files.is_empty() {
        return Ok(0);
    }

    let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
    let student_ids: BTreeSet<String> = stmt
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    #[derive(Default, Clone)]
    struct Accum {
        total_lines: i64,
        flagged_lines: i64,
        weighted_sum: f64,
        file_count: i64,
        flagged_count: i64,
        curriculum_sum: f64,
        curriculum_n: i64,
        stylometry_sum: f64,
        stylometry_n: i64,
        perplexity_sum: f64,
        perplexity_n: i64,
        llm_judge_sum: f64,
        llm_judge_n: i64,
        behavioral_sum: f64,
        behavioral_n: i64,
    }
    let mut student_data: HashMap<String, Accum> = student_ids
        .iter()
        .map(|sid| (sid.clone(), Accum::default()))
        .collect();

    for (
        file_path,
        repo_name,
        ai_probability,
        curriculum_score,
        stylometry_score,
        perplexity_score,
        llm_judge_score,
        _text_score,
        behavioral_score,
    ) in &files
    {
        let mut stmt = conn.prepare(
            "SELECT blame_author_login, COUNT(*) as line_count
             FROM fingerprints
             WHERE file_path = ? AND repo_full_name = ? AND sprint_id = ?
             GROUP BY blame_author_login",
        )?;
        let blame: Vec<(Option<String>, i64)> = stmt
            .query_map(params![file_path, repo_name, sprint_id], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        for (login_opt, lines) in blame {
            if lines < cfg.min_lines_for_attribution {
                continue;
            }
            let Some(login) = login_opt else { continue };
            // Resolve login → student via student_github_identity
            // (resolver-derived). TrackDev's `students.github_login` is
            // no longer trusted as a source for this mapping.
            let resolved: Option<String> = conn
                .query_row(
                    "SELECT student_id FROM student_github_identity
                     WHERE identity_kind = 'login' AND identity_value = LOWER(?)
                     ORDER BY weight DESC, confidence DESC, student_id
                     LIMIT 1",
                    [&login],
                    |r| r.get::<_, String>(0),
                )
                .ok();
            let Some(sid) = resolved else { continue };
            if !student_ids.contains(&sid) {
                continue;
            }
            let Some(d) = student_data.get_mut(&sid) else {
                continue;
            };
            d.total_lines += lines;
            d.file_count += 1;
            d.weighted_sum += ai_probability * lines as f64;
            if *ai_probability > cfg.elevated_threshold {
                d.flagged_lines += lines;
                d.flagged_count += 1;
            }
            if let Some(v) = curriculum_score {
                d.curriculum_sum += v * lines as f64;
                d.curriculum_n += lines;
            }
            if let Some(v) = stylometry_score {
                d.stylometry_sum += v * lines as f64;
                d.stylometry_n += lines;
            }
            if let Some(v) = perplexity_score {
                d.perplexity_sum += v * lines as f64;
                d.perplexity_n += lines;
            }
            if let Some(v) = llm_judge_score {
                d.llm_judge_sum += v * lines as f64;
                d.llm_judge_n += lines;
            }
            if let Some(v) = behavioral_score {
                d.behavioral_sum += v * lines as f64;
                d.behavioral_n += lines;
            }
        }
    }

    let mut count = 0usize;
    for (sid, d) in &student_data {
        if d.total_lines == 0 {
            continue;
        }
        let weighted_score = d.weighted_sum / d.total_lines as f64;
        let ai_ratio = d.flagged_lines as f64 / d.total_lines as f64;
        let risk = if weighted_score > cfg.high_threshold {
            "high"
        } else if weighted_score > cfg.elevated_threshold {
            "elevated"
        } else {
            "normal"
        };
        let n_signals = [
            d.curriculum_n,
            d.stylometry_n,
            d.perplexity_n,
            d.llm_judge_n,
            d.behavioral_n,
        ]
        .iter()
        .filter(|n| **n > 0)
        .count();
        let confidence = if n_signals >= 4 {
            "high"
        } else if n_signals >= 3 {
            "medium"
        } else {
            "low"
        };

        let tc: Option<f64> = conn
            .query_row(
                "SELECT text_consistency_score FROM text_consistency_scores WHERE student_id = ? AND sprint_id = ?",
                params![sid, sprint_id],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten();

        let avg = |sum: f64, n: i64| {
            if n > 0 {
                Some(round_half_even(sum / n as f64, 4))
            } else {
                None
            }
        };

        conn.execute(
            "INSERT OR REPLACE INTO student_sprint_ai_usage
             (student_id, sprint_id, project_id,
              total_authored_lines, ai_flagged_lines, ai_usage_ratio, weighted_ai_score,
              avg_curriculum_score, avg_stylometry_score, avg_perplexity_score,
              avg_llm_judge_score, text_consistency_score, avg_behavioral_score,
              risk_level, confidence, file_count_analyzed, file_count_flagged)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                sid,
                sprint_id,
                project_id,
                d.total_lines,
                d.flagged_lines,
                round_half_even(ai_ratio, 4),
                round_half_even(weighted_score, 4),
                avg(d.curriculum_sum, d.curriculum_n),
                avg(d.stylometry_sum, d.stylometry_n),
                avg(d.perplexity_sum, d.perplexity_n),
                avg(d.llm_judge_sum, d.llm_judge_n),
                tc,
                avg(d.behavioral_sum, d.behavioral_n),
                risk,
                confidence,
                d.file_count,
                d.flagged_count,
            ],
        )?;
        count += 1;
    }
    info!(count, project_id, "AI usage attributed to students");
    Ok(count)
}

pub fn run_full_fusion(
    conn: &Connection,
    repo_name: &str,
    project_id: i64,
    sprint_id: i64,
    cfg: &FusionConfig,
) -> rusqlite::Result<()> {
    fuse_all_signals(conn, repo_name, project_id, sprint_id, cfg)?;
    attribute_to_students(conn, project_id, sprint_id, cfg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_weights_sum_to_one() {
        let w = default_file_weights();
        let sum: f64 = w.values().sum();
        assert!((sum - 1.0).abs() < 1e-9, "weights sum = {sum}");
    }

    #[test]
    fn bayesian_fuse_single_signal_returns_that_score_exactly() {
        let mut scores: HashMap<&'static str, Option<f64>> = HashMap::new();
        scores.insert("curriculum", Some(0.6));
        let weights = default_file_weights();
        let (p, conf, risk) = bayesian_fuse(&scores, &weights, 0.4, 0.65);
        assert!((p - 0.6).abs() < 1e-9);
        assert_eq!(conf, "low");
        assert_eq!(risk, "elevated");
    }

    #[test]
    fn bayesian_fuse_empty_returns_normal_low() {
        let scores: HashMap<&'static str, Option<f64>> = HashMap::new();
        let weights = default_file_weights();
        let (p, conf, risk) = bayesian_fuse(&scores, &weights, 0.4, 0.65);
        assert_eq!(p, 0.0);
        assert_eq!(conf, "low");
        assert_eq!(risk, "normal");
    }

    #[test]
    fn pr_fuse_picks_top_three_by_weighted() {
        let weights = SignalWeights::default();
        let mut scores: HashMap<&'static str, Option<f64>> = HashMap::new();
        scores.insert("behavioral", Some(0.9));
        scores.insert("stylometric", Some(0.1));
        scores.insert("coherence", Some(0.2));
        scores.insert("heuristic", Some(0.5));
        let (p, _, _, top) = fuse_signals_pr(&scores, &weights);
        assert!(p > 0.0);
        assert_eq!(top.len(), 3);
        // Highest weighted should be behavioral (weight 0.35, score 0.9)
        assert_eq!(top[0]["signal"].as_str().unwrap(), "behavioral");
    }
}
