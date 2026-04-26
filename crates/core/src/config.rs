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
}

#[derive(Debug, Default, Deserialize)]
struct RawGrading {
    #[serde(default)]
    hidden_thresholds: bool,
    #[serde(default)]
    jitter_pct: Option<f64>,
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
    #[serde(default = "default_low_survival_absolute_floor")]
    low_survival_absolute_floor: f64,
    #[serde(default = "default_raw_normalized_divergence_threshold")]
    raw_normalized_divergence_threshold: f64,
}

fn default_low_survival_rate_stddev() -> f64 {
    1.5
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
}

fn default_working_dir() -> String {
    ".".to_string()
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
