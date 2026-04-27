//! LLM-judged architecture review with per-file caching (T-P3.3).
//!
//! Reads `config/architecture.md` (loaded by `crates/architecture/src/rubric.rs`)
//! and asks an LLM to grade each Java file in a cloned repo against the
//! stack-appropriate rubric section. Each violation the model returns
//! becomes one row in `architecture_violations` with `rule_kind = "llm"`,
//! a structured `explanation`, and a `(start_line, end_line)` range so
//! T-P3.1's blame attribution applies uniformly.
//!
//! ### Cache
//!
//! Keyed by `(file_sha, rubric_version_with_body_hash, model_id)`:
//!
//! - `file_sha` — sha256 of the file body. Changes when content changes.
//! - `rubric_version_with_body_hash` — `format!("{version}:{body_hash}")`
//!   built by `Rubric::cache_key_prefix()`. Changes when the rubric is
//!   edited substantively or the version frontmatter is bumped.
//! - `model_id` — the Anthropic model id. Different models may judge
//!   differently; cache keys keep them separate.
//!
//! On hit, the stored `response_json` is replayed; no API call. On miss,
//! the model is queried, the response persisted, then violations
//! emitted. `architecture_violations` rows themselves are *not* cached;
//! they're rebuilt from the cached response on every run, so the
//! existing per-(repo, sprint) DELETE-then-INSERT idempotency idiom
//! continues to apply without special cases.
//!
//! ### Determinism
//!
//! - `temperature = 0` on every API call.
//! - Strict JSON-shape validation; non-conforming responses log WARN
//!   and emit zero violations rather than retrying or guessing.
//! - Same `(file_sha, rubric_key, model_id)` → same cache hit → same
//!   `architecture_violations` rows on re-run.
//!
//! ### Failure modes
//!
//! - Missing `ANTHROPIC_API_KEY` → silent skip. The pipeline shouldn't
//!   hard-fail on missing credentials; this matches the `evaluate`
//!   crate's existing fallback contract.
//! - LLM returns line ranges outside `[1, file_lines]` → the offending
//!   violation is dropped at WARN level. Don't try to repair.

pub mod cache;
pub mod cli_judge;
pub mod judge;

use std::path::Path;

use rayon::prelude::*;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use sprint_grader_architecture::Rubric;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

pub use cli_judge::ClaudeCliJudge;
pub use judge::{Judge, JudgeError, LlmJudge, LlmResponse, LlmViolation};

