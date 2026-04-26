//! Deterministic PR-documentation heuristics. Mirror of `src/evaluate/heuristics.py`.

use once_cell::sync::Lazy;
use regex::RegexSet;
use rusqlite::{params, Connection};
use serde_json::json;
use tracing::info;

// Patterns that indicate a generic, low-effort PR title.
// Uses RegexSet so one pass over the title tests them all; each pattern is
// anchored and carries `(?i)` where the Python used re.IGNORECASE.
static GENERIC_PATTERNS: Lazy<RegexSet> = Lazy::new(|| {
    RegexSet::new([
        r"^.{0,9}$",
        r"(?i)^fix\s*(bug)?$",
        r"(?i)^update[sd]?$",
        r"(?i)^change[sd]?$",
        r"(?i)^wip$",
        r"(?i)^test$",
        r"(?i)^refactor$",
        r"(?i)^hotfix$",
        r"(?i)^merge\s",
        r"^[A-Z]+-\d+$",
        r"^(feature|bugfix|hotfix)/",
        r"(?i)^dev(elop)?$",
    ])
    .expect("generic title regex set")
});

pub fn is_empty_description(body: Option<&str>) -> bool {
    let body = match body {
        Some(b) => b,
        None => return true,
    };
    let stripped = body.trim();
    if stripped.len() < 20 {
        return true;
    }
    // Empty if every non-blank line is a `#`-prefixed heading — i.e. the
    // PR body is a template that was never filled in.
    !stripped
        .split('\n')
        .any(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
}

pub fn is_generic_title(title: Option<&str>) -> bool {
    let title = match title {
        Some(t) => t.trim(),
        None => return true,
    };
    if title.is_empty() {
        return true;
    }
    // `RegexSet::is_match` returns true iff at least one regex matches.
    GENERIC_PATTERNS.is_match(title)
}

/// Run EMPTY_DESCRIPTION + GENERIC_TITLE checks on every PR linked to a
/// non-USER_STORY task in this sprint. Writes rows into `flags`.
pub fn run_heuristics_for_sprint_id(
    conn: &Connection,
    sprint_id: i64,
) -> rusqlite::Result<(usize, usize)> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name,
                pr.title, pr.body, pr.author_id
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    let rows: Vec<(
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = stmt
        .query_map([sprint_id], |r| {
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

    let mut empty_count = 0usize;
    let mut generic_count = 0usize;
    for (pr_id, pr_number, repo, title, body, author_id) in rows {
        // Resolve to task assignee, falling back to PR author.
        let assignee: Option<String> = conn
            .query_row(
                "SELECT t.assignee_id FROM tasks t
                 JOIN task_pull_requests tpr ON tpr.task_id = t.id
                 WHERE tpr.pr_id = ? AND t.sprint_id = ? AND t.type != 'USER_STORY' LIMIT 1",
                params![&pr_id, sprint_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        let student_id = match assignee.or(author_id) {
            Some(s) => s,
            None => continue,
        };

        if is_empty_description(body.as_deref()) {
            let body_len = body.as_deref().map(str::len).unwrap_or(0);
            conn.execute(
                "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
                 VALUES (?, ?, ?, ?, ?)",
                params![
                    student_id,
                    sprint_id,
                    "EMPTY_DESCRIPTION",
                    "WARNING",
                    json!({
                        "pr_number": pr_number,
                        "repo": repo,
                        "body_length": body_len,
                    })
                    .to_string(),
                ],
            )?;
            empty_count += 1;
        }

        if is_generic_title(title.as_deref()) {
            conn.execute(
                "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
                 VALUES (?, ?, ?, ?, ?)",
                params![
                    student_id,
                    sprint_id,
                    "GENERIC_TITLE",
                    "INFO",
                    json!({
                        "pr_number": pr_number,
                        "repo": repo,
                        "title": title,
                    })
                    .to_string(),
                ],
            )?;
            generic_count += 1;
        }
    }
    info!(
        sprint_id,
        empty = empty_count,
        generic = generic_count,
        "heuristics done"
    );
    Ok((empty_count, generic_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_is_empty() {
        assert!(is_empty_description(None));
        assert!(is_empty_description(Some("")));
        assert!(is_empty_description(Some("   ")));
        assert!(is_empty_description(Some("short")));
    }

    #[test]
    fn template_only_counts_as_empty() {
        assert!(is_empty_description(Some(
            "# Summary\n## Testing\n## Details\n"
        )));
    }

    #[test]
    fn real_description_is_not_empty() {
        assert!(!is_empty_description(Some(
            "Implements the login controller to authenticate students against the backend."
        )));
    }

    #[test]
    fn generic_titles_match() {
        for t in [
            "fix",
            "fix bug",
            "update",
            "changes",
            "WIP",
            "test",
            "PROJ-42",
            "feature/login",
            "Merge branch main",
            "",
            "short",
            "develop",
        ] {
            assert!(is_generic_title(Some(t)), "expected generic: {t:?}");
        }
    }

    #[test]
    fn specific_title_is_not_generic() {
        assert!(!is_generic_title(Some(
            "Implement login controller with JWT-based auth"
        )));
    }
}
