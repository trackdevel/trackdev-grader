//! Full-pipeline variants: `run-all`, `go`, `go-quick`.
//!
//! Each variant:
//!   1. Purges existing DB state (go / go-quick only, when `--projects` given).
//!   2. Collects from TrackDev + GitHub, clones repos.
//!   3. Runs survival analysis (tolerant in go/go-quick; fatal in run-all).
//!   4. Runs the per-project analysis block in parallel with rayon — each
//!      worker opens its own `rusqlite::Connection` (SQLite WAL mode allows
//!      concurrent readers / serialized writers).
//!   5. Optional AI-detection block (go / go-quick).
//!   6. Cross-project trajectory aggregation.
//!   7. Optional reports (Excel + Markdown).

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::Connection;
use sprint_grader_core::{db::configure_pragmas, Config, Database};
use tracing::{info, warn};

use crate::android_repo_root;

/// Describes one of the three full-pipeline variants. Frozen so the CLI can
/// pattern-match by equality and callers can't silently tweak a single field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineVariant {
    RunAll,
    Go,
    GoQuick,
}

impl PipelineVariant {
    pub fn name(&self) -> &'static str {
        match self {
            PipelineVariant::RunAll => "run-all",
            PipelineVariant::Go => "go",
            PipelineVariant::GoQuick => "go-quick",
        }
    }

    fn ai_detection(&self) -> bool {
        matches!(self, PipelineVariant::Go | PipelineVariant::GoQuick)
    }

    fn purge_existing(&self) -> bool {
        matches!(self, PipelineVariant::Go | PipelineVariant::GoQuick)
    }
}

/// Configure a single `run_pipeline` call. Keyword-style so the CLI wrapper
/// stays readable and fields can be added without breaking call-sites.
#[derive(Debug, Clone)]
pub struct PipelineOptions {
    /// ISO `YYYY-MM-DD` — the reference date. Sprints with `start_date <= today`
    /// are processed; the one containing today is the current sprint.
    pub today: String,
    pub project_filter: Option<Vec<String>>,
    pub entregues_dir: PathBuf,
    pub config_dir: PathBuf,
    pub skip_github: bool,
    pub skip_repos: bool,
    pub skip_reports: bool,
    pub force_pr_refresh: bool,
    pub max_workers: Option<usize>,
}

impl PipelineOptions {
    pub fn minimal(today: String, entregues_dir: PathBuf, config_dir: PathBuf) -> Self {
        Self {
            today,
            project_filter: None,
            entregues_dir,
            config_dir,
            skip_github: false,
            skip_repos: false,
            skip_reports: false,
            force_pr_refresh: false,
            max_workers: None,
        }
    }
}

/// One project's view of "all sprints up to current" — `sprint_ids` is the
/// full list ordered `start_date ASC`, with the current sprint as the last
/// element. Empty sprint_ids means the project has no sprints that have
/// started yet for the given `today`.
#[derive(Debug, Clone)]
pub struct ProjectSprints {
    pub project_id: i64,
    pub name: String,
    pub sprint_ids: Vec<i64>,
}

/// Resolve, for every project (optionally filtered), the chronological list
/// of sprint ids whose `start_date <= today`. Projects with empty lists are
/// skipped in the output.
pub fn resolve_all_sprint_tuples(
    db: &Database,
    today: &str,
    projects: Option<&[String]>,
) -> Result<Vec<ProjectSprints>> {
    let project_rows: Vec<(i64, String)> = if let Some(names) = projects {
        let mut rows = Vec::new();
        for name in names {
            let pid: Option<i64> = db
                .conn
                .query_row("SELECT id FROM projects WHERE name = ?", [name], |r| {
                    r.get::<_, i64>(0)
                })
                .ok();
            match pid {
                Some(id) => rows.push((id, name.clone())),
                None => warn!(project = %name, "project not found — skipping"),
            }
        }
        rows
    } else {
        let mut stmt = db
            .conn
            .prepare("SELECT id, COALESCE(name, '') FROM projects ORDER BY id")?;
        let out: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        out
    };

    let mut groups = Vec::new();
    for (pid, name) in project_rows {
        let sprint_ids = db.sprint_ids_up_to_current(pid, today)?;
        if sprint_ids.is_empty() {
            warn!(project = %name, today, "no sprints with start_date <= today — skipping");
            continue;
        }
        groups.push(ProjectSprints {
            project_id: pid,
            name,
            sprint_ids,
        });
    }
    Ok(groups)
}

fn open_worker_conn(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    configure_pragmas(&conn)?;
    Ok(conn)
}

