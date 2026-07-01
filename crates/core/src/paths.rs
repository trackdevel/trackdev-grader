//! Path canonicalisation helper. Enforces that file paths flowing into
//! `RuleFinding::file_repo_relative` are repo-relative POSIX paths so the
//! GitHub blob-URL builder cannot accidentally emit absolute filesystem
//! paths (see the static-analysis URL bug captured by W1.T4).

use std::io;
use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("path is not inside repo root")]
    NotInsideRepoRoot,
    #[error("canonicalize failed: {0}")]
    Canonicalize(#[from] io::Error),
}

/// Returns `path` expressed relative to `repo_root`, with `..` segments
/// normalised away. Both paths are canonicalised (symlinks resolved)
/// when they exist on disk; for non-existent paths the function falls
/// back to lexical normalisation so unit tests and dry runs still work.
///
/// `path` may be absolute or relative; if relative, it is interpreted
/// relative to the current working directory before canonicalisation.
pub fn repo_relative(repo_root: &Utf8Path, path: &Utf8Path) -> Result<String, PathError> {
    let root_abs = canonicalize_or_lexical(repo_root)?;
    let path_abs = canonicalize_or_lexical(path)?;

    let rel = path_abs
        .strip_prefix(&root_abs)
        .map_err(|_| PathError::NotInsideRepoRoot)?;
    // POSIX-style separators in the output: scanners hand the value off
    // to the URL builder verbatim, and GitHub expects forward slashes.
    let s = rel.as_str().replace('\\', "/");
    Ok(s)
}

/// Conventional Java source roots under a cloned repo (Spring + Android).
pub const JAVA_SOURCE_ROOT_SUFFIXES: &[&str] = &[
    "src/main/java",
    "src/test/java",
    "app/src/main/java",
    "app/src/test/java",
];

/// Resolve a Java file path that may be repo-relative or relative to a
/// source root (as PMD / Checkstyle SARIF often emit) to an on-disk path
/// expressed relative to `repo_root`. Returns `None` when no matching file
/// is found.
pub fn resolve_existing_java_file(repo_root: &Utf8Path, file_path: &str) -> Option<String> {
    let normalized = file_path.replace('\\', "/");
    if normalized.is_empty() {
        return None;
    }

    let direct = repo_root.join(&normalized);
    if direct.is_file() {
        return repo_relative(repo_root, &direct).ok();
    }

    for suffix in JAVA_SOURCE_ROOT_SUFFIXES {
        let candidate = repo_root.join(suffix).join(&normalized);
        if candidate.is_file() {
            return repo_relative(repo_root, &candidate).ok();
        }
    }

    find_tracked_path_suffix(repo_root, &normalized)
}

/// `git ls-files` suffix lookup for paths like `org/foo/Bar.java` when the
/// analyzer omitted the `app/src/main/java/` prefix.
fn find_tracked_path_suffix(repo_root: &Utf8Path, suffix: &str) -> Option<String> {
    let file_name = suffix.rsplit('/').next()?;
    let output = Command::new("git")
        .args(["ls-files", "--", &format!("**/{file_name}")])
        .current_dir(repo_root.as_std_path())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut matches: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| *line == suffix || line.ends_with(&format!("/{suffix}")))
        .map(str::to_string)
        .collect();
    if matches.is_empty() {
        return None;
    }
    matches.sort_by_key(|p| java_path_preference_key(p));
    Some(matches[0].clone())
}

fn java_path_preference_key(path: &str) -> u8 {
    if path.contains("/src/main/java/") || path.contains("/src/test/java/") {
        0
    } else if path.contains("/build/generated/") {
        1
    } else {
        2
    }
}

fn canonicalize_or_lexical(path: &Utf8Path) -> Result<Utf8PathBuf, PathError> {
    // Try real canonicalize first (resolves symlinks).
    if let Ok(real) = path.canonicalize_utf8() {
        return Ok(real);
    }
    // Fall back to lexical normalisation: makes the function usable in
    // unit tests where the paths do not exist on disk, and stays
    // consistent for the path-classification half of the contract.
    Ok(lexical_normalize(path))
}

/// Lexical normalisation: resolves `..` and drops `.` segments without
/// touching the filesystem. The result is absolute when the input is.
fn lexical_normalize(path: &Utf8Path) -> Utf8PathBuf {
    let mut out = Utf8PathBuf::new();
    for component in path.components() {
        use camino::Utf8Component::*;
        match component {
            Prefix(p) => out.push(p.as_str()),
            RootDir => out.push("/"),
            CurDir => {}
            ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Normal(s) => out.push(s),
        }
    }
    out
}

