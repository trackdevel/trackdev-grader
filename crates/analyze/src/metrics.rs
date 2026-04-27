//! Per-student metric computation. Mirrors `src/analyze/metrics.py::_compute_for_sprint_id`.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use sprint_grader_core::time::parse_iso;
use tracing::info;

use crate::pr_weight::distribute_pr_weights_for_sprint;

/// Builds the `{"early": N, "mid": N, "late": N, "cramming": N}` JSON blob.
///
/// Serialised by hand so key order (insertion, not alphabetical) and
/// whitespace (`", "` / `": "`) match Python's `json.dumps` default output
/// byte-for-byte — the checksum-match in `student_sprint_metrics` depends
/// on it, and swapping `serde_json` formatting via features is heavier than
/// a four-key `format!`.
fn temporal_spread_json(early: i64, mid: i64, late: i64, cramming: i64) -> String {
    format!(
        "{{\"early\": {}, \"mid\": {}, \"late\": {}, \"cramming\": {}}}",
        early, mid, late, cramming
    )
}

/// Per-task-assignee temporal spread. A commit by user A on a PR linked to
/// user B's task counts toward B. For per-AUTHOR temporal data (genuine
/// timing of an individual's commits), use `student_sprint_temporal`.
fn compute_task_temporal_spread(
    conn: &Connection,
    sprint_id: i64,
    student_id: &str,
    sprint_start: &str,
    sprint_end: &str,
    cramming_hours: u32,
) -> String {
    let zero = || temporal_spread_json(0, 0, 0, 0);
    let start_dt = match parse_iso(sprint_start) {
        Some(d) => d,
        None => return zero(),
    };
    let end_dt = match parse_iso(sprint_end) {
        Some(d) => d,
        None => return zero(),
    };
    let total_seconds = (end_dt - start_dt).num_seconds() as f64;
    if total_seconds <= 0.0 {
        return zero();
    }
    let third = total_seconds / 3.0;
    let cramming_boundary = end_dt.timestamp() as f64 - cramming_hours as f64 * 3600.0;

    let mut stmt = match conn.prepare(
        "SELECT pc.timestamp
         FROM pr_commits pc
         JOIN pull_requests pr ON pr.id = pc.pr_id
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.assignee_id = ? AND t.type != 'USER_STORY'
           AND pc.timestamp IS NOT NULL AND pc.timestamp != ''",
    ) {
        Ok(s) => s,
        Err(_) => return zero(),
    };
    let rows = match stmt.query_map(params![sprint_id, student_id], |r| r.get::<_, String>(0)) {
        Ok(r) => r,
        Err(_) => return zero(),
    };

    let mut counts: HashMap<&str, i64> =
        HashMap::from([("early", 0), ("mid", 0), ("late", 0), ("cramming", 0)]);

    for row in rows.flatten() {
        let ts = match parse_iso(&row) {
            Some(t) => t.timestamp() as f64,
            None => continue,
        };
        let bucket = if ts >= cramming_boundary {
            "cramming"
        } else if ts < start_dt.timestamp() as f64 + third {
            "early"
        } else if ts < start_dt.timestamp() as f64 + 2.0 * third {
            "mid"
        } else {
            "late"
        };
        *counts.get_mut(bucket).unwrap() += 1;
    }

    temporal_spread_json(
        counts["early"],
        counts["mid"],
        counts["late"],
        counts["cramming"],
    )
}

