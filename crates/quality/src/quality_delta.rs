//! Sprint-over-sprint quality deltas. Mirrors `src/quality/quality_delta.py`.

use rusqlite::{params, Connection};
use tracing::info;

use sprint_grader_core::stats::mean;

type MethodMetricsRow = (Option<i64>, Option<i64>, Option<i64>, Option<f64>);
type AvgQualityTuple = (
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
);
type PrevQualityRow = (Option<f64>, Option<f64>, Option<f64>, Option<f64>);

pub fn compute_student_quality(
    conn: &Connection,
    student_id: &str,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT cyclomatic_complexity, cognitive_complexity, loc, maintainability_index
         FROM method_metrics
         WHERE author_id = ? AND sprint_id = ?",
    )?;
    let rows: Vec<MethodMetricsRow> = stmt
        .query_map(params![student_id, sprint_id], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<f64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let (avg_cc, avg_cog, avg_loc, pct_cc_10, avg_mi): AvgQualityTuple = if rows.is_empty() {
        (None, None, None, None, None)
    } else {
        let n = rows.len() as f64;
        let ccs: Vec<f64> = rows.iter().map(|r| r.0.unwrap_or(0) as f64).collect();
        let cogs: Vec<f64> = rows.iter().map(|r| r.1.unwrap_or(0) as f64).collect();
        let locs: Vec<f64> = rows.iter().map(|r| r.2.unwrap_or(0) as f64).collect();
        let mis: Vec<f64> = rows.iter().filter_map(|r| r.3).collect();
        let pct = ccs.iter().filter(|c| **c > 10.0).count() as f64 / n;
        let mi = if mis.is_empty() {
            None
        } else {
            Some(mean(&mis))
        };
        (
            Some(mean(&ccs)),
            Some(mean(&cogs)),
            Some(mean(&locs)),
            Some(pct),
            mi,
        )
    };

    let satd_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM satd_items WHERE author_id = ? AND sprint_id = ?",
            params![student_id, sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let test_loc: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(loc), 0) FROM method_metrics
             WHERE author_id = ? AND sprint_id = ?
                   AND (file_path LIKE '%Test%' OR file_path LIKE '%test%')",
            params![student_id, sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let prod_loc: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(loc), 0) FROM method_metrics
             WHERE author_id = ? AND sprint_id = ?
                   AND file_path NOT LIKE '%Test%' AND file_path NOT LIKE '%test%'",
            params![student_id, sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let test_ratio = if prod_loc > 0 {
        test_loc as f64 / prod_loc as f64
    } else {
        0.0
    };

    // Previous-sprint aggregates for the delta columns.
    let prev: Option<PrevQualityRow> = conn
        .query_row(
            "SELECT sq.avg_cc, sq.avg_cognitive_complexity, sq.pct_methods_cc_over_10,
                    sq.avg_maintainability
             FROM student_sprint_quality sq
             JOIN sprints sp ON sp.id = sq.sprint_id
             JOIN sprints sp_curr ON sp_curr.id = ?
             WHERE sq.student_id = ? AND sp.start_date < sp_curr.start_date
             ORDER BY sp.start_date DESC LIMIT 1",
            params![sprint_id, student_id],
            |r| {
                Ok((
                    r.get::<_, Option<f64>>(0)?,
                    r.get::<_, Option<f64>>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                ))
            },
        )
        .ok();

    let diff = |cur: Option<f64>, p: Option<Option<f64>>| -> Option<f64> {
        match (cur, p) {
            (Some(c), Some(Some(pv))) => Some(c - pv),
            _ => None,
        }
    };
    let (delta_cc, delta_cog, delta_pct, delta_mi) = match prev {
        Some((p_cc, p_cog, p_pct, p_mi)) => (
            diff(avg_cc, Some(p_cc)),
            diff(avg_cog, Some(p_cog)),
            diff(pct_cc_10, Some(p_pct)),
            diff(avg_mi, Some(p_mi)),
        ),
        None => (None, None, None, None),
    };

    conn.execute(
        "INSERT OR REPLACE INTO student_sprint_quality
         (student_id, sprint_id, avg_cc, avg_cognitive_complexity, avg_method_loc,
          pct_methods_cc_over_10, avg_maintainability, satd_count,
          satd_introduced, satd_removed, test_file_loc, test_to_code_ratio,
          delta_avg_cc, delta_avg_cognitive, delta_pct_cc_over_10, delta_maintainability)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            student_id,
            sprint_id,
            avg_cc,
            avg_cog,
            avg_loc,
            pct_cc_10,
            avg_mi,
            satd_count,
            // satd_introduced / satd_removed are filled by satd_delta
            None::<i64>,
            None::<i64>,
            test_loc,
            test_ratio,
            delta_cc,
            delta_cog,
            delta_pct,
            delta_mi,
        ],
    )?;
    Ok(())
}

pub fn compute_all_quality(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT author_id FROM method_metrics
         WHERE sprint_id = ? AND author_id IS NOT NULL",
    )?;
    let ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for sid in &ids {
        compute_student_quality(conn, sid, sprint_id)?;
    }
    info!(count = ids.len(), sprint_id, "quality deltas computed");
    Ok(())
}
