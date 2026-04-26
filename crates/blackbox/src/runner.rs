//! Hermetic CLI runner (T-T0.3).
//!
//! Wraps `assert_cmd::Command::cargo_bin("sprint-grader")` so each
//! scenario invokes the binary against a temp `--project-root` and
//! `--data-dir`, with `TRACKDEV_TOKEN` / `GITHUB_TOKEN` /
//! `ANTHROPIC_API_KEY` cleared. The runner does *not* own the
//! tempdir — callers pass it in (typically the same one the
//! [`crate::Fixture`] wrote into).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// Default `course.toml` body — enough to satisfy `Config::load` for
/// scenarios that don't need a specific knob set. The android/spring
/// build profiles are included so the CLI doesn't reject `compile`
/// invocations for missing match.
pub const DEFAULT_COURSE_TOML: &str = r#"
[course]
name = "blackbox-course"
num_sprints = 4
pm_base_url = "https://example.test"
github_org = "udg-pds"
course_id = 999

[thresholds]
carrying_team_pct = 0.40
cramming_hours = 24
cramming_commit_pct = 0.50
single_commit_dump_lines = 200
micro_pr_max_lines = 10
low_doc_score = 2
contribution_imbalance_stddev = 1.5
low_survival_rate_stddev = 1.5
low_survival_absolute_floor = 0.85
raw_normalized_divergence_threshold = 0.20

[[build.profiles]]
repo_pattern = "^android-"
command = "true"
timeout_seconds = 30
working_dir = "."
env = {}

[[build.profiles]]
repo_pattern = "^spring-"
command = "true"
timeout_seconds = 30
working_dir = "."
env = {}

[build]
max_parallel_builds = 2
stderr_max_chars = 2000
skip_already_tested = true
"#;

#[derive(Debug, Clone)]
pub struct RunnerOutput {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl RunnerOutput {
    pub fn assert_success(self) -> Self {
        assert!(
            self.status.success(),
            "binary exited {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.status.code(),
            self.stdout,
            self.stderr
        );
        self
    }
}

pub struct Runner {
    project_root: PathBuf,
    data_dir: PathBuf,
}

impl Runner {
    /// Build a runner over an already-laid-out tempdir. Writes a
    /// default `course.toml` if one isn't already present, but leaves
    /// existing config files alone so scenarios can override.
    pub fn new(project_root: &Path, data_dir: &Path) -> Result<Self> {
        let cfg = project_root.join("config");
        std::fs::create_dir_all(&cfg)?;
        let toml = cfg.join("course.toml");
        if !toml.exists() {
            std::fs::write(&toml, DEFAULT_COURSE_TOML)
                .with_context(|| format!("writing {}", toml.display()))?;
        }
        std::fs::create_dir_all(data_dir)?;
        Ok(Self {
            project_root: project_root.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
        })
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Overwrite the active `course.toml` with the given body.
    pub fn write_course_toml(&self, body: &str) -> Result<()> {
        std::fs::write(self.project_root.join("config").join("course.toml"), body)?;
        Ok(())
    }

    /// Drop a file under `<project_root>/config/<name>`.
    pub fn write_config_file(&self, name: &str, body: &str) -> Result<()> {
        std::fs::write(self.project_root.join("config").join(name), body)?;
        Ok(())
    }

    pub fn entregues_dir(&self) -> PathBuf {
        self.data_dir.join("entregues")
    }

    pub fn db_path(&self) -> PathBuf {
        self.entregues_dir().join("grading.db")
    }

    /// Invoke the `sprint-grader` binary with the given subcommand
    /// args. Network env vars are cleared and `RUST_LOG` is forced to
    /// a quiet default.
    pub fn run(&self, args: &[&str]) -> Result<RunnerOutput> {
        let binary = assert_cmd::cargo::cargo_bin("sprint-grader");
        let mut cmd = Command::new(binary);
        cmd.arg("--project-root")
            .arg(&self.project_root)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .args(args)
            .env_remove("TRACKDEV_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .env("RUST_LOG", "warn");
        let out = cmd
            .output()
            .with_context(|| format!("spawning sprint-grader {args:?}"))?;
        Ok(RunnerOutput {
            status: out.status,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}
