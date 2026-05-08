//! Markdown rubric loader (T-P3.2).
//!
//! Groundwork for T-P3.3 (LLM-judged architecture review). The rubric is
//! prose, written by the instructor, one file per stack. The orchestrator
//! detects the stack of each cloned repo and feeds the matching rubric to
//! the model alongside the file body.
//!
//! ### Format
//!
//! ```markdown
//! ---
//! version: 1
//! ---
//!
//! # <stack> rubric
//! ...prose...
//!
//! # Severity guidance
//! ...prose...
//! ```
//!
//! The body is sent to the judge verbatim — the loader does not split by
//! H1 heading, so `# Severity guidance` (or any other section the
//! instructor cares to include) reaches the model.
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
    pub body: String,
    pub body_hash: String,
}

impl Rubric {
    /// Cache key for T-P3.3. Combine with `model_id` and `file_sha` at
    /// the call site.
    pub fn cache_key_prefix(&self) -> String {
        format!("{}:{}", self.version, self.body_hash)
    }
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

    Ok(Rubric {
        version,
        body: body.trim().to_string(),
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

    const SAMPLE_SPRING: &str = "\
---
version: 2
---

# Spring Boot rubric

## Layering

- Controllers must not call repositories.
- Services return DTOs.

# Severity guidance

- CRITICAL — security impact.
";

    #[test]
    fn parses_frontmatter_and_extracts_body() {
        let r = parse(SAMPLE_SPRING).unwrap();
        assert_eq!(r.version, "2");
        assert!(r.body.contains("Controllers must not call repositories"));
        assert!(
            r.body.contains("Severity guidance"),
            "body keeps every H1 section, not just the first"
        );
        assert_eq!(r.body_hash.len(), 64, "sha256 hex");
    }

    #[test]
    fn body_hash_changes_when_content_edits_but_version_unchanged() {
        let edited = SAMPLE_SPRING.replace("Services return DTOs.", "Services return DTOs only.");
        let a = parse(SAMPLE_SPRING).unwrap();
        let b = parse(&edited).unwrap();
        assert_eq!(a.version, b.version, "version stayed constant");
        assert_ne!(a.body_hash, b.body_hash, "but body hash changed");
    }

    #[test]
    fn body_hash_stable_across_whitespace_only_edits() {
        let extra_blanks =
            SAMPLE_SPRING.replace("# Spring Boot rubric", "# Spring Boot rubric   \n\n\n");
        let a = parse(SAMPLE_SPRING).unwrap();
        let b = parse(&extra_blanks).unwrap();
        assert_eq!(
            a.body_hash, b.body_hash,
            "trailing spaces + extra blank lines must not move the hash"
        );
    }

    #[test]
    fn no_frontmatter_yields_default_version() {
        let body = "# Spring Boot rubric\n- a rule.\n";
        let r = parse(body).unwrap();
        assert_eq!(r.version, "0");
        assert!(r.body.contains("a rule"));
    }

    #[test]
    fn cache_key_prefix_concatenates_version_and_hash() {
        let r = parse(SAMPLE_SPRING).unwrap();
        let key = r.cache_key_prefix();
        assert!(key.starts_with("2:"), "version comes first");
        assert_eq!(key.len(), 2 + 64, "version + ':' + 64-char hash");
    }
}
