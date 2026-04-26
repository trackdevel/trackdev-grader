//! Per-PR line metrics: LAT, LAR, LS.
//! Mirrors `src/survival/diff_lines.py`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use rayon::prelude::*;
use regex::Regex;
use rusqlite::{params, params_from_iter, Connection};
use tracing::{debug, info, warn};

// ---- Comment / blank detection ----

static JAVA_LINE_COMMENT: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*//").unwrap());
static JAVA_BLOCK_OPEN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*/\*").unwrap());
static JAVA_BLOCK_CLOSE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\*/\s*$").unwrap());
static JAVA_BLOCK_CONT: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*\*").unwrap());
static XML_COMMENT_OPEN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*<!--").unwrap());
static XML_COMMENT_CLOSE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-->\s*$").unwrap());

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn is_comment_java(line: &str, in_block: bool) -> (bool, bool) {
    let stripped = line.trim_start();
    if in_block {
        if JAVA_BLOCK_CLOSE.is_match(stripped) {
            return (true, false);
        }
        return (true, true);
    }
    if JAVA_LINE_COMMENT.is_match(stripped) {
        return (true, false);
    }
    if JAVA_BLOCK_OPEN.is_match(stripped) {
        if JAVA_BLOCK_CLOSE.is_match(stripped) {
            return (true, false);
        }
        return (true, true);
    }
    if JAVA_BLOCK_CONT.is_match(stripped) {
        return (true, false);
    }
    (false, false)
}

fn is_comment_xml(line: &str, in_block: bool) -> (bool, bool) {
    let stripped = line.trim_start();
    if in_block {
        if XML_COMMENT_CLOSE.is_match(stripped) {
            return (true, false);
        }
        return (true, true);
    }
    if XML_COMMENT_OPEN.is_match(stripped) {
        if XML_COMMENT_CLOSE.is_match(stripped) {
            return (true, false);
        }
        return (true, true);
    }
    (false, false)
}

fn is_comment_line(line: &str, file_path: &str, in_block: bool) -> (bool, bool) {
    let ext = file_path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "java" => is_comment_java(line, in_block),
        "xml" => is_comment_xml(line, in_block),
        _ => (false, in_block),
    }
}

// ---- Per-repo file universe ----

fn code_patterns(repo_full_name: &str) -> Vec<&'static str> {
    let basename = repo_full_name
        .rsplit('/')
        .next()
        .unwrap_or(repo_full_name)
        .to_ascii_lowercase();
    if basename.starts_with("spring") {
        vec!["*.java"]
    } else if basename.starts_with("android") {
        vec!["*.java", "*.xml"]
    } else {
        warn!(
            repo = repo_full_name,
            "Repo does not match android-* or spring-* — defaulting to Java+XML"
        );
        vec!["*.java", "*.xml"]
    }
}

fn code_suffixes(repo_full_name: &str) -> Vec<&'static str> {
    code_patterns(repo_full_name)
        .into_iter()
        .map(|p| p.trim_start_matches('*'))
        .collect()
}

fn warn_if_kotlin_present(
    repo_path: &Path,
    base_sha: &str,
    last_sha: &str,
    pr_id: &str,
    repo_full_name: &str,
) {
    let out = Command::new("git")
        .args([
            "diff",
            "--name-only",
            &format!("{base_sha}..{last_sha}"),
            "--",
            "*.kt",
        ])
        .current_dir(repo_path)
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return,
    };
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return;
    }
    let files: Vec<&str> = text.lines().filter(|s| !s.trim().is_empty()).collect();
    if files.is_empty() {
        return;
    }
    let preview: Vec<&str> = files.iter().take(5).copied().collect();
    warn!(
        pr_id,
        repo = repo_full_name,
        count = files.len(),
        sample = ?preview,
        "PR contains Kotlin file(s)"
    );
}

// ---- Cosmetic change detection ----

