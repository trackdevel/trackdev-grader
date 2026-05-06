use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub course_name: String,
    pub num_sprints: u32,
    pub pm_base_url: String,
    pub github_org: String,
    pub course_id: u32,
    pub claude_scripts_path: String,
    pub thresholds: ThresholdConfig,
    pub trackdev_token: String,
    pub github_token: String,
    pub sprints: HashMap<u32, SprintDateRange>,
    pub teams: Vec<TeamConfig>,
    pub curriculum_slides_dir: Option<PathBuf>,
    pub curriculum_extra_imports: Vec<String>,
    pub curriculum_template_repos: HashMap<String, PathBuf>,
    /// When true, the pipeline auto-freezes the curriculum snapshot
    /// (`curriculum_concepts_snapshot`) for any sprint whose `end_date` is
    /// already in the past. Default false. T-P2.5.
    pub curriculum_freeze_after_sprint_end: bool,
    pub repo_analysis: RepoAnalysisConfig,
    pub build_profiles: Vec<BuildProfile>,
    pub build: BuildConfig,
    pub regularity: RegularityConfig,
    pub detector_thresholds: DetectorThresholdsConfig,
    pub grading: GradingConfig,
    pub mutation: MutationConfig,
    pub architecture: ArchitectureConfig,
    pub evaluate: EvaluateConfig,
}

/// PR-doc / task-description LLM evaluation config. Mirrors the
/// architecture LLM dispatcher: `judge = "claude-cli"` (default) shells
/// out to the local Claude Code CLI per call (no API key required;
/// uses the user's Claude.ai subscription); `judge = "anthropic-api"`
/// uses the direct Anthropic SDK and requires `ANTHROPIC_API_KEY`.
/// Missing prerequisites for the selected backend fall back to the
/// deterministic heuristic so a missing CLI binary doesn't hard-fail.
///
/// `model_id` is REQUIRED in `course.toml` — there is no default. This
/// is a deliberate guard against the historical bug where the CLI judge
/// silently fell back to whatever model the user's Claude session
/// pointed at (Opus on Max plans), draining quota.
#[derive(Debug, Clone)]
pub struct EvaluateConfig {
    /// Backend selector. One of `"claude-cli"` (default),
    /// `"anthropic-api"`, or `"deepseek-api"`.
    pub judge: String,
    /// Pinned model id (e.g. `claude-haiku-4-5-20251001`). REQUIRED in
    /// course.toml; passed verbatim to the CLI via `--model` and to the
    /// API as the `model` field. Also part of the cache key.
    pub model_id: String,
    /// Number of concurrent CLI / API invocations. Each `claude-cli`
    /// worker spawns a process, so be conservative on the user's
    /// subscription rate limits. Default 4.
    pub judge_workers: usize,
    /// Per-call subprocess timeout (seconds). Default 180.
    pub judge_timeout_seconds: u64,
    /// Path to the Claude Code CLI binary. Default `"claude"`
    /// (resolved against `$PATH`).
    pub claude_cli_path: String,
    /// DeepSeek-only: V4 `thinking` mode. `Some("enabled")` or
    /// `Some("disabled")` is forwarded as `{"thinking": {"type": ...}}`
    /// in the request body; `None` lets DeepSeek apply its server-side
    /// default. Ignored by `claude-cli` and `anthropic-api` backends.
    pub thinking: Option<String>,
}

/// Sentinel returned by [`EvaluateConfig::default`]. It deliberately
/// holds an empty `model_id`; callers MUST either populate `model_id`
/// from `course.toml` (the production path in `Config::load`) or set it
/// explicitly. Any code that ships `EvaluateConfig::default()` straight
/// to a Claude invocation without overriding `model_id` will fail at
/// the build_argv-style assertions or be rejected by the API.
impl Default for EvaluateConfig {
    fn default() -> Self {
        Self {
            judge: "claude-cli".to_string(),
            model_id: String::new(),
            judge_workers: 4,
            judge_timeout_seconds: 180,
            claude_cli_path: "claude".to_string(),
            thinking: None,
        }
    }
}

/// Architecture-conformance LLM config (T-P3.3). The structural and AST
/// scans (T-P2.2 / T-P3.1) always run when `architecture.toml` exists;
/// the LLM judge is opt-in. The judge backend is selectable: `claude-cli`
/// (the local Claude Code CLI; the default — no API key needed, uses
/// the user's subscription), `anthropic-api` (direct API; requires
/// `ANTHROPIC_API_KEY`), or `deepseek-api` (DeepSeek chat completions;
/// requires `DEEPSEEK_API_KEY`). Missing prerequisites fall back to a
/// silent skip — running without an LLM is a supported mode.
#[derive(Debug, Clone)]
pub struct ArchitectureConfig {
    /// When true AND the selected judge's prerequisites are met, the
    /// pipeline runs the per-file LLM rubric judge (T-P3.3). Default
    /// false.
    pub llm_review: bool,
    /// Which judge backend to use. `"claude-cli"` (default) shells out
    /// to the `claude` binary; `"anthropic-api"` uses the direct
    /// Anthropic SDK and requires `ANTHROPIC_API_KEY`; `"deepseek-api"`
    /// uses DeepSeek chat completions and requires `DEEPSEEK_API_KEY`.
    pub judge: String,
    pub model_id: String,
    pub max_tokens: u32,
    /// Path to the markdown rubric, relative to `config/`. Default
    /// `architecture.md` (T-P3.2).
    pub rubric_path: String,
    /// Files matching any of these globs are skipped before the LLM
    /// call (generated code, build outputs, R.java, etc).
    pub llm_skip_globs: Vec<String>,
    /// Number of concurrent judge invocations. For `claude-cli` each
    /// worker spawns a process, so be conservative on the user's
    /// subscription rate limits. Default 1.
    pub judge_workers: usize,
    /// Per-file judge timeout (seconds). Caps both API calls and CLI
    /// invocations. Default 180.
    pub judge_timeout_seconds: u64,
    /// Path to the Claude Code CLI binary. Default `"claude"` (resolved
    /// against `$PATH`); override if the CLI is in a non-standard
    /// location.
    pub claude_cli_path: String,
    /// DeepSeek-only: V4 `thinking` mode. See [`EvaluateConfig::thinking`].
    pub thinking: Option<String>,
}

/// Sentinel returned by [`ArchitectureConfig::default`]. It deliberately
/// holds an empty `model_id`; callers MUST either populate `model_id`
/// from `course.toml [architecture]` (the production path in
/// `Config::load`) or set it explicitly. There is no "default model" to
/// fall back to — see the comment on `EvaluateConfig::default`.
impl Default for ArchitectureConfig {
    fn default() -> Self {
        Self {
            llm_review: false,
            judge: "claude-cli".to_string(),
            model_id: String::new(),
            max_tokens: 1024,
            rubric_path: "architecture.md".to_string(),
            llm_skip_globs: vec![
                "**/build/**".to_string(),
                "**/generated/**".to_string(),
                "**/R.java".to_string(),
                "**/*$$*.java".to_string(),
            ],
            judge_workers: 1,
            judge_timeout_seconds: 180,
            claude_cli_path: "claude".to_string(),
            thinking: None,
        }
    }
}

