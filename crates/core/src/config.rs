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
    pub repo_analysis: RepoAnalysisConfig,
    pub build_profiles: Vec<BuildProfile>,
    pub build: BuildConfig,
    pub regularity: RegularityConfig,
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
    pub low_survival_rate_stddev: f64,
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
    #[serde(default = "default_low_survival_rate_stddev")]
    low_survival_rate_stddev: f64,
    #[serde(default = "default_raw_normalized_divergence_threshold")]
    raw_normalized_divergence_threshold: f64,
}

fn default_low_survival_rate_stddev() -> f64 {
    1.5
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
}

fn default_working_dir() -> String {
    ".".to_string()
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
            low_survival_rate_stddev: raw.thresholds.low_survival_rate_stddev,
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

        let build_profiles = raw
            .build
            .profiles
            .into_iter()
            .map(|p| BuildProfile {
                repo_pattern: p.repo_pattern,
                command: p.command,
                timeout_seconds: p.timeout_seconds,
                working_dir: p.working_dir,
                env: p.env,
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
            repo_analysis,
            build_profiles,
            build,
            regularity,
        })
    }
}
