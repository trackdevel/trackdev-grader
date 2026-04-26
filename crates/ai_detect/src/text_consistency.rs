//! Short-text consistency analysis.
//! Mirrors `src/ai_detect/text_consistency.py`.

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use tracing::{debug, info, warn};

const FORMAL_PATTERNS: &[&str] = &[
    "furthermore",
    "moreover",
    "consequently",
    "utilize",
    "implement",
    "facilitate",
    "ensure",
    "comprehensive",
    "This pull request implements",
    "This PR addresses",
];

const INFORMAL_PATTERNS: &[&str] = &[
    "gonna",
    "wanna",
    "kinda",
    "lol",
    "haha",
    "fix stuff",
    "fix things",
    "!!",
];

static FORMAL_RE: Lazy<Vec<Regex>> = Lazy::new(|| {
    FORMAL_PATTERNS
        .iter()
        .map(|p| Regex::new(&format!(r"(?i){}", regex::escape(p))).unwrap())
        .collect()
});
static INFORMAL_RE: Lazy<Vec<Regex>> = Lazy::new(|| {
    INFORMAL_PATTERNS
        .iter()
        .map(|p| Regex::new(&format!(r"(?i){}", regex::escape(p))).unwrap())
        .collect()
});
static PASSIVE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b(?:is|are|was|were|been|being)\s+\w+ed\b").unwrap());
static HEDGING: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "perhaps",
        "possibly",
        "might",
        "could",
        "may",
        "somewhat",
        "arguably",
        "likely",
        "unlikely",
        "presumably",
    ]
    .iter()
    .copied()
    .collect()
});
static CONTRACTION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\w+'\w+\b").unwrap());
static EMOJI_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"[\u{1F600}-\u{1F64F}\u{1F300}-\u{1F5FF}\u{1F680}-\u{1F6FF}\u{1F900}-\u{1F9FF}\u{2702}-\u{27B0}\u{FE00}-\u{FE0F}\u{1FA00}-\u{1FA6F}\u{1FA70}-\u{1FAFF}\u{2600}-\u{26FF}]+",
    )
    .unwrap()
});
static ABBREVIATION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:ASAP|FYI|AFAIK|IIRC|TBH|IMO|IMHO|BTW|WIP|LGTM|PTAL|TL;DR)\b").unwrap()
});
static TOKEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[a-zA-Z0-9]+(?:'[a-zA-Z]+)?").unwrap());
static SENTENCE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[.!?]+").unwrap());

fn tokenize(text: &str) -> Vec<String> {
    TOKEN_RE
        .find_iter(&text.to_lowercase())
        .map(|m| m.as_str().to_string())
        .collect()
}

fn split_sentences(text: &str) -> Vec<String> {
    SENTENCE_RE
        .split(text)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn compute_formality(text: &str) -> f64 {
    if text.is_empty() || text.trim().is_empty() {
        return 0.5;
    }
    let words = tokenize(text);
    if words.is_empty() {
        return 0.5;
    }
    let word_count = words.len() as f64;

    let formal_hits = FORMAL_RE.iter().filter(|p| p.is_match(text)).count() as f64;
    let informal_hits = INFORMAL_RE.iter().filter(|p| p.is_match(text)).count() as f64;

    let formal_density = formal_hits / (word_count / 10.0).max(1.0);
    let informal_density = informal_hits / (word_count / 10.0).max(1.0);
    let total_density = formal_density + informal_density;
    let keyword_score = if total_density > 0.0 {
        formal_density / total_density
    } else {
        0.5
    };

    let sentences = split_sentences(text);
    let avg_sentence_len = word_count / sentences.len().max(1) as f64;
    let sentence_score = ((avg_sentence_len - 5.0) / 30.0).clamp(0.0, 1.0);

    let passive_count = PASSIVE_RE.find_iter(text).count() as f64;
    let passive_score = (passive_count / (word_count / 20.0).max(1.0)).min(1.0);

    let hedging_count = words
        .iter()
        .filter(|w| HEDGING.contains(w.as_str()))
        .count() as f64;
    let hedging_score = (hedging_count / (word_count / 30.0).max(1.0)).min(1.0);

    let formality = if word_count < 20.0 {
        keyword_score * 0.75 + sentence_score * 0.10 + passive_score * 0.10 + hedging_score * 0.05
    } else {
        keyword_score * 0.45 + sentence_score * 0.20 + passive_score * 0.20 + hedging_score * 0.15
    };
    formality.clamp(0.0, 1.0)
}

pub fn compute_vocabulary_richness(text: &str) -> f64 {
    let words = tokenize(text);
    if words.is_empty() {
        return 0.0;
    }
    let unique: HashSet<&String> = words.iter().collect();
    unique.len() as f64 / words.len() as f64
}

#[derive(Debug, Clone)]
struct TextMetrics {
    total_word_count: i64,
    avg_sentence_length: f64,
    avg_word_length: f64,
    vocabulary_richness: f64,
    formality_score: f64,
    uses_contractions: bool,
    uses_emoji: bool,
    uses_abbreviations: bool,
}

fn compute_text_metrics(corpus: &str) -> TextMetrics {
    let words = tokenize(corpus);
    let word_count = words.len();
    let sentences = split_sentences(corpus);
    let avg_sentence_length = word_count as f64 / sentences.len().max(1) as f64;
    let avg_word_length = if word_count > 0 {
        words.iter().map(|w| w.chars().count()).sum::<usize>() as f64 / word_count as f64
    } else {
        0.0
    };
    let vocabulary_richness = compute_vocabulary_richness(corpus);
    let formality_score = compute_formality(corpus);
    let uses_contractions = CONTRACTION_RE.is_match(corpus);
    let uses_emoji = EMOJI_RE.is_match(corpus);
    let uses_abbreviations = ABBREVIATION_RE.is_match(corpus);
    TextMetrics {
        total_word_count: word_count as i64,
        avg_sentence_length,
        avg_word_length,
        vocabulary_richness,
        formality_score,
        uses_contractions,
        uses_emoji,
        uses_abbreviations,
    }
}

fn gather_student_text(
    conn: &Connection,
    student_id: &str,
    project_id: i64,
    sprint_id: Option<i64>,
) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    let (commit_rows, pr_rows) = if let Some(sid) = sprint_id {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT c.message
             FROM pr_commits c
             JOIN pull_requests pr ON pr.id = c.pr_id
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE pr.author_id = ? AND t.sprint_id = ? AND t.type != 'USER_STORY'
               AND c.message IS NOT NULL AND c.message != ''",
        )?;
        let commits: Vec<String> = stmt
            .query_map(params![student_id, sid], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pr.title, pr.body
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE pr.author_id = ? AND t.sprint_id = ? AND t.type != 'USER_STORY'",
        )?;
        let prs: Vec<(Option<String>, Option<String>)> = stmt
            .query_map(params![student_id, sid], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        (commits, prs)
    } else {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT c.message
             FROM pr_commits c
             JOIN pull_requests pr ON pr.id = c.pr_id
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             JOIN sprints s ON s.id = t.sprint_id
             WHERE pr.author_id = ? AND s.project_id = ? AND t.type != 'USER_STORY'
               AND c.message IS NOT NULL AND c.message != ''",
        )?;
        let commits: Vec<String> = stmt
            .query_map(params![student_id, project_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pr.title, pr.body
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             JOIN sprints s ON s.id = t.sprint_id
             WHERE pr.author_id = ? AND s.project_id = ? AND t.type != 'USER_STORY'",
        )?;
        let prs: Vec<(Option<String>, Option<String>)> = stmt
            .query_map(params![student_id, project_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        (commits, prs)
    };

    let commit_msgs = commit_rows;
    let mut pr_descs: Vec<String> = Vec::new();
    for (title, body) in pr_rows {
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = title {
            if !t.is_empty() {
                parts.push(t);
            }
        }
        if let Some(b) = body {
            if !b.is_empty() {
                parts.push(b);
            }
        }
        if !parts.is_empty() {
            pr_descs.push(parts.join(" "));
        }
    }
    Ok((commit_msgs, pr_descs))
}

pub fn build_text_profile(
    conn: &Connection,
    student_id: &str,
    project_id: i64,
) -> rusqlite::Result<()> {
    let (commit_msgs, pr_descs) = gather_student_text(conn, student_id, project_id, None)?;
    let corpus = format!("{} {}", commit_msgs.join(" "), pr_descs.join(" "));
    let metrics = compute_text_metrics(&corpus);

    if metrics.total_word_count < 100 {
        debug!(
            student_id,
            words = metrics.total_word_count,
            "skipping text profile: too few words"
        );
        return Ok(());
    }

    let pr_word_counts: Vec<usize> = if pr_descs.is_empty() {
        vec![0]
    } else {
        pr_descs.iter().map(|d| tokenize(d).len()).collect()
    };
    let avg_pr_description_length =
        pr_word_counts.iter().sum::<usize>() as f64 / pr_word_counts.len().max(1) as f64;

    conn.execute(
        "INSERT OR REPLACE INTO student_text_profile
         (student_id, project_id, total_word_count, avg_sentence_length,
          avg_word_length, vocabulary_richness, formality_score, error_rate,
          uses_contractions, uses_emoji, uses_abbreviations,
          avg_pr_description_length)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            student_id,
            project_id,
            metrics.total_word_count,
            metrics.avg_sentence_length,
            metrics.avg_word_length,
            metrics.vocabulary_richness,
            metrics.formality_score,
            0.0_f64,
            metrics.uses_contractions,
            metrics.uses_emoji,
            metrics.uses_abbreviations,
            avg_pr_description_length,
        ],
    )?;
    info!(
        student_id,
        words = metrics.total_word_count,
        "text profile built"
    );
    Ok(())
}

fn z_score(value: f64, baseline: f64, std: f64) -> f64 {
    if std < 1e-9 {
        0.0
    } else {
        ((value - baseline) / std).abs()
    }
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

fn std_estimate(baseline: f64) -> f64 {
    (baseline * 0.35).max(0.10)
}

pub fn compute_sprint_consistency(
    conn: &Connection,
    student_id: &str,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let project_id: Option<i64> = conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get::<_, i64>(0),
        )
        .ok();
    let Some(project_id) = project_id else {
        warn!(sprint_id, "sprint not found");
        return Ok(());
    };

    let profile: Option<(f64, f64, f64)> = conn
        .query_row(
            "SELECT formality_score, vocabulary_richness, avg_sentence_length
             FROM student_text_profile WHERE student_id = ? AND project_id = ?",
            params![student_id, project_id],
            |r| {
                Ok((
                    r.get::<_, f64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            },
        )
        .ok();
    let Some((base_formality, base_vocab, base_sent_len)) = profile else {
        debug!(student_id, "no baseline profile — skipping consistency");
        return Ok(());
    };

    let (commit_msgs, pr_descs) =
        gather_student_text(conn, student_id, project_id, Some(sprint_id))?;
    let corpus = format!("{} {}", commit_msgs.join(" "), pr_descs.join(" "));
    let sprint_metrics = compute_text_metrics(&corpus);
    if sprint_metrics.total_word_count < 10 {
        debug!(student_id, sprint_id, "too few words in sprint");
        return Ok(());
    }

    let pr_word_counts: Vec<usize> = if pr_descs.is_empty() {
        vec![0]
    } else {
        pr_descs.iter().map(|d| tokenize(d).len()).collect()
    };
    let sprint_avg_pr_desc =
        pr_word_counts.iter().sum::<usize>() as f64 / pr_word_counts.len().max(1) as f64;

    let formality_dev = clamp01(
        z_score(
            sprint_metrics.formality_score,
            base_formality,
            std_estimate(base_formality),
        ) / 3.0,
    );
    let vocabulary_dev = clamp01(
        z_score(
            sprint_metrics.vocabulary_richness,
            base_vocab,
            std_estimate(base_vocab),
        ) / 3.0,
    );
    let sentence_len_dev = clamp01(
        z_score(
            sprint_metrics.avg_sentence_length,
            base_sent_len,
            std_estimate(base_sent_len),
        ) / 3.0,
    );

    let text_consistency_score =
        formality_dev * 0.4 + vocabulary_dev * 0.3 + sentence_len_dev * 0.3;

    conn.execute(
        "INSERT OR REPLACE INTO text_consistency_scores
         (student_id, sprint_id,
          sprint_formality, sprint_avg_sentence_length,
          sprint_vocabulary_richness, sprint_avg_pr_desc_length,
          formality_deviation, vocabulary_deviation,
          sentence_length_deviation, text_consistency_score)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            student_id,
            sprint_id,
            sprint_metrics.formality_score,
            sprint_metrics.avg_sentence_length,
            sprint_metrics.vocabulary_richness,
            sprint_avg_pr_desc,
            formality_dev,
            vocabulary_dev,
            sentence_len_dev,
            text_consistency_score,
        ],
    )?;
    info!(
        student_id,
        sprint_id,
        score = text_consistency_score,
        "text consistency"
    );
    Ok(())
}

pub fn compute_all_text_consistency(
    conn: &Connection,
    project_id: i64,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
    let ids: Vec<String> = stmt
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    for sid in &ids {
        let has_profile: bool = conn
            .query_row(
                "SELECT 1 FROM student_text_profile WHERE student_id = ? AND project_id = ?",
                params![sid, project_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !has_profile {
            build_text_profile(conn, sid, project_id)?;
        }
        compute_sprint_consistency(conn, sid, sprint_id)?;
    }
    info!(
        count = ids.len(),
        project_id, sprint_id, "text consistency computed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formality_formal_text_scores_high() {
        let t = "This pull request implements a comprehensive authentication facility that utilizes JWT tokens.";
        let f = compute_formality(t);
        assert!(f > 0.5, "expected formality > 0.5, got {f}");
    }

    #[test]
    fn formality_informal_text_scores_low() {
        let t = "gonna fix stuff haha lol";
        let f = compute_formality(t);
        assert!(f < 0.5, "expected formality < 0.5, got {f}");
    }

    #[test]
    fn vocabulary_richness_is_1_for_unique_words() {
        let r = compute_vocabulary_richness("alpha beta gamma delta");
        assert!((r - 1.0).abs() < 1e-9);
    }

    #[test]
    fn vocabulary_richness_drops_with_repetition() {
        let r = compute_vocabulary_richness("the the the");
        assert!((r - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn empty_input_formality_is_neutral() {
        assert_eq!(compute_formality(""), 0.5);
    }
}