/// Run the per-project parallel analysis block — metrics, flags, inequality,
/// contribution, LLM eval (heuristic fallback), quality, process, task
/// similarity, temporal analysis. Each stage writes to rows keyed by its own
/// sprint_id so write contention stays minimal under WAL.
fn run_parallel_project_block(
    db_path: &Path,
    config: &Config,
    sprint_ids: &[i64],
    max_workers: usize,
    use_llm_pr_docs: bool,
) -> Result<Vec<ProjectResult>> {
    let workers = max_workers.max(1).min(sprint_ids.len().max(1));
    info!(
        workers,
        projects = sprint_ids.len(),
        "running parallel project stage"
    );

    // rayon's thread pool is used through par_iter; each iteration runs on a
    // pool thread and opens its own Connection. Connection is !Send, but
    // the closure body creates it after the split, so it never crosses
    // threads.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .context("building rayon thread pool")?;

    let results: Vec<ProjectResult> = pool.install(|| {
        sprint_ids
            .par_iter()
            .copied()
            .map(|sid| run_project_stage_block(db_path, config, sid, use_llm_pr_docs))
            .collect()
    });

    Ok(results)
}

#[derive(Debug, Clone)]
pub struct ProjectResult {
    pub sprint_id: i64,
    pub stage_errors: Vec<(String, String)>,
    pub elapsed_seconds: f64,
}

fn run_project_stage_block(
    db_path: &Path,
    config: &Config,
    sprint_id: i64,
    use_llm_pr_docs: bool,
) -> ProjectResult {
    let start = Instant::now();
    let mut errors: Vec<(String, String)> = Vec::new();

    let conn = match open_worker_conn(db_path) {
        Ok(c) => c,
        Err(e) => {
            errors.push(("open_db".into(), e.to_string()));
            return ProjectResult {
                sprint_id,
                stage_errors: errors,
                elapsed_seconds: start.elapsed().as_secs_f64(),
            };
        }
    };

    let mut stage = |name: &str, f: &mut dyn FnMut() -> rusqlite::Result<()>| {
        if let Err(e) = f() {
            warn!(sprint_id, stage = name, error = %e, "stage failed");
            errors.push((name.into(), e.to_string()));
        }
    };

    // PR compilation has been hoisted out of the per-sprint parallel block
    // (see `run_pipeline`): compiling per-sprint here meant the outer rayon
    // pool of N sprints × the inner pool of `max_parallel_builds` produced
    // N×M concurrent gradle processes (e.g. 4×5 = 20 with the default 5).
    // The hoisted call uses a single rayon pool sized exactly to
    // `config.build.max_parallel_builds`. We still run the cheap per-sprint
    // `summarize_compilation` here so flag detection sees fresh aggregates.
    stage("summarize_compilation", &mut || {
        sprint_grader_compile::summarize_compilation(&conn, sprint_id)
    });

    // Stage order matters: several flag detectors read derived tables.
    //   team_inequality      reads team_sprint_inequality      (← inequality)
    //   low_composite_score  reads student_sprint_contribution (← contribution)
    //   ghost_contributor    reads student_sprint_contribution
    //   hidden_contributor   reads student_sprint_contribution
    //   cramming             reads student_sprint_temporal     (← temporal)
    // On a fresh DB, running `flags` before its writers silently emits zero
    // flags. Keep inequality + contribution + temporal before flags.
    let cramming_hours = config.thresholds.cramming_hours;
    stage("metrics", &mut || {
        sprint_grader_analyze::metrics::compute_metrics_for_sprint_id(
            &conn,
            sprint_id,
            cramming_hours,
        )
    });
    stage("heuristics", &mut || {
        sprint_grader_evaluate::run_heuristics_for_sprint_id(&conn, sprint_id).map(|_| ())
    });
    stage("llm_eval_pr_docs", &mut || {
        sprint_grader_evaluate::run_pr_doc_evaluation_for_sprint_id(
            &conn,
            sprint_id,
            config,
            use_llm_pr_docs,
        )
        .map(|_| ())
    });
    stage("llm_eval_task_descriptions", &mut || {
        sprint_grader_evaluate::score_task_descriptions_for_sprint_id(&conn, sprint_id, config)
            .map(|_| ())
    });
    stage("inequality", &mut || {
        sprint_grader_analyze::compute_all_inequality(&conn, sprint_id)
    });
    stage("contribution", &mut || {
        sprint_grader_analyze::compute_all_contributions(&conn, sprint_id, None)
    });
    // temporal must run before `flags`: the cramming detector reads
    // student_sprint_temporal (per-author timing), populated here.
    stage("temporal", &mut || {
        sprint_grader_process::compute_all_temporal(&conn, sprint_id)
    });
    stage("flags", &mut || {
        sprint_grader_analyze::flags::detect_flags_for_sprint_id(&conn, sprint_id, config)
            .map(|_| ())
    });

    // behavioral + ai_probability (PR-level)
    stage("behavioral", &mut || {
        sprint_grader_ai_detect::compute_all_behavioral(&conn, sprint_id)
    });
    stage("ai_probability", &mut || {
        sprint_grader_ai_detect::compute_all_ai_probability(&conn, sprint_id, None)
    });

    // process block
    stage("planning", &mut || {
        sprint_grader_process::compute_all_planning(&conn, sprint_id)
    });
    stage("regularity", &mut || {
        sprint_grader_process::compute_all_regularity(&conn, sprint_id, &config.regularity)
    });
    stage("collaboration", &mut || {
        sprint_grader_process::compute_all_collaboration(&conn, sprint_id)
    });

    // repo_analysis block
    stage("task_similarity", &mut || {
        sprint_grader_repo_analysis::compute_task_similarity(
            &conn,
            sprint_id,
            &config.repo_analysis,
        )
        .map(|_| ())
    });
    stage("temporal_analysis", &mut || {
        sprint_grader_repo_analysis::compute_temporal_analysis(
            &conn,
            sprint_id,
            &config.repo_analysis,
        )
        .map(|_| ())
    });
    // Ownership snapshot (T-P2.3): truck factor + per-file dominant author.
    // Reads `fingerprints`, so depends on survival having populated them.
    stage("ownership", &mut || {
        sprint_grader_repo_analysis::compute_team_ownership(&conn, sprint_id).map(|_| ())
    });

    ProjectResult {
        sprint_id,
        stage_errors: errors,
        elapsed_seconds: start.elapsed().as_secs_f64(),
    }
}

