//! `sprint-grader` CLI entry point.
//!
//! Mirrors the subcommand tree of the Python `src/cli.py`. At this stage only
//! the command surface is wired up — each subcommand prints a "not yet
//! implemented" notice. Foundation milestone: `--help` renders, config loads,
//! and the grading DB opens with the full 41-table schema.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use sprint_grader_collect::{run_collection, CollectOpts};
use sprint_grader_core::{Config, Database};
use sprint_grader_orchestration::android_repo_root;
use sprint_grader_orchestration::pipeline::resolve_all_sprint_tuples;

fn parse_project_filter(projects: Option<String>) -> Option<Vec<String>> {
    projects.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
}

/// Render the table-by-table effect of `go` / `go-quick`'s purge step
/// without touching the DB. Exits the parent command after printing.
/// (T-P1.6 — `--dry-run` for go/go-quick previews the purge only.)
fn preview_go_purge(db: &Database, project_filter: Option<&[String]>) -> Result<()> {
    let names = match project_filter {
        Some(n) if !n.is_empty() => n,
        _ => {
            println!(
                "[dry-run] go/go-quick only purges when --projects is set; \
                 nothing would be deleted."
            );
            return Ok(());
        }
    };
    let mut project_ids: Vec<i64> = Vec::new();
    for name in names {
        if let Ok(pid) = db
            .conn
            .query_row("SELECT id FROM projects WHERE name = ?", [name], |r| {
                r.get::<_, i64>(0)
            })
        {
            project_ids.push(pid);
        }
    }
    if project_ids.is_empty() {
        println!("[dry-run] no projects matched filter; nothing would be deleted.");
        return Ok(());
    }
    let report = sprint_grader_orchestration::purge_projects(&db.conn, &project_ids, true)
        .context("purge_projects dry-run failed")?;
    println!("[dry-run] go/go-quick purge would affect:");
    for (table, count) in &report {
        println!("  {table}: {count}");
    }
    Ok(())
}

/// Resolve the project root used for config and `.env` loading.
/// Defaults to the directory where the CLI is executed.
fn default_project_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[derive(Debug, Parser)]
#[command(
    name = "sprint-grader",
    version,
    about = "Sprint Grading Pipeline — automated anomaly detection for student projects",
    long_about = None,
)]
struct Cli {
    /// Enable debug logging
    #[arg(short = 'v', long, global = true)]
    verbose: bool,

    /// Override the project root (contains config/ and .env)
    #[arg(long, global = true)]
    project_root: Option<PathBuf>,

