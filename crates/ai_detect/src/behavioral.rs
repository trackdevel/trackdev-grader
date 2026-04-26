//! Commit-pattern heuristics for AI detection.
//! Mirrors `src/ai_detect/behavioral.py`.

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use sprint_grader_core::time::parse_iso;
use tracing::info;

static GENERIC_COMMIT_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    [
        r"^(?i)(fix|update|add|change|remove|delete|refactor|clean ?up)\s*$",
        r"^(?i)(fix|update|add)\s+(stuff|things|code|bug|issue)$",
        r"^(?i)wip$",
        r"^(?i)initial commit$",
        r"^(?i)merge\s+branch",
        r"^(?i)[a-f0-9]{7,40}$",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

const FIXUP_KEYWORDS: &[&str] = &["fix", "typo", "missing", "forgot", "oops", "lint", "format"];

fn is_generic_message(msg: &str) -> bool {
    let first = msg.trim().lines().next().unwrap_or("");
    // Python uses `.match()` on the first line which is left-anchored. Regexes
    // above all start with `^` so `Regex::is_match` matches the same positions.
    GENERIC_COMMIT_PATTERNS.iter().any(|p| p.is_match(first))
}

#[derive(Debug, Clone)]
struct CommitRow {
    message: String,
    timestamp: Option<String>,
    additions: i64,
    deletions: i64,
}

fn detect_fixup_pattern(commits: &[CommitRow]) -> bool {
    if commits.len() < 2 {
        return false;
    }
    let first_lines = commits[0].additions + commits[0].deletions;
    if first_lines < 100 {
        return false;
    }
    for c in &commits[1..] {
        let lines = c.additions + c.deletions;
        let msg = c.message.to_lowercase();
        if lines > 30 {
            return false;
        }
        if !FIXUP_KEYWORDS.iter().any(|kw| msg.contains(kw)) {
            return false;
        }
    }
    true
}

pub fn compute_pr_behavioral(
    conn: &Connection,
    pr_id: &str,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let student_id: Option<String> = conn
        .query_row(
            "SELECT author_id FROM pull_requests WHERE id = ?",
            [pr_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let Some(student_id) = student_id else {
        return Ok(());
    };

    let mut stmt = conn.prepare(
        "SELECT message, timestamp, additions, deletions
         FROM pr_commits WHERE pr_id = ? ORDER BY timestamp",
    )?;
    let commits: Vec<CommitRow> = stmt
        .query_map([pr_id], |r| {
            Ok(CommitRow {
                message: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                timestamp: r.get::<_, Option<String>>(1)?,
                additions: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                deletions: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let commit_count = commits.len() as i64;
    if commit_count == 0 {
        return Ok(());
    }

    let lines: Vec<i64> = commits.iter().map(|c| c.additions + c.deletions).collect();
    let max_lines = *lines.iter().max().unwrap_or(&0);
    let total_lines: i64 = lines.iter().sum();

    let single_commit = commit_count == 1;

    let timestamps: Vec<DateTime<Utc>> = commits
        .iter()
        .filter_map(|c| c.timestamp.as_deref().and_then(parse_iso))
        .collect();

    let mut avg_minutes: Option<f64> = None;
    let mut lines_per_min: Option<f64> = None;
    let mut productivity_anomaly = false;

    if timestamps.len() >= 2 {
        let mut diffs_min: Vec<f64> = Vec::new();
        for i in 1..timestamps.len() {
            let diff = (timestamps[i] - timestamps[i - 1]).num_seconds() as f64 / 60.0;
            diffs_min.push(diff.max(0.0));
        }
        if !diffs_min.is_empty() {
            avg_minutes = Some(diffs_min.iter().sum::<f64>() / diffs_min.len() as f64);
        }
        let total_min =
            (timestamps[timestamps.len() - 1] - timestamps[0]).num_seconds() as f64 / 60.0;
        if total_min > 0.0 {
            lines_per_min = Some(total_lines as f64 / total_min);
            for i in 1..timestamps.len() {
                let diff_min = (timestamps[i] - timestamps[i - 1]).num_seconds() as f64 / 60.0;
                let commit_lines = if i < lines.len() { lines[i] } else { 0 };
                if commit_lines > 200 && diff_min < 15.0 {
                    productivity_anomaly = true;
                }
            }
        }
    }

    let has_fixup = detect_fixup_pattern(&commits);
    let has_tests = commits
        .iter()
        .any(|c| c.message.to_lowercase().contains("test"));
    let has_intermediate = lines.iter().filter(|l| **l > 0 && **l < 20).count() >= 1;
    let has_merges = commits
        .iter()
        .any(|c| c.message.to_lowercase().contains("merge"));

    let generic_count = commits
        .iter()
        .filter(|c| is_generic_message(&c.message))
        .count() as i64;
    let generic_ratio = generic_count as f64 / commit_count as f64;

    let avg_msg_len: f64 = commits
        .iter()
        .map(|c| c.message.chars().count() as f64)
        .sum::<f64>()
        / commit_count as f64;

    conn.execute(
        "INSERT OR REPLACE INTO pr_behavioral_signals
         (pr_id, student_id, sprint_id, commit_count, single_commit_pr,
          max_lines_per_commit, avg_minutes_between_commits, has_fixup_pattern,
          lines_per_minute, productivity_anomaly, has_test_adjustments,
          has_intermediate_changes, has_branch_merges, generic_message_ratio,
          avg_message_length)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            pr_id,
            student_id,
            sprint_id,
            commit_count,
            single_commit,
            max_lines,
            avg_minutes,
            has_fixup,
            lines_per_min,
            productivity_anomaly,
            has_tests,
            has_intermediate,
            has_merges,
            generic_ratio,
            avg_msg_len,
        ],
    )?;
    Ok(())
}

pub fn compute_all_behavioral(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    let pr_ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for id in &pr_ids {
        compute_pr_behavioral(conn, id, sprint_id)?;
    }
    info!(
        count = pr_ids.len(),
        sprint_id, "behavioral signals computed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_commit(msg: &str, add: i64, del: i64) -> CommitRow {
        CommitRow {
            message: msg.into(),
            timestamp: None,
            additions: add,
            deletions: del,
        }
    }

    #[test]
    fn generic_messages_detected() {
        assert!(is_generic_message("fix"));
        assert!(is_generic_message("Fix"));
        assert!(is_generic_message("Update"));
        assert!(is_generic_message("WIP"));
        assert!(is_generic_message("Merge branch 'main'"));
        assert!(!is_generic_message("Add login flow for students"));
    }

    #[test]
    fn fixup_requires_big_first_then_small_fix_style() {
        let commits = vec![
            mk_commit("Huge first commit", 200, 10), // 210 lines
            mk_commit("fix typo", 1, 0),
            mk_commit("fix missing import", 2, 1),
        ];
        assert!(detect_fixup_pattern(&commits));
    }

    #[test]
    fn fixup_rejects_single_commit() {
        let commits = vec![mk_commit("one", 200, 10)];
        assert!(!detect_fixup_pattern(&commits));
    }

    #[test]
    fn fixup_rejects_non_fixup_followups() {
        let commits = vec![
            mk_commit("Huge first commit", 200, 10),
            mk_commit("Add user view", 15, 0), // no fixup keyword
        ];
        assert!(!detect_fixup_pattern(&commits));
    }
}
