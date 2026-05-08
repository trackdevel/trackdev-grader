//! Temporal distribution of commit patterns. Mirrors `src/process/temporal.py`.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Timelike, Utc};
use rusqlite::{params, Connection};
use sprint_grader_core::time::parse_iso;
use tracing::info;

pub fn shannon_entropy_normalized(counts: &[i64], max_bins: usize) -> f64 {
    let total: i64 = counts.iter().sum();
    if total == 0 || max_bins <= 1 {
        return 0.0;
    }
    let h_max = (max_bins as f64).log2();
    if h_max == 0.0 {
        return 0.0;
    }
    let mut h = 0.0;
    for &c in counts {
        if c > 0 {
            let p = c as f64 / total as f64;
            h -= p * p.log2();
        }
    }
    h / h_max
}

/// Day offset from `anchor`, floor-divided, matching Python's
/// `(ts - anchor).days`. `chrono::TimeDelta::num_days` truncates toward zero
/// instead, so a commit 3.5 days before `anchor` returns -3 under
/// `num_days` but -4 under this helper (and Python). Preserving Python's
/// semantics keeps `active_days` and day-bucket membership aligned with
/// the reference.
fn day_offset(ts: DateTime<Utc>, anchor: DateTime<Utc>) -> i64 {
    (ts - anchor).num_seconds().div_euclid(86_400)
}

pub fn compute_temporal_metrics(
    conn: &Connection,
    student_id: &str,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let sprint: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT start_date, end_date FROM sprints WHERE id = ?",
            [sprint_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .ok();
    let (start_s, end_s) = match sprint {
        Some((Some(s), Some(e))) => (s, e),
        _ => return Ok(()),
    };
    let start = match parse_iso(&start_s) {
        Some(d) => d,
        None => return Ok(()),
    };
    let end = match parse_iso(&end_s) {
        Some(d) => d,
        None => return Ok(()),
    };
    // Python's `(end - start).days` applies the same floor-divide semantics.
    let sprint_days = ((end - start).num_seconds().div_euclid(86_400).max(1)) as usize;

    // Match commits to the student via `student_github_identity` (resolved
    // from task-PR evidence). TrackDev's `students.github_login` is no
    // longer trusted, so a student with no resolved identity yet simply has
    // no attributed commits — the function continues and writes a zero row.
    let mut stmt = conn.prepare(
        "SELECT pc.timestamp FROM pr_commits pc
         JOIN pull_requests pr ON pr.id = pc.pr_id
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         JOIN student_github_identity sgi
              ON sgi.identity_kind = 'login'
             AND sgi.identity_value = LOWER(pc.author_login)
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND sgi.student_id = ?",
    )?;
    let rows: Vec<String> = stmt
        .query_map(params![sprint_id, student_id], |r| {
            r.get::<_, Option<String>>(0).map(|o| o.unwrap_or_default())
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let timestamps: Vec<DateTime<Utc>> = rows
        .iter()
        .filter(|s| !s.is_empty())
        .filter_map(|s| parse_iso(s))
        .collect();
    if timestamps.is_empty() {
        conn.execute(
            "INSERT OR REPLACE INTO student_sprint_temporal
             (student_id, sprint_id, commit_entropy, active_days, active_days_ratio,
              cramming_ratio, weekend_ratio, night_ratio, longest_gap_days,
              is_cramming, is_steady)
             VALUES (?, ?, 0, 0, 0, 0, 0, 0, ?, 0, 0)",
            params![student_id, sprint_id, sprint_days as i64],
        )?;
        return Ok(());
    }

    // Bucket by day offset from sprint start.
    let mut day_counts: BTreeMap<i64, i64> = BTreeMap::new();
    for ts in &timestamps {
        let d = day_offset(*ts, start);
        *day_counts.entry(d).or_insert(0) += 1;
    }

    let active_days = day_counts.len();
    let active_ratio = active_days as f64 / sprint_days as f64;
    let counts: Vec<i64> = (0..sprint_days)
        .map(|d| day_counts.get(&(d as i64)).copied().unwrap_or(0))
        .collect();
    let entropy = shannon_entropy_normalized(&counts, sprint_days);

    // Cramming: commits in final 25 % of sprint.
    let threshold_day = (sprint_days as f64 * 0.75) as i64;
    let cramming_commits: i64 = day_counts
        .iter()
        .filter(|(d, _)| **d >= threshold_day)
        .map(|(_, c)| *c)
        .sum();
    let cramming_ratio = cramming_commits as f64 / timestamps.len() as f64;

    let weekend_count = timestamps
        .iter()
        .filter(|ts| ts.weekday().number_from_monday() >= 6)
        .count();
    let weekend_ratio = weekend_count as f64 / timestamps.len() as f64;

    let night_count = timestamps.iter().filter(|ts| ts.hour() < 6).count();
    let night_ratio = night_count as f64 / timestamps.len() as f64;

    let sorted_days: Vec<i64> = day_counts.keys().copied().collect();
    let longest_gap = if sorted_days.len() >= 2 {
        let mut gap = 0i64;
        for i in 1..sorted_days.len() {
            let g = sorted_days[i] - sorted_days[i - 1];
            if g > gap {
                gap = g;
            }
        }
        gap as f64
    } else {
        sprint_days as f64
    };

    let is_cramming = cramming_ratio > 0.70;
    let is_steady = entropy > 0.7 && active_ratio > 0.5;

    conn.execute(
        "INSERT OR REPLACE INTO student_sprint_temporal
         (student_id, sprint_id, commit_entropy, active_days, active_days_ratio,
          cramming_ratio, weekend_ratio, night_ratio, longest_gap_days,
          is_cramming, is_steady)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            student_id,
            sprint_id,
            entropy,
            active_days as i64,
            active_ratio,
            cramming_ratio,
            weekend_ratio,
            night_ratio,
            longest_gap,
            is_cramming,
            is_steady,
        ],
    )?;
    Ok(())
}

pub fn compute_all_temporal(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.id FROM students s
         WHERE s.team_project_id IN (SELECT project_id FROM sprints WHERE id = ?)",
    )?;
    let ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for sid in &ids {
        compute_temporal_metrics(conn, sid, sprint_id)?;
    }
    info!(count = ids.len(), sprint_id, "temporal metrics computed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_is_zero_on_single_bucket() {
        let counts = vec![0, 10, 0, 0];
        assert!(shannon_entropy_normalized(&counts, 4).abs() < 1e-9);
    }

    #[test]
    fn entropy_is_one_on_uniform_distribution() {
        let counts = vec![5, 5, 5, 5];
        assert!((shannon_entropy_normalized(&counts, 4) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn entropy_is_zero_on_empty() {
        assert!(shannon_entropy_normalized(&[], 10).abs() < 1e-9);
        assert!(shannon_entropy_normalized(&[0, 0, 0], 3).abs() < 1e-9);
    }

    #[test]
    fn entropy_between_on_skewed_distribution() {
        let counts = vec![8, 1, 1, 1];
        let h = shannon_entropy_normalized(&counts, 4);
        assert!(h > 0.0 && h < 1.0, "expected 0 < h < 1, got {h}");
    }
}
