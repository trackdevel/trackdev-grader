//! PR compilation testing engine. Mirrors `src/compile/builder.py`.
//!
//! Key design points:
//! * Each PR gets an isolated `git worktree add <tempdir> <sha> --detach`.
//! * The build command runs via `/bin/sh -c "<command>"` to match Python's
//!   `subprocess.run(..., shell=True)` semantics.
//! * The timeout is a *hard* one — we use the `wait-timeout` crate, which
//!   kills the process when it elapses (instead of hoping it exits cleanly).
//! * Output is truncated to the last `max_output_chars` bytes, same as Python's
//!   `result.stdout[-max_output_chars:]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::json;
use tracing::{error, info, warn};
use wait_timeout::ChildExt;

use sprint_grader_core::config::BuildProfile as ConfigBuildProfile;

#[derive(Debug, Clone)]
pub struct BuildProfileRe {
    pub repo_pattern: Regex,
    pub command: String,
    pub timeout_seconds: u64,
    pub working_dir: String,
    pub env: HashMap<String, String>,
}

pub fn load_build_profiles_from_config(
    profiles: &[ConfigBuildProfile],
) -> Result<Vec<BuildProfileRe>, String> {
    if profiles.is_empty() {
        return Err("No [[build.profiles]] defined in course.toml".into());
    }
    let mut out = Vec::with_capacity(profiles.len());
    for p in profiles {
        let re = Regex::new(&p.repo_pattern)
            .map_err(|e| format!("bad repo_pattern `{}`: {e}", p.repo_pattern))?;
        out.push(BuildProfileRe {
            repo_pattern: re,
            command: p.command.clone(),
            timeout_seconds: p.timeout_seconds,
            working_dir: p.working_dir.clone(),
            env: p.env.clone(),
        });
    }
    Ok(out)
}

pub fn match_profile<'a>(
    repo_name: &str,
    profiles: &'a [BuildProfileRe],
) -> Option<&'a BuildProfileRe> {
    profiles.iter().find(|p| p.repo_pattern.is_match(repo_name))
}

#[derive(Debug, Clone)]
pub struct BuildResult {
    pub compiles: bool,
    pub exit_code: i32,
    pub stdout_text: String,
    pub stderr_text: String,
    pub duration_seconds: f64,
    pub timed_out: bool,
    pub build_command: String,
    pub working_dir: String,
    pub merge_sha: String,
}

/// Keep only the trailing `max_chars` bytes (Python: `text[-max_output_chars:]`).
/// Bytes, not grapheme clusters, to match Python's slicing.
fn tail(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    // Find a char boundary at or after the target offset to avoid slicing mid-char.
    let mut start = text.len() - max_chars;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

fn rev_parse_head(repo_path: &Path) -> String {
    let out = Command::new("git")
        .args(["-C", repo_path.to_str().unwrap_or("."), "rev-parse", "HEAD"])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

fn ensure_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o755));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Run the build command at `cwd = repo_path / profile.working_dir` with
/// a hard timeout. Uses `/bin/sh -c` to honor shell metacharacters like
/// `./gradlew assembleDebug`.
pub fn run_build(
    repo_path: &Path,
    profile: &BuildProfileRe,
    max_output_chars: usize,
) -> BuildResult {
    let cwd = repo_path.join(&profile.working_dir);
    let merge_sha = rev_parse_head(repo_path);

    let gradlew = cwd.join("gradlew");
    if gradlew.exists() {
        ensure_executable(&gradlew);
    }

    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(&profile.command)
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &profile.env {
        cmd.env(k, v);
    }

    let start = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return BuildResult {
                compiles: false,
                exit_code: -1,
                stdout_text: String::new(),
                stderr_text: format!("failed to spawn build: {e}"),
                duration_seconds: 0.0,
                timed_out: false,
                build_command: profile.command.clone(),
                working_dir: cwd.to_string_lossy().into_owned(),
                merge_sha,
            };
        }
    };

    let timeout = Duration::from_secs(profile.timeout_seconds);
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let duration = start.elapsed().as_secs_f64();
            let output = child.wait_with_output();
            let (stdout, stderr) = match output {
                Ok(o) => (
                    String::from_utf8_lossy(&o.stdout).into_owned(),
                    String::from_utf8_lossy(&o.stderr).into_owned(),
                ),
                Err(_) => (String::new(), String::new()),
            };
            BuildResult {
                compiles: status.success(),
                exit_code: status.code().unwrap_or(-1),
                stdout_text: tail(&stdout, max_output_chars),
                stderr_text: tail(&stderr, max_output_chars),
                duration_seconds: (duration * 100.0).round() / 100.0,
                timed_out: false,
                build_command: profile.command.clone(),
                working_dir: cwd.to_string_lossy().into_owned(),
                merge_sha,
            }
        }
        Ok(None) => {
            // Hard kill — timeout exceeded.
            let _ = child.kill();
            let _ = child.wait();
            let duration = start.elapsed().as_secs_f64();
            BuildResult {
                compiles: false,
                exit_code: -1,
                stdout_text: String::new(),
                stderr_text: format!("Build timed out after {}s", profile.timeout_seconds),
                duration_seconds: (duration * 100.0).round() / 100.0,
                timed_out: true,
                build_command: profile.command.clone(),
                working_dir: cwd.to_string_lossy().into_owned(),
                merge_sha,
            }
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            BuildResult {
                compiles: false,
                exit_code: -1,
                stdout_text: String::new(),
                stderr_text: format!("wait_timeout error: {e}"),
                duration_seconds: start.elapsed().as_secs_f64(),
                timed_out: false,
                build_command: profile.command.clone(),
                working_dir: cwd.to_string_lossy().into_owned(),
                merge_sha,
            }
        }
    }
}

