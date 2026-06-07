//! Deterministic pre-filter for file-tier LLM candidates (Track B PB).

use anyhow::Result;
use rusqlite::{params, Connection};
use sprint_grader_core::QualityLlmConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCandidate {
    pub repo_full_name: String,
    pub file_path: String,
    pub statement_count: u32,
}

/// List `.java` files fingerprinted in repos linked to `project_id` via PRs.
pub fn list_file_candidates(
    conn: &Connection,
    project_id: i64,
    cfg: &QualityLlmConfig,
) -> Result<Vec<FileCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT f.repo_full_name, f.file_path, COUNT(*) AS stmt_count
         FROM fingerprints f
         WHERE f.file_path LIKE '%.java'
           AND f.repo_full_name IN (
             SELECT DISTINCT pr.repo_full_name
             FROM pull_requests pr
             JOIN pr_authors pa ON pa.pr_id = pr.id
             JOIN students s ON s.id = pa.student_id
             WHERE s.team_project_id = ?
           )
         GROUP BY f.repo_full_name, f.file_path
         HAVING stmt_count >= ?
         ORDER BY stmt_count DESC, f.repo_full_name, f.file_path",
    )?;
    let min = i64::from(cfg.min_surviving_statements);
    let rows = stmt.query_map(params![project_id, min], |row| {
        let count: i64 = row.get(2)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            count.max(0) as u32,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (repo, path, count) = row?;
        if cfg
            .skip_globs
            .iter()
            .any(|g| simple_glob_match(g, &path))
        {
            continue;
        }
        out.push(FileCandidate {
            repo_full_name: repo,
            file_path: path,
            statement_count: count,
        });
        if cfg.max_files_per_project > 0 && out.len() >= cfg.max_files_per_project {
            break;
        }
    }
    let _ = cfg.only_delivered_repos;
    Ok(out)
}

/// Path glob matcher (`**` = any segments, `*` = one segment).
fn simple_glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let p: Vec<&str> = path
        .split(['/', std::path::MAIN_SEPARATOR])
        .filter(|s| !s.is_empty())
        .collect();
    glob_recurse(&pat, &p)
}

fn glob_recurse(pat: &[&str], path: &[&str]) -> bool {
    if pat.is_empty() {
        return path.is_empty();
    }
    match pat[0] {
        "**" => {
            for i in 0..=path.len() {
                if glob_recurse(&pat[1..], &path[i..]) {
                    return true;
                }
            }
            false
        }
        "*" => !path.is_empty() && glob_recurse(&pat[1..], &path[1..]),
        lit => {
            if path.is_empty() {
                return false;
            }
            if !lit_matches(lit, path[0]) {
                return false;
            }
            glob_recurse(&pat[1..], &path[1..])
        }
    }
}

fn lit_matches(pattern_segment: &str, path_segment: &str) -> bool {
    if !pattern_segment.contains('*') {
        return pattern_segment == path_segment;
    }
    let frags: Vec<&str> = pattern_segment.split('*').collect();
    let mut cursor = path_segment;
    for (i, frag) in frags.iter().enumerate() {
        if frag.is_empty() {
            continue;
        }
        let pos = if i == 0 {
            if !cursor.starts_with(frag) {
                return false;
            }
            frag.len()
        } else if i == frags.len() - 1 {
            if !cursor.ends_with(frag) {
                return false;
            }
            return true;
        } else {
            match cursor.find(frag) {
                Some(p) => p + frag.len(),
                None => return false,
            }
        };
        cursor = &cursor[pos..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_glob_matches_build_paths() {
        assert!(simple_glob_match("**/build/**", "app/build/Foo.java"));
        assert!(!simple_glob_match("**/build/**", "app/src/Foo.java"));
    }
}