fn run_ai_detection_block(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    project_name: &str,
    entregues_dir: &Path,
    sprint_ordinal: u32,
) {
    let proj_dir = entregues_dir.join(project_name);
    if !proj_dir.is_dir() {
        return;
    }
    let repo_dirs = match std::fs::read_dir(&proj_dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    let fusion_cfg = sprint_grader_ai_detect::fusion::FusionConfig::default();
    for entry in repo_dirs.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let repo_path = entry.path();
        let repo_name = entry.file_name().to_string_lossy().into_owned();
        if let Err(e) = sprint_grader_ai_detect::scan_repo_curriculum(
            conn,
            &repo_path,
            &repo_name,
            project_id,
            sprint_id,
            sprint_ordinal as i64,
        ) {
            warn!(repo_name, error = %e, "curriculum scan failed");
        }
        if let Err(e) = sprint_grader_ai_detect::analyze_repo_stylometry(
            conn, &repo_path, &repo_name, sprint_id,
        ) {
            warn!(repo_name, error = %e, "stylometry failed");
        }
        if let Err(e) = sprint_grader_ai_detect::fusion::run_full_fusion(
            conn,
            &repo_name,
            project_id,
            sprint_id,
            &fusion_cfg,
        ) {
            warn!(repo_name, error = %e, "fusion failed");
        }
    }
    if let Err(e) =
        sprint_grader_ai_detect::compute_all_text_consistency(conn, project_id, sprint_id)
    {
        warn!(project_id, error = %e, "text consistency failed");
    }
}

