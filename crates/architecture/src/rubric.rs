//! Markdown rubric loader (T-P3.2).
//!
//! Groundwork for T-P3.3 (LLM-judged architecture review). The rubric is
//! prose, written by the instructor, split into per-stack sections by H1
//! heading. T-P3.3 will pick a section per file based on the cloned repo's
//! stack and feed it to the model alongside the file body.
//!
//! ### Format
//!
//! ```markdown
//! ---
//! version: 1
//! ---
//!
//! # Spring Boot rubric
//! ...prose...
//!
//! # Android rubric
//! ...prose...
//! ```
//!
//! ### Versioning
//!
//! Two cache-invalidation knobs:
//!
//! - `version` (frontmatter) — the *public* knob. Bump it deliberately
//!   when you want existing LLM judgements re-evaluated.
//! - `body_hash` — sha256 of the post-frontmatter body, after trimming
//!   trailing whitespace per line. The safety net: if the rubric is
//!   edited substantively without a version bump, the hash changes too,
//!   so the T-P3.3 cache key `{version}:{body_hash}` still invalidates.
//!
//! Whitespace-only edits do not change the hash — that's intentional, so
//! re-formatting the markdown source doesn't blow away the LLM cache.

use std::path::Path;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rubric {
    pub version: String,
    pub spring: String,
    pub android: String,
    pub body_hash: String,
}

impl Rubric {
    /// Resolve a stack alias to the matching section. Returns `None` if
    /// the rubric has no entry for the requested stack.
    pub fn for_stack(&self, stack: &str) -> Option<&str> {
        match normalize_stack(stack) {
            "spring" => (!self.spring.is_empty()).then_some(self.spring.as_str()),
            "android" => (!self.android.is_empty()).then_some(self.android.as_str()),
            _ => None,
        }
    }

    /// Cache key for T-P3.3. Combine with `model_id` and `file_sha` at
    /// the call site.
    pub fn cache_key_prefix(&self) -> String {
        format!("{}:{}", self.version, self.body_hash)
    }
}

fn normalize_stack(stack: &str) -> &str {
    let s = stack.trim().to_lowercase();
    if s.contains("spring") || s == "java-spring" || s == "backend" {
        return "spring";
    }
    if s.contains("android") || s == "java-android" || s == "mobile" {
        return "android";
    }
    "unknown"
}

pub fn load(path: &Path) -> anyhow::Result<Rubric> {
    let text = std::fs::read_to_string(path)?;
    parse(&text)
}

pub fn parse(text: &str) -> anyhow::Result<Rubric> {
    let (frontmatter, body) = split_frontmatter(text);
    let version = read_version(frontmatter).unwrap_or_else(|| "0".to_string());

    let normalized_body = normalize_for_hash(body);
    let body_hash = sha256_hex(&normalized_body);

    let (spring, android) = split_sections(body);

    Ok(Rubric {
        version,
        spring,
        android,
        body_hash,
    })
}

/// Returns `(frontmatter_text, body_text)`. If the file lacks frontmatter,
/// `frontmatter_text` is empty and `body_text` is the whole input.
fn split_frontmatter(text: &str) -> (&str, &str) {
    // YAML frontmatter is delimited by `---` on its own line. Accept the
    // file with or without a leading newline before the first `---`.
    let trimmed_start = text.trim_start_matches('\n');
    let leading_offset = text.len() - trimmed_start.len();
    if let Some(rest) = trimmed_start.strip_prefix("---\n") {
        // Find the closing `---` line.
        let mut idx = 0usize;
        for line in rest.split_inclusive('\n') {
            if line.trim_end_matches('\n') == "---" {
                let fm = &rest[..idx];
                let body_start = idx + line.len();
                let body = &rest[body_start..];
                let _ = leading_offset; // not needed once we slice from rest
                return (fm, body.trim_start_matches('\n'));
            }
            idx += line.len();
        }
    }
    ("", text)
}

