//! Compilation failure classification + per-project summaries.
//! Mirrors `src/compile/failure_analysis.py`.

use std::collections::BTreeMap;

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tracing::info;

static ERROR_PATTERNS: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    let mk = |s: &str, n: &'static str| (Regex::new(&format!("(?i){s}")).unwrap(), n);
    vec![
        mk(r"error: cannot find symbol", "missing_symbol"),
        mk(r"error: incompatible types", "type_mismatch"),
        mk(r"error: ';' expected", "syntax_error"),
        mk(
            r"error: class .+ is public, should be declared in a file named",
            "file_class_mismatch",
        ),
        mk(
            r"FAILURE: Build failed with an exception",
            "gradle_build_failure",
        ),
        mk(r"error: package .+ does not exist", "missing_package"),
        mk(
            r"error: method .+ in class .+ cannot be applied",
            "method_signature_error",
        ),
        mk(r"error: unreported exception", "uncaught_exception"),
        mk(r"Non-zero exit value", "compilation_error_generic"),
        mk(r"Could not resolve dependencies", "dependency_resolution"),
        mk(
            r"Execution failed for task ':.*:compile",
            "compile_task_failed",
        ),
    ]
});

#[derive(Debug, Clone)]
pub struct ErrorClass {
    pub pattern: String,
    pub count: usize,
    pub first_occurrence: String,
}

pub fn classify_errors(stderr: &str) -> Vec<ErrorClass> {
    if stderr.is_empty() {
        return Vec::new();
    }
    let mut counts: Vec<ErrorClass> = Vec::new();
    for (pattern, name) in ERROR_PATTERNS.iter() {
        let matches: Vec<_> = pattern.find_iter(stderr).collect();
        if matches.is_empty() {
            continue;
        }
        let first = {
            let m = matches[0];
            let slice = m.as_str();
            let max = slice.len().min(200);
            slice[..max].to_string()
        };
        counts.push(ErrorClass {
            pattern: (*name).to_string(),
            count: matches.len(),
            first_occurrence: first,
        });
    }
    counts.sort_by(|a, b| b.count.cmp(&a.count));
    counts
}