pub fn compute_metrics_for_sprint_id(
    conn: &Connection,
    sprint_id: i64,
    cramming_hours: u32,
) -> rusqlite::Result<()> {
    let sprint_row: Option<(i64, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT project_id, start_date, end_date FROM sprints WHERE id = ?",
            [sprint_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .ok();
    let (project_id, sprint_start, sprint_end) = match sprint_row {
        Some(s) => s,
        None => return Ok(()),
    };
    let sprint_start = sprint_start.unwrap_or_default();
    let sprint_end = sprint_end.unwrap_or_default();

    // student_sprint_metrics has no PRIMARY KEY declared in the schema, so
    // `INSERT OR REPLACE` degrades to plain INSERT and re-runs accumulate
    // duplicates. Mirror `flags.py`'s pattern and wipe the sprint's rows
    // before re-populating so the stage stays idempotent.
    conn.execute(
        "DELETE FROM student_sprint_metrics WHERE sprint_id = ?",
        [sprint_id],
    )?;

    // Weighted PR metrics: task_id → total weighted lines (additions + deletions).
    let weighted = distribute_pr_weights_for_sprint(conn, sprint_id)?;
    let mut task_weighted_lines: HashMap<i64, f64> = HashMap::new();
    for w in &weighted {
        *task_weighted_lines.entry(w.task_id).or_insert(0.0) += w.additions + w.deletions;
    }

    let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
    let student_ids: Vec<String> = stmt
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let team_total_pts: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
             WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'",
            [sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    for sid in &student_ids {
        let points: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
                 WHERE sprint_id = ? AND assignee_id = ? AND status = 'DONE'
                   AND type != 'USER_STORY'",
                params![sprint_id, sid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let points_share = if team_total_pts > 0 {
            points as f64 / team_total_pts as f64
        } else {
            0.0
        };

        // Weighted PR lines = sum of task_weighted_lines for this student's tasks.
        let mut task_stmt = conn.prepare(
            "SELECT id FROM tasks WHERE sprint_id = ? AND assignee_id = ? AND type != 'USER_STORY'",
        )?;
        let task_ids: Vec<i64> = task_stmt
            .query_map(params![sprint_id, sid], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(task_stmt);
        let weighted_lines: f64 = task_ids
            .iter()
            .map(|tid| task_weighted_lines.get(tid).copied().unwrap_or(0.0))
            .sum();

        let commit_count: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT pc.sha)
                 FROM pr_commits pc
                 JOIN pull_requests pr ON pr.id = pc.pr_id
                 JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                 JOIN tasks t ON t.id = tpr.task_id
                 WHERE t.sprint_id = ? AND t.assignee_id = ? AND t.type != 'USER_STORY'",
                params![sprint_id, sid],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let files_touched: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(pr.changed_files), 0)
                 FROM pull_requests pr
                 JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                 JOIN tasks t ON t.id = tpr.task_id
                 WHERE t.sprint_id = ? AND t.assignee_id = ? AND t.type != 'USER_STORY'",
                params![sprint_id, sid],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let reviews_given: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pr_reviews r
                 JOIN students s ON LOWER(s.github_login) = LOWER(r.reviewer_login)
                 WHERE s.id = ? AND r.pr_id IN (
                     SELECT DISTINCT pr.id FROM pull_requests pr
                     JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                     JOIN tasks t ON t.id = tpr.task_id
                     WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
                 )",
                params![sid, sprint_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let temporal = compute_task_temporal_spread(
            conn,
            sprint_id,
            sid,
            &sprint_start,
            &sprint_end,
            cramming_hours,
        );

        let avg_doc_score: Option<f64> = conn
            .query_row(
                "SELECT AVG(total_doc_score) FROM pr_doc_evaluation pde
                 JOIN pull_requests pr ON pr.id = pde.pr_id
                 JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                 JOIN tasks t ON t.id = tpr.task_id
                 WHERE t.sprint_id = ? AND t.assignee_id = ? AND t.type != 'USER_STORY'",
                params![sprint_id, sid],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten();

        conn.execute(
            "INSERT OR REPLACE INTO student_sprint_metrics
             (student_id, sprint_id, points_delivered, points_share,
              weighted_pr_lines, commit_count, files_touched,
              reviews_given, temporal_spread, avg_doc_score)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                sid,
                sprint_id,
                points,
                points_share,
                weighted_lines,
                commit_count,
                files_touched,
                reviews_given,
                temporal,
                avg_doc_score,
            ],
        )?;
    }

    info!(sprint_id, students = student_ids.len(), "metrics computed");
    Ok(())
}
