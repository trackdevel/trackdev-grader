//! Template repo diff: what did students write vs what came from the template.
//! Mirrors `src/curriculum/template_diff.py`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

const IGNORED_EXACT: &[&str] = &[
    ".git",
    ".gitignore",
    ".idea",
    ".gradle",
    "build",
    "gradle",
    "gradlew",
    "gradlew.bat",
    ".DS_Store",
    "__pycache__",
    "local.properties",
];
const IGNORED_SUFFIXES: &[&str] = &[".class", ".apk", ".jar", ".aar"];

const ANALYZED_EXTENSIONS: &[&str] = &[
    "java",
    "kt",
    "xml",
    "properties",
    "yaml",
    "yml",
    "gradle",
    "kts",
    "json",
];

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub file_path: String,
    pub status: String, // "added" | "modified" | "template_only"
    pub added_lines: Vec<usize>,
    pub total_lines: usize,
    pub content: String,
    pub template_content: Option<String>,
}

fn should_skip(rel: &Path) -> bool {
    for comp in rel.components() {
        let s = comp.as_os_str().to_string_lossy();
        if IGNORED_EXACT.iter().any(|p| s == *p) {
            return true;
        }
        for suf in IGNORED_SUFFIXES {
            if s.ends_with(suf) {
                return true;
            }
        }
    }
    false
}

fn should_analyze(path: &Path) -> bool {
    match path.extension() {
        Some(ext) => {
            let e = ext.to_string_lossy().to_lowercase();
            ANALYZED_EXTENSIONS.iter().any(|x| *x == e)
        }
        None => false,
    }
}

fn list_files(repo_path: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    if !repo_path.is_dir() {
        return out;
    }
    for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(repo_path) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if should_skip(&rel) {
            continue;
        }
        if !should_analyze(&rel) {
            continue;
        }
        out.insert(rel.to_string_lossy().to_string());
    }
    out
}

/// Longest-common-subsequence-based line diff: find 1-indexed line numbers in
/// `student_lines` that are not mapped from `template_lines` (i.e. insertions
/// or replacements from the template's perspective).
///
/// Matches `difflib.SequenceMatcher.get_opcodes()` semantics for the "insert"
/// and "replace" tags.
fn find_added_lines(template_lines: &[&str], student_lines: &[&str]) -> Vec<usize> {
    let n = template_lines.len();
    let m = student_lines.len();
    // Build LCS table.
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            if template_lines[i] == student_lines[j] {
                dp[i + 1][j + 1] = dp[i][j] + 1;
            } else {
                dp[i + 1][j + 1] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }
    // Backtrack to find which student lines are "matched" to a template line.
    let mut matched_student = vec![false; m];
    let mut i = n;
    let mut j = m;
    while i > 0 && j > 0 {
        if template_lines[i - 1] == student_lines[j - 1] {
            matched_student[j - 1] = true;
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    (0..m)
        .filter(|k| !matched_student[*k])
        .map(|k| k + 1)
        .collect()
}

pub fn compute_template_diff(student_repo: &Path, template_repo: &Path) -> Vec<FileDiff> {
    let student = list_files(student_repo);
    let template = list_files(template_repo);

    let mut results: Vec<FileDiff> = Vec::new();

    // Added files
    let mut added: Vec<&String> = student.difference(&template).collect();
    added.sort();
    for rel in added {
        let full: PathBuf = student_repo.join(rel);
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let total = lines.len();
        results.push(FileDiff {
            file_path: rel.clone(),
            status: "added".into(),
            added_lines: (1..=total).collect(),
            total_lines: total,
            content,
            template_content: None,
        });
    }

    // Modified files
    let mut both: Vec<&String> = student.intersection(&template).collect();
    both.sort();
    for rel in both {
        let student_path = student_repo.join(rel);
        let template_path = template_repo.join(rel);
        let student_content = match std::fs::read_to_string(&student_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let template_content = match std::fs::read_to_string(&template_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if student_content == template_content {
            continue;
        }
        let s_lines: Vec<&str> = student_content.split('\n').collect();
        let t_lines: Vec<&str> = template_content.split('\n').collect();
        let added = find_added_lines(&t_lines, &s_lines);
        if added.is_empty() {
            continue;
        }
        results.push(FileDiff {
            file_path: rel.clone(),
            status: "modified".into(),
            added_lines: added,
            total_lines: s_lines.len(),
            content: student_content,
            template_content: Some(template_content),
        });
    }

    // Template-only files
    let mut only_t: Vec<&String> = template.difference(&student).collect();
    only_t.sort();
    for rel in only_t {
        results.push(FileDiff {
            file_path: rel.clone(),
            status: "template_only".into(),
            added_lines: Vec::new(),
            total_lines: 0,
            content: String::new(),
            template_content: None,
        });
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_lines_matches_lcs_insert_replace() {
        let t = vec!["a", "b", "c"];
        let s = vec!["a", "X", "c", "Y"];
        let added = find_added_lines(&t, &s);
        // 'X' at student index 2 (1-indexed) and 'Y' at index 4
        assert_eq!(added, vec![2, 4]);
    }

    #[test]
    fn identical_yields_no_adds() {
        let lines = vec!["x", "y", "z"];
        let added = find_added_lines(&lines, &lines);
        assert!(added.is_empty());
    }
}
