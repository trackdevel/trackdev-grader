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

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
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
    /// T-P2.4: mutation-testing command (typically `./gradlew pitest`).
    /// `None` skips mutation for matching PRs silently.
    pub mutation_command: Option<String>,
    pub mutation_timeout_seconds: u64,
    pub mutation_report_path: String,
    /// Untracked files to copy from the source repo into every fresh
    /// worktree before the build runs. See `core::config::OverlayFile`.
    pub overlay_files: Vec<OverlayFile>,
}

#[derive(Debug, Clone)]
pub struct OverlayFile {
    pub src: String,
    pub dest: String,
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
            mutation_command: p.mutation_command.clone(),
            mutation_timeout_seconds: p.mutation_timeout_seconds,
            mutation_report_path: p.mutation_report_path.clone(),
            overlay_files: p
                .overlay_files
                .iter()
                .map(|o| OverlayFile {
                    src: o.src.clone(),
                    dest: o.dest.clone(),
                })
                .collect(),
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

/// Make the spawned child the leader of a new process group, so a group
/// kill takes down gradle / pitest *and* every helper it spawns. Without
/// this, killing the `/bin/sh -c "..."` parent orphans the underlying
/// `gradle` (or `sleep`, in tests), which then keeps stdout/stderr pipe
/// writers open and stalls our drain readers.
#[cfg(unix)]
fn spawn_in_own_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn spawn_in_own_process_group(_cmd: &mut Command) {}

/// Send SIGKILL to the entire process group led by `pid`.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // Negative PID targets the process group with that PGID. We made the
    // child its own group leader, so its PID is the PGID.
    let pgid = pid as i32;
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

/// Walk `/proc` for every descendant of `root_pid` (transitively, via
/// `Status: PPid:` lines) and SIGKILL each one. Necessary because the
/// gradle daemon `setsid()`s into its own session right after fork, so
/// `kill(-pgid, SIGKILL)` does not reach it; without this, every
/// timeout would leak a 2–4 GB daemon JVM. This is best-effort: missing
/// /proc entries or read errors are silently skipped.
#[cfg(unix)]
fn kill_descendant_tree(root_pid: u32) {
    use std::collections::HashMap;
    let mut ppid_of: HashMap<i32, i32> = HashMap::new();
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return,
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        let pid: i32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let status_path = entry.path().join("status");
        let status = match std::fs::read_to_string(&status_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Some(line) = status.lines().find(|l| l.starts_with("PPid:")) {
            if let Some(ppid_str) = line.split_whitespace().nth(1) {
                if let Ok(ppid) = ppid_str.parse::<i32>() {
                    ppid_of.insert(pid, ppid);
                }
            }
        }
    }
    // BFS from root_pid through ppid edges.
    let root = root_pid as i32;
    let mut to_kill: Vec<i32> = Vec::new();
    let mut frontier: Vec<i32> = vec![root];
    while let Some(parent) = frontier.pop() {
        for (&child, &p) in &ppid_of {
            if p == parent && !to_kill.contains(&child) && child != root {
                to_kill.push(child);
                frontier.push(child);
            }
        }
    }
    // Kill leaves first (innermost descendants) so a daemon that wants
    // to reap children sees them gone before we kill it.
    for pid in to_kill.iter().rev() {
        unsafe {
            libc::kill(*pid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_descendant_tree(_root_pid: u32) {}

/// Spawn a reader thread that streams bytes from `pipe` into a tail buffer
/// of at most `max_bytes`. Returning a join handle that yields the kept
/// bytes lets the main thread drain on its schedule, while the reader
/// thread keeps the OS pipe from filling up — without this, a verbose
/// build (gradle, pitest) blocks on `write()` after ~64 KB of output and
/// we report a spurious timeout.
#[cfg(test)]
fn spawn_tail_reader<R: Read + Send + 'static>(
    pipe: R,
    max_bytes: usize,
) -> JoinHandle<Vec<u8>> {
    spawn_tail_reader_tagged::<R>(None, pipe, max_bytes)
}

fn spawn_tail_reader_tagged<R: Read + Send + 'static>(
    tag: Option<(String, &'static str)>,
    mut pipe: R,
    max_bytes: usize,
) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut tail: VecDeque<u8> = VecDeque::with_capacity(max_bytes.min(64 * 1024));
        let mut line_buf: Vec<u8> = Vec::with_capacity(512);
        let mut chunk = [0u8; 8 * 1024];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Some((ref pr_short, stream)) = tag {
                        for &b in &chunk[..n] {
                            if b == b'\n' {
                                if !line_buf.is_empty() {
                                    let line = String::from_utf8_lossy(&line_buf);
                                    let trimmed = line.trim_end();
                                    if !trimmed.is_empty() {
                                        tracing::debug!(
                                            pr_id = %pr_short,
                                            stream,
                                            "{}",
                                            trimmed
                                        );
                                    }
                                    line_buf.clear();
                                }
                            } else if line_buf.len() < 4096 {
                                line_buf.push(b);
                            }
                        }
                    }
                    if max_bytes == 0 {
                        continue;
                    }
                    if n >= max_bytes {
                        tail.clear();
                        tail.extend(&chunk[n - max_bytes..n]);
                    } else {
                        let overflow = (tail.len() + n).saturating_sub(max_bytes);
                        if overflow > 0 {
                            tail.drain(..overflow);
                        }
                        tail.extend(&chunk[..n]);
                    }
                }
                Err(_) => break,
            }
        }
        if let Some((pr_short, stream)) = tag {
            if !line_buf.is_empty() {
                let line = String::from_utf8_lossy(&line_buf);
                let trimmed = line.trim_end();
                if !trimmed.is_empty() {
                    tracing::debug!(pr_id = %pr_short, stream, "{}", trimmed);
                }
            }
        }
        Vec::from(tail)
    })
}

