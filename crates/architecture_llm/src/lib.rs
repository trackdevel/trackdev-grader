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
pub mod deepseek_judge;
pub mod judge;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use sprint_grader_architecture::Rubric;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

/// How often the heartbeat thread emits "still waiting" progress while a
/// per-repo batch of LLM judge calls is in flight.
const HEARTBEAT_TICK_SECS: u64 = 30;
/// Cap on how many in-flight file paths the heartbeat names per tick;
/// the rest are summarised as "(+N more)" so the line stays readable.
const HEARTBEAT_FILE_PREVIEW: usize = 8;

pub use cli_judge::ClaudeCliJudge;
pub use deepseek_judge::DeepseekJudge;
pub use judge::{Judge, JudgeError, LlmJudge, LlmResponse, LlmViolation};

/// One LLM-driven evaluation run over a cloned repo. Artifact-shape
/// (T-P3.4): inserts rows into `architecture_violations` keyed by
/// `(repo_full_name, file_path, rule_name, offending_import, start_line)`,
/// sprint-free. Idempotent: prior LLM rows for this repo are deleted
/// first. Does **not** purge non-LLM rows — the AST + package-glob
/// path owns those.
///
/// `workers` controls intra-repo concurrency for cache-miss judge calls.
/// Cache lookups + DB writes stay serial on the single `Connection`;
/// only the slow judge call (Anthropic API, DeepSeek API, or `claude`
/// CLI subprocess) fans out across the worker pool.
///
/// Telemetry: each `judge.judge` call emits an `info!` line with the
/// per-file `elapsed_ms` + outcome; one per-repo summary at the end
/// reports `judged_ok` / `judged_failed` / `cached` counts plus p50 /
/// p95 wall-clock for the cache-miss calls.
#[allow(clippy::too_many_arguments)]
pub fn run_llm_review_for_repo(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
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

    // Idempotency: clear prior LLM rows for this repo and any attribution
    // rows that pointed at them, then re-insert from cache / API responses.
    // Mirrors crate `architecture`'s idiom.
    conn.execute(
        "DELETE FROM architecture_violation_attribution
         WHERE violation_rowid IN (
             SELECT rowid FROM architecture_violations
             WHERE repo_full_name = ? AND rule_kind = 'llm'
         )",
        params![repo_full_name],
    )?;
    conn.execute(
        "DELETE FROM architecture_violations
         WHERE repo_full_name = ? AND rule_kind = 'llm'",
        params![repo_full_name],
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

    let cached_count = resolved.len();
    let to_judge_count = to_judge.len();
    let started_phase2 = std::time::Instant::now();
    info!(
        repo = repo_full_name,
        cached = cached_count,
        to_judge = to_judge_count,
        workers,
        "llm review starting"
    );

    // Phase 2: parallel judge calls. Workers default to 1 (effectively
    // serial); higher values fan out across a Rayon pool. The pool is
    // local to this call so it doesn't contend with the workspace's
    // global `rayon::par_iter` pool elsewhere in the pipeline.
    //
    // Heartbeat: a separate std thread (NOT a rayon worker, so it doesn't
    // steal a slot from the API calls) watches a shared `pending` set and
    // logs "still waiting" every HEARTBEAT_TICK_SECS while the batch is
    // in flight. Each worker removes its file from the set on completion
    // (success OR failure), so by the time `pool.install` returns the
    // set is empty.
    let workers = workers.max(1);
    // Build the pool first so its construction failure path doesn't need
    // to tear down the heartbeat thread.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
        })?;

    let pending: Arc<Mutex<BTreeSet<String>>> =
        Arc::new(Mutex::new(to_judge.iter().map(|j| j.rel.clone()).collect()));
    let stop_heartbeat = Arc::new(AtomicBool::new(false));
    let heartbeat_handle = spawn_heartbeat(
        repo_full_name.to_string(),
        to_judge_count,
        pending.clone(),
        stop_heartbeat.clone(),
    );

    // Per-call telemetry: (file_sha, rel, total_lines, result, elapsed_ms).
    type JudgeOutcome = (String, String, u32, Result<String, JudgeError>, i64);
    let judge_outcomes: Vec<JudgeOutcome> = {
        pool.install(|| {
            to_judge
                .par_iter()
                .map(|job| {
                    let started = std::time::Instant::now();
                    let raw = judge
                        .judge(&job.rel, rubric_section, &job.bytes)
                        .and_then(|r| {
                            serde_json::to_string(&r).map_err(|e| JudgeError::Parse(e.to_string()))
                        });
                    let elapsed_ms = started.elapsed().as_millis() as i64;
                    // Mark this file as no longer pending BEFORE the
                    // info! log, so a heartbeat tick that fires
                    // concurrently doesn't list a file we just finished.
                    pending.lock().unwrap().remove(&job.rel);
                    match &raw {
                        Ok(_) => info!(
                            repo = repo_full_name,
                            file = %job.rel,
                            elapsed_ms,
                            "llm judge ok"
                        ),
                        Err(e) => info!(
                            repo = repo_full_name,
                            file = %job.rel,
                            elapsed_ms,
                            error = %e,
                            "llm judge failed"
                        ),
                    }
                    (
                        job.file_sha.clone(),
                        job.rel.clone(),
                        job.total_lines,
                        raw,
                        elapsed_ms,
                    )
                })
                .collect()
        })
    };
    stop_heartbeat.store(true, Ordering::Relaxed);
    let _ = heartbeat_handle.join();

    // Phase 3: cache writes + roll cache hits and successful misses into
    // a single `resolved` queue. Failed judge calls drop the file. We
    // also tally per-call elapsed times for the per-repo summary.
    let mut judged_ok = 0usize;
    let mut judged_failed = 0usize;
    let mut elapsed_samples: Vec<i64> = Vec::with_capacity(judge_outcomes.len());
    for (file_sha, rel, total_lines, raw_result, elapsed_ms) in judge_outcomes {
        elapsed_samples.push(elapsed_ms);
        match raw_result {
            Ok(raw) => {
                judged_ok += 1;
                cache::insert(conn, &file_sha, &rubric_key, judge.model_id(), &raw)?;
                resolved.push(ResolvedFile {
                    rel,
                    response_json: raw,
                    total_lines,
                });
            }
            Err(e) => {
                judged_failed += 1;
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
            insert_llm_violation(conn, repo_full_name, &rel, &rubric_key, &v)?;
            written += 1;
        }
    }

    let (p50_ms, p95_ms) = percentile_pair(&mut elapsed_samples, 50, 95);
    let phase2_total_ms = started_phase2.elapsed().as_millis() as i64;
    info!(
        repo = repo_full_name,
        violations = written,
        cached = cached_count,
        judged_ok,
        judged_failed,
        p50_ms,
        p95_ms,
        phase2_total_ms,
        rubric_key = %rubric_key,
        workers,
        "llm architecture review complete"
    );
    Ok(written)
}

