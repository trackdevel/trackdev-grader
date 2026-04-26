//! Sigmoid-based PR regularity scoring. Mirrors `src/process/regularity.py`.

use rusqlite::{params, Connection};
use tracing::info;

use sprint_grader_core::config::RegularityConfig;
use sprint_grader_core::stats::round_half_even;
use sprint_grader_core::time::parse_iso;

pub fn sigmoid_regularity(hours_before_deadline: f64, config: &RegularityConfig) -> f64 {
    if hours_before_deadline <= 0.0 {
        return config.after_deadline_score;
    }
    let x = hours_before_deadline - config.midpoint_hours;
    let score = 1.0 / (1.0 + (-config.steepness * x).exp());
    score.clamp(0.0, 1.0)
}

pub fn classify_band(score: f64, config: &RegularityConfig) -> &'static str {
    if score >= config.excellent_threshold {
        "excellent"
    } else if score >= config.good_threshold {
        "good"
    } else if score >= config.late_threshold {
        "late"
    } else if score >= config.cramming_threshold {
        "cramming"
    } else {
        "last_minute"
    }
}

pub fn compute_pr_regularity(
    conn: &Connection,
    pr_id: &str,
    config: &RegularityConfig,
) -> rusqlite::Result<bool> {
    let pr: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT merged_at, author_id FROM pull_requests WHERE id = ?",
            [pr_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .ok();
    let (merged_at, author_id) = match pr {
        Some((Some(m), a)) if !m.is_empty() => (m, a),
        _ => return Ok(false),
    };

    let sprint_row: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT sp.id, sp.end_date FROM sprints sp
             JOIN tasks t ON t.sprint_id = sp.id
             JOIN task_pull_requests tpr ON tpr.task_id = t.id
             WHERE tpr.pr_id = ? AND t.type != 'USER_STORY' LIMIT 1",
            [pr_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .ok();
    let (sprint_id, end_date) = match sprint_row {
        Some((s, Some(e))) if !e.is_empty() => (s, e),
        _ => return Ok(false),
    };

    let merged = match parse_iso(&merged_at) {
        Some(d) => d,
        None => return Ok(false),
    };
    let deadline = match parse_iso(&end_date) {
        Some(d) => d,
        None => return Ok(false),
    };

    let hours_before = (deadline - merged).num_seconds() as f64 / 3600.0;
    let score = sigmoid_regularity(hours_before, config);
    let band = classify_band(score, config);

    conn.execute(
        "INSERT OR REPLACE INTO pr_regularity
         (pr_id, sprint_id, student_id, merged_at, sprint_end,
          hours_before_deadline, regularity_score, regularity_band)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            pr_id,
            sprint_id,
            author_id,
            merged_at,
            end_date,
            round_half_even(hours_before, 2),
            round_half_even(score, 4),
            band,
        ],
    )?;
    Ok(true)
}

pub fn compute_student_regularity(
    conn: &Connection,
    student_id: &str,
    sprint_id: i64,
    config: &RegularityConfig,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT regularity_score, hours_before_deadline
         FROM pr_regularity WHERE student_id = ? AND sprint_id = ?",
    )?;
    let rows: Vec<(f64, f64)> = stmt
        .query_map(params![student_id, sprint_id], |r| {
            Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if rows.is_empty() {
        return Ok(());
    }
    let scores: Vec<f64> = rows.iter().map(|(s, _)| *s).collect();
    let hours: Vec<f64> = rows.iter().map(|(_, h)| *h).collect();

    let avg = scores.iter().sum::<f64>() / scores.len() as f64;
    let min = scores.iter().copied().fold(f64::INFINITY, f64::min);
    let pr_count = scores.len() as i64;
    let last_24h = hours.iter().filter(|h| **h < 24.0).count() as i64;
    let last_3h = hours.iter().filter(|h| **h < 3.0).count() as i64;
    let band = classify_band(avg, config);

    conn.execute(
        "INSERT OR REPLACE INTO student_sprint_regularity
         (student_id, sprint_id, avg_regularity, min_regularity,
          pr_count, prs_in_last_24h, prs_in_last_3h, regularity_band)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            student_id,
            sprint_id,
            round_half_even(avg, 4),
            round_half_even(min, 4),
            pr_count,
            last_24h,
            last_3h,
            band,
        ],
    )?;
    Ok(())
}

pub fn compute_all_regularity(
    conn: &Connection,
    sprint_id: i64,
    config: &RegularityConfig,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND pr.merged = 1 AND pr.merged_at IS NOT NULL",
    )?;
    let pr_ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut computed = 0;
    for id in &pr_ids {
        if compute_pr_regularity(conn, id, config)? {
            computed += 1;
        }
    }

    let mut stmt = conn.prepare(
        "SELECT DISTINCT student_id FROM pr_regularity
         WHERE sprint_id = ? AND student_id IS NOT NULL",
    )?;
    let students: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for sid in &students {
        compute_student_regularity(conn, sid, sprint_id, config)?;
    }
    info!(
        prs_scored = computed,
        students = students.len(),
        "regularity"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> RegularityConfig {
        RegularityConfig::default()
    }

    #[test]
    fn sigmoid_saturates_far_before_deadline() {
        let cfg = default_cfg();
        let s = sigmoid_regularity(500.0, &cfg);
        assert!(s > 0.99, "saturates high, got {s}");
    }

    #[test]
    fn sigmoid_crosses_half_at_midpoint() {
        let cfg = default_cfg();
        let s = sigmoid_regularity(cfg.midpoint_hours, &cfg);
        assert!((s - 0.5).abs() < 1e-9);
    }

    #[test]
    fn after_deadline_returns_configured_floor() {
        let cfg = default_cfg();
        assert!((sigmoid_regularity(0.0, &cfg) - cfg.after_deadline_score).abs() < 1e-9);
        assert!((sigmoid_regularity(-5.0, &cfg) - cfg.after_deadline_score).abs() < 1e-9);
    }

    #[test]
    fn bands_cover_full_range() {
        let cfg = default_cfg();
        assert_eq!(classify_band(0.95, &cfg), "excellent");
        assert_eq!(classify_band(0.60, &cfg), "good");
        assert_eq!(classify_band(0.30, &cfg), "late");
        assert_eq!(classify_band(0.10, &cfg), "cramming");
        assert_eq!(classify_band(0.01, &cfg), "last_minute");
    }
}