/// Mutation-testing config (T-P2.4). The actual mutation command lives
/// per-profile on `BuildProfile.mutation_command`; this block holds the
/// global on/off switch and the LOW_MUTATION_SCORE detector
/// thresholds. Default `enabled = false` so existing courses don't
/// accidentally pay the mutation-testing tax.
#[derive(Debug, Clone, Copy)]
pub struct MutationConfig {
    pub enabled: bool,
    /// LOW_MUTATION_SCORE fires INFO when mutation_score is below this
    /// (default 0.50).
    pub info_threshold: f64,
    /// LOW_MUTATION_SCORE escalates to WARNING below this threshold
    /// (default 0.30).
    pub warning_threshold: f64,
}

impl Default for MutationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            info_threshold: 0.50,
            warning_threshold: 0.30,
        }
    }
}

/// Anti-gaming config (T-P2.6). When `hidden_thresholds = true`, every
/// fractional detector knob is uniformly jittered by `± jitter_pct` at
/// pipeline start, seeded by `(today, course_id)` so the same `--today`
/// reproduces. Default `hidden_thresholds = false` keeps the original
/// fixed-threshold behaviour.
#[derive(Debug, Clone, Copy)]
pub struct GradingConfig {
    pub hidden_thresholds: bool,
    pub jitter_pct: f64,
}

