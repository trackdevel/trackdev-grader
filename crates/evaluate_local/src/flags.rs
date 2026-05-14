//! Deterministic short-circuit detectors. Run before embedding/regressor
//! so the obvious-zero cases never pay the GPU cost.
//!
//! Empty/generic-title detection wraps the re-exported helpers from
//! `sprint-grader-evaluate` so this crate doesn't drift from the
//! authoritative implementation. Task-id-only and md-link-only detection
//! is hand-rolled to avoid pulling `regex` / `fancy-regex` into the
//! crate's dep graph for just two literals (see plan §"New crate").

pub use sprint_grader_evaluate::{is_empty_description, is_generic_title};

/// Regex string literal, duplicated verbatim from
/// `crates/evaluate/src/llm_eval.rs:107` for spec parity. Matching uses
/// the hand-rolled [`is_task_id_only_body`] below; this constant exists so
/// future readers can confirm the contract.
pub const TASK_ID_ONLY_REGEX: &str = r"^(\s*[A-Za-z]+-\d+\s*[,;]?\s*)+$";

/// Regex string literal, duplicated verbatim from
/// `crates/evaluate/src/llm_eval.rs:115`. See [`is_task_md_link_only_body`].
pub const TASK_MD_LINK_ONLY_REGEX: &str =
    r"^(\s*\[[A-Za-z][A-Za-z0-9]*-\d+\]\([^)]+\)\s*[,;]?\s*)+$";

/// Short-circuit flag types. Order is the triage evaluation order — see
/// [`crate::triage::TriagePolicy::decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetFlag {
    /// The body is empty (or template-only) per
    /// [`sprint_grader_evaluate::is_empty_description`].
    EmptyBody,
    /// The body is one or more task ids (possibly markdown-linked) and
    /// nothing else — content-free.
    TaskIdOnlyBody,
    /// The title is generic per [`sprint_grader_evaluate::is_generic_title`].
    GenericTitle,
}

/// Run the deterministic detectors and return every flag that fires.
pub fn detect(title: Option<&str>, body: Option<&str>) -> Vec<DetFlag> {
    let mut out = Vec::new();
    if is_empty_description(body) {
        out.push(DetFlag::EmptyBody);
    } else if let Some(b) = body {
        // Only check task-id-only when the body isn't already empty —
        // EmptyBody dominates (longer-than-20-char gate above already
        // handled the underweight short-body case).
        let trimmed = b.trim();
        if !trimmed.is_empty()
            && (is_task_id_only_body(trimmed) || is_task_md_link_only_body(trimmed))
        {
            out.push(DetFlag::TaskIdOnlyBody);
        }
    }
    if is_generic_title(title) {
        out.push(DetFlag::GenericTitle);
    }
    out
}

/// Matches [`TASK_ID_ONLY_REGEX`]: the trimmed body is one or more
/// `[A-Za-z]+-\d+` tokens, separated by whitespace and optional `,` / `;`.
pub fn is_task_id_only_body(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    let mut saw_token = false;
    for raw_tok in trimmed.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        if raw_tok.is_empty() {
            continue;
        }
        if !is_task_id_token(raw_tok) {
            return false;
        }
        saw_token = true;
    }
    saw_token
}

fn is_task_id_token(s: &str) -> bool {
    // [A-Za-z]+-\d+
    let mut alpha = 0usize;
    let mut iter = s.chars();
    while let Some(c) = iter.next() {
        if c.is_ascii_alphabetic() {
            alpha += 1;
            continue;
        }
        if c == '-' && alpha > 0 {
            let mut digits = 0usize;
            for rest in iter.by_ref() {
                if rest.is_ascii_digit() {
                    digits += 1;
                } else {
                    return false;
                }
            }
            return digits > 0;
        }
        return false;
    }
    false
}

/// Matches [`TASK_MD_LINK_ONLY_REGEX`]: the trimmed body is one or more
/// `[<task-id>](<url>)` markdown links, optionally separated by `,` / `;`.
pub fn is_task_md_link_only_body(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    let mut saw_link = false;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] != b'[' {
            return false;
        }
        i += 1;
        let tag_start = i;
        // anchor text: [A-Za-z][A-Za-z0-9]*-\d+
        if i >= bytes.len() || !bytes[i].is_ascii_alphabetic() {
            return false;
        }
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric()) {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'-' {
            return false;
        }
        i += 1;
        let digit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == digit_start {
            return false;
        }
        if i >= bytes.len() || bytes[i] != b']' {
            return false;
        }
        let _ = tag_start;
        i += 1;
        if i >= bytes.len() || bytes[i] != b'(' {
            return false;
        }
        i += 1;
        // URL: one or more non-`)` chars
        let url_start = i;
        while i < bytes.len() && bytes[i] != b')' {
            i += 1;
        }
        if i == url_start {
            return false;
        }
        if i >= bytes.len() || bytes[i] != b')' {
            return false;
        }
        i += 1;
        saw_link = true;
        // Optional trailing whitespace + separator + whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && (bytes[i] == b',' || bytes[i] == b';') {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
        }
    }
    saw_link
}
