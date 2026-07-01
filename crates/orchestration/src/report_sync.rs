use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use sprint_grader_collect::{run_collection, CollectOpts};
use sprint_grader_core::{Config, Database};
use tracing::{info, warn};

use crate::pipeline::{rerun_post_collection_for_sprint_ids, resolve_all_sprint_tuples};

#[derive(Debug, Clone, Default)]
pub struct SyncReportsOptions {
    /// ISO `YYYY-MM-DD` — the reference date. Sprints with `start_date <= today`
    /// are re-synced; the one containing today is the current sprint.
    pub today: String,
    pub project_filter: Option<Vec<String>>,
    pub push: bool,
    /// Skip the TrackDev/GitHub collection pass and the post-collect
    /// analysis rerun (survival + per-sprint block + trajectory). Use
    /// when a `run-all` (or `go`) just finished and the DB is already
    /// fresh — sync-reports then becomes "render REPORT.md and
    /// optionally push", with no network round-trips.
    pub skip_collect: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SyncReportsResult {
    pub changed_sprints: usize,
    pub generated_reports: Vec<PathBuf>,
    pub published_repos: Vec<PathBuf>,
}

pub fn sync_reports_through_sprint(
    config: &Config,
    db_path: &Path,
    entregues_dir: &Path,
    opts: &SyncReportsOptions,
) -> Result<SyncReportsResult> {
    let db = Database::open(db_path).context("opening grading DB")?;
    db.create_tables().context("schema migration")?;

    // Default path runs a fresh collection + post-collect analysis
    // rerun. `skip_collect` short-circuits both — useful right after a
    // `run-all` / `go` when the DB is already current and you just
    // want to ship the rendered REPORT.md to teams without paying the
    // GitHub round-trip again.
    if !opts.skip_collect {
        // One collection pass — the collector internally walks every sprint with
        // `start_date <= today` per project. Layer-1/2 PR skips keep it cheap.
        let collect_opts = CollectOpts {
            today: opts.today.clone(),
            project_filter: opts.project_filter.clone(),
            skip_github: false,
            skip_repos: false,
            force_pr_refresh: false,
            repos_dir: Some(entregues_dir.to_path_buf()),
            ai_attribute_name: None,
        };
        run_collection(config, &db, &collect_opts).context("collect failed")?;
    }

    let groups = resolve_all_sprint_tuples(&db, &opts.today, opts.project_filter.as_deref())?;
    if groups.is_empty() {
        return Ok(SyncReportsResult::default());
    }
    let flat_sprint_ids: Vec<i64> = groups
        .iter()
        .flat_map(|g| g.sprint_ids.iter().copied())
        .collect();
    drop(db);

    if !opts.skip_collect {
        rerun_post_collection_for_sprint_ids(
            config,
            db_path,
            entregues_dir,
            &flat_sprint_ids,
            None,
        )
        .context("post-collection rerun failed")?;
    }

    let db = Database::open(db_path).context("reopening grading DB")?;
    db.create_tables().context("schema migration")?;

    // One multi-sprint REPORT.md per project, written into that project's
    // android repo clone. Git detects changed files on publish — content-level
    // dirty-check replaces the old snapshot-before/after heuristic.
    let mut repo_reports: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut generated_reports = Vec::new();
    for g in &groups {
        let Some(repo_root) = android_repo_root(entregues_dir, &g.name) else {
            warn!(
                project = %g.name,
                "android repo clone not found; skipping report publish"
            );
            continue;
        };
        sync_repo_to_origin_main(&repo_root).with_context(|| {
            format!(
                "failed to sync {} to origin/main before writing REPORT.md",
                repo_root.display()
            )
        })?;
        let report_path = repo_root.join("REPORT.md");
        // T-SA: sync-reports publishes to team repos. Strip the
        // static-analysis section regardless of `--push` — the file we
        // write here is what students see in their cloned working tree
        // even when push is off (instructor-only by phase-1 sign-off).
        sprint_grader_report::generate_markdown_report_multi_to_path_ex(
            &db.conn,
            g.project_id,
            &g.name,
            &g.sprint_ids,
            &report_path,
            false,
        )
        .with_context(|| format!("failed to generate {}", report_path.display()))?;
        generated_reports.push(report_path.clone());
        repo_reports
            .entry(repo_root.clone())
            .or_default()
            .push(report_path);
    }
    drop(db);

    let mut published_repos = Vec::new();
    if opts.push {
        let batch = publish_all_repo_updates(&repo_reports)?;
        published_repos = batch.pushed;
    }

    Ok(SyncReportsResult {
        changed_sprints: generated_reports.len(),
        generated_reports,
        published_repos,
    })
}

#[allow(dead_code)] // retained for test coverage; callers moved to git-level dirty-check.
fn project_has_pending_compilation(db: &Database, sprint_id: i64) -> Result<bool> {
    let pending: i64 = db.conn.query_row(
        "SELECT COUNT(*)
         FROM (
             SELECT DISTINCT
                 pr.id AS pr_id,
                 CASE
                     WHEN pr.repo_full_name IS NULL OR pr.repo_full_name = '' THEN NULL
                     WHEN instr(pr.repo_full_name, '/') > 0
                         THEN substr(pr.repo_full_name, instr(pr.repo_full_name, '/') + 1)
                     ELSE pr.repo_full_name
                 END AS repo_name,
                 (
                     SELECT pc.sha
                     FROM pr_commits pc
                     WHERE pc.pr_id = pr.id
                     ORDER BY pc.timestamp DESC
                     LIMIT 1
                 ) AS merge_sha
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
         ) pending
         LEFT JOIN pr_compilation compiled
           ON compiled.pr_id = pending.pr_id
          AND compiled.repo_name = pending.repo_name
         WHERE pending.repo_name IS NOT NULL
           AND pending.merge_sha IS NOT NULL
           AND (compiled.merge_sha IS NULL OR compiled.merge_sha != pending.merge_sha)",
        [sprint_id],
        |r| r.get(0),
    )?;
    Ok(pending > 0)
}

pub fn android_repo_root(entregues_dir: &Path, project_name: &str) -> Option<PathBuf> {
    let project_dir = entregues_dir.join(project_name);
    let entries = std::fs::read_dir(&project_dir).ok()?;
    for entry in entries.flatten() {
        let file_type = entry.file_type().ok()?;
        if !file_type.is_dir() {
            continue;
        }
        let repo_name = entry.file_name().to_string_lossy().to_string();
        if is_android_repo_name(&repo_name) {
            return Some(entry.path());
        }
    }
    None
}

fn is_android_repo_name(repo_name: &str) -> bool {
    let lower = repo_name.to_lowercase();
    lower.starts_with("android") || lower.contains("-android")
}

#[derive(Debug, Clone, Default)]
pub struct PublishBatchResult {
    pub pushed: Vec<PathBuf>,
    pub skipped_unchanged: Vec<PathBuf>,
}

/// Publish tracked report paths in each android clone. Logs per-repo progress
/// and a final summary. Repos whose report files are unchanged since the last
/// commit are skipped.
pub fn publish_all_repo_updates(
    repo_updates: &BTreeMap<PathBuf, Vec<PathBuf>>,
) -> Result<PublishBatchResult> {
    let total = repo_updates.len();
    if total == 0 {
        info!("publish: no android repo clones to update");
        return Ok(PublishBatchResult::default());
    }
    info!(
        repos = total,
        "publish: checking git status (set RUST_LOG=info if you see no per-repo lines)"
    );
    let mut result = PublishBatchResult::default();
    for (idx, (repo_root, paths)) in repo_updates.iter().enumerate() {
        let name = repo_short_name(repo_root);
        info!(
            repo = %name,
            step = idx + 1,
            total,
            path = %repo_root.display(),
            "publish: checking for changes"
        );
        let has_changes = repo_has_report_changes(repo_root, paths).with_context(|| {
            format!("git status failed for {}", repo_root.display())
        })?;
        if !has_changes {
            info!(repo = %name, "publish: unchanged — skip");
            result.skipped_unchanged.push(repo_root.clone());
            continue;
        }
        let rels = relative_report_paths(repo_root, paths)?;
        info!(
            repo = %name,
            files = ?rels,
            "publish: git add / commit / fetch / rebase / push"
        );
        publish_report_updates(repo_root, paths).with_context(|| {
            format!("failed to publish report updates for {}", repo_root.display())
        })?;
        info!(repo = %name, "publish: pushed to origin/main");
        result.pushed.push(repo_root.clone());
    }
    info!(
        pushed = result.pushed.len(),
        skipped = result.skipped_unchanged.len(),
        "publish complete"
    );
    Ok(result)
}

pub fn repo_has_report_changes(repo_root: &Path, report_paths: &[PathBuf]) -> Result<bool> {
    let rels = relative_report_paths(repo_root, report_paths)?;
    if rels.is_empty() {
        return Ok(false);
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root)
        .arg("status")
        .arg("--porcelain")
        .arg("--");
    for rel in &rels {
        cmd.arg(rel);
    }
    let output = cmd.output().context("running git status")?;
    if !output.status.success() {
        bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Pre-write contract for the android-repo report sync: when REPORT.md is
/// (re)written into the working tree, HEAD must be on `main` and `main` must
/// match `origin/main` exactly. We fetch first (so origin/main reflects any
/// student pushes since the previous run), switch to main, then `reset --hard`
/// — this is the same idiom used by `repo_manager::update`. Any local
/// uncommitted edits or unpushed commits in the clone are intentionally
/// discarded; the clone is the grader's working area and REPORT.md is
/// reproducible from the DB on every run.
pub(crate) fn sync_repo_to_origin_main(repo_root: &Path) -> Result<()> {
    let name = repo_short_name(repo_root);
    info!(repo = %name, "sync: git fetch origin");
    run_git(repo_root, &["fetch", "--quiet", "origin"], true)
        .with_context(|| format!("git fetch origin failed in {}", repo_root.display()))?;
    info!(repo = %name, "sync: git switch main && reset --hard origin/main");
    run_git(repo_root, &["switch", "main"], false)
        .with_context(|| format!("git switch main failed in {}", repo_root.display()))?;
    run_git(repo_root, &["reset", "--hard", "origin/main"], false).with_context(|| {
        format!(
            "git reset --hard origin/main failed in {}",
            repo_root.display()
        )
    })?;
    Ok(())
}

pub fn publish_report_updates(repo_root: &Path, report_paths: &[PathBuf]) -> Result<()> {
    ensure_command_available(repo_root)?;
    let rels = relative_report_paths(repo_root, report_paths)?;
    if rels.is_empty() {
        return Ok(());
    }

    run_git(repo_root, &["switch", "main"], false)?;

    let mut add_args = vec!["add".to_string(), "--".to_string()];
    add_args.extend(rels.iter().cloned());
    run_git_owned(repo_root, &add_args, false)?;

    let mut commit_args = vec![
        "commit".to_string(),
        "-m".to_string(),
        "Updated reports".to_string(),
        "--".to_string(),
    ];
    commit_args.extend(rels.iter().cloned());
    run_git_owned(repo_root, &commit_args, false)?;

    // Re-fetch and rebase: students may have pushed to origin/main since
    // sync_repo_to_origin_main ran for this repo. Without this, the push
    // gets rejected as a non-fast-forward.
    run_git(repo_root, &["fetch", "--quiet", "origin"], true)?;
    if let Err(err) = run_git(repo_root, &["rebase", "origin/main"], false) {
        let _ = run_git(repo_root, &["rebase", "--abort"], false);
        return Err(err.context("git rebase origin/main failed during publish"));
    }

    run_git(repo_root, &["push", "origin", "main"], true)?;
    Ok(())
}

fn relative_report_paths(repo_root: &Path, report_paths: &[PathBuf]) -> Result<Vec<String>> {
    report_paths
        .iter()
        .map(|path| {
            path.strip_prefix(repo_root)
                .with_context(|| {
                    format!(
                        "{} is not inside repo {}",
                        path.display(),
                        repo_root.display()
                    )
                })
                .map(|rel| rel.to_string_lossy().to_string())
        })
        .collect()
}

fn ensure_command_available(cwd: &Path) -> Result<()> {
    run_git(cwd, &["--version"], false).map(|_| ())
}

fn repo_short_name(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

/// Run `git` with optional non-interactive network env (`GIT_TERMINAL_PROMPT=0`
/// so fetch/push fail fast instead of blocking on a credential prompt).
fn run_git(cwd: &Path, args: &[&str], network: bool) -> Result<String> {
    let output = git_command(cwd, args, network, &[])
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        let hint = if network {
            " (network git: ensure GitHub credentials/SSH are configured; \
             GIT_TERMINAL_PROMPT=0 prevents hanging on password prompts)"
        } else {
            ""
        };
        bail!(
            "git {} failed{}: {}",
            args.join(" "),
            hint,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_git_owned(cwd: &Path, args: &[String], network: bool) -> Result<String> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = git_command(cwd, &arg_refs, network, args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        let hint = if network {
            " (network git: ensure GitHub credentials/SSH are configured; \
             GIT_TERMINAL_PROMPT=0 prevents hanging on password prompts)"
        } else {
            ""
        };
        bail!(
            "git {} failed{}: {}",
            args.join(" "),
            hint,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_command<'a>(
    cwd: &Path,
    args: &[&str],
    network: bool,
    owned_args: &'a [String],
) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    if owned_args.is_empty() {
        cmd.args(args);
    } else {
        cmd.args(owned_args);
    }
    if network {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
    cmd
}

#[allow(dead_code)]
fn run_cmd(cwd: &Path, bin: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(bin)
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("running {bin}"))?;
    if !output.status.success() {
        bail!(
            "{} {} failed: {}",
            bin,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[allow(dead_code)]
fn run_cmd_owned(cwd: &Path, bin: &str, args: &[String]) -> Result<String> {
    let output = Command::new(bin)
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("running {bin}"))?;
    if !output.status.success() {
        bail!(
            "{} {} failed: {}",
            bin,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{is_android_repo_name, project_has_pending_compilation, publish_report_updates};
    use sprint_grader_core::Database;
    use std::path::Path;

    fn setup_db() -> Database {
        let db = Database::open(Path::new(":memory:")).expect("in-memory db");
        db.create_tables().expect("schema");
        db
    }

    #[test]
    fn android_repo_name_detection_matches_existing_convention() {
        assert!(is_android_repo_name("android-pds26_5a"));
        assert!(is_android_repo_name("team-android-client"));
        assert!(!is_android_repo_name("spring-pds26_5a"));
    }

    #[test]
    fn pending_compilation_detects_new_or_changed_pr_builds() {
        let db = setup_db();
        db.conn
            .execute("INSERT INTO projects (id, name) VALUES (1, 'pds26-1a')", [])
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO sprints (id, project_id, name) VALUES (10, 1, 'Sprint 1')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO tasks (id, task_key, name, type, sprint_id) VALUES (100, 'PDS-1', 'Task', 'TASK', 10)",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO pull_requests (id, repo_full_name) VALUES ('pr-1', 'udg-pds/android-pds26_1a')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (100, 'pr-1')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO pr_commits (pr_id, sha, timestamp) VALUES ('pr-1', 'sha-1', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

        assert!(project_has_pending_compilation(&db, 10).unwrap());

        db.conn
            .execute(
                "INSERT INTO pr_compilation (pr_id, repo_name, sprint_id, compiles, exit_code, merge_sha, tested_at)
                 VALUES ('pr-1', 'android-pds26_1a', 10, 1, 0, 'sha-1', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

        assert!(!project_has_pending_compilation(&db, 10).unwrap());

        db.conn
            .execute(
                "INSERT INTO pr_commits (pr_id, sha, timestamp) VALUES ('pr-1', 'sha-2', '2026-01-02T00:00:00Z')",
                [],
            )
            .unwrap();

        assert!(project_has_pending_compilation(&db, 10).unwrap());
    }

    #[test]
    fn publish_rebases_onto_origin_main_when_remote_diverged() {
        use std::process::Command;
        use tempfile::TempDir;

        fn run_git(cwd: &Path, args: &[&str]) {
            let status = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .status()
                .expect("git invocation");
            assert!(status.success(), "git {:?} failed in {:?}", args, cwd);
        }

        fn configure_identity(repo: &Path) {
            run_git(repo, &["config", "user.email", "grader@example.com"]);
            run_git(repo, &["config", "user.name", "Grader"]);
        }

        let tmp = TempDir::new().unwrap();
        let remote = tmp.path().join("remote.git");
        let grader = tmp.path().join("grader");
        let other = tmp.path().join("other");

        std::fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "-q", "--bare", "-b", "main"]);

        std::fs::create_dir_all(&grader).unwrap();
        run_git(&grader, &["init", "-q", "-b", "main"]);
        configure_identity(&grader);
        std::fs::write(grader.join("seed.txt"), "seed").unwrap();
        run_git(&grader, &["add", "seed.txt"]);
        run_git(&grader, &["commit", "-q", "-m", "seed"]);
        run_git(
            &grader,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        run_git(&grader, &["push", "-q", "-u", "origin", "main"]);

        run_git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        configure_identity(&other);
        std::fs::write(other.join("student.txt"), "student").unwrap();
        run_git(&other, &["add", "student.txt"]);
        run_git(&other, &["commit", "-q", "-m", "student work"]);
        run_git(&other, &["push", "-q", "origin", "main"]);

        // Grader's local main is now stale relative to origin/main.
        let report_path = grader.join("REPORT.md");
        std::fs::write(&report_path, "report content").unwrap();

        publish_report_updates(&grader, &[report_path]).expect("publish should succeed");

        let log = Command::new("git")
            .args(["log", "--oneline", "-5"])
            .current_dir(&grader)
            .output()
            .expect("git log");
        assert!(log.status.success());
        let log_text = String::from_utf8_lossy(&log.stdout);
        assert!(log_text.contains("Updated reports"), "log: {log_text}");
        assert!(log_text.contains("student work"), "log: {log_text}");
        assert!(log_text.contains("seed"), "log: {log_text}");
    }
}