/// One LLM-driven evaluation run over a cloned repo for a sprint.
/// Inserts new rows into `architecture_violations` (idempotent: prior
/// LLM rows for this `(repo, sprint)` are deleted first). Does **not**
/// purge non-LLM rows — the AST + package-glob path owns those.
///
/// `workers` controls intra-repo concurrency for cache-miss judge calls.
/// Cache lookups + DB writes stay serial on the single `Connection`;
/// only the slow judge call (Anthropic API or `claude` CLI subprocess)
/// fans out across the worker pool.
#[allow(clippy::too_many_arguments)]
pub fn run_llm_review_for_repo(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    rubric: &Rubric,
    stack: &str,
    judge: &(dyn Judge + Send + Sync),
    skip_globs: &[String],
    workers: usize,
) -> rusqlite::Result<usize> {
    let rubric_section = match rubric.for_stack(stack) {
        Some(s) => s,
        None => {
            warn!(
                stack,
                "no rubric section for stack — skipping LLM architecture review"
            );
            return Ok(0);
        }
    };
    let rubric_key = rubric.cache_key_prefix();

    // Idempotency: clear prior LLM rows for this (repo, sprint) and any
    // attribution rows that pointed at them, then re-insert from cache /
    // API responses. Mirrors crate `architecture`'s idiom.
    conn.execute(
        "DELETE FROM architecture_violation_attribution
         WHERE violation_rowid IN (
             SELECT rowid FROM architecture_violations
             WHERE repo_full_name = ? AND sprint_id = ? AND rule_kind = 'llm'
         )",
        params![repo_full_name, sprint_id],
    )?;
    conn.execute(
        "DELETE FROM architecture_violations
         WHERE repo_full_name = ? AND sprint_id = ? AND rule_kind = 'llm'",
        params![repo_full_name, sprint_id],
    )?;

    // Phase 1: walk files, separate cache hits from misses. All DB I/O
    // happens here, on the single connection.
    struct JudgeJob {
        file_sha: String,
        rel: String,
        bytes: Vec<u8>,
        total_lines: u32,
    }
    struct ResolvedFile {
        rel: String,
        response_json: String,
        total_lines: u32,
    }

    let mut to_judge: Vec<JudgeJob> = Vec::new();
    let mut resolved: Vec<ResolvedFile> = Vec::new();

    for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("java") {
            continue;
        }
        let rel = match path.strip_prefix(repo_path) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(_) => continue,
        };
        if skip_globs.iter().any(|g| simple_glob_match(g, &rel)) {
            debug!(file = %rel, "skipping LLM review (skip glob)");
            continue;
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let file_sha = sha256_hex(&bytes);
        let total_lines = bytes.iter().filter(|b| **b == b'\n').count() as u32 + 1;
        match cache::lookup(conn, &file_sha, &rubric_key, judge.model_id())? {
            Some(j) => resolved.push(ResolvedFile {
                rel,
                response_json: j,
                total_lines,
            }),
            None => to_judge.push(JudgeJob {
                file_sha,
                rel,
                bytes,
                total_lines,
            }),
        }
    }

    // Phase 2: parallel judge calls. Workers default to 1 (effectively
    // serial); higher values fan out across a Rayon pool. The pool is
    // local to this call so it doesn't contend with the workspace's
    // global `rayon::par_iter` pool elsewhere in the pipeline.
    let workers = workers.max(1);
    let judge_outcomes: Vec<(String, String, u32, Result<String, JudgeError>)> = {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .map_err(|e| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                    e.to_string(),
                )))
            })?;
        pool.install(|| {
            to_judge
                .par_iter()
                .map(|job| {
                    let raw = judge
                        .judge(&job.rel, rubric_section, &job.bytes)
                        .and_then(|r| {
                            serde_json::to_string(&r).map_err(|e| JudgeError::Parse(e.to_string()))
                        });
                    (job.file_sha.clone(), job.rel.clone(), job.total_lines, raw)
                })
                .collect()
        })
    };

    // Phase 3: cache writes + roll cache hits and successful misses into
    // a single `resolved` queue. Failed judge calls drop the file (warn
    // logged once with the error).
    for (file_sha, rel, total_lines, raw_result) in judge_outcomes {
        match raw_result {
            Ok(raw) => {
                cache::insert(conn, &file_sha, &rubric_key, judge.model_id(), &raw)?;
                resolved.push(ResolvedFile {
                    rel,
                    response_json: raw,
                    total_lines,
                });
            }
            Err(e) => {
                warn!(file = %rel, error = %e, "judge call failed; skipping file");
            }
        }
    }

    // Phase 4: parse responses + insert violations.
    let mut written = 0usize;
    for ResolvedFile {
        rel,
        response_json,
        total_lines,
    } in resolved
    {
        let parsed: LlmResponse = match serde_json::from_str(&response_json) {
            Ok(p) => p,
            Err(e) => {
                warn!(file = %rel, error = %e, "cached judge response is not valid JSON; skipping");
                continue;
            }
        };
        for v in parsed.violations {
            if v.start_line < 1 || v.end_line < v.start_line || v.end_line > total_lines {
                warn!(
                    file = %rel,
                    rule_id = %v.rule_id,
                    start = v.start_line,
                    end = v.end_line,
                    file_lines = total_lines,
                    "dropping violation with out-of-range line numbers"
                );
                continue;
            }
            insert_llm_violation(conn, repo_full_name, sprint_id, &rel, &rubric_key, &v)?;
            written += 1;
        }
    }
    info!(
        repo = repo_full_name,
        sprint_id,
        violations = written,
        rubric_key = %rubric_key,
        workers,
        "llm architecture review complete"
    );
    Ok(written)
}

