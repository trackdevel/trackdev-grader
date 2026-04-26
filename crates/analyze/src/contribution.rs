//! Weighted contribution scoring per student per sprint.
//! Mirrors `src/analyze/contribution.py`.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use tracing::info;

use sprint_grader_core::stats::{mean, stddev_pop};

#[derive(Debug, Clone, Copy)]
pub struct ContributionWeights {
    pub code: f64,
    pub review: f64,
    pub task: f64,
    pub process: f64,
}

impl Default for ContributionWeights {
    fn default() -> Self {
        Self {
            code: 0.40,
            review: 0.20,
            task: 0.25,
            process: 0.15,
        }
    }
}

impl ContributionWeights {
    pub fn validate(self) -> Result<Self, String> {
        let total = self.code + self.review + self.task + self.process;
        if (total - 1.0).abs() > 0.01 {
            return Err(format!("Contribution weights must sum to 1.0, got {total}"));
        }
        Ok(self)
    }
}

/// Z-score normalize, then min-max scale to [0, 1]. Matches Python exactly.
fn normalize_within_team(values: &HashMap<String, f64>) -> HashMap<String, f64> {
    if values.is_empty() {
        return HashMap::new();
    }
    let vec: Vec<f64> = values.values().copied().collect();
    let m = mean(&vec);
    let std = stddev_pop(&vec);
    if std == 0.0 {
        return values.keys().map(|k| (k.clone(), 0.5)).collect();
    }
    let zs: HashMap<String, f64> = values
        .iter()
        .map(|(k, v)| (k.clone(), (v - m) / std))
        .collect();
    let z_min = zs.values().copied().fold(f64::INFINITY, f64::min);
    let z_max = zs.values().copied().fold(f64::NEG_INFINITY, f64::max);
    let z_range = z_max - z_min;
    if z_range == 0.0 {
        return values.keys().map(|k| (k.clone(), 0.5)).collect();
    }
    zs.into_iter()
        .map(|(k, v)| (k, (v - z_min) / z_range))
        .collect()
}

fn get_team_members(conn: &Connection, project_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
    let rows = stmt.query_map([project_id], |r| r.get::<_, String>(0))?;
    rows.collect()
}

struct TeamSignals {
    code: HashMap<String, f64>,
    review: HashMap<String, f64>,
    task: HashMap<String, f64>,
    process: HashMap<String, f64>,
}

/// One pass over the DB, yielding all four raw signals for every student in
/// `member_ids`. Replaces four per-student query loops (four SQLs × N members)
/// with five aggregate queries total, regardless of team size.
fn gather_signals(
    conn: &Connection,
    sprint_id: i64,
    member_ids: &[String],
) -> rusqlite::Result<TeamSignals> {
    let zero = || -> HashMap<String, f64> { member_ids.iter().map(|s| (s.clone(), 0.0)).collect() };
    let mut code = zero();
    let mut review = zero();
    let mut task = zero();
    let mut process = zero();

    let member_set: std::collections::HashSet<&str> =
        member_ids.iter().map(String::as_str).collect();

    // --- survival + PR lines (code signal) ---
    {
        let mut surviving: HashMap<String, i64> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT student_id, COALESCE(surviving_stmts_normalized, 0)
             FROM student_sprint_survival
             WHERE sprint_id = ?",
        )?;
        for row in stmt.query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (sid, v) = row?;
            if member_set.contains(sid.as_str()) {
                surviving.insert(sid, v);
            }
        }
        drop(stmt);

        let mut pr_lines: HashMap<String, i64> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT pr.author_id, COALESCE(SUM(pr.additions + pr.deletions), 0)
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
               AND pr.additions IS NOT NULL AND pr.author_id IS NOT NULL
             GROUP BY pr.author_id",
        )?;
        for row in stmt.query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (sid, v) = row?;
            if member_set.contains(sid.as_str()) {
                pr_lines.insert(sid, v);
            }
        }
        drop(stmt);

        for sid in member_ids {
            let surv = surviving.get(sid).copied().unwrap_or(0);
            let lines = pr_lines.get(sid).copied().unwrap_or(0);
            let value = if surv > 0 { surv } else { lines } as f64;
            code.insert(sid.clone(), value);
        }
    }

    // --- login → student_id map (shared by review + process) ---
    let mut login_to_sid: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, github_login FROM students WHERE team_project_id IN
                          (SELECT project_id FROM sprints WHERE id = ?)",
        )?;
        for row in stmt.query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })? {
            let (sid, login) = row?;
            if let Some(login) = login {
                if member_set.contains(sid.as_str()) {
                    login_to_sid.insert(login.to_lowercase(), sid);
                }
            }
        }
        drop(stmt);
    }

    // --- review signal: count PR reviews per reviewer_login ---
    {
        let mut stmt = conn.prepare(
            "SELECT LOWER(rv.reviewer_login), COUNT(*)
             FROM pr_reviews rv
             JOIN pull_requests pr ON pr.id = rv.pr_id
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
             GROUP BY LOWER(rv.reviewer_login)",
        )?;
        for row in stmt.query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (login_lc, cnt) = row?;
            if let Some(sid) = login_to_sid.get(&login_lc) {
                review.insert(sid.clone(), cnt as f64);
            }
        }
        drop(stmt);
    }

    // --- task signal: DONE estimation points per assignee ---
    {
        let mut stmt = conn.prepare(
            "SELECT assignee_id, COALESCE(SUM(estimation_points), 0)
             FROM tasks
             WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'
               AND assignee_id IS NOT NULL
             GROUP BY assignee_id",
        )?;
        for row in stmt.query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (sid, pts) = row?;
            if member_set.contains(sid.as_str()) {
                task.insert(sid, pts as f64);
            }
        }
        drop(stmt);
    }

    // --- process signal: distinct active-commit days per author_login ---
    {
        let mut stmt = conn.prepare(
            "SELECT LOWER(pc.author_login), COUNT(DISTINCT DATE(pc.timestamp))
             FROM pr_commits pc
             JOIN pull_requests pr ON pr.id = pc.pr_id
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
               AND pc.author_login IS NOT NULL
             GROUP BY LOWER(pc.author_login)",
        )?;
        for row in stmt.query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (login_lc, days) = row?;
            if let Some(sid) = login_to_sid.get(&login_lc) {
                process.insert(sid.clone(), days as f64);
            }
        }
        drop(stmt);
    }

    Ok(TeamSignals {
        code,
        review,
        task,
        process,
    })
}