// ---- Worktree helpers ----

fn have_ref(repo_path: &Path, sha: &str) -> bool {
    let out = Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap_or("."),
            "cat-file",
            "-t",
            sha,
        ])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

fn ensure_ref_available(repo_path: &Path, sha: &str) -> bool {
    if have_ref(repo_path, sha) {
        return true;
    }
    info!(sha = %&sha[..12.min(sha.len())], "ref missing locally — fetching branches");
    let _ = Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap_or("."),
            "fetch",
            "--all",
            "--quiet",
        ])
        .output();
    if have_ref(repo_path, sha) {
        return true;
    }
    info!(sha = %&sha[..12.min(sha.len())], "ref still missing — fetching PR refs");
    let _ = Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap_or("."),
            "fetch",
            "origin",
            "+refs/pull/*/head:refs/remotes/origin/pr/*",
        ])
        .output();
    have_ref(repo_path, sha)
}

/// RAII guard that unregisters a git worktree on drop — including on panic —
/// so `.git/worktrees/` doesn't accumulate stale entries when a build crashes.
struct WorktreeGuard {
    repo: PathBuf,
    path: PathBuf,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .args([
                "-C",
                self.repo.to_str().unwrap_or("."),
                "worktree",
                "remove",
                self.path.to_str().unwrap_or_default(),
                "--force",
            ])
            .output();
    }
}

/// Compile a PR in an isolated worktree. Returns `None` for infrastructure
/// errors (missing ref, worktree setup failure); otherwise a real `BuildResult`.
pub fn compile_pr_in_worktree(
    repo_path: &Path,
    merge_sha: &str,
    profile: &BuildProfileRe,
    pr_id: &str,
    max_output_chars: usize,
) -> Option<BuildResult> {
    if !ensure_ref_available(repo_path, merge_sha) {
        warn!(pr_id, sha = %&merge_sha[..12.min(merge_sha.len())],
              "ref not available after fetch — skipping");
        return None;
    }

    let prefix = format!("compile_{}_", &pr_id[..8.min(pr_id.len())]);
    let tempdir = match tempfile::Builder::new().prefix(&prefix).tempdir() {
        Ok(d) => d,
        Err(e) => {
            warn!(pr_id, error = %e, "tempdir creation failed");
            return None;
        }
    };
    let worktree_path = tempdir.path().to_path_buf();

    let add = Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap_or("."),
            "worktree",
            "add",
            worktree_path.to_str().unwrap_or_default(),
            merge_sha,
            "--detach",
        ])
        .output();
    match add {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
            warn!(pr_id, stderr = %stderr, "worktree setup failed");
            return None;
        }
        Err(e) => {
            warn!(pr_id, error = %e, "git worktree add failed");
            return None;
        }
    }

    // From here on, any panic or early return must still unregister the
    // worktree. `_guard` drops before `tempdir`, so git forgets the worktree
    // before the directory itself is removed.
    let _guard = WorktreeGuard {
        repo: repo_path.to_path_buf(),
        path: worktree_path.clone(),
    };

    let gradlew = worktree_path.join("gradlew");
    if gradlew.exists() {
        ensure_executable(&gradlew);
    }

    let result = run_build(&worktree_path, profile, max_output_chars);

    drop(_guard);
    drop(tempdir);

    Some(result)
}

// ---- Sprint-level driver ----

#[derive(Debug, Clone)]
struct PrBuildJob {
    pr_id: String,
    repo_path: PathBuf,
    repo_name: String,
    merge_sha: String,
    profile: BuildProfileRe,
    author_id: Option<String>,
    reviewer_ids: Vec<String>,
    pr_number: Option<i64>,
    sprint_id: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CompileSummary {
    pub total: usize,
    pub compiled: usize,
    pub failed: usize,
    pub skipped: usize,
}

fn build_status_label(result: &BuildResult) -> &'static str {
    if result.compiles {
        "PASS"
    } else {
        "FAIL"
    }
}

