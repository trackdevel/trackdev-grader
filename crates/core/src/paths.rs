//! Path canonicalisation helper. Enforces that file paths flowing into
//! `RuleFinding::file_repo_relative` are repo-relative POSIX paths so the
//! GitHub blob-URL builder cannot accidentally emit absolute filesystem
//! paths (see the static-analysis URL bug captured by W1.T4).

use std::io;

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
}