impl Default for GradingConfig {
    fn default() -> Self {
        Self {
            hidden_thresholds: false,
            jitter_pct: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SprintDateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

#[derive(Debug, Clone)]
pub struct ThresholdConfig {
    pub carrying_team_pct: f64,
    pub cramming_hours: u32,
    pub cramming_commit_pct: f64,
    pub single_commit_dump_lines: u32,
    pub micro_pr_max_lines: u32,
    pub low_doc_score: u32,
    pub contribution_imbalance_stddev: f64,
    pub contribution_imbalance_min_abs_deviation: f64,
    pub low_survival_rate_stddev: f64,
    pub low_survival_absolute_floor: f64,
    pub raw_normalized_divergence_threshold: f64,
}

#[derive(Debug, Clone)]
pub struct TeamConfig {
    pub id: String,
    pub name: String,
    pub pm_project_id: String,
    pub repos: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RepoAnalysisConfig {
    pub enable_task_similarity: bool,
    pub enable_temporal_analysis: bool,
    pub quality_eval_tasks: bool,
    pub max_clusters_per_task: u32,
    pub outlier_points_stddev: f64,
    pub outlier_lar_stddev: f64,
    pub group_min_size: u32,
    pub mad_k_threshold: f64,
    pub cosmetic_share_threshold: f64,
    pub temporal_early_hours: f64,
    pub temporal_moderate_hours: f64,
    pub temporal_late_hours: f64,
}

impl Default for RepoAnalysisConfig {
    fn default() -> Self {
        Self {
            enable_task_similarity: true,
            enable_temporal_analysis: true,
            quality_eval_tasks: false,
            max_clusters_per_task: 3,
            outlier_points_stddev: 2.0,
            outlier_lar_stddev: 1.5,
            group_min_size: 4,
            mad_k_threshold: 2.5,
            cosmetic_share_threshold: 0.5,
            temporal_early_hours: 96.0,
            temporal_moderate_hours: 72.0,
            temporal_late_hours: 48.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuildProfile {
    pub repo_pattern: String,
    pub command: String,
    pub timeout_seconds: u64,
    pub working_dir: String,
    pub env: HashMap<String, String>,
    /// T-P2.4: when set, run after a successful primary build to mutate
    /// only the lines changed by the PR. Typically `./gradlew pitest
    /// --info` for the Pitest Gradle plugin in `scmMutationCoverage`
    /// mode. `None` (the default) skips mutation testing for this
    /// profile silently.
    pub mutation_command: Option<String>,
    /// T-P2.4: hard timeout for the mutation run. Mutation testing is
    /// substantially slower than the primary build, so this defaults
    /// higher than `timeout_seconds`.
    pub mutation_timeout_seconds: u64,
    /// T-P2.4: relative path (from `working_dir`) of the Pitest XML
    /// report. Defaults to Pitest's standard output location.
    pub mutation_report_path: String,
    /// Untracked files copied from the source repo into every
    /// freshly-created worktree before the build runs. Designed for
    /// per-team build secrets like `app/google-services.json` that are
    /// not committed to git but the build needs to succeed. `src` is
    /// resolved relative to the cloned source repo
    /// (`<entregues_dir>/<project>/<repo>/<src>`), so a single profile
    /// entry serves every team — each team's own file is found by
    /// convention. `dest` is relative to the worktree root.
    pub overlay_files: Vec<OverlayFile>,
}

#[derive(Debug, Clone)]
pub struct OverlayFile {
    pub src: String,
    pub dest: String,
}

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub max_parallel_builds: u32,
    pub stderr_max_chars: u32,
    pub skip_already_tested: bool,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            max_parallel_builds: 5,
            stderr_max_chars: 2000,
            skip_already_tested: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegularityConfig {
    pub midpoint_hours: f64,
    pub steepness: f64,
    pub excellent_threshold: f64,
    pub good_threshold: f64,
    pub late_threshold: f64,
    pub cramming_threshold: f64,
    pub after_deadline_score: f64,
}

impl Default for RegularityConfig {
    fn default() -> Self {
        Self {
            midpoint_hours: 24.0,
            steepness: 0.15,
            excellent_threshold: 0.85,
            good_threshold: 0.50,
            late_threshold: 0.20,
            cramming_threshold: 0.08,
            after_deadline_score: 0.0,
        }
    }
}

/// Detector tuning knobs migrated out of `flags.rs::DETECTOR_DEFAULTS` and the
/// few remaining bare literals in `flags.rs` and `trajectory.rs`. Default
/// values are byte-identical with the prior literals (T-P1.3).
#[derive(Debug, Clone, Copy)]
pub struct DetectorThresholdsConfig {
    pub gini_warn: f64,
    pub gini_crit: f64,
    pub composite_warn: f64,
    pub composite_crit: f64,
    pub late_regularity: f64,
    pub team_inequality_outlier_deviation: f64,
    pub trajectory_cv_low: f64,
    pub trajectory_cv_high: f64,
    pub trajectory_slope_p_value: f64,
    pub regularity_declining_delta: f64,
    pub cosmetic_rewrite_pct_of_lat: f64,
    pub bulk_rename_adds_dels_ratio: f64,
    pub bulk_rename_line_floor: i64,
    /// `ARCHITECTURE_HOTSPOT` (T-P3.1): a student fires when their summed
    /// blame-attribution weight across the sprint's `architecture_violations`
    /// rows is at or above this value. 1.0 ≈ "owns one full violation"; the
    /// default 2.0 picks up students who own multiple violations or a clear
    /// majority share of one severe span.
    pub architecture_hotspot_min_weighted: f64,
    /// `STATIC_ANALYSIS_HOTSPOT` (T-SA): per-student companion to the
    /// PMD/Checkstyle/SpotBugs scan. Same semantics as
    /// `architecture_hotspot_min_weighted` but counted against
    /// `static_analysis_finding_attribution`. Default 10.0 keeps the flag
    /// effectively silent — phase-1 sign-off was "feedback only", and
    /// `REPORT.md` is committed back to team repos. Lower this in
    /// `course.toml [detector_thresholds]` when ready to grade.
    pub static_analysis_hotspot_min_weighted: f64,
    /// `COMPLEXITY_HOTSPOT` (T-CX) cyclomatic-complexity ceilings per method.
    /// Above `_warn` the rule fires WARNING; above `_crit` it fires CRITICAL.
    pub complexity_cc_warn: f64,
    pub complexity_cc_crit: f64,
    /// `COMPLEXITY_HOTSPOT` cognitive-complexity ceilings.
    pub complexity_cognitive_warn: f64,
    pub complexity_cognitive_crit: f64,
    /// `COMPLEXITY_HOTSPOT` max nesting-depth ceilings.
    pub complexity_nesting_warn: f64,
    pub complexity_nesting_crit: f64,
    /// `COMPLEXITY_HOTSPOT` long-method (LOC) ceilings.
    pub complexity_loc_warn: f64,
    pub complexity_loc_crit: f64,
    /// `COMPLEXITY_HOTSPOT` wide-signature (parameter count) ceilings.
    pub complexity_params_warn: f64,
    pub complexity_params_crit: f64,
    /// `COMPLEXITY_HOTSPOT` flag thresholds. The per-student score is
    /// `Σ over findings: weight * severity_rank(rule.severity)` where rank
    /// is critical=3, warning=2, info=1 (matches `severity_rank` in
    /// `flags.rs`). Above `_warn` the flag fires WARNING; above `_crit`
    /// CRITICAL.
    pub complexity_hotspot_warn: f64,
    pub complexity_hotspot_crit: f64,
}

impl Default for DetectorThresholdsConfig {
    fn default() -> Self {
        Self {
            gini_warn: 0.35,
            gini_crit: 0.50,
            composite_warn: 0.20,
            composite_crit: 0.10,
            late_regularity: 0.20,
            team_inequality_outlier_deviation: 0.35,
            trajectory_cv_low: 0.20,
            trajectory_cv_high: 0.40,
            trajectory_slope_p_value: 0.15,
            regularity_declining_delta: -0.30,
            cosmetic_rewrite_pct_of_lat: 0.05,
            bulk_rename_adds_dels_ratio: 0.8,
            bulk_rename_line_floor: 50,
            architecture_hotspot_min_weighted: 2.0,
            // Phase-1 default: feedback only. The flag rarely fires until
            // an instructor lowers this knob in course.toml.
            static_analysis_hotspot_min_weighted: 10.0,
            complexity_cc_warn: 10.0,
            complexity_cc_crit: 15.0,
            complexity_cognitive_warn: 15.0,
            complexity_cognitive_crit: 20.0,
            complexity_nesting_warn: 4.0,
            complexity_nesting_crit: 6.0,
            complexity_loc_warn: 60.0,
            complexity_loc_crit: 100.0,
            complexity_params_warn: 5.0,
            complexity_params_crit: 8.0,
            complexity_hotspot_warn: 4.0,
            complexity_hotspot_crit: 8.0,
        }
    }
}

// --- Raw TOML deserialization structs (internal) ---

#[derive(Debug, Deserialize)]
struct RawConfig {
    course: RawCourse,
    thresholds: RawThresholds,
    #[serde(default)]
    sprints: HashMap<String, RawSprintRange>,
    #[serde(default)]
    teams: Vec<RawTeam>,
    #[serde(default)]
    curriculum: RawCurriculum,
    #[serde(default)]
    repo_analysis: RawRepoAnalysis,
    #[serde(default)]
    build: RawBuild,
    #[serde(default)]
    regularity: Option<RawRegularity>,
    #[serde(default)]
    detector_thresholds: RawDetectorThresholds,
    #[serde(default)]
    grading: RawGrading,
    #[serde(default)]
    mutation: RawMutation,
    #[serde(default)]
    architecture: RawArchitecture,
    #[serde(default)]
    evaluate: RawEvaluate,
}

#[derive(Debug, Default, Deserialize)]
struct RawEvaluate {
    #[serde(default)]
    judge: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    judge_workers: Option<usize>,
    #[serde(default)]
    judge_timeout_seconds: Option<u64>,
    #[serde(default)]
    claude_cli_path: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawArchitecture {
    #[serde(default)]
    llm_review: bool,
    #[serde(default)]
    judge: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    rubric_path: Option<String>,
    #[serde(default)]
    llm_skip_globs: Option<Vec<String>>,
    #[serde(default)]
    judge_workers: Option<usize>,
    #[serde(default)]
    judge_timeout_seconds: Option<u64>,
    #[serde(default)]
    claude_cli_path: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawGrading {
    #[serde(default)]
    hidden_thresholds: bool,
    #[serde(default)]
    jitter_pct: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMutation {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    info_threshold: Option<f64>,
    #[serde(default)]
    warning_threshold: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawCourse {
    name: String,
    num_sprints: u32,
    pm_base_url: String,
    github_org: String,
    course_id: u32,
    #[serde(default)]
    claude_scripts_path: String,
}

#[derive(Debug, Deserialize)]
struct RawThresholds {
    carrying_team_pct: f64,
    cramming_hours: u32,
    cramming_commit_pct: f64,
    single_commit_dump_lines: u32,
    micro_pr_max_lines: u32,
    low_doc_score: u32,
    contribution_imbalance_stddev: f64,
    #[serde(default = "default_contribution_imbalance_min_abs_deviation")]
    contribution_imbalance_min_abs_deviation: f64,
    #[serde(default = "default_low_survival_rate_stddev")]
    low_survival_rate_stddev: f64,
    #[serde(default = "default_low_survival_absolute_floor")]
    low_survival_absolute_floor: f64,
    #[serde(default = "default_raw_normalized_divergence_threshold")]
    raw_normalized_divergence_threshold: f64,
}

fn default_low_survival_rate_stddev() -> f64 {
    1.5
}
fn default_contribution_imbalance_min_abs_deviation() -> f64 {
    0.05
}
fn default_low_survival_absolute_floor() -> f64 {
    0.85
}
fn default_raw_normalized_divergence_threshold() -> f64 {
    0.20
}

#[derive(Debug, Deserialize)]
struct RawSprintRange {
    start: String,
    end: String,
}

#[derive(Debug, Deserialize)]
struct RawTeam {
    id: String,
    name: String,
    pm_project_id: String,
    repos: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCurriculum {
    slides_dir: Option<PathBuf>,
    #[serde(default)]
    extra_allowed_imports: Vec<String>,
    android_template_repo: Option<PathBuf>,
    spring_template_repo: Option<PathBuf>,
    #[serde(default)]
    freeze_after_sprint_end: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RawRepoAnalysis {
    enable_task_similarity: Option<bool>,
    enable_temporal_analysis: Option<bool>,
    quality_eval_tasks: Option<bool>,
    max_clusters_per_task: Option<u32>,
    outlier_points_stddev: Option<f64>,
    outlier_lar_stddev: Option<f64>,
    group_min_size: Option<u32>,
    mad_k_threshold: Option<f64>,
    cosmetic_share_threshold: Option<f64>,
    temporal_early_hours: Option<f64>,
    temporal_moderate_hours: Option<f64>,
    temporal_late_hours: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawBuild {
    #[serde(default)]
    profiles: Vec<RawBuildProfile>,
    max_parallel_builds: Option<u32>,
    stderr_max_chars: Option<u32>,
    skip_already_tested: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawBuildProfile {
    repo_pattern: String,
    command: String,
    timeout_seconds: u64,
    #[serde(default = "default_working_dir")]
    working_dir: String,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    mutation_command: Option<String>,
    #[serde(default = "default_mutation_timeout_seconds")]
    mutation_timeout_seconds: u64,
    #[serde(default = "default_mutation_report_path")]
    mutation_report_path: String,
    #[serde(default)]
    overlay_files: Vec<RawOverlayFile>,
}

#[derive(Debug, Deserialize)]
struct RawOverlayFile {
    src: String,
    dest: String,
}

fn default_working_dir() -> String {
    ".".to_string()
}

fn default_mutation_timeout_seconds() -> u64 {
    600
}

fn default_mutation_report_path() -> String {
    "build/reports/pitest/mutations.xml".to_string()
}

/// Validate a `[architecture] thinking` / `[evaluate] thinking` value
/// from `course.toml`. The DeepSeek V4 API accepts only `"enabled"` or
/// `"disabled"`; anything else would be silently ignored by the server,
/// so we reject loudly at config-load instead of at request time. Empty
/// or missing → `None` (let server pick default).
fn parse_thinking_mode(raw: Option<String>, section: &str) -> Result<Option<String>> {
    let Some(v) = raw else {
        return Ok(None);
    };
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed {
        "enabled" | "disabled" => Ok(Some(trimmed.to_string())),
        other => Err(Error::ConfigInvalid(format!(
            "[{section}] thinking must be \"enabled\" or \"disabled\", got {other:?}"
        ))),
    }
}

/// Reject overlay paths that are absolute or contain a `..` segment.
/// `src` is resolved against the source repo root and `dest` against
/// the worktree root; both must stay inside their respective trees.
fn is_unsafe_overlay_path(p: &str) -> bool {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return true;
    }
    path.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

#[derive(Debug, Default, Deserialize)]
struct RawDetectorThresholds {
    gini_warn: Option<f64>,
    gini_crit: Option<f64>,
    composite_warn: Option<f64>,
    composite_crit: Option<f64>,
    late_regularity: Option<f64>,
    team_inequality_outlier_deviation: Option<f64>,
    trajectory_cv_low: Option<f64>,
    trajectory_cv_high: Option<f64>,
    trajectory_slope_p_value: Option<f64>,
    regularity_declining_delta: Option<f64>,
    cosmetic_rewrite_pct_of_lat: Option<f64>,
    bulk_rename_adds_dels_ratio: Option<f64>,
    bulk_rename_line_floor: Option<i64>,
    architecture_hotspot_min_weighted: Option<f64>,
    static_analysis_hotspot_min_weighted: Option<f64>,
    complexity_cc_warn: Option<f64>,
    complexity_cc_crit: Option<f64>,
    complexity_cognitive_warn: Option<f64>,
    complexity_cognitive_crit: Option<f64>,
    complexity_nesting_warn: Option<f64>,
    complexity_nesting_crit: Option<f64>,
    complexity_loc_warn: Option<f64>,
    complexity_loc_crit: Option<f64>,
    complexity_params_warn: Option<f64>,
    complexity_params_crit: Option<f64>,
    complexity_hotspot_warn: Option<f64>,
    complexity_hotspot_crit: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawRegularity {
    midpoint_hours: Option<f64>,
    steepness: Option<f64>,
    excellent_threshold: Option<f64>,
    good_threshold: Option<f64>,
    late_threshold: Option<f64>,
    cramming_threshold: Option<f64>,
    after_deadline_score: Option<f64>,
}

impl Config {
    /// Construct a `Config` populated with the same defaults that an empty
    /// `course.toml` would produce. Intended for tests in dependent crates
    /// that exercise pipeline functions taking `&Config` without wanting to
    /// touch the filesystem.
    pub fn test_default() -> Self {
        Config {
            course_name: "test-course".to_string(),
            num_sprints: 4,
            pm_base_url: "https://example.test".to_string(),
            github_org: "example".to_string(),
            course_id: 1,
            claude_scripts_path: String::new(),
            thresholds: ThresholdConfig {
                carrying_team_pct: 0.40,
                cramming_hours: 48,
                cramming_commit_pct: 0.70,
                single_commit_dump_lines: 200,
                micro_pr_max_lines: 10,
                low_doc_score: 2,
                contribution_imbalance_stddev: 1.5,
                contribution_imbalance_min_abs_deviation: 0.05,
                low_survival_rate_stddev: 1.5,
                low_survival_absolute_floor: 0.85,
                raw_normalized_divergence_threshold: 0.20,
            },
            trackdev_token: String::new(),
            github_token: String::new(),
            sprints: HashMap::new(),
            teams: Vec::new(),
            curriculum_slides_dir: None,
            curriculum_extra_imports: Vec::new(),
            curriculum_template_repos: HashMap::new(),
            curriculum_freeze_after_sprint_end: false,
            repo_analysis: RepoAnalysisConfig::default(),
            build_profiles: Vec::new(),
            build: BuildConfig::default(),
            regularity: RegularityConfig::default(),
            detector_thresholds: DetectorThresholdsConfig::default(),
            grading: GradingConfig::default(),
            mutation: MutationConfig::default(),
            architecture: ArchitectureConfig {
                model_id: "claude-haiku-4-5-20251001".to_string(),
                ..ArchitectureConfig::default()
            },
            evaluate: EvaluateConfig {
                model_id: "claude-haiku-4-5-20251001".to_string(),
                ..EvaluateConfig::default()
            },
        }
    }

    pub fn load(config_dir: &Path) -> Result<Self> {
        let toml_path = config_dir.join("course.toml");
        if !toml_path.exists() {
            return Err(Error::ConfigMissing(toml_path));
        }
        let text = std::fs::read_to_string(&toml_path)?;
        let raw: RawConfig = toml::from_str(&text)?;

        let thresholds = ThresholdConfig {
            carrying_team_pct: raw.thresholds.carrying_team_pct,
            cramming_hours: raw.thresholds.cramming_hours,
            cramming_commit_pct: raw.thresholds.cramming_commit_pct,
            single_commit_dump_lines: raw.thresholds.single_commit_dump_lines,
            micro_pr_max_lines: raw.thresholds.micro_pr_max_lines,
            low_doc_score: raw.thresholds.low_doc_score,
            contribution_imbalance_stddev: raw.thresholds.contribution_imbalance_stddev,
            contribution_imbalance_min_abs_deviation: raw
                .thresholds
                .contribution_imbalance_min_abs_deviation,
            low_survival_rate_stddev: raw.thresholds.low_survival_rate_stddev,
            low_survival_absolute_floor: raw.thresholds.low_survival_absolute_floor,
            raw_normalized_divergence_threshold: raw.thresholds.raw_normalized_divergence_threshold,
        };

        let mut sprints = HashMap::new();
        for (key, val) in raw.sprints {
            let num: u32 = key
                .parse()
                .map_err(|_| Error::ConfigInvalid(format!("sprint key not an integer: {key}")))?;
            let start = NaiveDate::parse_from_str(&val.start, "%Y-%m-%d")
                .map_err(|e| Error::ConfigInvalid(format!("sprint {num} start: {e}")))?;
            let end = NaiveDate::parse_from_str(&val.end, "%Y-%m-%d")
                .map_err(|e| Error::ConfigInvalid(format!("sprint {num} end: {e}")))?;
            sprints.insert(num, SprintDateRange { start, end });
        }

        let teams = raw
            .teams
            .into_iter()
            .map(|t| TeamConfig {
                id: t.id,
                name: t.name,
                pm_project_id: t.pm_project_id,
                repos: t.repos,
            })
            .collect();

        let trackdev_token = std::env::var("TRACKDEV_TOKEN").unwrap_or_default();
        if trackdev_token.is_empty() {
            tracing::warn!("TRACKDEV_TOKEN not set — collection stage will fail");
        }
        let github_token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
        if github_token.is_empty() {
            tracing::warn!("GITHUB_TOKEN not set — GitHub API calls will fail");
        }

        let mut template_repos = HashMap::new();
        if let Some(p) = raw.curriculum.android_template_repo {
            template_repos.insert("android".to_string(), p);
        }
        if let Some(p) = raw.curriculum.spring_template_repo {
            template_repos.insert("spring".to_string(), p);
        }

        let ra_defaults = RepoAnalysisConfig::default();
        let repo_analysis = RepoAnalysisConfig {
            enable_task_similarity: raw
                .repo_analysis
                .enable_task_similarity
                .unwrap_or(ra_defaults.enable_task_similarity),
            enable_temporal_analysis: raw
                .repo_analysis
                .enable_temporal_analysis
                .unwrap_or(ra_defaults.enable_temporal_analysis),
            quality_eval_tasks: raw
                .repo_analysis
                .quality_eval_tasks
                .unwrap_or(ra_defaults.quality_eval_tasks),
            max_clusters_per_task: raw
                .repo_analysis
                .max_clusters_per_task
                .unwrap_or(ra_defaults.max_clusters_per_task),
            outlier_points_stddev: raw
                .repo_analysis
                .outlier_points_stddev
                .unwrap_or(ra_defaults.outlier_points_stddev),
            outlier_lar_stddev: raw
                .repo_analysis
                .outlier_lar_stddev
                .unwrap_or(ra_defaults.outlier_lar_stddev),
            group_min_size: raw
                .repo_analysis
                .group_min_size
                .unwrap_or(ra_defaults.group_min_size),
            mad_k_threshold: raw
                .repo_analysis
                .mad_k_threshold
                .unwrap_or(ra_defaults.mad_k_threshold),
            cosmetic_share_threshold: raw
                .repo_analysis
                .cosmetic_share_threshold
                .unwrap_or(ra_defaults.cosmetic_share_threshold),
            temporal_early_hours: raw
                .repo_analysis
                .temporal_early_hours
                .unwrap_or(ra_defaults.temporal_early_hours),
            temporal_moderate_hours: raw
                .repo_analysis
                .temporal_moderate_hours
                .unwrap_or(ra_defaults.temporal_moderate_hours),
            temporal_late_hours: raw
                .repo_analysis
                .temporal_late_hours
                .unwrap_or(ra_defaults.temporal_late_hours),
        };

        let build_defaults = BuildConfig::default();
        let build = BuildConfig {
            max_parallel_builds: raw
                .build
                .max_parallel_builds
                .unwrap_or(build_defaults.max_parallel_builds),
            stderr_max_chars: raw
                .build
                .stderr_max_chars
                .unwrap_or(build_defaults.stderr_max_chars),
            skip_already_tested: raw
                .build
                .skip_already_tested
                .unwrap_or(build_defaults.skip_already_tested),
        };

        let build_profiles: Vec<BuildProfile> = raw
            .build
            .profiles
            .into_iter()
            .map(|p| {
                let overlay_files = p
                    .overlay_files
                    .into_iter()
                    .filter_map(|o| {
                        // Reject path escapes at config-load time so a typo in
                        // course.toml can't read or write outside the worktree.
                        if is_unsafe_overlay_path(&o.src) || is_unsafe_overlay_path(&o.dest) {
                            tracing::warn!(
                                src = %o.src,
                                dest = %o.dest,
                                "rejecting overlay_files entry with absolute or .. path",
                            );
                            None
                        } else {
                            Some(OverlayFile {
                                src: o.src,
                                dest: o.dest,
                            })
                        }
                    })
                    .collect();
                BuildProfile {
                    repo_pattern: p.repo_pattern,
                    command: p.command,
                    timeout_seconds: p.timeout_seconds,
                    working_dir: p.working_dir,
                    env: p.env,
                    mutation_command: p.mutation_command,
                    mutation_timeout_seconds: p.mutation_timeout_seconds,
                    mutation_report_path: p.mutation_report_path,
                    overlay_files,
                }
            })
            .collect();

        let regularity_defaults = RegularityConfig::default();
        let regularity = match raw.regularity {
            Some(r) => RegularityConfig {
                midpoint_hours: r
                    .midpoint_hours
                    .unwrap_or(regularity_defaults.midpoint_hours),
                steepness: r.steepness.unwrap_or(regularity_defaults.steepness),
                excellent_threshold: r
                    .excellent_threshold
                    .unwrap_or(regularity_defaults.excellent_threshold),
                good_threshold: r
                    .good_threshold
                    .unwrap_or(regularity_defaults.good_threshold),
                late_threshold: r
                    .late_threshold
                    .unwrap_or(regularity_defaults.late_threshold),
                cramming_threshold: r
                    .cramming_threshold
                    .unwrap_or(regularity_defaults.cramming_threshold),
                after_deadline_score: r
                    .after_deadline_score
                    .unwrap_or(regularity_defaults.after_deadline_score),
            },
            None => regularity_defaults,
        };

        let detector_defaults = DetectorThresholdsConfig::default();
        let detector_thresholds = DetectorThresholdsConfig {
            gini_warn: raw
                .detector_thresholds
                .gini_warn
                .unwrap_or(detector_defaults.gini_warn),
            gini_crit: raw
                .detector_thresholds
                .gini_crit
                .unwrap_or(detector_defaults.gini_crit),
            composite_warn: raw
                .detector_thresholds
                .composite_warn
                .unwrap_or(detector_defaults.composite_warn),
            composite_crit: raw
                .detector_thresholds
                .composite_crit
                .unwrap_or(detector_defaults.composite_crit),
            late_regularity: raw
                .detector_thresholds
                .late_regularity
                .unwrap_or(detector_defaults.late_regularity),
            team_inequality_outlier_deviation: raw
                .detector_thresholds
                .team_inequality_outlier_deviation
                .unwrap_or(detector_defaults.team_inequality_outlier_deviation),
            trajectory_cv_low: raw
                .detector_thresholds
                .trajectory_cv_low
                .unwrap_or(detector_defaults.trajectory_cv_low),
            trajectory_cv_high: raw
                .detector_thresholds
                .trajectory_cv_high
                .unwrap_or(detector_defaults.trajectory_cv_high),
            trajectory_slope_p_value: raw
                .detector_thresholds
                .trajectory_slope_p_value
                .unwrap_or(detector_defaults.trajectory_slope_p_value),
            regularity_declining_delta: raw
                .detector_thresholds
                .regularity_declining_delta
                .unwrap_or(detector_defaults.regularity_declining_delta),
            cosmetic_rewrite_pct_of_lat: raw
                .detector_thresholds
                .cosmetic_rewrite_pct_of_lat
                .unwrap_or(detector_defaults.cosmetic_rewrite_pct_of_lat),
            bulk_rename_adds_dels_ratio: raw
                .detector_thresholds
                .bulk_rename_adds_dels_ratio
                .unwrap_or(detector_defaults.bulk_rename_adds_dels_ratio),
            bulk_rename_line_floor: raw
                .detector_thresholds
                .bulk_rename_line_floor
                .unwrap_or(detector_defaults.bulk_rename_line_floor),
            architecture_hotspot_min_weighted: raw
                .detector_thresholds
                .architecture_hotspot_min_weighted
                .unwrap_or(detector_defaults.architecture_hotspot_min_weighted),
            static_analysis_hotspot_min_weighted: raw
                .detector_thresholds
                .static_analysis_hotspot_min_weighted
                .unwrap_or(detector_defaults.static_analysis_hotspot_min_weighted),
            complexity_cc_warn: raw
                .detector_thresholds
                .complexity_cc_warn
                .unwrap_or(detector_defaults.complexity_cc_warn),
            complexity_cc_crit: raw
                .detector_thresholds
                .complexity_cc_crit
                .unwrap_or(detector_defaults.complexity_cc_crit),
            complexity_cognitive_warn: raw
                .detector_thresholds
                .complexity_cognitive_warn
                .unwrap_or(detector_defaults.complexity_cognitive_warn),
            complexity_cognitive_crit: raw
                .detector_thresholds
                .complexity_cognitive_crit
                .unwrap_or(detector_defaults.complexity_cognitive_crit),
            complexity_nesting_warn: raw
                .detector_thresholds
                .complexity_nesting_warn
                .unwrap_or(detector_defaults.complexity_nesting_warn),
            complexity_nesting_crit: raw
                .detector_thresholds
                .complexity_nesting_crit
                .unwrap_or(detector_defaults.complexity_nesting_crit),
            complexity_loc_warn: raw
                .detector_thresholds
                .complexity_loc_warn
                .unwrap_or(detector_defaults.complexity_loc_warn),
            complexity_loc_crit: raw
                .detector_thresholds
                .complexity_loc_crit
                .unwrap_or(detector_defaults.complexity_loc_crit),
            complexity_params_warn: raw
                .detector_thresholds
                .complexity_params_warn
                .unwrap_or(detector_defaults.complexity_params_warn),
            complexity_params_crit: raw
                .detector_thresholds
                .complexity_params_crit
                .unwrap_or(detector_defaults.complexity_params_crit),
            complexity_hotspot_warn: raw
                .detector_thresholds
                .complexity_hotspot_warn
                .unwrap_or(detector_defaults.complexity_hotspot_warn),
            complexity_hotspot_crit: raw
                .detector_thresholds
                .complexity_hotspot_crit
                .unwrap_or(detector_defaults.complexity_hotspot_crit),
        };

        Ok(Config {
            course_name: raw.course.name,
            num_sprints: raw.course.num_sprints,
            pm_base_url: raw.course.pm_base_url,
            github_org: raw.course.github_org,
            course_id: raw.course.course_id,
            claude_scripts_path: raw.course.claude_scripts_path,
            thresholds,
            trackdev_token,
            github_token,
            sprints,
            teams,
            curriculum_slides_dir: raw.curriculum.slides_dir,
            curriculum_extra_imports: raw.curriculum.extra_allowed_imports,
            curriculum_template_repos: template_repos,
            curriculum_freeze_after_sprint_end: raw.curriculum.freeze_after_sprint_end,
            repo_analysis,
            build_profiles,
            build,
            regularity,
            detector_thresholds,
            grading: GradingConfig {
                hidden_thresholds: raw.grading.hidden_thresholds,
                jitter_pct: raw.grading.jitter_pct.unwrap_or(0.0),
            },
            mutation: MutationConfig {
                enabled: raw.mutation.enabled,
                info_threshold: raw
                    .mutation
                    .info_threshold
                    .unwrap_or(MutationConfig::default().info_threshold),
                warning_threshold: raw
                    .mutation
                    .warning_threshold
                    .unwrap_or(MutationConfig::default().warning_threshold),
            },
            architecture: {
                let arch_defaults = ArchitectureConfig::default();
                let arch_model_id = raw.architecture.model_id.ok_or_else(|| {
                    Error::ConfigInvalid(
                        "[architecture] model_id is required in course.toml — pin to a \
                         specific id (e.g. \"claude-haiku-4-5-20251001\" for Anthropic, \
                         or \"deepseek-chat\" for DeepSeek). There is no default to \
                         prevent silently falling back to the user's Claude session \
                         model (Opus on Max plans) or to a backend-default that \
                         drifts under your feet."
                            .to_string(),
                    )
                })?;
                if arch_model_id.trim().is_empty() {
                    return Err(Error::ConfigInvalid(
                        "[architecture] model_id must not be empty".to_string(),
                    ));
                }
                let arch_thinking = parse_thinking_mode(raw.architecture.thinking, "architecture")?;
                ArchitectureConfig {
                    llm_review: raw.architecture.llm_review,
                    judge: raw.architecture.judge.unwrap_or(arch_defaults.judge),
                    model_id: arch_model_id,
                    max_tokens: raw
                        .architecture
                        .max_tokens
                        .unwrap_or(arch_defaults.max_tokens),
                    rubric_path: raw
                        .architecture
                        .rubric_path
                        .unwrap_or(arch_defaults.rubric_path),
                    llm_skip_globs: raw
                        .architecture
                        .llm_skip_globs
                        .unwrap_or(arch_defaults.llm_skip_globs),
                    judge_workers: raw
                        .architecture
                        .judge_workers
                        .unwrap_or(arch_defaults.judge_workers)
                        .max(1),
                    judge_timeout_seconds: raw
                        .architecture
                        .judge_timeout_seconds
                        .unwrap_or(arch_defaults.judge_timeout_seconds),
                    claude_cli_path: raw
                        .architecture
                        .claude_cli_path
                        .unwrap_or(arch_defaults.claude_cli_path),
                    thinking: arch_thinking,
                }
            },
            evaluate: {
                let eval_defaults = EvaluateConfig::default();
                let eval_model_id = raw.evaluate.model_id.ok_or_else(|| {
                    Error::ConfigInvalid(
                        "[evaluate] model_id is required in course.toml — pin to a \
                         specific id (e.g. \"claude-haiku-4-5-20251001\" for Anthropic, \
                         or \"deepseek-chat\" for DeepSeek). There is no default to \
                         prevent silently falling back to the user's Claude session \
                         model (Opus on Max plans) or to a backend-default that \
                         drifts under your feet."
                            .to_string(),
                    )
                })?;
                if eval_model_id.trim().is_empty() {
                    return Err(Error::ConfigInvalid(
                        "[evaluate] model_id must not be empty".to_string(),
                    ));
                }
                let eval_thinking = parse_thinking_mode(raw.evaluate.thinking, "evaluate")?;
                EvaluateConfig {
                    judge: raw.evaluate.judge.unwrap_or(eval_defaults.judge),
                    model_id: eval_model_id,
                    judge_workers: raw
                        .evaluate
                        .judge_workers
                        .unwrap_or(eval_defaults.judge_workers)
                        .max(1),
                    judge_timeout_seconds: raw
                        .evaluate
                        .judge_timeout_seconds
                        .unwrap_or(eval_defaults.judge_timeout_seconds),
                    claude_cli_path: raw
                        .evaluate
                        .claude_cli_path
                        .unwrap_or(eval_defaults.claude_cli_path),
                    thinking: eval_thinking,
                }
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[course]
name = "test-course"
num_sprints = 4
pm_base_url = "https://example.test"
github_org = "example"
course_id = 1

[thresholds]
carrying_team_pct = 0.4
cramming_hours = 48
cramming_commit_pct = 0.7
single_commit_dump_lines = 200
micro_pr_max_lines = 10
low_doc_score = 2
contribution_imbalance_stddev = 1.5

[architecture]
model_id = "claude-haiku-4-5-20251001"

[evaluate]
model_id = "claude-haiku-4-5-20251001"
"#;

    fn write_config(dir: &Path, body: &str) {
        std::fs::write(dir.join("course.toml"), body).unwrap();
    }

    #[test]
    fn detector_thresholds_default_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), MINIMAL_TOML);
        let cfg = Config::load(tmp.path()).expect("load minimal config");
        let dt = cfg.detector_thresholds;
        let defaults = DetectorThresholdsConfig::default();
        assert_eq!(dt.gini_warn, defaults.gini_warn);
        assert_eq!(dt.gini_crit, defaults.gini_crit);
        assert_eq!(dt.composite_warn, defaults.composite_warn);
        assert_eq!(dt.composite_crit, defaults.composite_crit);
        assert_eq!(dt.late_regularity, defaults.late_regularity);
        assert_eq!(
            dt.team_inequality_outlier_deviation,
            defaults.team_inequality_outlier_deviation
        );
        assert_eq!(dt.trajectory_cv_low, defaults.trajectory_cv_low);
        assert_eq!(dt.trajectory_cv_high, defaults.trajectory_cv_high);
        assert_eq!(
            dt.trajectory_slope_p_value,
            defaults.trajectory_slope_p_value
        );
        assert_eq!(
            dt.regularity_declining_delta,
            defaults.regularity_declining_delta
        );
        assert_eq!(
            dt.cosmetic_rewrite_pct_of_lat,
            defaults.cosmetic_rewrite_pct_of_lat
        );
        assert_eq!(
            dt.bulk_rename_adds_dels_ratio,
            defaults.bulk_rename_adds_dels_ratio
        );
        assert_eq!(dt.bulk_rename_line_floor, defaults.bulk_rename_line_floor);
    }

    #[test]
    fn curriculum_freeze_after_sprint_end_defaults_to_false() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), MINIMAL_TOML);
        let cfg = Config::load(tmp.path()).expect("load minimal config");
        assert!(!cfg.curriculum_freeze_after_sprint_end);
    }

    #[test]
    fn curriculum_freeze_after_sprint_end_can_be_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!("{MINIMAL_TOML}\n[curriculum]\nfreeze_after_sprint_end = true\n");
        write_config(tmp.path(), &body);
        let cfg = Config::load(tmp.path()).expect("load with freeze flag");
        assert!(cfg.curriculum_freeze_after_sprint_end);
    }

    #[test]
    fn build_profile_overlay_files_round_trip_and_path_safety() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!(
            "{MINIMAL_TOML}\n\
             [[build.profiles]]\n\
             repo_pattern = \"^android-\"\n\
             command = \"./gradlew assembleDebug\"\n\
             timeout_seconds = 300\n\
             overlay_files = [\n  \
                 {{ src = \"app/google-services.json\", dest = \"app/google-services.json\" }},\n  \
                 {{ src = \"local.properties\",         dest = \"local.properties\" }},\n  \
                 {{ src = \"../escape\",                dest = \"app/x\" }},\n  \
                 {{ src = \"app/y\",                    dest = \"/etc/passwd\" }},\n\
             ]\n",
        );
        write_config(tmp.path(), &body);
        let cfg = Config::load(tmp.path()).expect("load with overlay_files");
        let android = &cfg.build_profiles[0];
        // Two safe entries kept; the two unsafe entries (parent escape,
        // absolute dest) are dropped at config-load.
        assert_eq!(android.overlay_files.len(), 2);
        assert_eq!(android.overlay_files[0].src, "app/google-services.json");
        assert_eq!(android.overlay_files[0].dest, "app/google-services.json");
        assert_eq!(android.overlay_files[1].src, "local.properties");
    }

    /// `course.toml` MUST pin both judges' `model_id`. There is no
    /// silent fall-back to a default — the historical bug that draining
    /// Opus quota came from exactly such a fall-back. These four tests
    /// lock that contract in.
    const MINIMAL_NO_LLM_BLOCKS: &str = r#"
[course]
name = "test-course"
num_sprints = 4
pm_base_url = "https://example.test"
github_org = "example"
course_id = 1

[thresholds]
carrying_team_pct = 0.4
cramming_hours = 48
cramming_commit_pct = 0.7
single_commit_dump_lines = 200
micro_pr_max_lines = 10
low_doc_score = 2
contribution_imbalance_stddev = 1.5
"#;

    #[test]
    fn missing_architecture_model_id_rejects_with_clear_message() {
        let tmp = tempfile::tempdir().unwrap();
        // Has [evaluate] model_id but no [architecture] block at all.
        let body = format!("{MINIMAL_NO_LLM_BLOCKS}\n[evaluate]\nmodel_id = \"x\"\n");
        write_config(tmp.path(), &body);
        let err = Config::load(tmp.path()).expect_err("must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("[architecture] model_id is required"),
            "error must name the missing field: {msg}"
        );
    }

    #[test]
    fn missing_evaluate_model_id_rejects_with_clear_message() {
        let tmp = tempfile::tempdir().unwrap();
        // Has [architecture] model_id but no [evaluate] block at all.
        let body = format!("{MINIMAL_NO_LLM_BLOCKS}\n[architecture]\nmodel_id = \"x\"\n");
        write_config(tmp.path(), &body);
        let err = Config::load(tmp.path()).expect_err("must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("[evaluate] model_id is required"),
            "error must name the missing field: {msg}"
        );
    }

    #[test]
    fn empty_architecture_model_id_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!(
            "{MINIMAL_NO_LLM_BLOCKS}\n[architecture]\nmodel_id = \"\"\n[evaluate]\nmodel_id = \"x\"\n"
        );
        write_config(tmp.path(), &body);
        let err = Config::load(tmp.path()).expect_err("must reject empty");
        assert!(err
            .to_string()
            .contains("[architecture] model_id must not be empty"));
    }

    #[test]
    fn empty_evaluate_model_id_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!(
            "{MINIMAL_NO_LLM_BLOCKS}\n[architecture]\nmodel_id = \"x\"\n[evaluate]\nmodel_id = \"\"\n"
        );
        write_config(tmp.path(), &body);
        let err = Config::load(tmp.path()).expect_err("must reject empty");
        assert!(err
            .to_string()
            .contains("[evaluate] model_id must not be empty"));
    }

    #[test]
    fn deepseek_judge_round_trips_through_load() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!(
            "{MINIMAL_NO_LLM_BLOCKS}\n[architecture]\nllm_review = true\njudge = \"deepseek-api\"\nmodel_id = \"deepseek-chat\"\n[evaluate]\njudge = \"deepseek-api\"\nmodel_id = \"deepseek-chat\"\n"
        );
        write_config(tmp.path(), &body);
        let cfg = Config::load(tmp.path()).expect("load deepseek config");
        assert_eq!(cfg.architecture.judge, "deepseek-api");
        assert_eq!(cfg.architecture.model_id, "deepseek-chat");
        assert!(cfg.architecture.llm_review);
        assert_eq!(cfg.evaluate.judge, "deepseek-api");
        assert_eq!(cfg.evaluate.model_id, "deepseek-chat");
        assert!(cfg.architecture.thinking.is_none());
        assert!(cfg.evaluate.thinking.is_none());
    }

    #[test]
    fn deepseek_thinking_knob_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!(
            "{MINIMAL_NO_LLM_BLOCKS}\n\
             [architecture]\nllm_review = true\njudge = \"deepseek-api\"\nmodel_id = \"deepseek-v4-pro\"\nthinking = \"enabled\"\n\
             [evaluate]\njudge = \"deepseek-api\"\nmodel_id = \"deepseek-v4-flash\"\nthinking = \"disabled\"\n"
        );
        write_config(tmp.path(), &body);
        let cfg = Config::load(tmp.path()).expect("load deepseek thinking knob");
        assert_eq!(cfg.architecture.thinking.as_deref(), Some("enabled"));
        assert_eq!(cfg.evaluate.thinking.as_deref(), Some("disabled"));
    }

    #[test]
    fn deepseek_thinking_rejects_unknown_value() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!(
            "{MINIMAL_NO_LLM_BLOCKS}\n\
             [architecture]\nmodel_id = \"x\"\nthinking = \"sometimes\"\n\
             [evaluate]\nmodel_id = \"y\"\n"
        );
        write_config(tmp.path(), &body);
        let err = Config::load(tmp.path()).expect_err("must reject unknown mode");
        let msg = err.to_string();
        assert!(
            msg.contains("[architecture] thinking must be"),
            "error must name the rejected value: {msg}"
        );
    }

    #[test]
    fn detector_thresholds_override_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!(
            "{MINIMAL_TOML}\n[detector_thresholds]\ngini_warn = 0.42\ncomposite_crit = 0.05\nbulk_rename_line_floor = 100\nregularity_declining_delta = -0.10\n"
        );
        write_config(tmp.path(), &body);
        let cfg = Config::load(tmp.path()).expect("load overridden config");
        assert_eq!(cfg.detector_thresholds.gini_warn, 0.42);
        assert_eq!(cfg.detector_thresholds.composite_crit, 0.05);
        assert_eq!(cfg.detector_thresholds.bulk_rename_line_floor, 100);
        assert_eq!(cfg.detector_thresholds.regularity_declining_delta, -0.10);
        // Untouched keys keep defaults.
        let defaults = DetectorThresholdsConfig::default();
        assert_eq!(cfg.detector_thresholds.gini_crit, defaults.gini_crit);
    }
}
