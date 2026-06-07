//! Map `repo_full_name` + project layout to on-disk clone paths.

use std::path::{Path, PathBuf};

/// Local clone directory: `{entregues}/{project}/{repo_short_name}/`.
pub fn local_repo_dir(entregues_dir: &Path, project_name: &str, repo_full_name: &str) -> PathBuf {
    let repo_short = repo_full_name
        .rsplit('/')
        .next()
        .unwrap_or(repo_full_name);
    entregues_dir.join(project_name).join(repo_short)
}

pub fn local_file_path(
    entregues_dir: &Path,
    project_name: &str,
    repo_full_name: &str,
    file_path: &str,
) -> PathBuf {
    local_repo_dir(entregues_dir, project_name, repo_full_name).join(file_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_github_full_name_to_short_dir() {
        let p = local_repo_dir(
            Path::new("/data/entregues"),
            "team-01",
            "udg-pds/spring-foo",
        );
        assert_eq!(p, Path::new("/data/entregues/team-01/spring-foo"));
    }
}
