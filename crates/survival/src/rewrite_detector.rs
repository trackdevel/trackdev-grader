//! Detect cosmetic rewrites — blame author changed, normalized fingerprint unchanged.
//! Mirrors `src/survival/rewrite_detector.py`.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use serde_json::json;
use tracing::info;

type FpKey = (String, String, i64);
type FpData = (Option<String>, Option<String>, Option<String>);

pub fn detect_rewrites(
    conn: &Connection,
    current_sprint_ids: &[i64],
    previous_sprint_ids: &[i64],
) -> rusqlite::Result<()> {
    // Clear old rows for the current sprint IDs.
    for sid in current_sprint_ids {
        conn.execute("DELETE FROM cosmetic_rewrites WHERE sprint_id = ?", [sid])?;
    }

    if previous_sprint_ids.is_empty() {
        info!(
            "No previous sprint data — skipping detailed rewrite detection. \
             RAW_NORMALIZED_DIVERGENCE flag will still work from current data."
        );
        return Ok(());
    }

    let mut total: i64 = 0;

    for (cur_sid, prev_sid) in current_sprint_ids.iter().zip(previous_sprint_ids.iter()) {
        let project_id: Option<i64> = conn
            .query_row(
                "SELECT project_id FROM sprints WHERE id = ?",
                [*cur_sid],
                |r| r.get(0),
            )
            .ok();
        if project_id.is_none() {
            continue;
        }

        // Previous sprint fingerprints.
        let mut stmt = conn.prepare(
            "SELECT file_path, repo_full_name, statement_index,
                    raw_fingerprint, normalized_fingerprint, blame_author_login
             FROM fingerprints WHERE sprint_id = ?",
        )?;
        let rows = stmt.query_map([*prev_sid], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut prev_lookup: HashMap<FpKey, FpData> = HashMap::new();
        for r in rows {
            let (fp, repo, idx, raw, norm, author) = r?;
            prev_lookup.insert((fp, repo, idx), (raw, norm, author));
        }
        drop(stmt);

        // Current sprint fingerprints — compare.
        let mut stmt = conn.prepare(
            "SELECT file_path, repo_full_name, statement_index,
                    raw_fingerprint, normalized_fingerprint, blame_author_login
             FROM fingerprints WHERE sprint_id = ?",
        )?;
        let rows = stmt.query_map([*cur_sid], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;

        let mut groups: HashMap<(String, String, String, String), i64> = HashMap::new();
        for r in rows {
            let (file_path, repo, idx, raw_fp, norm_fp, cur_author) = r?;
            let key = (file_path.clone(), repo.clone(), idx);
            let prev = match prev_lookup.get(&key) {
                Some(v) => v,
                None => continue,
            };
            let (prev_raw, prev_norm, prev_author) = prev;
            let cur_author = match cur_author.as_ref() {
                Some(a) if !a.is_empty() => a,
                _ => continue,
            };
            let prev_author = match prev_author.as_ref() {
                Some(a) if !a.is_empty() => a,
                _ => continue,
            };
            if cur_author == prev_author {
                continue;
            }
            if norm_fp.is_some() && norm_fp == *prev_norm && raw_fp != *prev_raw {
                let group_key = (file_path, repo, prev_author.clone(), cur_author.clone());
                *groups.entry(group_key).or_insert(0) += 1;
            }
        }
        drop(stmt);

        for ((file_path, repo, orig_author, rewriter), count) in groups {
            let orig_student = login_to_student_id(conn, &orig_author)?;
            let rewriter_student = login_to_student_id(conn, &rewriter)?;
            let details = json!({
                "original_github_login": orig_author,
                "rewriter_github_login": rewriter,
                "statements_affected": count,
            })
            .to_string();
            conn.execute(
                "INSERT INTO cosmetic_rewrites
                 (sprint_id, file_path, repo_full_name,
                  original_author_id, rewriter_id,
                  statements_affected, change_type, details)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    cur_sid,
                    file_path,
                    repo,
                    orig_student.unwrap_or(orig_author),
                    rewriter_student.unwrap_or(rewriter),
                    count,
                    "variable_rename",
                    details,
                ],
            )?;
            total += count;
        }
    }

    info!(total, "Detected cosmetic rewrite instances");
    Ok(())
}

fn login_to_student_id(conn: &Connection, github_login: &str) -> rusqlite::Result<Option<String>> {
    let row = conn
        .query_row(
            "SELECT id FROM students WHERE github_login = ?",
            [github_login],
            |r| r.get::<_, String>(0),
        )
        .ok();
    Ok(row)
}