/// True when `path` looks like a repo-relative POSIX path: no leading
/// `/`, no Windows drive prefix, no `..` segments. A safe gate for
/// strings flowing into `RuleFinding::file_repo_relative`.
pub fn is_repo_relative(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return false;
    }
    // Windows drive letter prefix, e.g. "C:\..." or "C:/...".
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return false;
    }
    // Reject any `..` segment.
    !path.split(['/', '\\']).any(|seg| seg == "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let dir = TempDir::new().unwrap();
        // Canonicalise to handle macOS /var → /private/var symlink etc.
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let path = Utf8PathBuf::from_path_buf(canonical).unwrap();
        (dir, path)
    }

    #[test]
    fn repo_relative_strips_root_prefix_when_path_is_inside_repo() {
        let (_g, root) = utf8_tempdir();
        let nested = root.join("src/Foo.java");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "").unwrap();
        let rel = repo_relative(&root, &nested).unwrap();
        assert_eq!(rel, "src/Foo.java");
    }

    #[test]
    fn repo_relative_rejects_path_outside_root() {
        let (_g, root) = utf8_tempdir();
        let outside = Utf8PathBuf::from("/etc/passwd");
        let err = repo_relative(&root, &outside).unwrap_err();
        assert!(matches!(err, PathError::NotInsideRepoRoot));
    }

    #[test]
    fn repo_relative_normalises_parent_segments() {
        let (_g, root) = utf8_tempdir();
        // /tmp/r/a/../b/c → b/c
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        let with_dotdot = root.join("a/../b/c");
        // The target c/ does not exist on disk; this exercises the
        // lexical-normalisation fallback for the path argument.
        let rel = repo_relative(&root, &with_dotdot).unwrap();
        assert_eq!(rel, "b/c");
    }

    #[test]
    fn is_repo_relative_accepts_posix_path() {
        assert!(is_repo_relative("src/Foo.java"));
        assert!(is_repo_relative("Foo.java"));
        assert!(is_repo_relative("a/b/c.txt"));
    }

    #[test]
    fn is_repo_relative_rejects_absolute_or_parent_paths() {
        assert!(!is_repo_relative(""));
        assert!(!is_repo_relative("/home/u/Foo.java"));
        assert!(!is_repo_relative("../x"));
        assert!(!is_repo_relative("a/../b"));
    }

    #[test]
    fn is_repo_relative_rejects_windows_style_paths() {
        assert!(!is_repo_relative("C:/Foo.java"));
        assert!(!is_repo_relative("C:\\Foo.java"));
        assert!(!is_repo_relative("\\foo"));
    }

    #[test]
    fn resolve_existing_java_file_finds_under_app_src_main_java() {
        let (_g, root) = utf8_tempdir();
        let nested = root.join("app/src/main/java/org/example/Foo.java");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "class Foo {}\n").unwrap();
        run_git_init(&root);
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "init"]);

        let resolved =
            resolve_existing_java_file(&root, "org/example/Foo.java").expect("must resolve");
        assert_eq!(resolved, "app/src/main/java/org/example/Foo.java");
    }

    #[test]
    fn resolve_existing_java_file_prefers_src_over_build_generated() {
        let (_g, root) = utf8_tempdir();
        let src = root.join("app/src/main/java/org/example/Bar.java");
        let gen = root
            .join("app/build/generated/ap_generated_sources/out/org/example/Bar.java");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::create_dir_all(gen.parent().unwrap()).unwrap();
        std::fs::write(&src, "class Bar {}\n").unwrap();
        std::fs::write(&gen, "class Bar {}\n").unwrap();
        run_git_init(&root);
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "init"]);

        let resolved =
            resolve_existing_java_file(&root, "org/example/Bar.java").expect("must resolve");
        assert_eq!(resolved, "app/src/main/java/org/example/Bar.java");
    }

    fn run_git_init(root: &Utf8Path) {
        use std::process::Command;
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(root.as_std_path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@example.com"])
            .current_dir(root.as_std_path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(root.as_std_path())
            .status()
            .unwrap();
    }

    fn run_git(root: &Utf8Path, args: &[&str]) {
        use std::process::Command;
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(root.as_std_path())
                .status()
                .unwrap()
                .success()
        );
    }
}
