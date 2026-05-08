//! Git blame parsing and author attribution. Mirrors `src/survival/blame.py`.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct BlameLine {
    pub line_number: u32,
    pub commit_sha: String,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct StatementBlame {
    pub commit_sha: String,
    pub author_email: String,
    pub author_name: String,
    pub author_login: Option<String>,
    pub student_id: Option<String>,
    pub pr_id: Option<String>,
    pub unanimous: bool,
}

/// Parse `git blame --porcelain` output into per-line records.
pub fn parse_porcelain(output: &str) -> HashMap<u32, BlameLine> {
    let lines: Vec<&str> = output.split('\n').collect();
    let mut result: HashMap<u32, BlameLine> = HashMap::new();
    let mut commit_cache: HashMap<String, CommitMeta> = HashMap::new();

    let mut i: usize = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.is_empty() {
            i += 1;
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            i += 1;
            continue;
        }

        let sha = parts[0].to_string();
        if sha.len() != 40
            || !sha
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            i += 1;
            continue;
        }
        let final_line: u32 = match parts[2].parse() {
            Ok(n) => n,
            Err(_) => {
                i += 1;
                continue;
            }
        };
        i += 1;

        if !commit_cache.contains_key(&sha) {
            let mut meta = CommitMeta::default();
            while i < lines.len() && !lines[i].starts_with('\t') {
                let header = lines[i];
                if let Some(rest) = header.strip_prefix("author ") {
                    meta.author = rest.to_string();
                } else if let Some(rest) = header.strip_prefix("author-mail ") {
                    meta.author_mail = rest.trim_matches(|c| c == '<' || c == '>').to_string();
                } else if let Some(rest) = header.strip_prefix("author-time ") {
                    meta.author_time = rest.to_string();
                } else if let Some(rest) = header.strip_prefix("summary ") {
                    meta.summary = rest.to_string();
                }
                i += 1;
            }
            commit_cache.insert(sha.clone(), meta);
        } else {
            while i < lines.len() && !lines[i].starts_with('\t') {
                i += 1;
            }
        }

        if i < lines.len() && lines[i].starts_with('\t') {
            i += 1;
        }

        let meta = commit_cache.get(&sha).cloned().unwrap_or_default();
        result.insert(
            final_line,
            BlameLine {
                line_number: final_line,
                commit_sha: sha,
                author_name: meta.author,
                author_email: meta.author_mail,
                author_time: meta.author_time.parse().unwrap_or(0),
                summary: meta.summary,
            },
        );
    }

    result
}

#[derive(Debug, Default, Clone)]
struct CommitMeta {
    author: String,
    author_mail: String,
    author_time: String,
    summary: String,
}

/// If `<repo>/.git-blame-ignore-revs` exists, return its path so callers can
/// pass `--ignore-revs-file <path>` to `git blame`. Returns `None` when the
/// file is absent, so repos that haven't opted in are unaffected.
fn ignore_revs_file_arg(repo_path: &Path) -> Option<String> {
    let path = repo_path.join(".git-blame-ignore-revs");
    if path.is_file() {
        Some(path.to_string_lossy().into_owned())
    } else {
        None
    }
}

pub fn blame_file(repo_path: &Path, file_path: &str) -> HashMap<u32, BlameLine> {
    let mut args: Vec<String> = vec!["blame".into(), "--porcelain".into(), "-w".into()];
    if let Some(p) = ignore_revs_file_arg(repo_path) {
        args.push("--ignore-revs-file".into());
        args.push(p);
    }
    args.push("--".into());
    args.push(file_path.to_string());
    let output = Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .output();
    let out = match output {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    parse_porcelain(&text)
}

pub fn blame_file_with_copy_detection(
    repo_path: &Path,
    file_path: &str,
) -> HashMap<u32, BlameLine> {
    let mut args: Vec<String> = vec![
        "blame".into(),
        "--porcelain".into(),
        "-w".into(),
        "-C".into(),
        "-C".into(),
    ];
    if let Some(p) = ignore_revs_file_arg(repo_path) {
        args.push("--ignore-revs-file".into());
        args.push(p);
    }
    args.push("--".into());
    args.push(file_path.to_string());
    let output = Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .output();
    let out = match output {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    parse_porcelain(&text)
}

pub fn blame_statement(
    blame_lines: &HashMap<u32, BlameLine>,
    start_line: u32,
    end_line: u32,
) -> Option<StatementBlame> {
    let mut relevant: Vec<&BlameLine> = Vec::new();
    for ln in start_line..=end_line {
        if let Some(bl) = blame_lines.get(&ln) {
            relevant.push(bl);
        }
    }
    if relevant.is_empty() {
        return None;
    }

    let mut email_counts: HashMap<&str, usize> = HashMap::new();
    for bl in &relevant {
        *email_counts.entry(bl.author_email.as_str()).or_insert(0) += 1;
    }
    let (majority_email, _) = email_counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(k, c)| (*k, *c))
        .unwrap();
    let unanimous = email_counts.len() == 1;

    let representative = relevant
        .iter()
        .find(|bl| bl.author_email == majority_email)
        .copied()
        .unwrap();

    let mut sha_counts: HashMap<&str, usize> = HashMap::new();
    for bl in &relevant {
        *sha_counts.entry(bl.commit_sha.as_str()).or_insert(0) += 1;
    }
    let (majority_sha, _) = sha_counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(k, c)| (*k, *c))
        .unwrap();

    Some(StatementBlame {
        commit_sha: majority_sha.to_string(),
        author_email: representative.author_email.clone(),
        author_name: representative.author_name.clone(),
        author_login: None,
        student_id: None,
        pr_id: None,
        unanimous,
    })
}

