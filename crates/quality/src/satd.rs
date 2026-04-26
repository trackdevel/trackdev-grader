//! Self-Admitted Technical Debt detection. Mirrors `src/quality/satd.py`.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use tree_sitter::{Node, Parser};

static JAVA_LANG: Lazy<tree_sitter::Language> = Lazy::new(|| tree_sitter_java::LANGUAGE.into());

/// One category → patterns. Categories are evaluated in this order; the first
/// category with a match wins (matches Python's `break`/`break` outer logic).
static SATD_CATEGORIES: Lazy<Vec<(&'static str, Vec<Regex>)>> = Lazy::new(|| {
    let mk = |patterns: &[&str]| -> Vec<Regex> {
        patterns
            .iter()
            .map(|p| Regex::new(&format!("(?i){p}")).expect("satd regex"))
            .collect()
    };
    vec![
        (
            "design",
            mk(&[
                r"\bTODO\b",
                r"\bFIXME\b",
                r"\bHACK\b",
                r"\bXXX\b",
                r"\bWORKAROUND\b",
                r"quick\s+fix",
                r"ugly",
                r"temporary",
                r"refactor\s+this",
                r"needs?\s+refactoring",
                r"tech\s*debt",
                r"should\s+be\s+replaced",
                r"not\s+ideal",
            ]),
        ),
        (
            "requirement",
            mk(&[
                r"\bTBD\b",
                r"to\s+be\s+determined",
                r"not\s+yet\s+implemented",
                r"placeholder",
                r"stub",
            ]),
        ),
        (
            "defect",
            mk(&[
                r"\bBUG\b",
                r"\bFIXME\b.*bug",
                r"known\s+issue",
                r"doesn'?t\s+work",
                r"broken",
            ]),
        ),
        (
            "test",
            mk(&[r"skip\s*test", r"disabled\s*test", r"@Ignore", r"@Disabled"]),
        ),
        (
            "documentation",
            mk(&[r"update\s+docs?", r"document\s+this", r"missing\s+docs?"]),
        ),
    ]
});

const COMMENT_NODE_TYPES: &[&str] = &["line_comment", "block_comment"];

#[derive(Debug, Clone)]
pub struct SatdMatch {
    pub file_path: String,
    pub line_number: u32,
    pub category: &'static str,
    pub keyword: String,
    pub comment_text: String,
}

fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

pub fn scan_comments(file_path: &str, source: &[u8]) -> Vec<SatdMatch> {
    let mut parser = Parser::new();
    if parser.set_language(&JAVA_LANG).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let mut matches: Vec<SatdMatch> = Vec::new();
    fn walk(node: Node, source: &[u8], file_path: &str, out: &mut Vec<SatdMatch>) {
        if COMMENT_NODE_TYPES.contains(&node.kind()) {
            let text = node_text(node, source);
            let line = (node.start_position().row as u32) + 1;
            // First category with a match wins.
            'outer: for (category, patterns) in SATD_CATEGORIES.iter() {
                for pat in patterns {
                    if let Some(m) = pat.find(&text) {
                        let stripped = text.trim();
                        let comment_text: String = stripped.chars().take(200).collect();
                        out.push(SatdMatch {
                            file_path: file_path.to_string(),
                            line_number: line,
                            category,
                            keyword: m.as_str().to_string(),
                            comment_text,
                        });
                        break 'outer;
                    }
                }
            }
        }
        for c in children(node) {
            walk(c, source, file_path, out);
        }
    }
    walk(tree.root_node(), source, file_path, &mut matches);
    matches
}

pub fn compute_satd_for_repo(
    conn: &Connection,
    repo_path: &Path,
    sprint_id: i64,
    author_map: Option<&BTreeMap<String, String>>,
) -> rusqlite::Result<usize> {
    let mut count = 0usize;
    walk_java_files(repo_path, repo_path, &mut |rel_path, bytes| {
        let items = scan_comments(rel_path, &bytes);
        for item in items {
            let author = author_map.and_then(|m| m.get(rel_path).cloned());
            conn.execute(
                "INSERT OR REPLACE INTO satd_items
                 (file_path, line_number, sprint_id, author_id, category, keyword, comment_text)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    item.file_path,
                    item.line_number as i64,
                    sprint_id,
                    author,
                    item.category,
                    item.keyword,
                    item.comment_text,
                ],
            )?;
            count += 1;
        }
        Ok::<_, rusqlite::Error>(())
    })?;
    Ok(count)
}

fn walk_java_files<F>(repo_root: &Path, dir: &Path, f: &mut F) -> rusqlite::Result<()>
where
    F: FnMut(&str, Vec<u8>) -> rusqlite::Result<()>,
{
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_java_files(repo_root, &p, f)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("java") {
            let bytes = match std::fs::read(&p) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let rel = p
                .strip_prefix(repo_root)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            f(&rel, bytes)?;
        }
    }
    Ok(())
}

/// Per-student SATD introduced/removed between two sprints, keyed by
/// `(file_path, comment_text)` presence. Mirrors `satd_delta` in Python.
pub fn satd_delta(
    conn: &Connection,
    sprint_id_prev: i64,
    sprint_id_curr: i64,
) -> rusqlite::Result<BTreeMap<String, (i64, i64)>> {
    let collect = |sid: i64| -> rusqlite::Result<HashSet<(String, String)>> {
        let mut stmt =
            conn.prepare("SELECT file_path, comment_text FROM satd_items WHERE sprint_id = ?")?;
        let rows = stmt.query_map([sid], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut set = HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        Ok(set)
    };
    let prev = collect(sprint_id_prev)?;
    let curr = collect(sprint_id_curr)?;
    let introduced: HashSet<_> = curr.difference(&prev).cloned().collect();
    let removed: HashSet<_> = prev.difference(&curr).cloned().collect();

    let mut result: BTreeMap<String, (i64, i64)> = BTreeMap::new();

    for (fp, text) in introduced {
        let author: Option<String> = conn
            .query_row(
                "SELECT author_id FROM satd_items
                 WHERE sprint_id = ? AND file_path = ? AND comment_text = ?",
                params![sprint_id_curr, fp, text],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        if let Some(aid) = author {
            result.entry(aid).or_insert((0, 0)).0 += 1;
        }
    }
    for (fp, text) in removed {
        let author: Option<String> = conn
            .query_row(
                "SELECT author_id FROM satd_items
                 WHERE sprint_id = ? AND file_path = ? AND comment_text = ?",
                params![sprint_id_prev, fp, text],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        if let Some(aid) = author {
            result.entry(aid).or_insert((0, 0)).1 += 1;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_is_design_category() {
        let src = b"class A { void f() { /* TODO: refactor this mess */ int x = 1; } }";
        let matches = scan_comments("A.java", src);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].category, "design");
    }

    #[test]
    fn tbd_is_requirement_category() {
        let src = b"class A { /* TBD: spec not yet finalized */ int x; }";
        let matches = scan_comments("A.java", src);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].category, "requirement");
    }

    #[test]
    fn clean_file_has_no_satd() {
        let src = b"class A { /** Javadoc comment. */ int x; }";
        assert!(scan_comments("A.java", src).is_empty());
    }
}
