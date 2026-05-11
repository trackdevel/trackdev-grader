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
use sprint_grader_orchestration::pipeline::resolve_all_sprint_tuples;
use sprint_grader_orchestration::{
    android_repo_root, publish_report_updates, repo_has_report_changes,
};

fn parse_project_filter(projects: Option<String>) -> Option<Vec<String>> {
    projects.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
}

/// Parse `15m`, `3h`, `2d` into a `chrono::Duration`. Suffix is required:
/// the value must be a positive integer followed by exactly one of `m` / `h`
/// / `d`. Returns the duration or an error suitable for `?` propagation.
fn parse_interval(s: &str) -> Result<chrono::Duration> {
    let s = s.trim();
    let (num_str, unit) = s
        .split_at_checked(s.len().saturating_sub(1))
        .ok_or_else(|| anyhow::anyhow!("empty interval"))?;
    let n: i64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid interval '{s}'; expected e.g. 15m, 3h, 2d"))?;
    if n <= 0 {
        anyhow::bail!("interval must be positive: {s}");
    }
    match unit {
        "m" => Ok(chrono::Duration::minutes(n)),
        "h" => Ok(chrono::Duration::hours(n)),
        "d" => Ok(chrono::Duration::days(n)),
        other => anyhow::bail!("unknown interval unit '{other}'; expected m, h, or d"),
    }
}