/// Clone or update every repo referenced by pull_requests in the DB. Mirrors
/// Python's `orchestration.clone_repos`. When `project_ids` is `Some`, scope
/// the clone set to repos whose author belongs to one of those projects so
/// `--projects` doesn't trigger 30+ unrelated `git fetch`es.
fn clone_repos_from_db(
    db: &Database,
    entregues_dir: &Path,
    project_ids: Option<&[i64]>,
) -> Result<()> {
    let rows: Vec<(String, Option<String>)> = if let Some(ids) = project_ids {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = (0..ids.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT DISTINCT pr.repo_full_name, p.name as project_name
             FROM pull_requests pr
             JOIN students s ON s.id = pr.author_id
             JOIN projects p ON p.id = s.team_project_id
             WHERE pr.repo_full_name IS NOT NULL AND pr.repo_full_name != ''
               AND p.id IN ({placeholders})"
        );
        let mut stmt = db.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
        let collected = stmt
            .query_map(params.as_slice(), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        collected
    } else {
        let mut stmt = db.conn.prepare(
            "SELECT DISTINCT pr.repo_full_name, p.name as project_name
             FROM pull_requests pr
             JOIN students s ON s.id = pr.author_id
             JOIN projects p ON p.id = s.team_project_id
             WHERE pr.repo_full_name IS NOT NULL AND pr.repo_full_name != ''",
        )?;
        let collected = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        collected
    };
    if rows.is_empty() {
        return Ok(());
    }

    let token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
    let mgr =
        sprint_grader_collect::repo_manager::RepoManager::new(entregues_dir.to_path_buf(), token);
    for (repo_full_name, project_name) in rows {
        let folder = project_name
            .clone()
            .unwrap_or_else(|| repo_full_name.clone());
        if let Err(e) = mgr.clone_or_update(&repo_full_name, &folder) {
            warn!(repo = %repo_full_name, error = %e, "repo clone/update failed");
        }
    }
    Ok(())
}

pub fn rerun_post_collection_for_sprint_ids(
    config: &Config,
    db_path: &Path,
    entregues_dir: &Path,
    sprint_ids: &[i64],
    max_workers: Option<usize>,
) -> Result<()> {
    if sprint_ids.is_empty() {
        return Ok(());
    }

    let db = Database::open(db_path).context("opening grading DB")?;
    db.create_tables().context("schema migration")?;
    let data_dir = entregues_dir.parent().unwrap_or(entregues_dir);

    // Survival is per-sprint — each call needs the sprint's ordinal so that
    // its inner `sprint_id_for_project(ord)` lookup hits the same sprint.
    for sid in sprint_ids {
        let ord = sprint_grader_survival::survival::ordinal_for_sprint_id(&db, *sid).unwrap_or(1);
        if let Err(e) = sprint_grader_survival::survival::compute_survival(
            &db,
            ord,
            data_dir,
            Some(vec![*sid]),
            config.detector_thresholds.cosmetic_rewrite_pct_of_lat,
        ) {
            warn!(sprint_id = sid, error = %e, "survival failed");
        }
    }
    drop(db);

    let workers = max_workers.unwrap_or(sprint_ids.len());
    let results =
        run_parallel_project_block(db_path, config, sprint_ids, workers, true)?;
    for r in &results {
        if !r.stage_errors.is_empty() {
            let failed: Vec<&str> = r.stage_errors.iter().map(|(k, _)| k.as_str()).collect();
            warn!(
                sprint_id = r.sprint_id,
                failed = ?failed,
                elapsed = format!("{:.1}s", r.elapsed_seconds),
                "post-collection rerun had stage failures"
            );
        }
    }

    let db = Database::open(db_path).context("reopening grading DB")?;
    db.create_tables().context("schema migration")?;
    // Derive project scope from the sprint_ids we just reran, so trajectory
    // recomputation doesn't sweep unrelated projects' rows.
    let mut project_ids: Vec<i64> = Vec::new();
    for sid in sprint_ids {
        if let Ok(Some(pid)) =
            db.conn
                .query_row("SELECT project_id FROM sprints WHERE id = ?", [sid], |r| {
                    r.get::<_, Option<i64>>(0)
                })
        {
            if !project_ids.contains(&pid) {
                project_ids.push(pid);
            }
        }
    }
    sprint_grader_analyze::compute_all_trajectories_filtered(
        &db.conn,
        &config.detector_thresholds,
        Some(&project_ids),
    )
    .context("trajectory failed")?;
    Ok(())
}

/// SIGKILL every gradle daemon JVM owned by the current user. Stale
/// daemons accumulate from prior runs that timed out — gradle's daemon
/// `setsid()`s into its own session right after fork, so the
/// compile-stage's group-kill on timeout cannot reach it. A 2–4 GB
/// orphan daemon per leaked run starves the host of RAM and tilts the
/// next run toward OOM-kills mid-build, which manifest as gradle CLI
/// hangs (the dead daemon never sends the build-complete socket ack).
///
/// Pure-Rust `/proc` walk so we don't depend on `pkill`. Best-effort:
/// errors are logged at debug level only.
/// Public entry point: kill any leaked gradle daemons + worktrees from
/// prior crashed runs and clear the daemon registry. Safe to call from
/// any compile-related subcommand at start. Logs `swept_*` counts.
pub fn sweep_pre_compile_state(entregues_dir: &Path) {
    kill_stale_gradle_daemons();
    purge_gradle_daemon_registry();
    sweep_stale_worktrees(entregues_dir);
}

fn kill_stale_gradle_daemons() {
    #[cfg(unix)]
    {
        let me = unsafe { libc::geteuid() };
        let proc_dir = match std::fs::read_dir("/proc") {
            Ok(d) => d,
            Err(_) => return,
        };
        let mut killed = 0usize;
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
            // Owner must match the current uid; otherwise skip.
            let stat_path = entry.path().join("status");
            let owner_ok = std::fs::read_to_string(&stat_path)
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("Uid:"))
                        .and_then(|l| l.split_whitespace().nth(1).map(|v| v.parse::<u32>().ok()))
                        .flatten()
                })
                .map(|uid| uid == me)
                .unwrap_or(false);
            if !owner_ok {
                continue;
            }
            // Match GradleDaemon bootstrap class in cmdline (NUL-separated argv).
            let cmdline_path = entry.path().join("cmdline");
            let cmdline = match std::fs::read(&cmdline_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let is_daemon = cmdline
                .split(|b| *b == 0)
                .any(|arg| arg == b"org.gradle.launcher.daemon.bootstrap.GradleDaemon");
            if !is_daemon {
                continue;
            }
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            killed += 1;
        }
        if killed > 0 {
            info!(killed, "swept stale gradle daemons before pipeline start");
        }
    }
}

/// Best-effort cache invalidation: gradle's daemon registry remembers
/// daemons we just SIGKILLed. Subsequent runs would log "could not be
/// reused" noise and try to handshake with the dead PIDs first. We
/// remove the per-version registry binary; gradle re-creates it.
fn purge_gradle_daemon_registry() {
    let home = match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => return,
    };
    purge_registry_under(&home.join(".gradle").join("daemon"));

    // Per-worker GRADLE_USER_HOMEs (compile_stage uses
    // $HOME/.gradle-grader-workers/w<N>/) keep their own daemon
    // registries. We must clear theirs too, otherwise a leftover dead-PID
    // entry in worker-N's registry will block worker-N's first new build.
    let workers_root = home.join(".gradle-grader-workers");
    if let Ok(workers) = std::fs::read_dir(&workers_root) {
        for w in workers.flatten() {
            purge_registry_under(&w.path().join("daemon"));
        }
    }
}

