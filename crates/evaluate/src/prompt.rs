//! Token-minimal rubric text and per-PR user payloads for LLM judges.
//!
//! CLI backends (`claude-cli`, `cursor-cli`) send the full rubric on every
//! subprocess call, so [`RUBRIC_PR`] points at the compact asset. The verbose
//! rubric remains in `assets/prompts/rubric_pr.md` for human editing reference.

/// Compact rubric baked into the binary (see `assets/prompts/rubric_pr_compact.md`).
pub const RUBRIC_PR: &str = include_str!("../assets/prompts/rubric_pr_compact.md");

/// Human-tunable full rubric; not sent to judges by default.
pub const RUBRIC_PR_VERBOSE: &str = include_str!("../assets/prompts/rubric_pr.md");

pub const RUBRIC_TASK: &str = include_str!("../assets/prompts/rubric_task_compact.md");

pub const RUBRIC_TASK_VERBOSE: &str = include_str!("../assets/prompts/rubric_task.md");

/// Cap PR bodies sent to judges. DB max is ~5,231 chars; 5,500 covers all PRs
/// without sending unbounded boilerplate.
pub const MAX_PR_BODY_CHARS: usize = 5_500;

/// Cap task names in task-description judging.
pub const MAX_TASK_NAME_CHARS: usize = 500;

/// Truncate at a Unicode scalar boundary (not byte index).
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Minimal per-PR user turn: task + story + title + body only (no repo/PR#).
pub fn build_pr_judge_user_message(
    task_name: &str,
    parent_story: &str,
    title: &str,
    body: Option<&str>,
) -> String {
    let task = truncate_chars(task_name.trim(), MAX_TASK_NAME_CHARS);
    let story = truncate_chars(parent_story.trim(), MAX_TASK_NAME_CHARS);
    let title = truncate_chars(title.trim(), MAX_TASK_NAME_CHARS);
    let body_raw = body.map(str::trim).filter(|b| !b.is_empty()).unwrap_or("");
    let body = if body_raw.is_empty() {
        "(empty)".to_string()
    } else {
        truncate_chars(body_raw, MAX_PR_BODY_CHARS)
    };
    format!("task:{task}\nstory:{story}\ntitle:{title}\nbody:\n{body}")
}

/// Minimal per-task user turn for task-description judging.
pub fn build_task_judge_user_message(
    task_key: &str,
    parent_story: &str,
    description: Option<&str>,
) -> String {
    let key = truncate_chars(task_key.trim(), 64);
    let story = truncate_chars(parent_story.trim(), MAX_TASK_NAME_CHARS);
    let desc = description
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or("(empty)");
    let desc = truncate_chars(desc, MAX_PR_BODY_CHARS);
    format!("key:{key}\nstory:{story}\ndesc:\n{desc}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_adds_ellipsis_when_over_limit() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
    }

    #[test]
    fn build_pr_judge_user_message_omits_repo_and_pr_number() {
        let msg = build_pr_judge_user_message("Login", "US-1", "Fix auth", Some("body"));
        assert!(msg.contains("task:Login"));
        assert!(!msg.contains("PR #"));
        assert!(!msg.contains("repo:"));
    }

    #[test]
    fn compact_rubric_is_shorter_than_verbose() {
        assert!(
            RUBRIC_PR.len() < RUBRIC_PR_VERBOSE.len() / 2,
            "compact={} verbose={}",
            RUBRIC_PR.len(),
            RUBRIC_PR_VERBOSE.len()
        );
    }
}