/// Render the table-by-table effect of `go` / `go-quick`'s purge step
/// without touching the DB. Exits the parent command after printing.
/// (T-P1.6 — `--dry-run` for go/go-quick previews the purge only.)
fn preview_go_purge(db: &Database, project_filter: Option<&[String]>) -> Result<()> {
    let project_ids: Vec<i64> = match project_filter {
        Some(names) if !names.is_empty() => names
            .iter()
            .filter_map(|name| {
                db.conn
                    .query_row("SELECT id FROM projects WHERE name = ?", [name], |r| {
                        r.get::<_, i64>(0)
                    })
                    .ok()
            })
            .collect(),
        _ => {
            let mut stmt = db.conn.prepare("SELECT id FROM projects ORDER BY id")?;
            let ids: Vec<i64> = stmt
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<_>>()?;
            ids
        }
    };
    if project_ids.is_empty() {
        println!("[dry-run] no projects in DB; nothing would be deleted.");
        return Ok(());
    }
    let report = sprint_grader_orchestration::purge_projects(&db.conn, &project_ids, true)
        .context("purge_projects dry-run failed")?;
    let scope = if project_filter.is_some() {
        format!("{} filtered project(s)", project_ids.len())
    } else {
        format!("ALL {} project(s) in DB", project_ids.len())
    };
    println!("[dry-run] go/go-quick purge would affect {scope}:");
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
    long_about = "Sprint Grading Pipeline — automated anomaly detection for student projects.\n\
                  \n\
                  Four orchestrated pipelines, picked by use case:\n\
                  \n  \
                  run-all    Cumulative additive run. Incremental collection (per-PR watermark\n             \
                             + GitHub ETag); per project, skips survival/compile/architecture\n             \
                             when no new PRs/tasks were collected. No AI detection. Survival\n             \
                             errors are FATAL.\n  \
                  iterate    Same as run-all, but skips the per-file LLM architecture rubric.\n             \
                             Use mid-sprint when you want fresh metrics and reports without\n             \
                             paying for the slow LLM judge.\n  \
                  go-quick   ALWAYS PURGES then re-collects from scratch. PR doc eval forced to\n             \
                             heuristic (no Claude calls); static analysis off by default. AI\n             \
                             detection on. Tolerates survival errors.\n  \
                  go         End-of-sprint full run. ALWAYS PURGES then re-collects from scratch.\n             \
                             LLM PR doc eval (when ANTHROPIC_API_KEY set), AI detection, and\n             \
                             LLM architecture rubric (when configured). Tolerates survival errors.\n\
                  \n\
                  --projects is a scope reducer\n\
                  ─────────────────────────────\n\
                  --projects <slug,…> only narrows the blast radius — it never changes what a\n\
                  command does. For go/go-quick, the purge always runs: with --projects it wipes\n\
                  only the listed projects; without it, it wipes every project in the DB. The\n\
                  cascade clears pull_requests, tasks, sprints, fingerprints, pr_github_etags,\n\
                  the per-PR last_github_fetch_updated_at watermark, and every derived table —\n\
                  which is why go/go-quick always re-fetch every PR (the watermark + ETag cache\n\
                  is gone). That is the end-of-sprint contract: rebuild from scratch.\n\
                  \n\
                  Use --dry-run on go/go-quick to preview the cascade per table before any\n\
                  pipeline stage runs. See `<subcommand> --help` for the full per-variant contract."
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
        /// Compile only PRs with this number (repeat for multiple; matches across all repos)
        #[arg(long, value_name = "NUMBER")]
        pr: Vec<i64>,
        /// Skip PRs already compiled within this interval (e.g. 15m, 3h, 2d).
        /// Applies even with --force.
        #[arg(long, value_name = "INTERVAL")]
        skip_delay: Option<String>,
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
    /// Re-run flag detection only (skips metrics, survival, and all other stages).
    /// Use this after tweaking thresholds in course.toml or after `compile --force`.
    Flags {
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
    /// Run Java static analyzers (PMD, Checkstyle, SpotBugs) and
    /// attribute findings via git blame. Reads `config/static_analysis.toml`
    /// — absent file is a hard error here (unlike the orchestrated runs,
    /// which silently skip the stage when the config file is missing).
    StaticAnalysis {
        #[command(flatten)]
        projects: ProjectsArg,
        /// Skip SpotBugs even if `static_analysis.toml` enables it.
        /// Useful for fast iteration since SpotBugs is the only analyzer
        /// that requires compiled `.class` files.
        #[arg(long)]
        no_spotbugs: bool,
    },
    /// Per-method complexity & testability scan (T-CX). Writes
    /// `method_complexity_findings`, `method_complexity_attribution`,
    /// `method_metrics`, and a `method_complexity_runs` row per
    /// (repo, sprint). Re-runs against the same git HEAD short-circuit
    /// to keep `report` regeneration cheap.
    ComplexityScan {
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
        /// Commit and push updated report files directly to `main`
        #[arg(long)]
        push: bool,
        /// Additionally render an instructor-only `REPORT_PROFESSOR.md`
        /// to the parent of each android repo. Includes per-student
        /// weighted attribution + the COMPLEXITY_HOTSPOT band — never
        /// committed to the team repo. T-CX (step 7).
        #[arg(long)]
        professor_report: bool,
    },
    /// Refresh reports for every sprint up to `today` and publish them to the
    /// Android repo clones.
    SyncReports {
        #[command(flatten)]
        projects: ProjectsArg,
        /// Commit and push updated report files directly to `main`
        #[arg(long)]
        push: bool,
        /// Skip the TrackDev/GitHub collection pass and the post-collect
        /// analysis rerun. Use right after a `run-all` / `go` when the
        /// DB is already current — sync-reports then becomes pure
        /// rendering: regenerate REPORT.md per project (and `--push`
        /// it) without paying for another GitHub round-trip.
        #[arg(long)]
        skip_collect: bool,
    },

    // Full-pipeline variants
    /// Cumulative additive pipeline (no purge, no AI detection).
    ///
    /// Every stage runs against sprints with `start_date <= --today`, but the run is incremental on every axis. Collection skips PRs whose `updated_at` matches the per-PR watermark `last_github_fetch_updated_at`, and GitHub conditional GETs (If-None-Match etag) avoid downloading unchanged payloads on the rest. Repos are cloned or fetched only for projects that actually received new PRs/tasks during this run. Survival, compile, and architecture stages are skipped per project when the post-collect snapshot shows no new PRs/tasks for that project. Even when compile runs, PRs whose merge_sha already has a `pr_compilation` row are not rebuilt (config.build.skip_already_tested).
    ///
    /// Survival errors are FATAL — the run aborts. Use `go` if you'd rather get a partial report.
    ///
    /// PR doc eval: heuristic always; LLM if ANTHROPIC_API_KEY is set. LLM architecture rubric: yes when [architecture] llm_review = true and the configured judge backend is reachable. AI detection: NO. Static analysis runs by default; pass --skip-static-analysis to bypass.
    ///
    /// Use this for the regular cumulative grading run.
    RunAll {
        #[command(flatten)]
        projects: ProjectsArg,
        #[arg(long)]
        skip_github: bool,
        #[arg(long)]
        skip_repos: bool,
        /// Skip the Java static-analysis (PMD/Checkstyle/SpotBugs) stage.
        /// Default: stage runs when `config/static_analysis.toml` exists.
        #[arg(long)]
        skip_static_analysis: bool,
        #[arg(long)]
        force_pr_refresh: bool,
        /// After analysis, force-rebase each project's android clone
        /// onto `origin/main` and render the team-facing REPORT.md
        /// (static-analysis section stripped) into the clone. Nothing
        /// is committed or pushed — review the result with `git diff`
        /// inside each clone and publish later via
        /// `sprint-grader sync-reports --skip-collect --push` or a
        /// manual `git push`.
        #[arg(long)]
        reports: bool,
    },
    /// End-of-sprint full run: purge → re-collect → full analysis + AI detection.
    ///
    /// ALWAYS PURGES before collecting. --projects only narrows the blast radius: with --projects, the cascade is scoped to the listed projects; without it, every project in the DB is wiped. Either way the purge clears pull_requests, tasks, sprints, fingerprints, pr_github_etags, the per-PR `last_github_fetch_updated_at` watermark, and every derived table — which is why re-collection re-fetches every PR (the cache is gone). That is the end-of-sprint contract: rebuild from scratch.
    ///
    /// Unlike run-all, every stage runs for every in-scope project regardless of whether new PRs/tasks were collected — there's no "skip if nothing changed" gate.
    ///
    /// Survival errors are TOLERATED so a partial REPORT.md still lands.
    ///
    /// PR doc eval: heuristic always; LLM if ANTHROPIC_API_KEY is set. AI detection: yes. LLM architecture rubric: yes when configured. Static analysis runs by default; pass --skip-static-analysis to bypass.
    ///
    /// Pass --dry-run to preview the cascade per table and exit before the pipeline runs. --require-clean-tree refuses to start when `git status --porcelain` is non-empty.
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
        /// Skip the Java static-analysis (PMD/Checkstyle/SpotBugs) stage.
        /// Default: stage runs when `config/static_analysis.toml` exists.
        #[arg(long)]
        skip_static_analysis: bool,
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
        /// Render the team-facing REPORT.md into each project's
        /// android clone after analysis (no commit, no push). See
        /// `run-all --reports` for details.
        #[arg(long)]
        reports: bool,
    },
    /// Like `run-all` but skips the per-file LLM architecture rubric.
    ///
    /// Identical incremental semantics to run-all: no purge, watermark +
    /// etag caching for collection, per-project skip when no new PRs/tasks,
    /// per-PR `pr_compilation` cache by merge_sha. The only behavioural
    /// difference is that the architecture LLM judge (the slow per-file
    /// pass) is bypassed even when [architecture] llm_review = true. The
    /// AST-based architecture scan still runs and writes
    /// architecture_violations.
    ///
    /// Survival errors are FATAL (same as run-all).
    ///
    /// PR doc eval: heuristic always; LLM if ANTHROPIC_API_KEY is set.
    /// AI detection: NO. Static analysis runs by default; pass
    /// --skip-static-analysis to bypass.
    ///
    /// Use this when iterating mid-sprint and you want fresh metrics +
    /// reports without paying for the per-file LLM rubric.
    Iterate {
        #[command(flatten)]
        projects: ProjectsArg,
        #[arg(long)]
        skip_github: bool,
        #[arg(long)]
        skip_repos: bool,
        /// Skip the Java static-analysis (PMD/Checkstyle/SpotBugs) stage.
        /// Default: stage runs when `config/static_analysis.toml` exists.
        #[arg(long)]
        skip_static_analysis: bool,
        #[arg(long)]
        force_pr_refresh: bool,
        /// Render the team-facing REPORT.md into each project's
        /// android clone after analysis (no commit, no push). See
        /// `run-all --reports` for details.
        #[arg(long)]
        reports: bool,
    },
    /// Like `go`, but heuristic-only PR doc eval and no static analysis by default.
    ///
    /// ALWAYS PURGES before collecting (same cascade as `go`). --projects only narrows the blast radius: with --projects, the cascade is scoped to the listed projects; without it, every project in the DB is wiped. The cascade clears pr_github_etags and the per-PR `last_github_fetch_updated_at` watermark, so re-collection re-fetches every PR.
    ///
    /// Differs from `go` in two ways. First, PR doc eval ALWAYS uses the heuristic scorer, even with ANTHROPIC_API_KEY set (avoids per-PR Claude calls). Second, static analysis is SKIPPED by default (PMD/Checkstyle/SpotBugs adds 10-20 min); pass --run-static-analysis to opt in.
    ///
    /// Survival errors are TOLERATED. AI detection: yes. LLM architecture rubric: yes when configured.
    ///
    /// Designed for mid-sprint iteration. Pass --dry-run to preview the cascade per table; --require-clean-tree to refuse a dirty tree.
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
        /// Run the Java static-analysis stage even on go-quick.
        /// Default: go-quick **skips** static analysis to keep iteration
        /// fast (the stage adds 10–20 minutes per run); pass this to
        /// force it on.
        #[arg(long)]
        run_static_analysis: bool,
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
        /// Render the team-facing REPORT.md into each project's
        /// android clone after analysis (no commit, no push). See
        /// `run-all --reports` for details.
        #[arg(long)]
        reports: bool,
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
    /// any LLM. Reads `config/architecture-spring.md` or
    /// `config/architecture-android.md` (paths configurable via
    /// `[architecture]` in `course.toml`). T-P3.2.
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
        Command::Compile {
            projects,
            force,
            pr,
            skip_delay,
        } => {
            let skip_recent_within = skip_delay
                .as_deref()
                .map(parse_interval)
                .transpose()
                .context("invalid --skip-delay")?;
            // Stale gradle daemons or worktrees from a prior crashed run
            // tie up the per-version daemon registry; new builds wait
            // forever on a busy/dead daemon. Always sweep at compile start.
            sprint_grader_orchestration::pipeline::sweep_pre_compile_state(&entregues_dir);

            let profiles =
                sprint_grader_compile::load_build_profiles_from_config(&config.build_profiles)
                    .map_err(|e| anyhow::anyhow!(e))?;
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            let skip_tested = config.build.skip_already_tested && !force;
            let pr_filter: Option<&[i64]> = if pr.is_empty() { None } else { Some(&pr) };
            // Single combined batch across every sprint of every project in
            // scope: one rayon pool, one watchdog stream, one cold-start
            // amortisation for the per-worker GRADLE_USER_HOME warm-up.
            let all_sprint_ids: Vec<i64> = groups
                .iter()
                .flat_map(|g| g.sprint_ids.iter().copied())
                .collect();
            if !all_sprint_ids.is_empty() {
                // --force without --pr: purge existing compilation rows for
                // the whole sprint scope before recompiling. Without this,
                // PRs that can no longer be reached (missing repo, no commits,
                // no matching profile) keep stale rows from prior runs, giving
                // the false impression that the DB was not updated. The purge
                // is intentionally scoped to "no --pr" so that a targeted
                // `--force --pr 42` only touches that single PR and leaves
                // other rows intact.
                if force && pr.is_empty() {
                    let phs = all_sprint_ids
                        .iter()
                        .map(|_| "?")
                        .collect::<Vec<_>>()
                        .join(",");
                    db.conn
                        .execute(
                            &format!("DELETE FROM pr_compilation WHERE sprint_id IN ({phs})"),
                            rusqlite::params_from_iter(all_sprint_ids.iter()),
                        )
                        .context("failed to purge pr_compilation for --force")?;
                    db.conn
                        .execute(
                            &format!("DELETE FROM pr_mutation WHERE sprint_id IN ({phs})"),
                            rusqlite::params_from_iter(all_sprint_ids.iter()),
                        )
                        .context("failed to purge pr_mutation for --force")?;
                    db.conn
                        .execute(
                            &format!(
                                "DELETE FROM compilation_failure_summary WHERE sprint_id IN ({phs})"
                            ),
                            rusqlite::params_from_iter(all_sprint_ids.iter()),
                        )
                        .context("failed to purge compilation_failure_summary for --force")?;
                    info!(
                        sprints = all_sprint_ids.len(),
                        "purged stale compilation rows for --force recompile"
                    );
                }
                sprint_grader_compile::check_compilations_parallel(
                    &db.conn,
                    &all_sprint_ids,
                    &entregues_dir,
                    &profiles,
                    config.build.max_parallel_builds as usize,
                    config.build.stderr_max_chars as usize,
                    skip_tested,
                    config.mutation.enabled,
                    pr_filter,
                    skip_recent_within,
                )
                .context("compile failed")?;
                // Compilation summary is sprint-scoped; run it for each.
                for sid in &all_sprint_ids {
                    sprint_grader_compile::summarize_compilation(&db.conn, *sid)
                        .with_context(|| format!("compile summary failed for sprint_id {sid}"))?;
                }
                // Recompute compile-dependent flags so that PRs that now
                // pass (or newly fail) are reflected immediately without
                // requiring a full `analyze` re-run.
                if force {
                    for sid in &all_sprint_ids {
                        sprint_grader_analyze::redetect_compile_flags_for_sprint_id(&db.conn, *sid)
                            .with_context(|| {
                                format!("compile flag redetection failed for sprint_id {sid}")
                            })?;
                    }
                }
                // Kill gradle daemons spawned during this run. The pre-run sweep
                // handles daemons from prior runs; this handles ones started now.
                sprint_grader_orchestration::pipeline::sweep_pre_compile_state(&entregues_dir);
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
        Command::Flags { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for sid in groups.iter().flat_map(|g| g.sprint_ids.iter().copied()) {
                let n = sprint_grader_analyze::flags::detect_flags_for_sprint_id(
                    &db.conn, sid, &config,
                )
                .with_context(|| format!("flags failed for sprint_id {sid}"))?;
                info!(sprint_id = sid, flags = n, "flags recomputed");
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
            // Scope trajectory recompute to the projects we just touched
            // (or the whole DB when no filter is provided).
            let project_ids: Option<Vec<i64>> = if filter.is_some() {
                Some(groups.iter().map(|g| g.project_id).collect())
            } else {
                None
            };
            sprint_grader_analyze::compute_all_trajectories_filtered(
                &db.conn,
                &config.detector_thresholds,
                project_ids.as_deref(),
            )
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
        Command::StaticAnalysis {
            projects,
            no_spotbugs,
        } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            let rules_path = config_dir.join("static_analysis.toml");
            let mut rules =
                sprint_grader_static_analysis::Rules::load(&rules_path).with_context(|| {
                    format!(
                        "loading {} (this subcommand requires the file to exist; \
                         the orchestrated runs silently skip when it's absent)",
                        rules_path.display()
                    )
                })?;
            if no_spotbugs {
                rules.spotbugs.enabled = false;
            }
            // T-P3.4 PR 3: artifact-shape — one scan per project per
            // run. The CLI subcommand previously iterated sprint_ids;
            // findings are sprint-free now, so a single call suffices.
            for g in &groups {
                let project_root = entregues_dir.join(&g.name);
                sprint_grader_static_analysis::scan_project_to_db(&db.conn, &project_root, &rules)
                    .with_context(|| format!("static-analysis failed for project {}", g.name))?;
            }
        }
        Command::ComplexityScan { projects } => {
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for g in &groups {
                let project_root = entregues_dir.join(&g.name);
                for sid in &g.sprint_ids {
                    sprint_grader_quality::testability::scan_project_to_db(
                        &db.conn,
                        &project_root,
                        *sid,
                        g.project_id,
                        &config.detector_thresholds,
                    )
                    .with_context(|| format!("complexity scan failed for sprint_id {sid}"))?;
                }
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
            // Peer-group analysis is project-scoped now: one pass per
            // project across every sprint up to today, not per-sprint.
            let filter = parse_project_filter(projects.projects);
            let groups = resolve_all_sprint_tuples(&db, &today, filter.as_deref())?;
            for g in &groups {
                sprint_grader_repo_analysis::compute_task_similarity(
                    &db.conn,
                    g.project_id,
                    &g.sprint_ids,
                    &config.repo_analysis,
                )
                .with_context(|| {
                    format!(
                        "task_similarity failed for project_id {} ({})",
                        g.project_id, g.name
                    )
                })?;
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
        Command::Report {
            projects,
            push,
            professor_report,
        } => {
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
            let mut repo_reports: std::collections::BTreeMap<PathBuf, Vec<PathBuf>> =
                std::collections::BTreeMap::new();
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
                repo_reports
                    .entry(repo_root.clone())
                    .or_default()
                    .push(report_path);

                if professor_report {
                    // Parent of the android repo — `data/entregues/<project>/`
                    // by convention. Stays OUT of the android repo's git
                    // tree so the per-student attribution + flag rendering
                    // never gets pushed to the team's branch.
                    let prof_path = repo_root
                        .parent()
                        .unwrap_or(&repo_root)
                        .join("REPORT_PROFESSOR.md");
                    sprint_grader_report::generate_markdown_report_multi_to_path_with_opts(
                        &db.conn,
                        g.project_id,
                        &g.name,
                        &g.sprint_ids,
                        &prof_path,
                        sprint_grader_report::MultiReportOptions {
                            include_static_analysis: true,
                            professor_view: true,
                        },
                    )
                    .with_context(|| {
                        format!("Professor report failed for {}", prof_path.display())
                    })?;
                    info!(path = %prof_path.display(), "professor report written");
                }
            }

            if push {
                let mut published_repos = 0usize;
                for (repo_root, report_paths) in &repo_reports {
                    if !repo_has_report_changes(repo_root, report_paths)
                        .with_context(|| format!("git status failed for {}", repo_root.display()))?
                    {
                        continue;
                    }
                    publish_report_updates(repo_root, report_paths)
                        .with_context(|| format!("publish failed for {}", repo_root.display()))?;
                    published_repos += 1;
                }
                info!(published_repos, "reports pushed");
            }

            info!(
                output = %entregues_dir.display(),
                projects = groups.len(),
                today = %today,
                "reports generated"
            );
        }
        Command::SyncReports {
            projects,
            push,
            skip_collect,
        } => {
            drop(db);
            let result = sprint_grader_orchestration::sync_reports_through_sprint(
                &config,
                &db_path,
                &entregues_dir,
                &sprint_grader_orchestration::SyncReportsOptions {
                    today: today.clone(),
                    project_filter: parse_project_filter(projects.projects),
                    push,
                    skip_collect,
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
            skip_static_analysis,
            force_pr_refresh,
            reports,
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
                skip_static_analysis,
                skip_arch_llm: false,
                force_pr_refresh,
                max_workers: None,
                team_reports: reports,
            };
            sprint_grader_orchestration::run_pipeline(
                &config,
                &db_path,
                sprint_grader_orchestration::PipelineVariant::RunAll,
                &opts,
            )
            .context("run-all pipeline failed")?;
        }
        Command::Iterate {
            projects,
            skip_github,
            skip_repos,
            skip_static_analysis,
            force_pr_refresh,
            reports,
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
                skip_static_analysis,
                skip_arch_llm: true,
                force_pr_refresh,
                max_workers: None,
                team_reports: reports,
            };
            sprint_grader_orchestration::run_pipeline(
                &config,
                &db_path,
                sprint_grader_orchestration::PipelineVariant::RunAll,
                &opts,
            )
            .context("iterate pipeline failed")?;
        }
        Command::Go {
            projects,
            skip_github,
            skip_repos,
            skip_perplexity: _,
            skip_llm_judge: _,
            skip_static_analysis,
            force_pr_refresh,
            dry_run,
            require_clean_tree,
            reports,
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
                skip_static_analysis,
                skip_arch_llm: false,
                force_pr_refresh,
                max_workers: None,
                team_reports: reports,
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
            run_static_analysis,
            force_pr_refresh,
            dry_run,
            require_clean_tree,
            reports,
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
                // go-quick skips static analysis by default; --run-static-analysis
                // overrides. Mirrors how go-quick skips the LLM judge by default.
                skip_static_analysis: !run_static_analysis,
                skip_arch_llm: false,
                force_pr_refresh,
                max_workers: None,
                team_reports: reports,
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
            let normalized = stack.trim().to_lowercase();
            let rel = if matches!(normalized.as_str(), "spring" | "java-spring" | "backend")
                || normalized.contains("spring")
            {
                &config.architecture.spring_rubric_path
            } else if matches!(normalized.as_str(), "android" | "java-android" | "mobile")
                || normalized.contains("android")
            {
                &config.architecture.android_rubric_path
            } else {
                anyhow::bail!(
                    "unknown stack '{}' — expected 'spring' / 'android' (or an alias like 'backend' / 'mobile')",
                    stack
                );
            };
            let path = config_dir.join(rel);
            if !path.is_file() {
                anyhow::bail!(
                    "architecture rubric not found at {} — write the rubric first or pass a different --project-root",
                    path.display()
                );
            }
            let rubric = sprint_grader_architecture::rubric::load(&path)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            println!("{}", rubric.body);
            println!();
            println!("version={} body_hash={}", rubric.version, rubric.body_hash);
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