fn purge_registry_under(daemon_root: &Path) {
    let versions = match std::fs::read_dir(daemon_root) {
        Ok(d) => d,
        Err(_) => return,
    };
    for entry in versions.flatten() {
        let registry = entry.path().join("registry.bin");
        if registry.exists() {
            let _ = std::fs::remove_file(&registry);
        }
    }
}

/// Each PR build runs in a `tempfile::tempdir()` named `compile_<sha8>_…`
/// under `$TMPDIR` (typically `/tmp`). The `WorktreeGuard` Drop runs
/// `git worktree remove --force` on the registered worktree, but the
/// directory itself is only `unlink`'d when `TempDir::Drop` runs — which
/// it does NOT when our process is hard-killed. Result: hundreds of
/// `/tmp/compile_*` leftovers from prior crashed runs accumulate (1.6 GB
/// observed). We sweep them on next start.
///
/// Also `git worktree prune` the source repos so the `.git/worktrees/`
/// registry doesn't keep stale entries that confuse subsequent
/// `git worktree add` calls.
fn sweep_stale_worktrees(entregues_dir: &Path) {
    let tmp = std::env::temp_dir();
    let entries = match std::fs::read_dir(&tmp) {
        Ok(d) => d,
        Err(_) => return,
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("compile_") {
            continue;
        }
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        info!(removed, "swept stale /tmp/compile_* worktree directories");
    }

    // Prune git worktrees in each entregues subdir so a fresh
    // `git worktree add` doesn't trip over forgotten registrations.
    let projects = match std::fs::read_dir(entregues_dir) {
        Ok(d) => d,
        Err(_) => return,
    };
    for project in projects.flatten() {
        let pdir = match std::fs::read_dir(project.path()) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for repo in pdir.flatten() {
            let dot_git = repo.path().join(".git");
            if !dot_git.exists() {
                continue;
            }
            let _ = std::process::Command::new("git")
                .args([
                    "-C",
                    repo.path().to_str().unwrap_or("."),
                    "worktree",
                    "prune",
                ])
                .output();
        }
    }
}

