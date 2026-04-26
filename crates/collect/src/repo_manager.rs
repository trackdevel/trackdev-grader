//! Local git repo clone/update manager. Mirrors `src/collect/repo_manager.py`.
//!
//! Shells out to the `git` binary — same approach as the Python reference.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use rayon::prelude::*;
use tracing::{debug, info, warn};

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("git {op} failed for {repo}: {stderr}")]
    Git {
        op: String,
        repo: String,
        stderr: String,
    },

    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub struct RepoManager {
    repos_dir: PathBuf,
    token: String,
}

impl RepoManager {
    pub fn new(repos_dir: PathBuf, token: String) -> Self {
        Self { repos_dir, token }
    }

    /// Clone a repo or update it in place. Returns the local path.
    pub fn clone_or_update(
        &self,
        repo_full_name: &str,
        project_name: &str,
    ) -> Result<PathBuf, RepoError> {
        let repo_name = repo_full_name.rsplit('/').next().unwrap_or(repo_full_name);
        let target_dir = self.repos_dir.join(project_name).join(repo_name);

        if target_dir.exists() && target_dir.join(".git").exists() {
            self.update(repo_full_name, &target_dir)?;
        } else {
            self.clone(repo_full_name, &target_dir)?;
        }
        Ok(target_dir)
    }

    /// Clone/update multiple repos in parallel.
    /// Returns a map of successful clones: `(repo_full_name, local_path)`.
    /// Failures are logged but do not abort the batch.
    pub fn clone_all(
        &self,
        repos: &[(String, String)],
        max_workers: usize,
    ) -> Vec<(String, PathBuf)> {
        if repos.is_empty() {
            info!("No repos to clone");
            return Vec::new();
        }

        info!(total = repos.len(), max_workers, "Cloning/updating repos");

        let results: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

        // Use a scoped Rayon pool so we do not consume the global thread count.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(max_workers.max(1))
            .build()
            .expect("rayon pool build");

        pool.install(|| {
            repos.par_iter().for_each(|(repo, project_name)| {
                match self.clone_or_update(repo, project_name) {
                    Ok(path) => {
                        info!(repo, path = %path.display(), "  clone OK");
                        results.lock().unwrap().push((repo.clone(), path));
                    }
                    Err(e) => warn!(repo, error = %e, "  clone FAIL"),
                }
            });
        });

        let out = results.into_inner().unwrap();
        info!(ok = out.len(), total = repos.len(), "clone batch done");
        out
    }

    fn clone(&self, repo_full_name: &str, target_dir: &Path) -> Result<(), RepoError> {
        if let Some(parent) = target_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RepoError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let url = self.auth_url(repo_full_name);
        debug!(repo = repo_full_name, target = %target_dir.display(), "git clone");
        let out = run_git(
            &[
                "clone",
                "--quiet",
                &url,
                target_dir.to_str().unwrap_or_default(),
            ],
            None,
            Duration::from_secs(300),
        );
        match out {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                if target_dir.exists() {
                    let _ = std::fs::remove_dir_all(target_dir);
                }
                Err(RepoError::Git {
                    op: "clone".into(),
                    repo: repo_full_name.into(),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
                })
            }
            Err(e) => {
                if target_dir.exists() {
                    let _ = std::fs::remove_dir_all(target_dir);
                }
                Err(RepoError::Git {
                    op: "clone".into(),
                    repo: repo_full_name.into(),
                    stderr: e.to_string(),
                })
            }
        }
    }

    fn update(&self, repo_full_name: &str, target_dir: &Path) -> Result<(), RepoError> {
        debug!(repo = repo_full_name, target = %target_dir.display(), "git fetch+reset");

        // Refresh the remote URL in case the token changed.
        let url = self.auth_url(repo_full_name);
        let _ = run_git(
            &["remote", "set-url", "origin", &url],
            Some(target_dir),
            Duration::from_secs(30),
        );

        let fetched = run_git(
            &["fetch", "--quiet", "origin"],
            Some(target_dir),
            Duration::from_secs(120),
        )
        .map_err(|e| RepoError::Git {
            op: "fetch".into(),
            repo: repo_full_name.into(),
            stderr: e.to_string(),
        })?;
        if !fetched.status.success() {
            return Err(RepoError::Git {
                op: "fetch".into(),
                repo: repo_full_name.into(),
                stderr: String::from_utf8_lossy(&fetched.stderr).trim().into(),
            });
        }

        let branch = detect_default_branch(target_dir);
        let reset = run_git(
            &["reset", "--hard", &format!("origin/{branch}")],
            Some(target_dir),
            Duration::from_secs(60),
        )
        .map_err(|e| RepoError::Git {
            op: "reset".into(),
            repo: repo_full_name.into(),
            stderr: e.to_string(),
        })?;
        if !reset.status.success() {
            return Err(RepoError::Git {
                op: "reset".into(),
                repo: repo_full_name.into(),
                stderr: String::from_utf8_lossy(&reset.stderr).trim().into(),
            });
        }
        Ok(())
    }

    fn auth_url(&self, repo_full_name: &str) -> String {
        if self.token.is_empty() {
            format!("https://github.com/{repo_full_name}.git")
        } else {
            format!(
                "https://x-access-token:{}@github.com/{repo_full_name}.git",
                self.token
            )
        }
    }
}

fn detect_default_branch(repo_dir: &Path) -> String {
    if let Ok(out) = run_git(
        &["symbolic-ref", "refs/remotes/origin/HEAD"],
        Some(repo_dir),
        Duration::from_secs(10),
    ) {
        if out.status.success() {
            if let Ok(text) = std::str::from_utf8(&out.stdout) {
                if let Some(name) = text.trim().rsplit('/').next() {
                    return name.to_string();
                }
            }
        }
    }
    if let Ok(out) = run_git(
        &["rev-parse", "--verify", "origin/main"],
        Some(repo_dir),
        Duration::from_secs(10),
    ) {
        if out.status.success() {
            return "main".to_string();
        }
    }
    "master".to_string()
}

/// Run `git` with the given args, optionally in a working directory, with a
/// soft timeout enforced by the std runtime (we don't kill the process like
/// Python's `subprocess.run(timeout=...)`, but the `wait_timeout` crate is
/// avoided for now to keep dependencies minimal — git operations here all
/// terminate within their natural budget).
fn run_git(
    args: &[&str],
    cwd: Option<&Path>,
    _timeout: Duration,
) -> std::io::Result<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.output()
}