fn compute_author_breakdown(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT pc.author_id, COUNT(*) AS total,
                SUM(CASE WHEN pc.compiles = 0 THEN 1 ELSE 0 END) AS failed
         FROM pr_compilation pc
         JOIN students s ON s.id = pc.author_id
         WHERE pc.sprint_id = ? AND s.team_project_id = ?
         GROUP BY pc.author_id",
    )?;
    let rows = stmt.query_map(params![sprint_id, project_id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = serde_json::Map::new();
    for row in rows {
        let (author, total, failed) = row?;
        let fail_rate = if total > 0 {
            ((failed as f64 / total as f64) * 1000.0).round() / 1000.0
        } else {
            0.0
        };
        if let Some(a) = author {
            out.insert(
                a,
                json!({
                    "total": total,
                    "failed": failed,
                    "fail_rate": fail_rate,
                }),
            );
        }
    }
    Ok(Value::Object(out))
}

fn compute_reviewer_breakdown(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<Value> {
    let mut stmt = conn
        .prepare("SELECT pr_id, reviewer_ids, compiles FROM pr_compilation WHERE sprint_id = ?")?;
    let rows = stmt.query_map([sprint_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            r.get::<_, Option<bool>>(2)?.unwrap_or(false),
        ))
    })?;

    let mut stats: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for row in rows {
        let (_pr_id, reviewer_json, compiles) = row?;
        let reviewers: Vec<String> = serde_json::from_str(&reviewer_json).unwrap_or_default();
        for rid in reviewers {
            let belongs: Option<i64> = conn
                .query_row(
                    "SELECT team_project_id FROM students WHERE id = ?",
                    [&rid],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .ok()
                .flatten();
            if belongs != Some(project_id) {
                continue;
            }
            let entry = stats.entry(rid).or_insert((0, 0));
            entry.0 += 1;
            if !compiles {
                entry.1 += 1;
            }
        }
    }

    let mut out = serde_json::Map::new();
    for (rid, (total, failed)) in stats {
        let fail_rate = if total > 0 {
            ((failed as f64 / total as f64) * 1000.0).round() / 1000.0
        } else {
            0.0
        };
        out.insert(
            rid,
            json!({
                "reviewed_total": total,
                "reviewed_failed": failed,
                "fail_rate": fail_rate,
            }),
        );
    }
    Ok(Value::Object(out))
}

pub fn summarize_compilation(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.team_project_id AS project_id
         FROM pr_compilation pc
         JOIN students s ON s.id = pc.author_id
         WHERE pc.sprint_id = ? AND s.team_project_id IS NOT NULL",
    )?;
    let project_ids: Vec<i64> = stmt
        .query_map([sprint_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    for project_id in &project_ids {
        let (total, compiling, failing): (i64, i64, i64) = conn.query_row(
            "SELECT COUNT(*) AS total,
                    SUM(CASE WHEN pc.compiles = 1 THEN 1 ELSE 0 END) AS compiling,
                    SUM(CASE WHEN pc.compiles = 0 THEN 1 ELSE 0 END) AS failing
             FROM pr_compilation pc
             JOIN students s ON s.id = pc.author_id
             WHERE pc.sprint_id = ? AND s.team_project_id = ?",
            params![sprint_id, project_id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            },
        )?;
        let compile_rate = if total > 0 {
            compiling as f64 / total as f64
        } else {
            0.0
        };

        // Aggregate error classification across all failing PRs.
        let mut stmt = conn.prepare(
            "SELECT pc.stderr_text FROM pr_compilation pc
             JOIN students s ON s.id = pc.author_id
             WHERE pc.sprint_id = ? AND s.team_project_id = ? AND pc.compiles = 0",
        )?;
        let stderrs: Vec<String> = stmt
            .query_map(params![sprint_id, project_id], |r| {
                Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default())
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        let mut merged: BTreeMap<String, i64> = BTreeMap::new();
        for s in stderrs {
            for e in classify_errors(&s) {
                *merged.entry(e.pattern).or_insert(0) += e.count as i64;
            }
        }
        let mut top: Vec<(String, i64)> = merged.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        let top_errors: Vec<Value> = top
            .into_iter()
            .take(5)
            .map(|(k, v)| json!({"pattern": k, "count": v}))
            .collect();

        let author_bd = compute_author_breakdown(conn, sprint_id, *project_id)?;
        let reviewer_bd = compute_reviewer_breakdown(conn, sprint_id, *project_id)?;

        conn.execute(
            "INSERT OR REPLACE INTO compilation_failure_summary
             (sprint_id, project_id, total_prs, compiling_prs, failing_prs,
              compile_rate, author_breakdown, reviewer_breakdown, top_errors)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                sprint_id,
                project_id,
                total,
                compiling,
                failing,
                compile_rate,
                author_bd.to_string(),
                reviewer_bd.to_string(),
                Value::Array(top_errors).to_string(),
            ],
        )?;
    }

    info!(projects = project_ids.len(), "Compilation summary computed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_gradle_errors() {
        let stderr = "\
foo.java:12: error: cannot find symbol
foo.java:13: error: cannot find symbol
FAILURE: Build failed with an exception
Execution failed for task ':app:compileDebugJavaWithJavac'.
";
        let cls = classify_errors(stderr);
        let names: Vec<&str> = cls.iter().map(|c| c.pattern.as_str()).collect();
        assert!(names.contains(&"missing_symbol"));
        assert!(names.contains(&"gradle_build_failure"));
        assert!(names.contains(&"compile_task_failed"));
        // Sorted by count desc → missing_symbol (2) should come first.
        assert_eq!(cls[0].pattern, "missing_symbol");
        assert_eq!(cls[0].count, 2);
    }

    #[test]
    fn empty_stderr_returns_empty() {
        assert!(classify_errors("").is_empty());
    }
}