/// Wait for a drain reader, salvaging an empty Vec on poison/panic so
/// build bookkeeping never aborts on a reader thread issue.
fn join_reader(handle: JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
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
    run_build_tagged(repo_path, profile, max_output_chars, "")
}

/// Same as `run_build` but tags every line of streamed stdout/stderr
/// with `pr_short` at debug level, so a `RUST_LOG=sprint_grader_compile=debug`
/// run shows live gradle task progress per PR. Pass `""` to disable.
pub fn run_build_tagged(
    repo_path: &Path,
    profile: &BuildProfileRe,
    max_output_chars: usize,
    pr_short: &str,
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
        // /dev/null on stdin: the child runs in its own process group
        // (see `spawn_in_own_process_group`), so it is NOT the
        // foreground pgrp of our controlling tty. Any read it does on
        // stdin from a tty triggers SIGTTIN, which by POSIX default
        // STOPS the process. Gradle's wrapper / AGP / kotlinc each
        // probe stdin in some path and used to wedge entire builds.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Belt-and-suspenders: tell gradle to never use the rich/interactive
    // console renderer regardless of TERM/tty heuristics. Append a
    // CLI-level env var that gradle inspects.
    cmd.env("GRADLE_OPTS", {
        let prev = profile.env.get("GRADLE_OPTS").cloned().unwrap_or_default();
        if prev.is_empty() {
            "-Dorg.gradle.console=plain".to_string()
        } else {
            format!("{prev} -Dorg.gradle.console=plain")
        }
    });
    for (k, v) in &profile.env {
        if k == "GRADLE_OPTS" {
            continue; // already merged above
        }
        cmd.env(k, v);
    }
    spawn_in_own_process_group(&mut cmd);

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

    // Drain stdout/stderr concurrently into ring buffers so the child never
    // blocks on a full pipe (caused spurious timeouts on chatty gradle
    // builds) and we keep at most `max_output_chars` bytes per stream
    // resident in memory.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let tag_stdout = if pr_short.is_empty() {
        None
    } else {
        Some((pr_short.to_string(), "stdout"))
    };
    let tag_stderr = if pr_short.is_empty() {
        None
    } else {
        Some((pr_short.to_string(), "stderr"))
    };
    let stdout_handle =
        stdout_pipe.map(|p| spawn_tail_reader_tagged(tag_stdout, p, max_output_chars));
    let stderr_handle =
        stderr_pipe.map(|p| spawn_tail_reader_tagged(tag_stderr, p, max_output_chars));

    let pid = child.id();
    let timeout = Duration::from_secs(profile.timeout_seconds);
    let wait_result = child.wait_timeout(timeout);
    let (status_opt, timed_out, wait_err) = match wait_result {
        Ok(Some(status)) => (Some(status), false, None),
        Ok(None) => {
            // Reach the gradle daemon: it `setsid()`s into its own
            // session, so the process-group kill alone misses it.
            kill_descendant_tree(pid);
            kill_process_group(pid);
            let _ = child.kill();
            let _ = child.wait();
            (None, true, None)
        }
        Err(e) => {
            kill_descendant_tree(pid);
            kill_process_group(pid);
            let _ = child.kill();
            let _ = child.wait();
            (None, false, Some(e.to_string()))
        }
    };
    let duration = start.elapsed().as_secs_f64();

    let stdout_bytes = stdout_handle.map(join_reader).unwrap_or_default();
    let stderr_bytes = stderr_handle.map(join_reader).unwrap_or_default();
    let stdout_text = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let mut stderr_text = String::from_utf8_lossy(&stderr_bytes).into_owned();

    if timed_out {
        let note = format!("\n[Build timed out after {}s]", profile.timeout_seconds);
        stderr_text.push_str(&note);
    }
    if let Some(msg) = wait_err.as_ref() {
        stderr_text.push_str(&format!("\n[wait_timeout error: {msg}]"));
    }

    let (compiles, exit_code) = match status_opt {
        Some(s) => (s.success(), s.code().unwrap_or(-1)),
        None => (false, -1),
    };

    BuildResult {
        compiles,
        exit_code,
        stdout_text: tail(&stdout_text, max_output_chars),
        stderr_text: tail(&stderr_text, max_output_chars),
        duration_seconds: (duration * 100.0).round() / 100.0,
        timed_out,
        build_command: profile.command.clone(),
        working_dir: cwd.to_string_lossy().into_owned(),
        merge_sha,
    }
}