fn insert_llm_violation(
    conn: &Connection,
    repo_full_name: &str,
    file_path: &str,
    rubric_key: &str,
    v: &LlmViolation,
) -> rusqlite::Result<()> {
    // The new artifact PK includes start_line, so the historical
    // `<rule_id>@L<line>` disambiguator on `offending_import` is no
    // longer needed for uniqueness. We keep `rule_id` itself in
    // `offending_import` so queries that join on the column have a
    // stable handle to the rule hit.
    conn.execute(
        "INSERT OR REPLACE INTO architecture_violations
            (repo_full_name, file_path, rule_name,
             violation_kind, offending_import, severity,
             start_line, end_line, rule_kind, rule_version, explanation)
         VALUES (?, ?, ?, 'llm', ?, ?, ?, ?, 'llm', ?, ?)",
        params![
            repo_full_name,
            file_path,
            v.rule_id,
            v.rule_id,
            v.severity,
            v.start_line as i64,
            v.end_line as i64,
            rubric_key,
            v.explanation,
        ],
    )?;
    Ok(())
}

/// Spawn the heartbeat thread that logs "still waiting on N/M files"
/// every `HEARTBEAT_TICK_SECS` while the per-repo batch is in flight.
/// Cancellation is via the shared `stop` flag (set by the main thread
/// after `pool.install` returns). Polls every 200ms so cancel is prompt.
fn spawn_heartbeat(
    repo_full_name: String,
    total: usize,
    pending: Arc<Mutex<BTreeSet<String>>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        const POLL_INTERVAL_MS: u64 = 200;
        let started = Instant::now();
        let mut ticks_logged: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            let elapsed = started.elapsed().as_secs();
            let due = elapsed / HEARTBEAT_TICK_SECS;
            if due > ticks_logged {
                ticks_logged = due;
                let snapshot: Vec<String> = {
                    let g = pending.lock().expect("heartbeat pending poisoned");
                    g.iter().cloned().collect()
                };
                if !snapshot.is_empty() {
                    let take = snapshot.len().min(HEARTBEAT_FILE_PREVIEW);
                    let head: Vec<String> = snapshot.iter().take(take).cloned().collect();
                    let extra = snapshot.len() - take;
                    let preview = if extra == 0 {
                        head.join(", ")
                    } else {
                        format!("{} (+{} more)", head.join(", "), extra)
                    };
                    info!(
                        repo = %repo_full_name,
                        pending = snapshot.len(),
                        total,
                        elapsed_s = elapsed,
                        files = %preview,
                        "llm review heartbeat — still waiting"
                    );
                }
            }
            thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        }
    })
}

/// Approximate p_a / p_b percentiles of a slice of i64 samples. Mutates
/// the slice (in-place sort). Returns 0 / 0 when empty. Cheap nearest-rank.
fn percentile_pair(samples: &mut [i64], p_a: u32, p_b: u32) -> (i64, i64) {
    if samples.is_empty() {
        return (0, 0);
    }
    samples.sort_unstable();
    let pick = |p: u32| {
        let n = samples.len();
        let idx = ((p as usize * n).saturating_sub(1)) / 100;
        samples[idx.min(n - 1)]
    };
    (pick(p_a), pick(p_b))
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

    #[test]
    fn heartbeat_thread_exits_promptly_when_stopped() {
        // The heartbeat polls every 200ms, so a clean shutdown should
        // join in well under 1s. This guards against the cancellation
        // path being broken by a longer sleep or a missing exit check.
        let pending = Arc::new(Mutex::new(BTreeSet::from([
            "a.java".to_string(),
            "b.java".to_string(),
        ])));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_heartbeat("repo".into(), 2, pending, stop.clone());
        let joined_within = {
            let started = Instant::now();
            stop.store(true, Ordering::Relaxed);
            handle.join().expect("heartbeat thread panicked");
            started.elapsed()
        };
        assert!(
            joined_within < Duration::from_millis(800),
            "heartbeat took too long to exit: {joined_within:?}"
        );
    }
}