pub fn run_pipeline(
    config: &Config,
    db_path: &Path,
    variant: PipelineVariant,
    opts: &PipelineOptions,
) -> Result<()> {
    sweep_pre_compile_state(&opts.entregues_dir);

    let total_stages = if variant.ai_detection() { 6 } else { 5 };
    info!(
        variant = variant.name(),
        today = %opts.today,
        total_stages,
        "pipeline start"
    );

    let db = Database::open(db_path).context("opening grading DB")?;
    db.create_tables().context("schema migration")?;

    // T-P2.6: jitter the detector thresholds (seeded by today + course_id)
    // when `[grading] hidden_thresholds = true`. The audit row always
    // lands so re-runs can be cross-referenced even with jitter disabled.
    let mut config = config.clone();
    let course_id = config.course_id;
    let jitter_record =
        sprint_grader_core::jitter::apply_threshold_jitter(&mut config, &opts.today, course_id);
    if let Err(e) = sprint_grader_core::jitter::record_pipeline_run(&db.conn, &jitter_record) {
        warn!(error = %e, "could not write pipeline_run row (non-fatal)");
    }
    let config = &config;

    // Stage 0: purge existing (go / go-quick only)
    if variant.purge_existing() {
        if let Some(names) = &opts.project_filter {
            let mut project_ids: Vec<i64> = Vec::new();
            for name in names {
                if let Ok(pid) =
                    db.conn
                        .query_row("SELECT id FROM projects WHERE name = ?", [name], |r| {
                            r.get::<_, i64>(0)
                        })
                {
                    project_ids.push(pid);
                }
            }
            if !project_ids.is_empty() {
                info!(
                    projects = ?names,
                    "purging existing project data before collection"
                );
                crate::purge::purge_projects(&db.conn, &project_ids, false)
                    .context("purge_projects failed")?;
            }
        }
    }

    // Stage 1: collection (one pass; collector internally walks every sprint
    // with start_date <= today per project).
    info!(
        stage = 1,
        total = total_stages,
        today = %opts.today,
        "collecting data"
    );
    let collect_opts = sprint_grader_collect::CollectOpts {
        today: opts.today.clone(),
        project_filter: opts.project_filter.clone(),
        skip_github: opts.skip_github,
        skip_repos: opts.skip_repos,
        force_pr_refresh: opts.force_pr_refresh,
        repos_dir: Some(opts.entregues_dir.clone()),
    };
    sprint_grader_collect::run_collection(config, &db, &collect_opts)
        .context("collection failed")?;

    // Resolve `--projects` slug filter to project_ids ONCE; from here on
    // every globally-iterating stage takes this list so `--projects`
    // strictly scopes the run.
    let scoped_project_ids: Option<Vec<i64>> = match opts.project_filter.as_deref() {
        Some(names) if !names.is_empty() => {
            let mut ids: Vec<i64> = Vec::with_capacity(names.len());
            for name in names {
                if let Ok(pid) =
                    db.conn
                        .query_row("SELECT id FROM projects WHERE name = ?", [name], |r| {
                            r.get::<_, i64>(0)
                        })
                {
                    ids.push(pid);
                }
            }
            Some(ids)
        }
        _ => None,
    };
    let project_ids_filter: Option<&[i64]> = scoped_project_ids.as_deref();

    if !opts.skip_repos && !opts.skip_github {
        clone_repos_from_db(&db, &opts.entregues_dir, project_ids_filter)?;
    }

    // T-P2.1: per-student estimation-bias fitter. Pools every estimated
    // task across every sprint of each project (Bayesian posterior wants
    // all the data). Runs *before* the per-sprint flag detection inside
    // the parallel block so `student_estimation_bias` is populated when
    // the ESTIMATION_BIAS detector queries it on the same run. The fit
    // depends only on `tasks` + `sprints`, both populated by `collect`.
    match sprint_grader_estimation::fit_and_persist_for_projects(&db.conn, project_ids_filter) {
        Ok(n) => info!(students_written = n, "estimation bias fitting done"),
        Err(e) => warn!(error = %e, "estimation bias fitting failed"),
    }

    // Resolve sprint groupings after collection.
    let groups = resolve_all_sprint_tuples(&db, &opts.today, opts.project_filter.as_deref())?;
    if groups.is_empty() {
        warn!("no projects matched — nothing to process");
        return Ok(());
    }
    let flat_sprint_ids: Vec<i64> = groups
        .iter()
        .flat_map(|g| g.sprint_ids.iter().copied())
        .collect();

    // Stage 2: survival — one pass per sprint (each with its ordinal).
    info!(stage = 2, total = total_stages, "survival analysis");
    let data_dir = opts.entregues_dir.parent().unwrap_or(&opts.entregues_dir);
    for sid in &flat_sprint_ids {
        let ord = sprint_grader_survival::survival::ordinal_for_sprint_id(&db, *sid).unwrap_or(1);
        if let Err(e) = sprint_grader_survival::survival::compute_survival(
            &db,
            ord,
            data_dir,
            Some(vec![*sid]),
            config.detector_thresholds.cosmetic_rewrite_pct_of_lat,
        ) {
            if variant.ai_detection() {
                warn!(sprint_id = sid, error = %e, "survival failed (tolerant in go/go-quick)");
            } else {
                return Err(e).context("survival failed");
            }
        }
    }

    // PR compilation: ONE rayon pool sized to `max_parallel_builds`, sweeping
    // every PR across every sprint. Hoisted out of the per-sprint parallel
    // block to avoid N(sprints) × M(builds) concurrent gradle processes —
    // observed as 20 in flight with the default M=5 and 4 sprints.
    {
        let profiles =
            match sprint_grader_compile::load_build_profiles_from_config(&config.build_profiles) {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "build profile load failed; skipping compile");
                    Vec::new()
                }
            };
        if !profiles.is_empty() {
            let max_parallel = config.build.max_parallel_builds as usize;
            let stderr_cap = config.build.stderr_max_chars as usize;
            let skip_tested = config.build.skip_already_tested;
            let mutation_enabled = config.mutation.enabled;
            if let Err(e) = sprint_grader_compile::check_compilations_parallel(
                &db.conn,
                &flat_sprint_ids,
                &opts.entregues_dir,
                &profiles,
                max_parallel,
                stderr_cap,
                skip_tested,
                mutation_enabled,
                None,
                None,
            ) {
                warn!(error = %e, "compile sweep failed");
            }
        }
    }

    // Close the master DB before rayon workers open their own connections.
    // Keep the schema already applied; workers just call `Connection::open`.
    drop(db);

    // Stage 3: parallel per-(project, sprint) analysis.
    info!(
        stage = 3,
        total = total_stages,
        sprints = flat_sprint_ids.len(),
        "per-project parallel block"
    );
    let max_workers = opts.max_workers.unwrap_or(flat_sprint_ids.len());
    let results = run_parallel_project_block(
        db_path,
        config,
        &flat_sprint_ids,
        max_workers,
        !matches!(variant, PipelineVariant::GoQuick),
    )?;
    for r in &results {
        if r.stage_errors.is_empty() {
            info!(
                sprint_id = r.sprint_id,
                elapsed = format!("{:.1}s", r.elapsed_seconds),
                "project ok"
            );
        } else {
            let failed: Vec<&str> = r.stage_errors.iter().map(|(k, _)| k.as_str()).collect();
            warn!(
                sprint_id = r.sprint_id,
                failed = ?failed,
                elapsed = format!("{:.1}s", r.elapsed_seconds),
                "project had {} stage failure(s)",
                r.stage_errors.len()
            );
        }
    }

    // Kill any gradle daemons spawned during this run. The pre-run sweep
    // handled daemons from *prior* runs; this one handles daemons that were
    // started during the compile stage just completed and are now idle.
    // Daemons setsid() into their own session so the build-time group-kill
    // only fires on timeout; clean builds leave the daemon alive otherwise.
    kill_stale_gradle_daemons();
    purge_gradle_daemon_registry();

    // Re-open the master DB for the tail (AI detection + trajectory + reports).
    let db = Database::open(db_path).context("reopening grading DB")?;

    // T-P2.5: auto-freeze the curriculum for any sprint whose end_date has
    // passed. The freeze is idempotent so running this on every pipeline
    // invocation is safe — already-frozen sprints become no-ops.
    if config.curriculum_freeze_after_sprint_end {
        for sid in &flat_sprint_ids {
            let end_date: Option<String> = db
                .conn
                .query_row("SELECT end_date FROM sprints WHERE id = ?", [*sid], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .ok()
                .flatten();
            if let Some(end_date) = end_date {
                if end_date.as_str() < opts.today.as_str() {
                    let ord = sprint_grader_survival::survival::ordinal_for_sprint_id(&db, *sid)
                        .unwrap_or(1) as i64;
                    if let Err(e) =
                        sprint_grader_curriculum::freeze_curriculum_for_sprint(&db.conn, *sid, ord)
                    {
                        warn!(sprint_id = sid, error = %e, "curriculum freeze failed");
                    }
                }
            }
        }
    }

    // T-P2.2: architecture conformance scan. Runs on every variant
    // (not gated by ai_detection) — it's a deterministic scan of the
    // already-cloned repos, not an LLM call. Skips silently when
    // `architecture.toml` is absent or the project dir doesn't exist.
    let arch_rules_path = opts.config_dir.join("architecture.toml");
    if arch_rules_path.is_file() {
        match sprint_grader_architecture::ArchitectureRules::load(&arch_rules_path) {
            Ok(arch_rules) => {
                for g in &groups {
                    let project_root = opts.entregues_dir.join(&g.name);
                    for sid in &g.sprint_ids {
                        if let Err(e) = sprint_grader_architecture::scan_project_to_db(
                            &db.conn,
                            &project_root,
                            *sid,
                            &arch_rules,
                        ) {
                            warn!(project = %g.name, sprint_id = sid, error = %e, "architecture scan failed");
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, path = %arch_rules_path.display(), "architecture rules load failed")
            }
        }
    } else {
        info!(
            path = %arch_rules_path.display(),
            "architecture.toml absent — skipping architecture scan"
        );
    }

    // T-P3.3: LLM-judged architecture review. Gated by config flag +
    // judge backend prerequisites:
    //   - `judge = "claude-cli"` (default) requires the local `claude`
    //     binary on `$PATH` (or via `claude_cli_path`). No API key.
    //   - `judge = "anthropic-api"` requires `ANTHROPIC_API_KEY`.
    // Either way, missing prerequisite → silent skip; never hard-fail.
    if config.architecture.llm_review {
        let judge_kind = config.architecture.judge.as_str();
        let judge_box: Option<Box<dyn sprint_grader_architecture_llm::Judge + Send + Sync>> =
            match judge_kind {
                "claude-cli" => {
                    if !sprint_grader_architecture_llm::ClaudeCliJudge::is_available(
                        &config.architecture.claude_cli_path,
                    ) {
                        info!(
                            cli_path = %config.architecture.claude_cli_path,
                            "[architecture] llm_review = true but `claude` CLI is not available — skipping LLM review"
                        );
                        None
                    } else {
                        Some(Box::new(
                            sprint_grader_architecture_llm::ClaudeCliJudge::new(
                                config.architecture.claude_cli_path.clone(),
                                config.architecture.model_id.clone(),
                                config.architecture.judge_timeout_seconds,
                            ),
                        ))
                    }
                }
                "anthropic-api" => {
                    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
                    if api_key.is_empty() {
                        info!(
                            "[architecture] llm_review = true with judge = \"anthropic-api\" but ANTHROPIC_API_KEY is empty — skipping LLM review"
                        );
                        None
                    } else {
                        match sprint_grader_architecture_llm::LlmJudge::new(
                            &api_key,
                            config.architecture.model_id.clone(),
                            config.architecture.max_tokens,
                        ) {
                            Ok(j) => Some(Box::new(j)),
                            Err(e) => {
                                warn!(error = %e, "could not construct Anthropic client; skipping LLM review");
                                None
                            }
                        }
                    }
                }
                other => {
                    warn!(
                        judge = %other,
                        "[architecture] unknown judge — expected \"claude-cli\" or \"anthropic-api\"; skipping LLM review"
                    );
                    None
                }
            };

        if let Some(judge_box) = judge_box {
            let rubric_path = opts.config_dir.join(&config.architecture.rubric_path);
            let workers = config.architecture.judge_workers.max(1);
            match sprint_grader_architecture::rubric::load(&rubric_path) {
                Ok(rubric) => {
                    info!(
                        judge = %judge_kind,
                        workers,
                        "[architecture] running LLM review"
                    );
                    let judge = judge_box;
                    for g in &groups {
                        let project_root = opts.entregues_dir.join(&g.name);
                        for sid in &g.sprint_ids {
                            let entries = match std::fs::read_dir(&project_root) {
                                Ok(e) => e,
                                Err(_) => continue,
                            };
                            for entry in entries.flatten() {
                                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                                    continue;
                                }
                                let repo_path = entry.path();
                                let repo_full_name =
                                    entry.file_name().to_string_lossy().into_owned();
                                let stack = if repo_full_name.starts_with("android-") {
                                    "android"
                                } else {
                                    "spring"
                                };
                                // The judge implementation does its own
                                // intra-repo concurrency via run_llm_review_for_repo;
                                // we currently iterate (repo, sprint) pairs
                                // sequentially in the orchestrator. Worker-pool
                                // concurrency for the per-file calls happens
                                // inside `run_llm_review_for_repo` (see
                                // architecture_llm/src/lib.rs) when
                                // `architecture.judge_workers > 1`.
                                if let Err(e) =
                                    sprint_grader_architecture_llm::run_llm_review_for_repo(
                                        &db.conn,
                                        &repo_path,
                                        &repo_full_name,
                                        *sid,
                                        &rubric,
                                        stack,
                                        judge.as_ref(),
                                        &config.architecture.llm_skip_globs,
                                        workers,
                                    )
                                {
                                    warn!(repo = %repo_full_name, sprint_id = sid, error = %e, "LLM architecture review failed");
                                }
                            }
                        }
                    }
                    // Architecture stage wrote new violation rows; re-run
                    // attribution per (repo, sprint) so the LLM rows pick
                    // up blame weights too.
                    for g in &groups {
                        let project_root = opts.entregues_dir.join(&g.name);
                        let entries = match std::fs::read_dir(&project_root) {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        for entry in entries.flatten() {
                            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                                continue;
                            }
                            let repo_path = entry.path();
                            let repo_full_name = entry.file_name().to_string_lossy().into_owned();
                            for sid in &g.sprint_ids {
                                if let Err(e) =
                                    sprint_grader_architecture::attribute_violations_for_repo(
                                        &db.conn,
                                        &repo_path,
                                        &repo_full_name,
                                        *sid,
                                    )
                                {
                                    warn!(repo = %repo_full_name, sprint_id = sid, error = %e, "post-LLM attribution failed");
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, path = %rubric_path.display(), "rubric load failed; skipping LLM review");
                }
            }
        }
    }

    // Stage 4: AI detection (go / go-quick) — per (project, sprint).
    if variant.ai_detection() {
        info!(stage = 4, total = total_stages, "AI detection");
        for g in &groups {
            for sid in &g.sprint_ids {
                let ord =
                    sprint_grader_survival::survival::ordinal_for_sprint_id(&db, *sid).unwrap_or(1);
                run_ai_detection_block(
                    &db.conn,
                    *sid,
                    g.project_id,
                    &g.name,
                    &opts.entregues_dir,
                    ord,
                );
            }
        }
    }

    // Stage 5: trajectory aggregation (runs once — cross-sprint).
    let trajectory_stage = if variant.ai_detection() { 5 } else { 4 };
    info!(
        stage = trajectory_stage,
        total = total_stages,
        "trajectory aggregation"
    );
    sprint_grader_analyze::compute_all_trajectories_filtered(
        &db.conn,
        &config.detector_thresholds,
        project_ids_filter,
    )
    .context("trajectory failed")?;

    // Stage 6: reports
    if opts.skip_reports {
        info!("--skip-reports set; stopping after analysis");
        return Ok(());
    }
    let report_stage = if variant.ai_detection() { 6 } else { 5 };
    info!(
        stage = report_stage,
        total = total_stages,
        "generating reports"
    );

    // Excel: flat list of sprint ids; xlsx.rs groups into sprint_K/ subdirs.
    sprint_grader_report::generate_reports(&db.conn, &flat_sprint_ids, &opts.entregues_dir, None)
        .context("Excel report generation failed")?;

    // Markdown: one multi-sprint REPORT.md per Android repository.
    for g in &groups {
        let Some(repo_root) = android_repo_root(&opts.entregues_dir, &g.name) else {
            warn!(
                project = %g.name,
                "android repo clone not found; skipping Markdown report"
            );
            continue;
        };
        let report_path = repo_root.join("REPORT.md");
        if let Err(e) = sprint_grader_report::generate_markdown_report_multi_to_path(
            &db.conn,
            g.project_id,
            &g.name,
            &g.sprint_ids,
            &report_path,
        ) {
            warn!(project = %g.name, path = %report_path.display(), error = %e, "markdown report failed");
        }
    }

    info!(variant = variant.name(), "pipeline complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_name_matches_python() {
        assert_eq!(PipelineVariant::RunAll.name(), "run-all");
        assert_eq!(PipelineVariant::Go.name(), "go");
        assert_eq!(PipelineVariant::GoQuick.name(), "go-quick");
    }

    #[test]
    fn only_go_variants_run_ai_detection() {
        assert!(!PipelineVariant::RunAll.ai_detection());
        assert!(PipelineVariant::Go.ai_detection());
        assert!(PipelineVariant::GoQuick.ai_detection());
    }

    #[test]
    fn only_go_variants_purge_existing() {
        assert!(!PipelineVariant::RunAll.purge_existing());
        assert!(PipelineVariant::Go.purge_existing());
        assert!(PipelineVariant::GoQuick.purge_existing());
    }
}