/// Result of a Pitest run scoped to a single PR's worktree.
#[derive(Debug, Clone, Default)]
pub struct MutationResult {
    pub mutants_total: u64,
    pub mutants_killed: u64,
    pub mutation_score: Option<f64>,
    pub duration_seconds: f64,
    pub timed_out: bool,
}

/// Run the profile's `mutation_command` inside `repo_path /
/// working_dir`, then read & parse the Pitest XML at
/// `repo_path / working_dir / mutation_report_path`. Skips silently
/// (returns `None`) when the profile has no `mutation_command`.
///
/// Pitest's own runner is parallel at the JVM level; we don't add a
/// second layer. The hard process-kill timeout uses
/// `mutation_timeout_seconds` (default 600s, an order of magnitude
/// larger than `timeout_seconds`).
pub fn run_mutation(repo_path: &Path, profile: &BuildProfileRe) -> Option<MutationResult> {
    run_mutation_tagged(repo_path, profile, "")
}

pub fn run_mutation_tagged(
    repo_path: &Path,
    profile: &BuildProfileRe,
    pr_short: &str,
) -> Option<MutationResult> {
    let cmd_str = profile.mutation_command.as_ref()?;
    let cwd = repo_path.join(&profile.working_dir);

    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(cmd_str)
        .current_dir(&cwd)
        .stdin(Stdio::null()) // see SIGTTIN comment in run_build
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.env("GRADLE_OPTS", {
        let prev = profile.env.get("GRADLE_OPTS").cloned().unwrap_or_default();
        if prev.is_empty() {
            "-Dorg.gradle.console=plain".to_string()
        } else {
            format!("{prev} -Dorg.gradle.console=plain")
        }
    });
    for (k, v) in &profile.env {
        if k == "GRADLE_OPTS" {
            continue;
        }
        cmd.env(k, v);
    }
    spawn_in_own_process_group(&mut cmd);

    let start = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to spawn mutation command");
            return Some(MutationResult {
                duration_seconds: 0.0,
                ..MutationResult::default()
            });
        }
    };

    // Pitest is even more verbose than gradle assemble; drain into bounded
    // ring buffers (output is discarded, but draining is what stops the
    // child from blocking on a full pipe).
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    const MUTATION_TAIL_BYTES: usize = 4 * 1024;
    let tag_stdout = if pr_short.is_empty() {
        None
    } else {
        Some((pr_short.to_string(), "mutation:stdout"))
    };
    let tag_stderr = if pr_short.is_empty() {
        None
    } else {
        Some((pr_short.to_string(), "mutation:stderr"))
    };
    let stdout_handle =
        stdout_pipe.map(|p| spawn_tail_reader_tagged(tag_stdout, p, MUTATION_TAIL_BYTES));
    let stderr_handle =
        stderr_pipe.map(|p| spawn_tail_reader_tagged(tag_stderr, p, MUTATION_TAIL_BYTES));

    let pid = child.id();
    let timeout = Duration::from_secs(profile.mutation_timeout_seconds);
    let timed_out = match child.wait_timeout(timeout) {
        Ok(Some(_status)) => false,
        Ok(None) => {
            kill_descendant_tree(pid);
            kill_process_group(pid);
            let _ = child.kill();
            let _ = child.wait();
            true
        }
        Err(e) => {
            warn!(error = %e, "wait_timeout error during mutation run");
            kill_descendant_tree(pid);
            kill_process_group(pid);
            let _ = child.kill();
            let _ = child.wait();
            if let Some(h) = stdout_handle {
                let _ = join_reader(h);
            }
            if let Some(h) = stderr_handle {
                let _ = join_reader(h);
            }
            return None;
        }
    };
    if let Some(h) = stdout_handle {
        let _ = join_reader(h);
    }
    if let Some(h) = stderr_handle {
        let _ = join_reader(h);
    }
    let duration = (start.elapsed().as_secs_f64() * 100.0).round() / 100.0;

    if timed_out {
        info!(secs = duration, "mutation run timed out");
        return Some(MutationResult {
            duration_seconds: duration,
            timed_out: true,
            ..MutationResult::default()
        });
    }

    let report = cwd.join(&profile.mutation_report_path);
    if !report.exists() {
        warn!(path = %report.display(), "Pitest report not found after run");
        return Some(MutationResult {
            duration_seconds: duration,
            ..MutationResult::default()
        });
    }
    match crate::pitest::parse_pitest_xml(&report) {
        Ok(s) => Some(MutationResult {
            mutants_total: s.mutants_total,
            mutants_killed: s.mutants_killed,
            mutation_score: s.score(),
            duration_seconds: duration,
            timed_out: false,
        }),
        Err(e) => {
            warn!(path = %report.display(), error = %e, "Pitest XML read failed");
            Some(MutationResult {
                duration_seconds: duration,
                ..MutationResult::default()
            })
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

/// Copy each `overlay_files[i].src` from the source repo's working
/// tree into the freshly-created worktree at `overlay_files[i].dest`.
/// This is how we get per-team build secrets (e.g.
/// `app/google-services.json`) — which are NOT committed to git, so a
/// `git worktree add <sha>` checkout doesn't carry them — into the
/// worktree before `./gradlew` runs.
///
/// Missing source → `warn!` and continue. Don't fail the build before
/// gradle has had a chance to either succeed without it or report a
/// clearer error. Path safety has already been enforced at config-load
/// time (`is_unsafe_overlay_path`), so we trust the components here.
fn apply_overlay_files(
    repo_path: &Path,
    worktree_path: &Path,
    overlays: &[OverlayFile],
    pr_id: &str,
) {
    if overlays.is_empty() {
        return;
    }
    for o in overlays {
        let src = repo_path.join(&o.src);
        let dest = worktree_path.join(&o.dest);
        if !src.exists() {
            warn!(
                pr_id,
                src = %src.display(),
                dest = %o.dest,
                "overlay source missing — build may fail unless the file is optional"
            );
            continue;
        }
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(
                    pr_id,
                    parent = %parent.display(),
                    error = %e,
                    "could not create parent dir for overlay dest — skipping"
                );
                continue;
            }
        }
        match std::fs::copy(&src, &dest) {
            Ok(_) => info!(
                pr_id,
                src = %o.src,
                dest = %o.dest,
                "overlay file copied into worktree"
            ),
            Err(e) => warn!(
                pr_id,
                src = %src.display(),
                dest = %dest.display(),
                error = %e,
                "overlay copy failed — continuing"
            ),
        }
    }
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

/// Combined per-PR worktree result: the primary build plus, when the
/// build passed and the profile has a `mutation_command`, the Pitest
/// summary. `mutation` is `None` when mutation testing was skipped
/// (build failed, or `mutation_command` is unset, or
/// `mutation_enabled` was false at the call site).
#[derive(Debug, Clone)]
pub struct PrBuildOutput {
    pub build: BuildResult,
    pub mutation: Option<MutationResult>,
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

    apply_overlay_files(repo_path, &worktree_path, &profile.overlay_files, pr_id);

    let pr_short = &pr_id[..8.min(pr_id.len())];
    let result = run_build_tagged(&worktree_path, profile, max_output_chars, pr_short);

    drop(_guard);
    drop(tempdir);

    Some(result)
}

/// Same as [`compile_pr_in_worktree`] but, on a successful primary
/// build, also runs the profile's `mutation_command` inside the same
/// worktree. Mutation is skipped when:
/// * the global `mutation_enabled` flag is false, or
/// * the profile has no `mutation_command`, or
/// * the primary build failed.
///
/// Sharing the worktree means the mutation tool sees the same compiled
/// output the primary build produced (Pitest reads `build/classes`),
/// avoiding an expensive recompile.
pub fn compile_and_mutate_pr_in_worktree(
    repo_path: &Path,
    merge_sha: &str,
    profile: &BuildProfileRe,
    pr_id: &str,
    max_output_chars: usize,
    mutation_enabled: bool,
) -> Option<PrBuildOutput> {
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

    let _guard = WorktreeGuard {
        repo: repo_path.to_path_buf(),
        path: worktree_path.clone(),
    };

    let gradlew = worktree_path.join("gradlew");
    if gradlew.exists() {
        ensure_executable(&gradlew);
    }

    apply_overlay_files(repo_path, &worktree_path, &profile.overlay_files, pr_id);

    let pr_short = &pr_id[..8.min(pr_id.len())];
    let build = run_build_tagged(&worktree_path, profile, max_output_chars, pr_short);
    let mutation = if mutation_enabled && build.compiles && profile.mutation_command.is_some() {
        run_mutation_tagged(&worktree_path, profile, pr_short)
    } else {
        None
    };

    drop(_guard);
    drop(tempdir);

    Some(PrBuildOutput { build, mutation })
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

/// Single-sprint convenience wrapper. Most callers should prefer
/// [`check_compilations_parallel`] which takes `&[sprint_id]` and
/// batches every PR across every sprint into one rayon pool.
#[allow(clippy::too_many_arguments)]
pub fn check_sprint_compilations_parallel(
    conn: &Connection,
    sprint_id: i64,
    entregues_dir: &Path,
    profiles: &[BuildProfileRe],
    max_workers: usize,
    stderr_max_chars: usize,
    skip_tested: bool,
    mutation_enabled: bool,
) -> rusqlite::Result<CompileSummary> {
    check_compilations_parallel(
        conn,
        &[sprint_id],
        entregues_dir,
        profiles,
        max_workers,
        stderr_max_chars,
        skip_tested,
        mutation_enabled,
    )
}

/// Parallel multi-sprint PR compilation with worktrees. Collects every
/// PR across every sprint in `sprint_ids` into one job list, then runs
/// them through a single rayon pool of `max_workers` workers — so the
/// per-worker GRADLE_USER_HOME warm-up cost is paid once for the whole
/// project, not once per sprint.
#[allow(clippy::too_many_arguments)]
pub fn check_compilations_parallel(
    conn: &Connection,
    sprint_ids: &[i64],
    entregues_dir: &Path,
    profiles: &[BuildProfileRe],
    max_workers: usize,
    stderr_max_chars: usize,
    skip_tested: bool,
    mutation_enabled: bool,
) -> rusqlite::Result<CompileSummary> {
    if sprint_ids.is_empty() {
        return Ok(CompileSummary::default());
    }
    let mut prs: Vec<(String, String)> = Vec::new();
    {
        let placeholders = std::iter::repeat("?")
            .take(sprint_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT DISTINCT pr.id, pr.repo_full_name
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id IN ({placeholders}) AND t.type != 'USER_STORY'",
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(sprint_ids.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?;
        for r in rows {
            prs.push(r?);
        }
    }
    // Fallback sprint id used when a PR has no task→sprint join (rare).
    // Pick the first sprint deterministically.
    let fallback_sprint_id = sprint_ids[0];

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
            .unwrap_or(fallback_sprint_id);

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
        max_workers,
        "Compiling PRs (worktree mode, per-worker GRADLE_USER_HOME so daemons don't share registry)"
    );

    // Per-worker isolated GRADLE_USER_HOME. With a shared ~/.gradle,
    // every concurrent ./gradlew invocation contends on a single
    // ~/.gradle/daemon/<ver>/registry.bin file lock; with 5 in flight
    // and a few mid-run daemon SIGKILLs leaving stale entries, the
    // remaining CLIs deadlock-queue forever (observed: first 1-5 PRs
    // pass, all subsequent stall to 300s timeout). Giving each rayon
    // worker its own home → its own daemon, its own registry, zero
    // cross-worker contention. Workers reuse their home across PRs so
    // dep cache + warm daemons stay in scope.
    let gradle_homes_root: PathBuf = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".gradle-grader-workers");
    if let Err(e) = std::fs::create_dir_all(&gradle_homes_root) {
        warn!(path = %gradle_homes_root.display(), error = %e,
              "could not create per-worker gradle home root; builds will share ~/.gradle");
    }

    // Run the actual builds off the SQLite connection (Connection is not Send),
    // then re-enter the main thread to write results. We use `rayon::join` on a
    // scoped pool so we can control concurrency.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_workers.max(1))
        .build()
        .expect("rayon pool");

    // Live progress: each worker reports start/finish, and a watchdog
    // thread emits a snapshot every 30s of which PRs are still in flight
    // and how long they've been running. The watchdog stops when
    // `done.load() == jobs.len()`.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let total_jobs = jobs.len();
    type InFlightMap = HashMap<String, (String, i64, Instant)>;
    let in_flight: Arc<Mutex<InFlightMap>> = Arc::new(Mutex::new(HashMap::new()));
    let started_count = Arc::new(AtomicUsize::new(0));
    let finished_count = Arc::new(AtomicUsize::new(0));
    let watchdog_stop = Arc::new(AtomicUsize::new(0));

    let watchdog = {
        let in_flight = Arc::clone(&in_flight);
        let started_count = Arc::clone(&started_count);
        let finished_count = Arc::clone(&finished_count);
        let watchdog_stop = Arc::clone(&watchdog_stop);
        thread::spawn(move || {
            let tick = Duration::from_secs(30);
            let started_at = Instant::now();
            loop {
                thread::sleep(tick);
                if watchdog_stop.load(Ordering::SeqCst) == 1 {
                    return;
                }
                let started = started_count.load(Ordering::SeqCst);
                let finished = finished_count.load(Ordering::SeqCst);
                let snapshot: Vec<(String, String, i64, u64)> = {
                    let guard = in_flight.lock().unwrap();
                    guard
                        .iter()
                        .map(|(pr_id, (repo, num, t))| {
                            (
                                pr_id.clone(),
                                repo.clone(),
                                *num,
                                t.elapsed().as_secs(),
                            )
                        })
                        .collect()
                };
                let mut sorted = snapshot;
                sorted.sort_by_key(|(_, _, _, secs)| std::cmp::Reverse(*secs));
                let preview: Vec<String> = sorted
                    .iter()
                    .take(8)
                    .map(|(pr, repo, num, secs)| {
                        format!("{repo}#{num}({}…) {secs}s", &pr[..8.min(pr.len())])
                    })
                    .collect();
                info!(
                    elapsed_s = started_at.elapsed().as_secs(),
                    started,
                    finished,
                    in_flight = sorted.len(),
                    pending = total_jobs.saturating_sub(started),
                    longest = ?preview,
                    "compile progress"
                );
            }
        })
    };

    use rayon::prelude::*;
    let results: Vec<(PrBuildJob, Option<PrBuildOutput>)> = pool.install(|| {
        jobs.par_iter()
            .map(|job| {
                let pr_short = &job.pr_id[..8.min(job.pr_id.len())];
                let started = Instant::now();
                {
                    let mut guard = in_flight.lock().unwrap();
                    guard.insert(
                        job.pr_id.clone(),
                        (job.repo_name.clone(), job.pr_number.unwrap_or(0), started),
                    );
                }
                let n_started = started_count.fetch_add(1, Ordering::SeqCst) + 1;
                info!(
                    pr_id = pr_short,
                    repo = %job.repo_name,
                    pr_number = job.pr_number,
                    started = n_started,
                    finished = finished_count.load(Ordering::SeqCst),
                    pending = total_jobs.saturating_sub(n_started),
                    "compile start"
                );

                // Stamp this worker's GRADLE_USER_HOME into the
                // per-job profile env. rayon::current_thread_index is
                // None when a closure runs outside any pool (impossible
                // here, but fall back to 0 defensively).
                let worker_idx = rayon::current_thread_index().unwrap_or(0);
                let mut profile_for_job = job.profile.clone();
                let worker_home = gradle_homes_root.join(format!("w{worker_idx}"));
                if let Err(e) = std::fs::create_dir_all(&worker_home) {
                    warn!(
                        path = %worker_home.display(),
                        worker = worker_idx,
                        error = %e,
                        "could not create per-worker gradle home; falling back to default"
                    );
                } else {
                    profile_for_job
                        .env
                        .insert("GRADLE_USER_HOME".to_string(), worker_home.to_string_lossy().into_owned());
                }

                let r = compile_and_mutate_pr_in_worktree(
                    &job.repo_path,
                    &job.merge_sha,
                    &profile_for_job,
                    &job.pr_id,
                    10_000,
                    mutation_enabled,
                );

                let dur = started.elapsed().as_secs_f64();
                let outcome = match r.as_ref() {
                    None => "skipped",
                    Some(o) if o.build.timed_out => "TIMEOUT",
                    Some(o) if o.build.compiles => "PASS",
                    Some(_) => "FAIL",
                };
                {
                    let mut guard = in_flight.lock().unwrap();
                    guard.remove(&job.pr_id);
                }
                let n_finished = finished_count.fetch_add(1, Ordering::SeqCst) + 1;
                info!(
                    pr_id = pr_short,
                    repo = %job.repo_name,
                    pr_number = job.pr_number,
                    secs = format!("{:.1}", dur),
                    outcome,
                    finished = n_finished,
                    pending = total_jobs.saturating_sub(n_finished),
                    "compile end"
                );
                (job.clone(), r)
            })
            .collect()
    });

    watchdog_stop.store(1, Ordering::SeqCst);
    let _ = watchdog.join();

    let mut compiled = 0usize;
    let mut failed = 0usize;
    for (job, output) in results {
        let output = match output {
            Some(r) => r,
            None => {
                skipped += 1;
                continue;
            }
        };
        let result = output.build;

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

        if let Some(m) = output.mutation.as_ref() {
            // Persist mutation summary alongside the compilation row.
            // INSERT OR REPLACE keys on (pr_id, repo_name) so re-runs
            // overwrite. NULL `mutation_score` is meaningful: report
            // existed but every mutant was non-viable, or the run
            // timed out / failed mid-way.
            conn.execute(
                "INSERT OR REPLACE INTO pr_mutation
                 (pr_id, repo_name, sprint_id, mutants_total, mutants_killed,
                  mutation_score, duration_seconds)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    job.pr_id,
                    job.repo_name,
                    job.sprint_id,
                    m.mutants_total as i64,
                    m.mutants_killed as i64,
                    m.mutation_score,
                    m.duration_seconds,
                ],
            )?;
            info!(
                pr_id = %job.pr_id,
                repo = %job.repo_name,
                mutants = m.mutants_total,
                killed = m.mutants_killed,
                score = m.mutation_score,
                secs = m.duration_seconds,
                timed_out = m.timed_out,
                "PR mutation result"
            );
        }

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
    fn tail_reader_caps_output_to_last_max_bytes() {
        use std::io::Cursor;
        let payload = (0u8..200).cycle().take(64 * 1024).collect::<Vec<_>>();
        let cursor = Cursor::new(payload.clone());
        let handle = spawn_tail_reader(cursor, 4096);
        let got = handle.join().unwrap();
        assert_eq!(got.len(), 4096);
        assert_eq!(got.as_slice(), &payload[payload.len() - 4096..]);
    }

    #[test]
    fn tail_reader_returns_full_output_when_under_cap() {
        use std::io::Cursor;
        let payload = b"short build output".to_vec();
        let cursor = Cursor::new(payload.clone());
        let handle = spawn_tail_reader(cursor, 4096);
        assert_eq!(handle.join().unwrap(), payload);
    }

    #[test]
    fn tail_keeps_trailing_bytes() {
        assert_eq!(tail("0123456789", 4), "6789");
        assert_eq!(tail("short", 100), "short");
    }

    #[test]
    fn apply_overlay_files_missing_source_only_warns_and_does_not_panic() {
        // Single overlay entry whose source does not exist. Compile must
        // proceed unchanged: no panic, no error returned (function is
        // infallible), no dest file created.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        let overlays = vec![OverlayFile {
            src: "app/google-services.json".into(),
            dest: "app/google-services.json".into(),
        }];
        // `apply_overlay_files` returns (); calling it is the assertion
        // that it does not panic on a missing source.
        apply_overlay_files(&repo, &worktree, &overlays, "ff8081...");

        // No partial state was written.
        assert!(!worktree.join("app/google-services.json").exists());
        assert!(
            !worktree.join("app").exists(),
            "missing source must not auto-create the dest parent dir either"
        );
    }

    #[test]
    fn apply_overlay_files_copies_present_sources_and_creates_dest_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(repo.join("app")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(repo.join("app/google-services.json"), b"{ \"team\": 4 }").unwrap();
        std::fs::write(repo.join("local.properties"), b"sdk.dir=/opt/android").unwrap();

        let overlays = vec![
            OverlayFile {
                src: "app/google-services.json".into(),
                dest: "app/google-services.json".into(),
            },
            OverlayFile {
                src: "local.properties".into(),
                dest: "local.properties".into(),
            },
            OverlayFile {
                src: "missing.json".into(),
                dest: "app/missing.json".into(),
            },
        ];
        apply_overlay_files(&repo, &worktree, &overlays, "ff8081...");

        let copied = std::fs::read(worktree.join("app/google-services.json")).unwrap();
        assert_eq!(copied, b"{ \"team\": 4 }");
        assert!(worktree.join("local.properties").exists());
        // Missing source skipped without panicking, and we did NOT create
        // a dest file from thin air.
        assert!(!worktree.join("app/missing.json").exists());
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
                mutation_command: None,
                mutation_timeout_seconds: 600,
                mutation_report_path: "build/reports/pitest/mutations.xml".into(),
                overlay_files: vec![],
            },
            BuildProfileRe {
                repo_pattern: Regex::new(r"^spring-").unwrap(),
                command: "./gradlew bootJar".into(),
                timeout_seconds: 180,
                working_dir: ".".into(),
                env: HashMap::new(),
                mutation_command: None,
                mutation_timeout_seconds: 600,
                mutation_report_path: "build/reports/pitest/mutations.xml".into(),
                overlay_files: vec![],
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