    /// Override where pipeline state lives (grading.db, repo clones, per-sprint report output).
    /// Defaults to `./data` relative to the current working directory when not set.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    /// Reference date (ISO `YYYY-MM-DD`). All sprints with `start_date <= today`
    /// are processed; the sprint containing today is the current sprint.
    /// Defaults to today's UTC date.
    #[arg(long, global = true, value_name = "YYYY-MM-DD")]
    today: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
struct ProjectsArg {
    /// Comma-separated project names (default: all)
    #[arg(long)]
    projects: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Stage 1: TrackDev + GitHub data collection and repo cloning.
    Collect {
        #[command(flatten)]
        projects: ProjectsArg,
        /// Skip GitHub API calls (TrackDev only)
        #[arg(long)]
        skip_github: bool,
        /// Skip repo cloning/updating
        #[arg(long)]
        skip_repos: bool,
        /// Re-fetch GitHub data even for merged/closed PRs already in the DB
        #[arg(long)]
        force_pr_refresh: bool,
    },
    /// Stage 1.5: PR compilation testing.
    Compile {
        #[command(flatten)]
        projects: ProjectsArg,
        /// Re-test PRs already in the database
        #[arg(long)]
        force: bool,
    },
    /// Stage 2: survival analysis (parse, normalize, fingerprint, blame, rates).
    Survive {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Stage 3: per-student metrics and flag detection.
    Analyze {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Stage 4: LLM PR documentation scoring.
    Evaluate {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Team inequality, contribution, and trajectory metrics.
    Inequality {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Code quality metrics (complexity, Halstead, SATD, deltas).
    Quality {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Process metrics (planning, regularity, temporal, collaboration).
    Process {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// AI usage detection (behavioral + stylometry + curriculum + text + fusion).
    AiDetect {
        #[command(flatten)]
        projects: ProjectsArg,
        /// Accepted for shell-script compatibility (perplexity is not in the Rust port).
        #[arg(long)]
        skip_perplexity: bool,
        /// Accepted for shell-script compatibility (LLM-as-judge is not in the Rust port).
        #[arg(long)]
        skip_llm: bool,
    },
    /// Cluster tasks by (stack, layer, action) with MAD-based outlier detection.
    TaskSimilarity {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Classify merged PRs into submission timing tiers.
    TemporalAnalysis {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Build or inspect the curriculum knowledge base from course slides.
    Curriculum {
        /// Parse slides and rebuild concepts DB
        #[arg(long)]
        rebuild: bool,
    },
    /// Freeze the curriculum-as-taught for a specific sprint into
    /// `curriculum_concepts_snapshot`. Idempotent: re-running for a sprint
    /// that's already frozen is a no-op. T-P2.5.
    FreezeCurriculum {
        /// 1-based sprint ordinal (e.g. `--sprint 2` for the second sprint).
        /// All projects' sprints with this ordinal are frozen.
        #[arg(long)]
        sprint: u32,
    },
    /// Generate Excel (.xlsx) + Markdown (.md) multi-sprint project report.
    Report {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Refresh reports for every sprint up to `today` and publish them to the
    /// Android repo clones.
    SyncReports {
        #[command(flatten)]
        projects: ProjectsArg,
        /// Commit and push updated report files directly to `main`
        #[arg(long)]
        push: bool,
    },

    // Full-pipeline variants
    /// Full pipeline: collect + analyze + report for every sprint up to today (no AI detection).
    RunAll {
        #[command(flatten)]
        projects: ProjectsArg,
        #[arg(long)]
        skip_github: bool,
        #[arg(long)]
        skip_repos: bool,
        #[arg(long)]
        force_pr_refresh: bool,
    },
    /// End-of-sprint evaluation: purge, re-collect, full analysis + AI detection.
    Go {
        #[command(flatten)]
        projects: ProjectsArg,
        #[arg(long)]
        skip_github: bool,
        #[arg(long)]
        skip_repos: bool,
        #[arg(long)]
        skip_perplexity: bool,
        #[arg(long)]
        skip_llm_judge: bool,
        #[arg(long)]
        force_pr_refresh: bool,
        /// Preview the purge step's effect and exit before any pipeline
        /// stage runs. (T-P1.6)
        #[arg(long)]
        dry_run: bool,
        /// Refuse to start if `git status --porcelain` reports a dirty
        /// working tree.
        #[arg(long)]
        require_clean_tree: bool,
    },
    /// Like `go` but skips the Section-3 LLM code review.
    GoQuick {
        #[command(flatten)]
        projects: ProjectsArg,
        #[arg(long)]
        skip_github: bool,
        #[arg(long)]
        skip_repos: bool,
        #[arg(long)]
        skip_perplexity: bool,
        #[arg(long)]
        skip_llm_judge: bool,
        #[arg(long)]
        force_pr_refresh: bool,
        /// Preview the purge step's effect and exit before any pipeline
        /// stage runs. (T-P1.6)
        #[arg(long)]
        dry_run: bool,
        /// Refuse to start if `git status --porcelain` reports a dirty
        /// working tree.
        #[arg(long)]
        require_clean_tree: bool,
    },

    // Diagnostics
    /// Dump LAT/LAR/LS computation for specific PRs.
    DebugPrLines {
        #[command(flatten)]
        projects: ProjectsArg,
    },
    /// Drop cached derived rows for every sprint up to today so they can be recomputed.
    PurgeCache {
        #[command(flatten)]
        projects: ProjectsArg,
        #[arg(long)]
        line_metrics: bool,
        #[arg(long)]
        survival: bool,
        #[arg(long)]
        compilation: bool,
        #[arg(long)]
        doc_eval: bool,
        /// Preview the purge: print per-table row counts and exit without
        /// modifying the DB.
        #[arg(long)]
        dry_run: bool,
        /// Refuse to purge if `git status --porcelain` reports a dirty
        /// working tree (guard against accidental purge during manual edits).
        #[arg(long)]
        require_clean_tree: bool,
    },
    /// Print the resolved architecture rubric for one stack
    /// (`spring` or `android`) so it can be inspected without running
    /// any LLM. Reads `config/architecture.md`. T-P3.2.
    ArchitectureRubric {
        /// Stack alias: `spring` / `backend`, or `android` / `mobile`.
        #[arg(long)]
        stack: String,
    },
    /// Diff two `grading.db` files table-by-table (dual-run verification).
    DiffDb {
        /// First DB (e.g. Python-produced reference)
        db_a: PathBuf,
        /// Second DB (e.g. Rust-produced output)
        db_b: PathBuf,
        /// Comma-separated table whitelist
        #[arg(long)]
        tables: Option<String>,
        /// Only diff derived tables (skip projects/students/tasks/PRs)
        #[arg(long)]
        derived_only: bool,
        /// Drop columns from the checksum — repeat as `--ignore-cols table:col1,col2`
        #[arg(long, action = clap::ArgAction::Append)]
        ignore_cols: Vec<String>,
        /// Emit per-row diffs for mismatched tables
        #[arg(long)]
        dump_diffs: bool,
        /// Max row-diff lines per mismatched table
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Float tolerance for `--dump-diffs` (checksum stays byte-exact)
        #[arg(long, default_value_t = 0.0)]
        tol: f64,
    },
}

fn setup_logging(verbose: bool) {
    let default_level = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("sprint_grader={default_level}")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .init();
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    setup_logging(cli.verbose);

    let today = cli
        .today
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().date_naive().to_string());

    let project_root = cli.project_root.unwrap_or_else(default_project_root);
    let config_dir = project_root.join("config");
    // `--data-dir` decouples pipeline state from the config root so the tool
    // can run outside the claude-eval tree; default to `./data` under the
    // current working directory when the flag is not set.
    let data_dir = cli.data_dir.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("data")
    });
    let entregues_dir = data_dir.join("entregues");
    let db_path = entregues_dir.join("grading.db");
    std::fs::create_dir_all(&entregues_dir)
        .with_context(|| format!("failed to create data dir {}", entregues_dir.display()))?;
    info!(
        data_dir = %data_dir.display(),
        db_path = %db_path.display(),
        "data location resolved"
    );

    // Load .env from project_root/.env (Python uses CONFIG_DIR.parent / .env).
    let _ = dotenvy::from_path(project_root.join(".env"));

    let config = Config::load(&config_dir)
        .with_context(|| format!("failed to load config from {}", config_dir.display()))?;
    info!("loaded config: course={}", config.course_name);

    let db = Database::open(&db_path)
        .with_context(|| format!("failed to open grading DB at {}", db_path.display()))?;
    db.create_tables()
        .context("failed to create/migrate schema")?;

    let table_count = sprint_grader_core::db::list_tables(&db.conn)?.len();
    info!(tables = table_count, db_path = %db.db_path.display(), "database ready");

    match cli.command {
        Command::Collect {
            projects,
            skip_github,
            skip_repos,
            force_pr_refresh,
        } => {
            let opts = CollectOpts {
                today: today.clone(),
                project_filter: parse_project_filter(projects.projects),
                skip_github,
                skip_repos,
                force_pr_refresh,
                repos_dir: Some(entregues_dir.clone()),
            };
            run_collection(&config, &db, &opts).context("collect failed")?;
        }
        Command::Survive { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for g in &groups {
                for sid in &g.sprint_ids {
                    let ord = sprint_grader_survival::survival::ordinal_for_sprint_id(&db, *sid)
                        .unwrap_or(1);
                    sprint_grader_survival::survival::compute_survival(
                        &db,
                        ord,
                        &data_dir,
                        Some(vec![*sid]),
                        config.detector_thresholds.cosmetic_rewrite_pct_of_lat,
                    )
                    .with_context(|| format!("survive failed for sprint_id {sid}"))?;
                }
            }
        }
        Command::Compile { projects, force } => {
            let profiles =
                sprint_grader_compile::load_build_profiles_from_config(&config.build_profiles)
                    .map_err(|e| anyhow::anyhow!(e))?;
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            let skip_tested = config.build.skip_already_tested && !force;
            for g in &groups {
                for sid in &g.sprint_ids {
                    sprint_grader_compile::check_sprint_compilations_parallel(
                        &db.conn,
                        *sid,
                        &entregues_dir,
                        &profiles,
                        config.build.max_parallel_builds as usize,
                        config.build.stderr_max_chars as usize,
                        skip_tested,
                        config.mutation.enabled,
                    )
                    .with_context(|| format!("compile failed for sprint_id {sid}"))?;
                    sprint_grader_compile::summarize_compilation(&db.conn, *sid)
                        .with_context(|| format!("compile summary failed for sprint_id {sid}"))?;
                }
            }
        }
        Command::Analyze { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for sid in groups.iter().flat_map(|g| g.sprint_ids.iter().copied()) {
                sprint_grader_analyze::metrics::compute_metrics_for_sprint_id(
                    &db.conn,
                    sid,
                    config.thresholds.cramming_hours,
                )
                .with_context(|| format!("metrics failed for sprint_id {sid}"))?;
                sprint_grader_analyze::flags::detect_flags_for_sprint_id(&db.conn, sid, &config)
                    .with_context(|| format!("flags failed for sprint_id {sid}"))?;
            }
        }
        Command::Inequality { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for sid in groups.iter().flat_map(|g| g.sprint_ids.iter().copied()) {
                sprint_grader_analyze::compute_all_inequality(&db.conn, sid)
                    .with_context(|| format!("inequality failed for sprint_id {sid}"))?;
                sprint_grader_analyze::compute_all_contributions(&db.conn, sid, None)
                    .with_context(|| format!("contribution failed for sprint_id {sid}"))?;
            }
            sprint_grader_analyze::compute_all_trajectories(&db.conn, &config.detector_thresholds)
                .context("trajectory failed")?;
        }
        Command::Evaluate { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for sid in groups.iter().flat_map(|g| g.sprint_ids.iter().copied()) {
                sprint_grader_evaluate::run_heuristics_for_sprint_id(&db.conn, sid)
                    .with_context(|| format!("heuristics failed for sprint_id {sid}"))?;
                sprint_grader_evaluate::run_llm_evaluation_for_sprint_id(&db.conn, sid, &config)
                    .with_context(|| format!("llm evaluation failed for sprint_id {sid}"))?;
                sprint_grader_evaluate::score_task_descriptions_for_sprint_id(
                    &db.conn, sid, &config,
                )
                .with_context(|| format!("task description scoring failed for sprint_id {sid}"))?;
            }
        }
        Command::Quality { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for sid in groups.iter().flat_map(|g| g.sprint_ids.iter().copied()) {
                sprint_grader_quality::compute_all_quality(&db.conn, sid)
                    .with_context(|| format!("quality failed for sprint_id {sid}"))?;
            }
        }
        Command::Process { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for sid in groups.iter().flat_map(|g| g.sprint_ids.iter().copied()) {
                sprint_grader_process::compute_all_planning(&db.conn, sid)
                    .with_context(|| format!("planning failed for sprint_id {sid}"))?;
                sprint_grader_process::compute_all_regularity(&db.conn, sid, &config.regularity)
                    .with_context(|| format!("regularity failed for sprint_id {sid}"))?;
                sprint_grader_process::compute_all_temporal(&db.conn, sid)
                    .with_context(|| format!("temporal failed for sprint_id {sid}"))?;
                sprint_grader_process::compute_all_collaboration(&db.conn, sid)
                    .with_context(|| format!("collaboration failed for sprint_id {sid}"))?;
            }
        }
        Command::TaskSimilarity { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for sid in groups.iter().flat_map(|g| g.sprint_ids.iter().copied()) {
                sprint_grader_repo_analysis::compute_task_similarity(
                    &db.conn,
                    sid,
                    &config.repo_analysis,
                )
                .with_context(|| format!("task_similarity failed for sprint_id {sid}"))?;
            }
        }
        Command::TemporalAnalysis { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for sid in groups.iter().flat_map(|g| g.sprint_ids.iter().copied()) {
                sprint_grader_repo_analysis::compute_temporal_analysis(
                    &db.conn,
                    sid,
                    &config.repo_analysis,
                )
                .with_context(|| format!("temporal_analysis failed for sprint_id {sid}"))?;
            }
        }
        Command::AiDetect { projects, .. } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            let fusion_cfg = sprint_grader_ai_detect::fusion::FusionConfig::default();
            for g in &groups {
                for sid in &g.sprint_ids {
                    sprint_grader_ai_detect::compute_all_behavioral(&db.conn, *sid)
                        .with_context(|| format!("behavioral failed for sprint_id {sid}"))?;
                    let ord = sprint_grader_survival::survival::ordinal_for_sprint_id(&db, *sid)
                        .unwrap_or(1);
                    let proj_dir = entregues_dir.join(&g.name);
                    if proj_dir.is_dir() {
                        if let Ok(repo_dirs) = std::fs::read_dir(&proj_dir) {
                            for entry in repo_dirs.flatten() {
                                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                                    continue;
                                }
                                let repo_path = entry.path();
                                let repo_name = entry.file_name().to_string_lossy().into_owned();
                                let _ = sprint_grader_ai_detect::analyze_repo_stylometry(
                                    &db.conn, &repo_path, &repo_name, *sid,
                                );
                                let _ = sprint_grader_ai_detect::scan_repo_curriculum(
                                    &db.conn,
                                    &repo_path,
                                    &repo_name,
                                    g.project_id,
                                    *sid,
                                    ord as i64,
                                );
                                let _ = sprint_grader_ai_detect::fusion::run_full_fusion(
                                    &db.conn,
                                    &repo_name,
                                    g.project_id,
                                    *sid,
                                    &fusion_cfg,
                                );
                            }
                        }
                        let _ = sprint_grader_ai_detect::compute_all_text_consistency(
                            &db.conn,
                            g.project_id,
                            *sid,
                        );
                    }
                    sprint_grader_ai_detect::compute_all_ai_probability(&db.conn, *sid, None)
                        .with_context(|| format!("PR AI probability failed for sprint_id {sid}"))?;
                }
            }
        }
        Command::Curriculum { rebuild } => {
            if !rebuild {
                let count: i64 = db
                    .conn
                    .query_row("SELECT COUNT(*) FROM curriculum_concepts", [], |r| r.get(0))
                    .unwrap_or(0);
                info!(
                    concepts = count,
                    "curriculum DB (pass --rebuild to regenerate)"
                );
            } else {
                let slides_dir = config.curriculum_slides_dir.clone();
                let Some(slides_dir) = slides_dir else {
                    anyhow::bail!("curriculum.slides_dir not set in course.toml — cannot rebuild");
                };
                sprint_grader_curriculum::build_curriculum_db(
                    &db.conn,
                    &slides_dir,
                    &config.curriculum_extra_imports,
                    config.num_sprints,
                )
                .context("curriculum rebuild failed")?;
            }
        }
        Command::FreezeCurriculum { sprint } => {
            // Resolve the DB sprint_id for every project at the requested
            // ordinal (1-based). The same ordinal can map to several
            // sprint_id rows when multiple projects share a course.
            let mut stmt = db.conn.prepare(
                "SELECT sp.id FROM sprints sp
                 WHERE (
                     SELECT COUNT(*) FROM sprints sp2
                     WHERE sp2.project_id = sp.project_id AND sp2.start_date <= sp.start_date
                 ) = ?",
            )?;
            let sprint_ids: Vec<i64> = stmt
                .query_map([sprint as i64], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            if sprint_ids.is_empty() {
                anyhow::bail!(
                    "no sprint with ordinal {} found — run `collect` first or check num_sprints",
                    sprint
                );
            }
            let mut total_written = 0usize;
            for sid in sprint_ids {
                let n = sprint_grader_curriculum::freeze_curriculum_for_sprint(
                    &db.conn,
                    sid,
                    sprint as i64,
                )
                .with_context(|| format!("freeze sprint_id={sid}"))?;
                total_written += n;
            }
            info!(
                sprint = sprint,
                rows_written = total_written,
                "curriculum frozen"
            );
        }
        Command::Report { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            if groups.is_empty() {
                anyhow::bail!(
                    "no project has sprints with start_date <= {} — run `collect` first",
                    today
                );
            }
            let project_name_set: std::collections::HashSet<String> =
                groups.iter().map(|g| g.name.clone()).collect();
            let flat_sprint_ids: Vec<i64> = groups
                .iter()
                .flat_map(|g| g.sprint_ids.iter().copied())
                .collect();

            // Excel: one workbook per (project, sprint); xlsx.rs places each
            // sprint's files inside `{entregues_dir}/sprint_K/`.
            sprint_grader_report::generate_reports(
                &db.conn,
                &flat_sprint_ids,
                &entregues_dir,
                Some(&project_name_set),
            )
            .context("Excel report generation failed")?;

            // Markdown: one multi-sprint REPORT.md per Android repository,
            // at `{entregues_dir}/{project}/android-*/REPORT.md`.
            for g in &groups {
                let Some(repo_root) = android_repo_root(&entregues_dir, &g.name) else {
                    warn!(
                        project = %g.name,
                        "android repo clone not found; skipping Markdown report"
                    );
                    continue;
                };
                let report_path = repo_root.join("REPORT.md");
                sprint_grader_report::generate_markdown_report_multi_to_path(
                    &db.conn,
                    g.project_id,
                    &g.name,
                    &g.sprint_ids,
                    &report_path,
                )
                .with_context(|| format!("Markdown report failed for {}", report_path.display()))?;
            }
            info!(
                output = %entregues_dir.display(),
                projects = groups.len(),
                today = %today,
                "reports generated"
            );
        }
        Command::SyncReports { projects, push } => {
            drop(db);
            let result = sprint_grader_orchestration::sync_reports_through_sprint(
                &config,
                &db_path,
                &entregues_dir,
                &sprint_grader_orchestration::SyncReportsOptions {
                    today: today.clone(),
                    project_filter: parse_project_filter(projects.projects),
                    push,
                },
            )
            .context("sync-reports failed")?;
            info!(
                changed_sprints = result.changed_sprints,
                generated_reports = result.generated_reports.len(),
                published_repos = result.published_repos.len(),
                "report sync complete"
            );
        }
        Command::RunAll {
            projects,
            skip_github,
            skip_repos,
            force_pr_refresh,
        } => {
            drop(db);
            let opts = sprint_grader_orchestration::pipeline::PipelineOptions {
                today: today.clone(),
                project_filter: parse_project_filter(projects.projects),
                entregues_dir: entregues_dir.clone(),
                config_dir: config_dir.clone(),
                skip_github,
                skip_repos,
                skip_reports: false,
                force_pr_refresh,
                max_workers: None,
            };
            sprint_grader_orchestration::run_pipeline(
                &config,
                &db_path,
                sprint_grader_orchestration::PipelineVariant::RunAll,
                &opts,
            )
            .context("run-all pipeline failed")?;
        }
        Command::Go {
            projects,
            skip_github,
            skip_repos,
            skip_perplexity: _,
            skip_llm_judge: _,
            force_pr_refresh,
            dry_run,
            require_clean_tree,
        } => {
            let project_filter = parse_project_filter(projects.projects);
            if require_clean_tree {
                if let Err(msg) = sprint_grader_orchestration::ensure_clean_tree(
                    std::env::current_dir()?.as_path(),
                ) {
                    anyhow::bail!(msg);
                }
            }
            if dry_run {
                preview_go_purge(&db, project_filter.as_deref())?;
                return Ok(());
            }
            drop(db);
            let opts = sprint_grader_orchestration::pipeline::PipelineOptions {
                today: today.clone(),
                project_filter,
                entregues_dir: entregues_dir.clone(),
                config_dir: config_dir.clone(),
                skip_github,
                skip_repos,
                skip_reports: false,
                force_pr_refresh,
                max_workers: None,
            };
            sprint_grader_orchestration::run_pipeline(
                &config,
                &db_path,
                sprint_grader_orchestration::PipelineVariant::Go,
                &opts,
            )
            .context("go pipeline failed")?;
        }
        Command::GoQuick {
            projects,
            skip_github,
            skip_repos,
            skip_perplexity: _,
            skip_llm_judge: _,
            force_pr_refresh,
            dry_run,
            require_clean_tree,
        } => {
            let project_filter = parse_project_filter(projects.projects);
            if require_clean_tree {
                if let Err(msg) = sprint_grader_orchestration::ensure_clean_tree(
                    std::env::current_dir()?.as_path(),
                ) {
                    anyhow::bail!(msg);
                }
            }
            if dry_run {
                preview_go_purge(&db, project_filter.as_deref())?;
                return Ok(());
            }
            drop(db);
            let opts = sprint_grader_orchestration::pipeline::PipelineOptions {
                today: today.clone(),
                project_filter,
                entregues_dir: entregues_dir.clone(),
                config_dir: config_dir.clone(),
                skip_github,
                skip_repos,
                skip_reports: false,
                force_pr_refresh,
                max_workers: None,
            };
            sprint_grader_orchestration::run_pipeline(
                &config,
                &db_path,
                sprint_grader_orchestration::PipelineVariant::GoQuick,
                &opts,
            )
            .context("go-quick pipeline failed")?;
        }
        Command::DebugPrLines { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            let flat_sprint_ids: Vec<i64> = groups
                .iter()
                .flat_map(|g| g.sprint_ids.iter().copied())
                .collect();
            let project_pairs: Vec<(i64, String)> = groups
                .iter()
                .flat_map(|g| g.sprint_ids.iter().map(move |sid| (*sid, g.name.clone())))
                .collect();
            sprint_grader_orchestration::debug_pr_lines(
                &db,
                &data_dir,
                &flat_sprint_ids,
                &project_pairs,
                config.detector_thresholds.cosmetic_rewrite_pct_of_lat,
            )
            .context("debug-pr-lines failed")?;
        }
        Command::PurgeCache {
            projects,
            line_metrics,
            survival,
            compilation,
            doc_eval,
            dry_run,
            require_clean_tree,
        } => {
            let targets = sprint_grader_orchestration::CacheTargets {
                line_metrics,
                survival,
                compilation,
                doc_eval,
            };
            if !targets.any() {
                anyhow::bail!(
                    "pass at least one of --line-metrics, --survival, --compilation, --doc-eval"
                );
            }
            if require_clean_tree {
                if let Err(msg) = sprint_grader_orchestration::ensure_clean_tree(
                    std::env::current_dir()?.as_path(),
                ) {
                    anyhow::bail!(msg);
                }
            }
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            let sprint_ids: Vec<i64> = groups
                .iter()
                .flat_map(|g| g.sprint_ids.iter().copied())
                .collect();
            let project_ids: Option<Vec<i64>> = if filter.is_some() {
                Some(groups.iter().map(|g| g.project_id).collect())
            } else {
                None
            };
            let deleted = sprint_grader_orchestration::purge_cache(
                &db.conn,
                &sprint_ids,
                project_ids.as_deref(),
                targets,
                dry_run,
            )
            .context("purge-cache failed")?;
            if dry_run {
                println!("[dry-run] purge-cache would affect:");
                for (table, count) in &deleted {
                    println!("  {table}: {count}");
                }
            } else {
                for (table, count) in &deleted {
                    info!(table = %table, count, "purged");
                }
            }
        }
        Command::ArchitectureRubric { stack } => {
            let path = config_dir.join("architecture.md");
            if !path.is_file() {
                anyhow::bail!(
                    "architecture rubric not found at {} — write the rubric first or pass a different --project-root",
                    path.display()
                );
            }
            let rubric = sprint_grader_architecture::rubric::load(&path)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            let body = rubric.for_stack(&stack).ok_or_else(|| {
                anyhow::anyhow!(
                    "architecture.md has no section for stack '{}' (expected 'spring' / 'android' or an alias like 'backend' / 'mobile')",
                    stack
                )
            })?;
            println!("{body}");
            println!();
            println!(
                "version={} body_hash={}",
                rubric.version, rubric.body_hash
            );
        }
        Command::DiffDb {
            db_a,
            db_b,
            tables,
            derived_only,
            ignore_cols,
            dump_diffs,
            limit,
            tol,
        } => {
            let table_list: Vec<String> = match tables {
                Some(s) => s
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                None => Vec::new(),
            };
            let ignore_map = sprint_grader_orchestration::parse_ignore_cols(&ignore_cols)
                .context("invalid --ignore-cols")?;
            let opts = sprint_grader_orchestration::DiffOptions {
                tables: table_list,
                derived_only,
                ignore_cols: ignore_map,
                dump_diffs,
                row_limit: limit,
                float_tol: tol,
            };
            let mismatches = sprint_grader_orchestration::run_diff(&db_a, &db_b, &opts)
                .context("diff-db failed")?;
            if mismatches > 0 {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
