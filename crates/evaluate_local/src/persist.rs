//! Grid-snap quantisation and `pr_doc_evaluation` writes.
//!
//! Score levels match the rubric grid:
//! - title: 0.0 to 2.0 in 0.25 steps (9 levels)
//! - description: 0.0 to 4.0 in 0.25 steps (17 levels)
//!
//! The `update_avg_doc_score` body is intentionally mirrored from
//! `crates/evaluate/src/llm_eval.rs::update_avg_doc_score` (the source is
//! `pub(crate)`; cross-crate re-export would noisily widen its public
//! surface for a one-call duplication).

use rusqlite::{params, Connection};

pub const PR_TITLE_LEVELS: [f64; 9] = [0.0, 0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
pub const PR_DESC_LEVELS: [f64; 17] = [
    0.0, 0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 2.75, 3.0, 3.25, 3.5, 3.75, 4.0,
];

fn snap_to_grid(v: f64, levels: &[f64], lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        return lo;
    }
    let clamped = v.clamp(lo, hi);
    let mut best = levels[0];
    let mut best_dist = (best - clamped).abs();
    for &lvl in &levels[1..] {
        let d = (lvl - clamped).abs();
        if d < best_dist {
            best = lvl;
            best_dist = d;
        }
    }
    best
}

pub fn snap_title(v: f64) -> f64 {
    snap_to_grid(v, &PR_TITLE_LEVELS, 0.0, 2.0)
}

pub fn snap_description(v: f64) -> f64 {
    snap_to_grid(v, &PR_DESC_LEVELS, 0.0, 4.0)
}

/// One row of input to `write_pr_row`. Borrows the strings so callers
/// don't have to allocate when the values come from a SELECT result.
#[derive(Debug)]
pub struct PrPersistRow<'a> {
    pub pr_id: &'a str,
    pub sprint_id: i64,
    pub title_score: f64,
    pub description_score: f64,
    pub total_doc_score: f64,
    pub justification: String,
}

/// Insert a single row into `pr_doc_evaluation`. The caller's resume-guard
/// `NOT IN (SELECT pr_id ...)` filter is the only correctness mechanism
/// against duplicate insertion — the table has no declared PK, so
/// `INSERT OR REPLACE` degrades to plain INSERT here.
pub fn write_pr_row(conn: &Connection, row: &PrPersistRow<'_>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO pr_doc_evaluation
            (pr_id, sprint_id, title_score, description_score,
             total_doc_score, justification)
         VALUES (?, ?, ?, ?, ?, ?)",
        params![
            row.pr_id,
            row.sprint_id,
            row.title_score,
            row.description_score,
            row.total_doc_score,
            &row.justification,
        ],
    )?;
    Ok(())
}

/// Recompute `student_sprint_metrics.avg_doc_score` for every assignee in
/// this sprint. Mirrors `evaluate::llm_eval::update_avg_doc_score`; see
/// the module doc for why this is duplicated rather than re-exported.
pub fn update_avg_doc_score(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT t.assignee_id FROM tasks t
         WHERE t.sprint_id = ? AND t.assignee_id IS NOT NULL AND t.type != 'USER_STORY'",
    )?;
    let ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for sid in ids {
        let avg: Option<f64> = conn
            .query_row(
                "SELECT AVG(pde.total_doc_score) FROM pr_doc_evaluation pde
                 JOIN pull_requests pr ON pr.id = pde.pr_id
                 JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                 JOIN tasks t ON t.id = tpr.task_id
                 WHERE t.sprint_id = ? AND t.assignee_id = ? AND t.type != 'USER_STORY'",
                params![sprint_id, sid],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten();
        if let Some(a) = avg {
            conn.execute(
                "UPDATE student_sprint_metrics SET avg_doc_score = ?
                 WHERE student_id = ? AND sprint_id = ?",
                params![a, sid, sprint_id],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_title_clamps_then_snaps_to_quarter_grid() {
        assert_eq!(snap_title(0.0), 0.0);
        assert_eq!(snap_title(0.13), 0.25);
        assert_eq!(snap_title(0.12), 0.0);
        assert_eq!(snap_title(2.0), 2.0);
        // Clamp above 2.0.
        assert_eq!(snap_title(5.0), 2.0);
        // Clamp below 0.0.
        assert_eq!(snap_title(-0.5), 0.0);
    }

    #[test]
    fn snap_description_clamps_then_snaps_to_quarter_grid() {
        assert_eq!(snap_description(0.0), 0.0);
        assert_eq!(snap_description(4.0), 4.0);
        assert_eq!(snap_description(3.99), 4.0);
        assert_eq!(snap_description(3.85), 3.75);
        assert_eq!(snap_description(100.0), 4.0);
        assert_eq!(snap_description(-3.5), 0.0);
    }

    #[test]
    fn snap_nan_maps_to_lower_bound() {
        assert_eq!(snap_title(f64::NAN), 0.0);
        assert_eq!(snap_description(f64::NAN), 0.0);
    }

    #[test]
    fn snap_infinity_clamps_to_endpoints() {
        assert_eq!(snap_title(f64::INFINITY), 2.0);
        assert_eq!(snap_title(f64::NEG_INFINITY), 0.0);
        assert_eq!(snap_description(f64::INFINITY), 4.0);
        assert_eq!(snap_description(f64::NEG_INFINITY), 0.0);
    }
}