fn insert_llm_violation(
    conn: &Connection,
    repo_full_name: &str,
    sprint_id: i64,
    file_path: &str,
    rubric_key: &str,
    v: &LlmViolation,
) -> rusqlite::Result<()> {
    // Disambiguator suffix: same rule_id can fire on multiple ranges in
    // one file; the composite PK on architecture_violations would
    // collapse them otherwise.
    let descriptor = format!("{}@L{}", v.rule_id, v.start_line);
    conn.execute(
        "INSERT OR REPLACE INTO architecture_violations
            (repo_full_name, sprint_id, file_path, rule_name,
             violation_kind, offending_import, severity,
             start_line, end_line, rule_kind, rule_version, explanation)
         VALUES (?, ?, ?, ?, 'llm', ?, ?, ?, ?, 'llm', ?, ?)",
        params![
            repo_full_name,
            sprint_id,
            file_path,
            v.rule_id,
            descriptor,
            v.severity,
            v.start_line as i64,
            v.end_line as i64,
            rubric_key,
            v.explanation,
        ],
    )?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for b in out {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Minimal `**`/`*` glob matcher specialised for path segments separated
/// by `/`. Mirrors the architecture-glob style without pulling a glob
/// crate dependency. `**` matches any number of path segments (including
/// zero); `*` matches a single segment; literals match exactly.
fn simple_glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let p: Vec<&str> = path
        .split(['/', std::path::MAIN_SEPARATOR])
        .filter(|s| !s.is_empty())
        .collect();
    glob_recurse(&pat, &p)
}

fn glob_recurse(pat: &[&str], path: &[&str]) -> bool {
    if pat.is_empty() {
        return path.is_empty();
    }
    match pat[0] {
        "**" => {
            for i in 0..=path.len() {
                if glob_recurse(&pat[1..], &path[i..]) {
                    return true;
                }
            }
            false
        }
        "*" => !path.is_empty() && glob_recurse(&pat[1..], &path[1..]),
        lit => {
            if path.is_empty() {
                return false;
            }
            if !lit_matches(lit, path[0]) {
                return false;
            }
            glob_recurse(&pat[1..], &path[1..])
        }
    }
}

fn lit_matches(pattern_segment: &str, path_segment: &str) -> bool {
    if !pattern_segment.contains('*') {
        return pattern_segment == path_segment;
    }
    // Single in-segment '*' (e.g. `*$$*.java`); split on '*' and require
    // each fragment to appear in order in the path segment.
    let frags: Vec<&str> = pattern_segment.split('*').collect();
    let mut cursor = path_segment;
    for (i, frag) in frags.iter().enumerate() {
        if frag.is_empty() {
            continue;
        }
        let pos = if i == 0 {
            if !cursor.starts_with(frag) {
                return false;
            }
            frag.len()
        } else if i == frags.len() - 1 {
            if !cursor.ends_with(frag) {
                return false;
            }
            return true;
        } else {
            match cursor.find(frag) {
                Some(p) => p + frag.len(),
                None => return false,
            }
        };
        cursor = &cursor[pos..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_double_star_skips_generated() {
        assert!(simple_glob_match(
            "**/generated/**",
            "app/build/generated/source/Foo.java"
        ));
        assert!(!simple_glob_match(
            "**/generated/**",
            "app/src/main/java/Foo.java"
        ));
    }

    #[test]
    fn glob_match_in_segment_star_matches_dollar_dollar() {
        assert!(simple_glob_match("**/*$$*.java", "x/y/Foo$$Hilt.java"));
        assert!(!simple_glob_match("**/*$$*.java", "x/y/FooBar.java"));
    }

    #[test]
    fn glob_match_r_java() {
        assert!(simple_glob_match(
            "**/R.java",
            "app/src/main/java/com/x/R.java"
        ));
        assert!(!simple_glob_match(
            "**/R.java",
            "app/src/main/java/com/x/RR.java"
        ));
    }
}