static STRING_LIT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#""(?:[^"\\]|\\.)*""#).unwrap());
static CHAR_LIT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"'(?:[^'\\]|\\.)*'").unwrap());
static NUM_LIT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\d[\d_.]*[lLfFdD]?\b").unwrap());
static IDENT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b[a-zA-Z_]\w*\b").unwrap());
static WS_RUN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").unwrap());

fn normalize_for_cosmetic(line: &str) -> String {
    let s = line.trim();
    let s = STRING_LIT_RE.replace_all(s, r#""_""#).into_owned();
    let s = CHAR_LIT_RE.replace_all(&s, "'_'").into_owned();
    let s = NUM_LIT_RE.replace_all(&s, "_N_").into_owned();
    let s = IDENT_RE.replace_all(&s, "_ID_").into_owned();
    WS_RUN_RE.replace_all(&s, " ").trim().to_string()
}

// ---- Unified diff parsing ----

#[derive(Debug, Clone, Default)]
pub struct DiffHunk {
    pub file_path: String,
    pub removed_lines: Vec<String>,
    pub added_lines: Vec<String>,
}

pub fn parse_unified_diff(diff_text: &str) -> Vec<DiffHunk> {
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_idx: Option<usize> = None;

    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            if let Some(idx) = line.find(" b/") {
                current_file = Some(line[idx + 3..].to_string());
            }
            current_idx = None;
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if line.starts_with("@@") {
            if let Some(ref fp) = current_file {
                hunks.push(DiffHunk {
                    file_path: fp.clone(),
                    ..Default::default()
                });
                current_idx = Some(hunks.len() - 1);
            }
            continue;
        }
        let idx = match current_idx {
            Some(i) => i,
            None => continue,
        };
        if let Some(rest) = line.strip_prefix('-') {
            hunks[idx].removed_lines.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix('+') {
            hunks[idx].added_lines.push(rest.to_string());
        }
    }
    hunks
}

// ---- Per-PR metric result ----

#[derive(Debug, Clone, Default)]
pub struct PrLineMetrics {
    pub lat: i64,
    pub lar: i64,
    pub ls: i64,
    /// Lines Deleted (non-blank, non-comment) with cosmetic substitutions
    /// removed — analogous to `lar`, but for the removed side of each hunk.
    /// Captures legitimate cleanup value from refactors.
    pub ld: i64,
    pub cosmetic_lines: i64,
    pub cosmetic_report: String,
}

// ---- Git helpers ----

/// Public alias used by the orchestration crate's `debug-pr-lines` diagnostic.
pub fn default_branch_for(repo_path: &Path) -> String {
    get_default_branch(repo_path)
}

fn get_default_branch(repo_path: &Path) -> String {
    let out = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(repo_path)
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            if let Ok(s) = std::str::from_utf8(&o.stdout) {
                if let Some(name) = s.trim().rsplit('/').next() {
                    return name.to_string();
                }
            }
        }
    }
    for branch in ["main", "master"] {
        let o = Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
            .current_dir(repo_path)
            .output();
        if let Ok(o) = o {
            if o.status.success() {
                return branch.to_string();
            }
        }
    }
    "main".to_string()
}

fn get_file_from_branch(repo_path: &Path, branch: &str, file_path: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["show", &format!("{branch}:{file_path}")])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ensure_commits_available(repo_path: &Path, commit_shas: &[String]) {
    for sha in commit_shas {
        let o = Command::new("git")
            .args(["cat-file", "-t", sha])
            .current_dir(repo_path)
            .output();
        if let Ok(o) = o {
            if !o.status.success() {
                debug!(sha = %&sha[..12.min(sha.len())], "Commit not local; fetching PR refs");
                let _ = Command::new("git")
                    .args([
                        "fetch",
                        "origin",
                        "+refs/pull/*/head:refs/remotes/origin/pr/*",
                    ])
                    .current_dir(repo_path)
                    .output();
                return;
            }
        }
    }
}