fn build_failure_reason(result: &BuildResult) -> Option<&'static str> {
    if result.compiles {
        None
    } else if result.timed_out {
        Some("timeout")
    } else {
        Some("build_error")
    }
}

fn get_merge_sha(conn: &Connection, pr_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT sha FROM pr_commits WHERE pr_id = ? ORDER BY timestamp DESC LIMIT 1",
        [pr_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

fn get_reviewer_ids(conn: &Connection, pr_id: &str) -> Vec<String> {
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT s.id
         FROM pr_reviews rv
         JOIN students s ON LOWER(s.github_login) = LOWER(rv.reviewer_login)
         WHERE rv.pr_id = ?",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([pr_id], |r| r.get::<_, String>(0)) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    rows.filter_map(Result::ok).collect()
}

/// Parallel sprint compilation with worktrees. Mirrors
/// `check_sprint_compilations_parallel` in the Python reference.
pub fn check_sprint_compilations_parallel(
    conn: &Connection,
    sprint_id: i64,
    entregues_dir: &Path,
    profiles: &[BuildProfileRe],
    max_workers: usize,
    stderr_max_chars: usize,
    skip_tested: bool,
) -> rusqlite::Result<CompileSummary> {
    let mut prs: Vec<(String, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pr.id, pr.repo_full_name
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
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

    let total = prs.len();
    let mut skipped = 0usize;
    let mut jobs: Vec<PrBuildJob> = Vec::new();

    for (pr_id, repo_full) in &prs {
        if repo_full.is_empty() {
            skipped += 1;
            continue;
        }
        let repo_name = repo_full
            .rsplit('/')
            .next()
            .unwrap_or(repo_full)
            .to_string();

        let profile = match match_profile(&repo_name, profiles) {
            Some(p) => p.clone(),
            None => {
                warn!(repo = %repo_name, "No build profile matches — skipping");
                skipped += 1;
                continue;
            }
        };

        let merge_sha = match get_merge_sha(conn, pr_id) {
            Some(s) => s,
            None => {
                warn!(pr_id = %pr_id, "No commits — skipping");
                skipped += 1;
                continue;
            }
        };

        if skip_tested {
            let existing: Option<String> = conn
                .query_row(
                    "SELECT merge_sha FROM pr_compilation WHERE pr_id = ? AND repo_name = ?",
                    params![pr_id, repo_name],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();
            if matches!(existing, Some(ref sha) if sha == &merge_sha) {
                skipped += 1;
                continue;
            }
        }

        let project_name: Option<String> = conn
            .query_row(
                "SELECT p.name FROM projects p
                 JOIN students s ON s.team_project_id = p.id
                 JOIN pull_requests pr ON pr.author_id = s.id
                 WHERE pr.id = ?",
                [pr_id],
                |r| r.get::<_, String>(0),
            )
            .ok();
        let project_name = match project_name {
            Some(n) => n,
            None => {
                skipped += 1;
                continue;
            }
        };

        let repo_path = entregues_dir.join(&project_name).join(&repo_name);
        if !repo_path.exists() {
            warn!(path = %repo_path.display(), pr_id = %pr_id, "Repo not found");
            skipped += 1;
            continue;
        }

        let (author_id, pr_number): (Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT author_id, pr_number FROM pull_requests WHERE id = ?",
                [pr_id],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?)),
            )
            .unwrap_or((None, None));
        let reviewer_ids = get_reviewer_ids(conn, pr_id);

        let resolved_sprint_id: i64 = conn
            .query_row(
                "SELECT t.sprint_id FROM tasks t
                 JOIN task_pull_requests tpr ON tpr.task_id = t.id
                 WHERE tpr.pr_id = ? AND t.type != 'USER_STORY' LIMIT 1",
                [pr_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(sprint_id);

        jobs.push(PrBuildJob {
            pr_id: pr_id.clone(),
            repo_path,
            repo_name,
            merge_sha,
            profile,
            author_id,
            reviewer_ids,
            pr_number,
            sprint_id: resolved_sprint_id,
        });
    }

    if jobs.is_empty() {
        info!(total, skipped, "Compilation testing: nothing to do");
        return Ok(CompileSummary {
            total,
            compiled: 0,
            failed: 0,
            skipped,
        });
    }

    info!(
        jobs = jobs.len(),
        max_workers, "Compiling PRs (worktree mode)"
    );

    // Run the actual builds off the SQLite connection (Connection is not Send),
    // then re-enter the main thread to write results. We use `rayon::join` on a
    // scoped pool so we can control concurrency.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_workers.max(1))
        .build()
        .expect("rayon pool");

    use rayon::prelude::*;
    let results: Vec<(PrBuildJob, Option<BuildResult>)> = pool.install(|| {
        jobs.par_iter()
            .map(|job| {
                let r = compile_pr_in_worktree(
                    &job.repo_path,
                    &job.merge_sha,
                    &job.profile,
                    &job.pr_id,
                    10_000,
                );
                (job.clone(), r)
            })
            .collect()
    });

    let mut compiled = 0usize;
    let mut failed = 0usize;
    for (job, result) in results {
        let result = match result {
            Some(r) => r,
            None => {
                skipped += 1;
                continue;
            }
        };

        let now = Utc::now().to_rfc3339();
        let reviewer_json = json!(job.reviewer_ids).to_string();
        let truncated_stderr = tail(&result.stderr_text, stderr_max_chars);
        conn.execute(
            "INSERT OR REPLACE INTO pr_compilation
             (pr_id, repo_name, sprint_id, author_id, reviewer_ids, pr_number,
              merge_sha, compiles, exit_code, stdout_text, stderr_text,
              duration_seconds, build_command, working_dir, timed_out, tested_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                job.pr_id,
                job.repo_name,
                job.sprint_id,
                job.author_id,
                reviewer_json,
                job.pr_number,
                result.merge_sha,
                result.compiles,
                result.exit_code,
                result.stdout_text,
                truncated_stderr,
                result.duration_seconds,
                result.build_command,
                result.working_dir,
                result.timed_out,
                now,
            ],
        )?;

        let status = build_status_label(&result);
        let failure_reason = build_failure_reason(&result);
        info!(
            pr_id = %job.pr_id,
            repo = %job.repo_name,
            pr_number = job.pr_number,
            status,
            failure_reason,
            secs = result.duration_seconds,
            "PR compilation result"
        );
        if result.compiles {
            compiled += 1;
        } else {
            failed += 1;
            let stderr_trimmed = result.stderr_text.trim();
            if !stderr_trimmed.is_empty() {
                error!(
                    pr_id = %job.pr_id,
                    repo = %job.repo_name,
                    stderr = %tail(stderr_trimmed, stderr_max_chars),
                    "build stderr"
                );
            }
        }
    }

    info!(total, compiled, failed, skipped, "Compilation testing done");
    Ok(CompileSummary {
        total,
        compiled,
        failed,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_trailing_bytes() {
        assert_eq!(tail("0123456789", 4), "6789");
        assert_eq!(tail("short", 100), "short");
    }

    #[test]
    fn tail_handles_multibyte_boundary() {
        let s = "αβγδε"; // each 2 bytes
        let got = tail(s, 5);
        assert!(got.is_char_boundary(0));
        // We can't guarantee exactly 5 bytes, but the result must be a valid
        // UTF-8 suffix.
        assert!(s.ends_with(&got));
    }

    #[test]
    fn profile_matching_picks_first_match() {
        let profiles = vec![
            BuildProfileRe {
                repo_pattern: Regex::new(r"^android-").unwrap(),
                command: "./gradlew assembleDebug".into(),
                timeout_seconds: 300,
                working_dir: ".".into(),
                env: HashMap::new(),
            },
            BuildProfileRe {
                repo_pattern: Regex::new(r"^spring-").unwrap(),
                command: "./gradlew bootJar".into(),
                timeout_seconds: 180,
                working_dir: ".".into(),
                env: HashMap::new(),
            },
        ];
        assert_eq!(
            match_profile("android-pds26_1a", &profiles)
                .unwrap()
                .command,
            "./gradlew assembleDebug"
        );
        assert_eq!(
            match_profile("spring-pds26_1a", &profiles).unwrap().command,
            "./gradlew bootJar"
        );
        assert!(match_profile("frontend", &profiles).is_none());
    }

    #[test]
    fn status_label_collapses_failures_to_pass_or_fail() {
        let pass = BuildResult {
            compiles: true,
            exit_code: 0,
            stdout_text: String::new(),
            stderr_text: String::new(),
            duration_seconds: 1.0,
            timed_out: false,
            build_command: String::new(),
            working_dir: String::new(),
            merge_sha: String::new(),
        };
        let timeout = BuildResult {
            compiles: false,
            exit_code: -1,
            stdout_text: String::new(),
            stderr_text: String::new(),
            duration_seconds: 1.0,
            timed_out: true,
            build_command: String::new(),
            working_dir: String::new(),
            merge_sha: String::new(),
        };

        assert_eq!(build_status_label(&pass), "PASS");
        assert_eq!(build_status_label(&timeout), "FAIL");
        assert_eq!(build_failure_reason(&pass), None);
        assert_eq!(build_failure_reason(&timeout), Some("timeout"));
    }
}