fn read_version(frontmatter: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("version:") {
            return Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    None
}

/// Split the body into `(spring, android)` sections by H1 heading.
/// Heading text is matched case-insensitively against `spring` and
/// `android`. Anything before the first H1 is dropped (rubric-level
/// preamble has no stack to attach to). Anything between subsequent H1s
/// after the first two stacks is dropped — the format is fixed.
fn split_sections(body: &str) -> (String, String) {
    let mut spring = String::new();
    let mut android = String::new();
    let mut current: Option<&'static str> = None;

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            let h = rest.trim().to_lowercase();
            if h.contains("spring") {
                current = Some("spring");
                continue;
            } else if h.contains("android") {
                current = Some("android");
                continue;
            } else {
                current = None;
                continue;
            }
        }
        match current {
            Some("spring") => {
                spring.push_str(line);
                spring.push('\n');
            }
            Some("android") => {
                android.push_str(line);
                android.push('\n');
            }
            _ => {}
        }
    }
    (spring.trim().to_string(), android.trim().to_string())
}

/// Strip per-line trailing whitespace and collapse runs of blank lines.
/// Goal: hash is stable across formatting churn (extra trailing spaces,
/// runs of empty lines) but changes on real edits.
fn normalize_for_hash(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut prev_blank = false;
    for line in body.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if !prev_blank {
                out.push('\n');
            }
            prev_blank = true;
        } else {
            out.push_str(trimmed);
            out.push('\n');
            prev_blank = false;
        }
    }
    out
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let bytes = h.finalize();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
---
version: 2
---

# Spring Boot rubric

## Layering

- Controllers must not call repositories.
- Services return DTOs.

# Android rubric

## Layering

- Activities use repositories only.
";

    #[test]
    fn parses_frontmatter_and_extracts_sections() {
        let r = parse(SAMPLE).unwrap();
        assert_eq!(r.version, "2");
        assert!(r.spring.contains("Controllers must not call repositories"));
        assert!(r.android.contains("Activities use repositories only"));
        assert!(!r.spring.is_empty(), "spring section non-empty");
        assert_eq!(r.body_hash.len(), 64, "sha256 hex");
    }

    #[test]
    fn body_hash_changes_when_content_edits_but_version_unchanged() {
        let edited = SAMPLE.replace("Services return DTOs.", "Services return DTOs only.");
        let a = parse(SAMPLE).unwrap();
        let b = parse(&edited).unwrap();
        assert_eq!(a.version, b.version, "version stayed constant");
        assert_ne!(a.body_hash, b.body_hash, "but body hash changed");
    }

    #[test]
    fn body_hash_stable_across_whitespace_only_edits() {
        let extra_blanks = SAMPLE.replace("# Spring Boot rubric", "# Spring Boot rubric   \n\n\n");
        let a = parse(SAMPLE).unwrap();
        let b = parse(&extra_blanks).unwrap();
        assert_eq!(
            a.body_hash, b.body_hash,
            "trailing spaces + extra blank lines must not move the hash"
        );
    }

    #[test]
    fn for_stack_is_case_insensitive_and_normalises_aliases() {
        let r = parse(SAMPLE).unwrap();
        assert!(r.for_stack("spring").is_some());
        assert!(r.for_stack("SPRING").is_some());
        assert!(r.for_stack("java-spring").is_some());
        assert!(r.for_stack("backend").is_some());
        assert!(r.for_stack("android").is_some());
        assert!(r.for_stack("Android").is_some());
        assert!(r.for_stack("mobile").is_some());
        assert!(r.for_stack("kotlin-only").is_none());
    }

    #[test]
    fn no_frontmatter_yields_default_version() {
        let body = "# Spring Boot rubric\n- a rule.\n# Android rubric\n- another.\n";
        let r = parse(body).unwrap();
        assert_eq!(r.version, "0");
        assert!(r.spring.contains("a rule"));
    }

    #[test]
    fn cache_key_prefix_concatenates_version_and_hash() {
        let r = parse(SAMPLE).unwrap();
        let key = r.cache_key_prefix();
        assert!(key.starts_with("2:"), "version comes first");
        assert_eq!(key.len(), 2 + 64, "version + ':' + 64-char hash");
    }
}