pub fn compute_all_contributions(
    conn: &Connection,
    sprint_id: i64,
    weights: Option<ContributionWeights>,
) -> rusqlite::Result<()> {
    let w = weights.unwrap_or_default();

    let mut stmt = conn.prepare("SELECT DISTINCT project_id FROM sprints WHERE id = ?")?;
    let project_ids: Vec<i64> = stmt
        .query_map([sprint_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    for pid in project_ids {
        let members = get_team_members(conn, pid)?;
        if members.len() < 2 {
            continue;
        }

        let signals = gather_signals(conn, sprint_id, &members)?;

        let code_n = normalize_within_team(&signals.code);
        let review_n = normalize_within_team(&signals.review);
        let task_n = normalize_within_team(&signals.task);
        let proc_n = normalize_within_team(&signals.process);

        let mut composites: HashMap<String, f64> = HashMap::new();
        for sid in &members {
            let c = w.code * code_n.get(sid).copied().unwrap_or(0.5)
                + w.review * review_n.get(sid).copied().unwrap_or(0.5)
                + w.task * task_n.get(sid).copied().unwrap_or(0.5)
                + w.process * proc_n.get(sid).copied().unwrap_or(0.5);
            composites.insert(sid.clone(), c);
        }

        let vals: Vec<f64> = composites.values().copied().collect();
        let m = mean(&vals);
        let std = stddev_pop(&vals);
        let std = if std > 0.0 { std } else { 1.0 };

        let mut ranked: Vec<(String, f64)> =
            composites.iter().map(|(k, v)| (k.clone(), *v)).collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let ranks: HashMap<String, i64> = ranked
            .iter()
            .enumerate()
            .map(|(i, (sid, _))| (sid.clone(), (i + 1) as i64))
            .collect();

        for sid in &members {
            let composite = composites.get(sid).copied().unwrap_or(0.0);
            conn.execute(
                "INSERT OR REPLACE INTO student_sprint_contribution
                 (student_id, sprint_id, code_signal, review_signal, task_signal,
                  process_signal, composite_score, team_rank, z_score_from_mean)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    sid,
                    sprint_id,
                    code_n.get(sid).copied().unwrap_or(0.5),
                    review_n.get(sid).copied().unwrap_or(0.5),
                    task_n.get(sid).copied().unwrap_or(0.5),
                    proc_n.get(sid).copied().unwrap_or(0.5),
                    composite,
                    ranks.get(sid).copied().unwrap_or(0),
                    (composite - m) / std,
                ],
            )?;
        }
        info!(
            project_id = pid,
            top = %ranked.first().map(|(k, _)| k.as_str()).unwrap_or(""),
            bottom = %ranked.last().map(|(k, _)| k.as_str()).unwrap_or(""),
            "contribution scores"
        );
    }
    Ok(())
}