fn find_base_sha(
    repo_path: &Path,
    default_branch: &str,
    first_sha: &str,
    last_sha: &str,
) -> Option<String> {
    for branch_ref in [
        format!("origin/{default_branch}"),
        default_branch.to_string(),
    ] {
        let out = Command::new("git")
            .args(["merge-base", &branch_ref, last_sha])
            .current_dir(repo_path)
            .output();
        let out = match out {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        let base = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if base.is_empty() {
            continue;
        }
        if base != last_sha {
            return Some(base);
        }

        // Case 2: merge commit merge. Find the merge commit whose parent is last_sha.
        let rev_out = Command::new("git")
            .args([
                "rev-list",
                "--parents",
                "--ancestry-path",
                &format!("{last_sha}..{branch_ref}"),
            ])
            .current_dir(repo_path)
            .output();
        let rev_out = match rev_out {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        let rev_text = String::from_utf8_lossy(&rev_out.stdout).into_owned();
        for line in rev_text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[2..].contains(&last_sha) {
                return Some(parts[1].to_string());
            }
        }
    }

    warn!(
        first_sha = %&first_sha[..first_sha.len().min(12)],
        last_sha = %&last_sha[..last_sha.len().min(12)],
        default_branch = %default_branch,
        "find_base_sha: merge-base lookup failed, falling back to first_sha^1; \
         LAT/LAR/LS may be overstated for rebased PRs",
    );
    let out = Command::new("git")
        .args(["rev-parse", &format!("{first_sha}^1")])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn build_repo_line_index(repo_path: &Path, branch: &str, repo_full_name: &str) -> HashSet<String> {
    let suffixes = code_suffixes(repo_full_name);
    let ls = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", branch])
        .current_dir(repo_path)
        .output();
    let ls = match ls {
        Ok(o) if o.status.success() => o,
        _ => return HashSet::new(),
    };
    let paths: Vec<String> = String::from_utf8_lossy(&ls.stdout)
        .lines()
        .filter(|p| !p.trim().is_empty() && suffixes.iter().any(|s| p.ends_with(s)))
        .map(str::to_string)
        .collect();

    let mut out: HashSet<String> = HashSet::new();
    for path in paths {
        let content = match get_file_from_branch(repo_path, branch, &path) {
            Some(c) => c,
            None => continue,
        };
        let mut in_block = false;
        for line in content.split('\n') {
            if is_blank(line) {
                continue;
            }
            let (is_comment, new_state) = is_comment_line(line, &path, in_block);
            in_block = new_state;
            if is_comment {
                continue;
            }
            out.insert(line.trim().to_string());
        }
    }
    out
}

// ---- Core per-PR computation ----

pub fn compute_metrics_for_pr(
    repo_path: &Path,
    pr_id: &str,
    commit_shas: &[String],
    default_branch: &str,
    repo_full_name: &str,
    head_line_index: Option<&HashSet<String>>,
) -> Option<PrLineMetrics> {
    if commit_shas.is_empty() {
        return None;
    }

    ensure_commits_available(repo_path, commit_shas);

    let first_sha = &commit_shas[0];
    let last_sha = commit_shas.last().unwrap();

    let base_sha = find_base_sha(repo_path, default_branch, first_sha, last_sha)?;
    warn_if_kotlin_present(repo_path, &base_sha, last_sha, pr_id, repo_full_name);

    let patterns = code_patterns(repo_full_name);
    let mut args: Vec<String> = vec![
        "diff".into(),
        "--no-color".into(),
        "--unified=0".into(),
        format!("{base_sha}..{last_sha}"),
        "--".into(),
    ];
    for p in &patterns {
        args.push((*p).to_string());
    }
    let diff_out = Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .output();
    let diff_text = match diff_out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    };
    if diff_text.trim().is_empty() {
        debug!(pr_id, base = %&base_sha[..12.min(base_sha.len())], head = %&last_sha[..12.min(last_sha.len())], "No diff output");
        return Some(PrLineMetrics::default());
    }
    let diff_text = if diff_text.contains("Binary files") {
        diff_text
            .lines()
            .filter(|l| !l.starts_with("Binary files "))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        diff_text
    };

    let hunks = parse_unified_diff(&diff_text);

    // LAT: non-blank, non-comment lines added in the code universe.
    let mut lat_lines: Vec<(String, String)> = Vec::new();
    for hunk in &hunks {
        let mut in_block = false;
        for line in &hunk.added_lines {
            if is_blank(line) {
                continue;
            }
            let (is_comment, new_state) = is_comment_line(line, &hunk.file_path, in_block);
            in_block = new_state;
            if is_comment {
                continue;
            }
            lat_lines.push((hunk.file_path.clone(), line.clone()));
        }
    }
    let lat = lat_lines.len() as i64;

    // LDR: non-blank, non-comment lines removed. Same filter as LAT but on
    // the removed side — captures refactor/cleanup value we would otherwise
    // miss (a PR that deletes 500 lines of boilerplate reads as 0 LS today).
    let mut ldr: i64 = 0;
    for hunk in &hunks {
        let mut in_block = false;
        for line in &hunk.removed_lines {
            if is_blank(line) {
                continue;
            }
            let (is_comment, new_state) = is_comment_line(line, &hunk.file_path, in_block);
            in_block = new_state;
            if is_comment {
                continue;
            }
            ldr += 1;
        }
    }

    // LAR: subtract cosmetic-only substitutions (paired by position within hunk).
    let mut cosmetic_count: i64 = 0;
    let mut cosmetic_examples: Vec<String> = Vec::new();
    for hunk in &hunks {
        if hunk.removed_lines.is_empty() || hunk.added_lines.is_empty() {
            continue;
        }
        let n_pairs = hunk.removed_lines.len().min(hunk.added_lines.len());
        for i in 0..n_pairs {
            let added = &hunk.added_lines[i];
            let removed = &hunk.removed_lines[i];
            if is_blank(added) || is_blank(removed) {
                continue;
            }
            let na = normalize_for_cosmetic(added);
            let nr = normalize_for_cosmetic(removed);
            if na == nr && added.trim() != removed.trim() {
                cosmetic_count += 1;
                if cosmetic_examples.len() < 3 {
                    cosmetic_examples.push(format!("  {:?} → {:?}", removed.trim(), added.trim()));
                }
            }
        }
    }
    let lar = (lat - cosmetic_count).max(0);
    // LD: removed non-blank/non-comment lines, with cosmetic substitutions
    // removed (mirroring how LAR strips cosmetic churn from LAT).
    let ld = (ldr - cosmetic_count).max(0);

    let mut cosmetic_report = String::new();
    if lat > 0 && (cosmetic_count as f64) / (lat as f64) > 0.05 {
        let pct = (cosmetic_count as f64) / (lat as f64) * 100.0;
        cosmetic_report = format!("{cosmetic_count} cosmetic changes ({pct:.0}% of LAT).");
        if !cosmetic_examples.is_empty() {
            cosmetic_report.push_str(" Examples:\n");
            cosmetic_report.push_str(&cosmetic_examples.join("\n"));
        }
    }

    // LS: which LAT lines survive in HEAD (any file)?
    let fresh_index;
    let index: &HashSet<String> = match head_line_index {
        Some(idx) => idx,
        None => {
            fresh_index = build_repo_line_index(repo_path, default_branch, repo_full_name);
            &fresh_index
        }
    };
    let survived = lat_lines
        .iter()
        .filter(|(_fp, line)| index.contains(line.trim()))
        .count() as i64;

    Some(PrLineMetrics {
        lat,
        lar,
        ls: survived,
        ld,
        cosmetic_lines: cosmetic_count,
        cosmetic_report,
    })
}

// ---- Sprint-level driver ----

pub fn compute_pr_line_metrics(
    conn: &Connection,
    sprint_id: i64,
    repo_map: &HashMap<String, PathBuf>,
    max_workers: usize,
    include_all_merged: bool,
) -> rusqlite::Result<i64> {
    // Gather PRs (merged, linked to non-USER_STORY tasks in this sprint).
    let mut prs: Vec<(String, String)> = Vec::new(); // (pr_id, repo_full_name)
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pr.id, pr.repo_full_name
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY' AND pr.merged = 1",
        )?;
        let rows = stmt.query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?;
        for r in rows {
            prs.push(r?);
        }
    }

    if include_all_merged {
        let mut sprint_stmt =
            conn.prepare("SELECT start_date, end_date FROM sprints WHERE id = ?")?;
        if let Ok((start, end)) = sprint_stmt.query_row([sprint_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        }) {
            if let (Some(start), Some(end)) = (start, end) {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT pr.id, pr.repo_full_name
                     FROM pull_requests pr
                     WHERE pr.merged = 1
                       AND pr.merged_at >= ? AND pr.merged_at <= ?",
                )?;
                let rows = stmt.query_map([&start, &end], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                })?;
                let mut seen: HashSet<String> = prs.iter().map(|(id, _)| id.clone()).collect();
                for r in rows {
                    let (id, repo) = r?;
                    if seen.insert(id.clone()) {
                        prs.push((id, repo));
                    }
                }
            }
        }
    }

    if prs.is_empty() {
        return Ok(0);
    }

    // Commits per PR (ordered by timestamp).
    let mut pr_commits: HashMap<String, Vec<String>> = HashMap::new();
    for (pr_id, _) in &prs {
        let mut stmt =
            conn.prepare("SELECT sha FROM pr_commits WHERE pr_id = ? ORDER BY timestamp ASC")?;
        let rows = stmt.query_map([pr_id], |r| r.get::<_, String>(0))?;
        let shas: Vec<String> = rows.collect::<rusqlite::Result<_>>()?;
        if !shas.is_empty() {
            pr_commits.insert(pr_id.clone(), shas);
        }
    }

    // Cache check — skip PRs whose last SHA already has a stored line-metrics row.
    let cached: HashMap<String, Option<String>> = {
        let mut stmt =
            conn.prepare("SELECT pr_id, merge_sha FROM pr_line_metrics WHERE sprint_id = ?")?;
        let rows = stmt.query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut m: HashMap<String, Option<String>> = HashMap::new();
        for r in rows {
            let (id, sha) = r?;
            m.insert(id, sha);
        }
        m
    };

    let mut prs_to_process: Vec<(String, String)> = Vec::new();
    let mut skipped_cached = 0usize;
    for (pr_id, repo) in &prs {
        let last_sha = pr_commits.get(pr_id).and_then(|v| v.last()).cloned();
        if let (Some(last), Some(Some(cached_sha))) = (last_sha.as_ref(), cached.get(pr_id)) {
            if cached_sha == last {
                skipped_cached += 1;
                continue;
            }
        }
        prs_to_process.push((pr_id.clone(), repo.clone()));
    }
    if skipped_cached > 0 {
        info!(
            skipped = skipped_cached,
            "Skipping PRs with cached line metrics (unchanged merge SHA)"
        );
    }

    // Per-repo caches, computed lazily on-demand.
    let branch_cache: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
    let head_index_cache: Mutex<HashMap<String, HashSet<String>>> = Mutex::new(HashMap::new());

    let get_branch = |repo_full_name: &str| -> String {
        let mut bc = branch_cache.lock().unwrap();
        if let Some(b) = bc.get(repo_full_name) {
            return b.clone();
        }
        let b = match repo_map.get(repo_full_name) {
            Some(p) => get_default_branch(p),
            None => "main".to_string(),
        };
        bc.insert(repo_full_name.to_string(), b.clone());
        b
    };

    let get_head_index = |repo_full_name: &str| -> HashSet<String> {
        {
            let cache = head_index_cache.lock().unwrap();
            if let Some(idx) = cache.get(repo_full_name) {
                return idx.clone();
            }
        }
        let repo_path = match repo_map.get(repo_full_name) {
            Some(p) if p.is_dir() => p.clone(),
            _ => {
                let mut cache = head_index_cache.lock().unwrap();
                cache.insert(repo_full_name.to_string(), HashSet::new());
                return HashSet::new();
            }
        };
        let branch = get_branch(repo_full_name);
        let idx = build_repo_line_index(&repo_path, &branch, repo_full_name);
        info!(
            repo = repo_full_name,
            branch = %branch,
            unique = idx.len(),
            "HEAD line index built"
        );
        let mut cache = head_index_cache.lock().unwrap();
        cache.insert(repo_full_name.to_string(), idx.clone());
        idx
    };

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_workers.max(1))
        .build()
        .expect("rayon pool");

    let results: Vec<(String, Option<PrLineMetrics>, Option<String>)> = pool.install(|| {
        prs_to_process
            .par_iter()
            .map(|(pr_id, repo_full_name)| {
                let repo_path = match repo_map.get(repo_full_name) {
                    Some(p) if p.is_dir() => p.clone(),
                    _ => {
                        warn!(pr_id, repo = repo_full_name, "Repo not found");
                        return (pr_id.clone(), None, None);
                    }
                };
                let shas = match pr_commits.get(pr_id) {
                    Some(v) => v.clone(),
                    None => {
                        warn!(pr_id, "No commits for PR");
                        return (pr_id.clone(), None, None);
                    }
                };
                let branch = get_branch(repo_full_name);
                let head_idx = get_head_index(repo_full_name);
                let metrics = compute_metrics_for_pr(
                    &repo_path,
                    pr_id,
                    &shas,
                    &branch,
                    repo_full_name,
                    Some(&head_idx),
                );
                let last_sha = shas.last().cloned();
                (pr_id.clone(), metrics, last_sha)
            })
            .collect()
    });

    let mut count = 0i64;
    for (pr_id, metrics, merge_sha) in results {
        let metrics = match metrics {
            Some(m) => m,
            None => continue,
        };
        conn.execute(
            "INSERT OR REPLACE INTO pr_line_metrics
             (pr_id, sprint_id, lat, lar, ls, ld, cosmetic_lines, cosmetic_report, merge_sha)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                pr_id,
                sprint_id,
                metrics.lat,
                metrics.lar,
                metrics.ls,
                metrics.ld,
                metrics.cosmetic_lines,
                metrics.cosmetic_report,
                merge_sha,
            ],
        )?;
        count += 1;
    }

    info!(
        processed = count,
        total = prs.len(),
        sprint_id,
        cached = skipped_cached,
        "Computed line metrics"
    );
    // Keep `params_from_iter` import used to avoid unused warnings in future grows.
    let _ = params_from_iter::<[&dyn rusqlite::ToSql; 0]>([]);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO(P0-7): assert the warn fires on fallback. Requires `tracing_test`
    // or a custom subscriber. Not added in this chunk to avoid a new dep.

    #[test]
    fn parses_unified_diff() {
        let diff = "\
diff --git a/Foo.java b/Foo.java
--- a/Foo.java
+++ b/Foo.java
@@ -1,3 +1,3 @@
-int x = 1;
+int x = 2;
 unchanged
-int y = 3;
+int y = 4;
";
        let hunks = parse_unified_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file_path, "Foo.java");
        assert_eq!(hunks[0].removed_lines.len(), 2);
        assert_eq!(hunks[0].added_lines.len(), 2);
    }

    #[test]
    fn cosmetic_normalizer_folds_rename_and_whitespace() {
        let a = "    int count = 0;";
        let b = "int total = 0;";
        assert_eq!(normalize_for_cosmetic(a), normalize_for_cosmetic(b));
    }
}