/// Commit SHA → (pr_id, optional author_login).
pub type CommitPrMap = HashMap<String, (String, Option<String>)>;

pub fn build_commit_to_pr_map(conn: &Connection) -> rusqlite::Result<CommitPrMap> {
    let mut stmt = conn.prepare("SELECT sha, pr_id, author_login FROM pr_commits")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out: CommitPrMap = HashMap::new();
    for r in rows {
        let (sha, pr_id, login) = r?;
        out.insert(sha, (pr_id, login));
    }
    Ok(out)
}

/// email (lowercased) or github_login (lowercased or `<login>@users.noreply.github.com`)
/// → (student_id, github_login).
pub type EmailStudentMap = HashMap<String, (String, Option<String>)>;

/// Build the blame-side identity map. **Sole source of truth:**
/// `student_github_identity` — the table populated by
/// `collect::identity_resolver` from task-PR evidence. TrackDev's
/// `students.github_login` is no longer consulted here (it is unreliable
/// — many students leave it blank or fill it incorrectly).
pub fn build_email_to_student_map(conn: &Connection) -> rusqlite::Result<EmailStudentMap> {
    let mut out: EmailStudentMap = HashMap::new();

    let mut stmt = conn.prepare(
        "SELECT student_id, identity_kind, identity_value
         FROM student_github_identity",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for r in rows {
        let (student_id, kind, value) = r?;
        let key = value.to_lowercase();
        let login = if kind == "login" { Some(value) } else { None };
        out.entry(key.clone())
            .or_insert_with(|| (student_id.clone(), login.clone()));
        if kind == "login" {
            out.entry(format!("{key}@users.noreply.github.com"))
                .or_insert_with(|| (student_id.clone(), login.clone()));
        }
    }

    Ok(out)
}

pub fn resolve_blame_authors(
    statement_blames: &mut [StatementBlame],
    commit_pr_map: &CommitPrMap,
    email_student_map: &EmailStudentMap,
) {
    for sb in statement_blames.iter_mut() {
        if let Some((pr_id, login)) = commit_pr_map.get(&sb.commit_sha) {
            sb.pr_id = Some(pr_id.clone());
            sb.author_login = login.clone();
        }
        let email_key = sb.author_email.to_lowercase();
        if let Some((student_id, github_login)) = email_student_map.get(&email_key) {
            sb.student_id = Some(student_id.clone());
            if sb.author_login.is_none() {
                sb.author_login = github_login.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_blame_entry() {
        let out = "\
0123456789abcdef0123456789abcdef01234567 1 1
author Alice
author-mail <alice@example.com>
author-time 1234567890
author-tz +0000
summary First commit
filename foo.java
\tpublic class Foo {
";
        let map = parse_porcelain(out);
        let bl = &map[&1];
        assert_eq!(bl.commit_sha, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(bl.author_name, "Alice");
        assert_eq!(bl.author_email, "alice@example.com");
        assert_eq!(bl.author_time, 1234567890);
        assert_eq!(bl.summary, "First commit");
    }

    #[test]
    fn reuses_cached_commit_metadata() {
        let out = "\
0123456789abcdef0123456789abcdef01234567 1 1
author Alice
author-mail <alice@example.com>
author-time 1234567890
summary First commit
filename foo.java
\tline one
0123456789abcdef0123456789abcdef01234567 2 2
filename foo.java
\tline two
";
        let map = parse_porcelain(out);
        assert_eq!(map[&1].author_name, "Alice");
        assert_eq!(map[&2].author_name, "Alice");
        assert_eq!(
            map[&2].commit_sha,
            "0123456789abcdef0123456789abcdef01234567"
        );
    }

    #[test]
    fn ignore_revs_file_arg_returns_some_when_file_present() {
        let dir = std::env::temp_dir().join(format!(
            "sprint_grader_blame_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Absent: returns None.
        assert!(ignore_revs_file_arg(&dir).is_none());

        // Present: returns Some with the absolute path.
        let revs = dir.join(".git-blame-ignore-revs");
        std::fs::write(&revs, "abc123\n").unwrap();
        let got = ignore_revs_file_arg(&dir).expect("file is present");
        assert!(
            got.ends_with(".git-blame-ignore-revs"),
            "expected suffix, got {got}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // TODO(T-P2.7): full integration test that builds a tiny git repo with
    // tempfile + Command::new("git"), reformats whitespace in a second commit,
    // and asserts blame attributes the line to the original author. Deferred
    // because survival has no test-fixture infrastructure yet (no tempfile
    // dev-dep) and the chunk says to defer if heavy.
}
