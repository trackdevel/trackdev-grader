//! 29 flag detectors. Mirrors `src/analyze/flags.py`.
//!
//! Each detector is a free function `fn(conn, sprint_id, ctx) -> Vec<Flag>`.
//! The dispatcher runs them all, logs per-detector counts, and persists rows.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tracing::{info, warn};

use sprint_grader_core::config::{Config, DetectorThresholdsConfig, ThresholdConfig};
use sprint_grader_core::stats::{median_upper, percentile_pos, round_half_even, stddev_pop};

#[allow(clippy::type_complexity)]
mod row_aliases {
    pub type PrAuthorRepoLines = (String, Option<i64>, Option<String>, Option<String>, i64);
    pub type PrAuthorRepoLogin = (
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    pub type PrAuthorRepoNum = (String, Option<i64>, Option<String>, Option<String>);
    pub type PrFingerprintRow = (
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    pub type CrossTeamRow = (
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    pub type PrReviewRow = (
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
        i64,
        i64,
    );
    pub type PrCommitsRow = (
        String,
        i64,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    pub type DoneTaskRow = (i64, Option<String>, Option<String>, Option<String>, i64);
    pub type DonePrFullRow = (
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        i64,
    );
    pub type FlagDetailRow = (
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
    );
    pub type StudentMetricRow = (i64, String, Option<f64>, Option<f64>, Option<f64>);
    pub type StudentFloatsRow = (
        String,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
    );
    pub type CompilationRow = (
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    pub type ApprovedBrokenRow = (String, Option<String>, Option<String>, Option<i64>);
    pub type SuspectFastTaskRow = (
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<String>,
    );
}
use row_aliases::*;

#[derive(Debug, Clone)]
pub struct Flag {
    pub student_id: String,
    pub flag_type: &'static str,
    pub severity: &'static str,
    pub details: Value,
}

// Detector tuning knobs were previously local consts here. They moved to
// `sprint_grader_core::config::DetectorThresholdsConfig` (T-P1.3) and are now
// loaded from `course.toml [detector_thresholds]`. Detectors take
// `&DetectorThresholdsConfig` directly.

fn round3(x: f64) -> f64 {
    round_half_even(x, 3)
}
fn round2(x: f64) -> f64 {
    round_half_even(x, 2)
}

// ---- Individual detectors ----

fn zero_tasks(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM students WHERE team_project_id IN
         (SELECT project_id FROM sprints WHERE id = ?)",
    )?;
    let ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for sid in ids {
        let done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE sprint_id = ? AND assignee_id = ? AND status = 'DONE' AND type != 'USER_STORY'",
            params![sprint_id, &sid],
            |r| r.get(0),
        ).unwrap_or(0);
        if done == 0 {
            flags.push(Flag {
                student_id: sid,
                flag_type: "ZERO_TASKS",
                severity: "CRITICAL",
                details: json!({"message": "Student completed 0 tasks this sprint"}),
            });
        }
    }
    Ok(flags)
}

fn carrying_team(
    conn: &Connection,
    sprint_id: i64,
    thresh: &ThresholdConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let project_id: Option<i64> = conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get(0),
        )
        .ok();
    let project_id = match project_id {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let team_total: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
             WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'",
            [sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if team_total == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
    let ids: Vec<String> = stmt
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for sid in ids {
        let pts: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
             WHERE sprint_id = ? AND assignee_id = ? AND status = 'DONE' AND type != 'USER_STORY'",
                params![sprint_id, &sid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let share = pts as f64 / team_total as f64;
        if share > thresh.carrying_team_pct {
            flags.push(Flag {
                student_id: sid,
                flag_type: "CARRYING_TEAM",
                severity: "WARNING",
                details: json!({
                    "points": pts,
                    "team_total": team_total,
                    "share": round3(share),
                    "threshold": thresh.carrying_team_pct,
                }),
            });
        }
    }
    Ok(flags)
}

fn contribution_imbalance(
    conn: &Connection,
    sprint_id: i64,
    thresh: &ThresholdConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let project_id: Option<i64> = conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get(0),
        )
        .ok();
    let project_id = match project_id {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
    let ids: Vec<String> = stmt
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let n = ids.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    let expected = 1.0 / n as f64;
    let mut shares: Vec<(String, f64)> = Vec::new();
    for sid in &ids {
        let share: f64 = conn
            .query_row(
                "SELECT points_share FROM student_sprint_metrics WHERE student_id = ? AND sprint_id = ?",
                params![sid, sprint_id],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten()
            .unwrap_or(0.0);
        shares.push((sid.clone(), share));
    }
    let vals: Vec<f64> = shares.iter().map(|(_, s)| *s).collect();
    if vals.is_empty() {
        return Ok(Vec::new());
    }
    let std = stddev_pop(&vals);
    let mut flags = Vec::new();
    if std > 0.0 {
        for (sid, share) in shares {
            let z = (share - expected).abs() / std;
            if z > thresh.contribution_imbalance_stddev {
                flags.push(Flag {
                    student_id: sid,
                    flag_type: "CONTRIBUTION_IMBALANCE",
                    severity: "WARNING",
                    details: json!({
                        "share": round3(share),
                        "expected": round3(expected),
                        "z_score": round2(z),
                    }),
                });
            }
        }
    }
    Ok(flags)
}

fn low_code_high_points(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT student_id, points_delivered, weighted_pr_lines
         FROM student_sprint_metrics WHERE sprint_id = ?",
    )?;
    let rows: Vec<(String, i64, f64)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if rows.len() < 2 {
        return Ok(Vec::new());
    }
    let pts: Vec<f64> = rows.iter().map(|(_, p, _)| *p as f64).collect();
    let lines: Vec<f64> = rows.iter().map(|(_, _, l)| *l).collect();
    let pts_median = median_upper(&pts);
    let lines_p25 = percentile_pos(&lines, 1, 4);
    let mut flags = Vec::new();
    for (sid, p, l) in rows {
        if (p as f64) > pts_median && l < lines_p25 {
            flags.push(Flag {
                student_id: sid,
                flag_type: "LOW_CODE_HIGH_POINTS",
                severity: "WARNING",
                details: json!({
                    "points": p,
                    "weighted_lines": round_to(l, 1),
                    "team_pts_median": pts_median,
                    "team_lines_p25": round_to(lines_p25, 1),
                }),
            });
        }
    }
    Ok(flags)
}

fn round_to(x: f64, digits: u32) -> f64 {
    round_half_even(x, digits)
}

fn team_inequality_is_material_outlier(value: f64, average: f64, deviation: f64) -> bool {
    if average.abs() < f64::EPSILON {
        return value.abs() > f64::EPSILON;
    }
    ((value - average).abs() / average) >= deviation
}

fn point_code_mismatch(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT student_id, points_share, weighted_pr_lines
         FROM student_sprint_metrics WHERE sprint_id = ?",
    )?;
    let rows: Vec<(String, f64, f64)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let total_lines: f64 = rows.iter().map(|(_, _, l)| *l).sum();
    if total_lines == 0.0 {
        return Ok(Vec::new());
    }
    let mut flags = Vec::new();
    for (sid, pts_share, lines) in rows {
        let code_share = lines / total_lines;
        let gap = (pts_share - code_share).abs();
        if gap > 0.25 {
            flags.push(Flag {
                student_id: sid,
                flag_type: "POINT_CODE_MISMATCH",
                severity: "INFO",
                details: json!({
                    "points_share": round3(pts_share),
                    "code_share": round3(code_share),
                    "gap": round3(gap),
                }),
            });
        }
    }
    Ok(flags)
}

// Reads `student_sprint_temporal` (per-author, keyed on the commit author's
// github_login, populated by the `temporal` process stage). Previously this
// detector read `student_sprint_metrics.temporal_spread`, which is keyed on
// `task.assignee_id` — so a teammate's late-night commits on Bob's task
// flagged Bob, not the actual late-night committer. See T-P1.1.
fn cramming(
    conn: &Connection,
    sprint_id: i64,
    thresh: &ThresholdConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT student_id, cramming_ratio FROM student_sprint_temporal WHERE sprint_id = ?",
    )?;
    let rows: Vec<(String, Option<f64>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, ratio) in rows {
        let cramming_ratio = match ratio {
            Some(r) => r,
            None => continue,
        };
        if cramming_ratio > thresh.cramming_commit_pct {
            flags.push(Flag {
                student_id: sid,
                flag_type: "CRAMMING",
                severity: "WARNING",
                details: json!({
                    "cramming_ratio": round3(cramming_ratio),
                    "threshold": thresh.cramming_commit_pct,
                }),
            });
        }
    }
    Ok(flags)
}

fn micro_prs(
    conn: &Connection,
    sprint_id: i64,
    thresh: &ThresholdConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT t.assignee_id FROM tasks t
         WHERE t.sprint_id = ? AND t.assignee_id IS NOT NULL AND t.type != 'USER_STORY'",
    )?;
    let ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for sid in ids {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(pr.additions, 0) + COALESCE(pr.deletions, 0) as total_lines
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.assignee_id = ? AND t.type != 'USER_STORY'",
        )?;
        let prs: Vec<i64> = stmt
            .query_map(params![sprint_id, &sid], |r| {
                Ok(r.get::<_, Option<i64>>(0)?.unwrap_or(0))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let micro_count = prs
            .iter()
            .filter(|&&x| x <= thresh.micro_pr_max_lines as i64)
            .count();
        if micro_count >= 3 && !prs.is_empty() && micro_count as f64 / prs.len() as f64 > 0.5 {
            flags.push(Flag {
                student_id: sid,
                flag_type: "MICRO_PRS",
                severity: "INFO",
                details: json!({
                    "micro_prs": micro_count,
                    "total_prs": prs.len(),
                    "threshold_lines": thresh.micro_pr_max_lines,
                }),
            });
        }
    }
    Ok(flags)
}

fn single_commit_dump(
    conn: &Connection,
    sprint_id: i64,
    thresh: &ThresholdConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.author_id,
                COALESCE(pr.additions, 0) + COALESCE(pr.deletions, 0) as total_lines
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    let rows: Vec<PrAuthorRepoLines> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (pr_id, pr_number, repo, author_id, total_lines) in rows {
        let commit_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pr_commits WHERE pr_id = ?",
                [&pr_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if commit_count == 1 && total_lines > thresh.single_commit_dump_lines as i64 {
            let sid: Option<String> = conn
                .query_row(
                    "SELECT t.assignee_id FROM tasks t
                     JOIN task_pull_requests tpr ON tpr.task_id = t.id
                     WHERE tpr.pr_id = ? AND t.sprint_id = ? AND t.type != 'USER_STORY' LIMIT 1",
                    params![&pr_id, sprint_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .or(author_id);
            if let Some(sid) = sid {
                flags.push(Flag {
                    student_id: sid,
                    flag_type: "SINGLE_COMMIT_DUMP",
                    severity: "WARNING",
                    details: json!({
                        "pr_number": pr_number,
                        "repo": repo,
                        "total_lines": total_lines,
                        "threshold": thresh.single_commit_dump_lines,
                    }),
                });
            }
        }
    }
    Ok(flags)
}

fn no_reviews_received(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY' AND pr.merged = 1",
    )?;
    let rows: Vec<(String, Option<i64>, Option<String>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (pr_id, pr_number, repo) in rows {
        let reviews: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pr_reviews WHERE pr_id = ?",
                [&pr_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if reviews == 0 {
            let sid: Option<String> = conn
                .query_row(
                    "SELECT t.assignee_id FROM tasks t
                     JOIN task_pull_requests tpr ON tpr.task_id = t.id
                     WHERE tpr.pr_id = ? AND t.sprint_id = ? AND t.type != 'USER_STORY' LIMIT 1",
                    params![&pr_id, sprint_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();
            if let Some(sid) = sid {
                flags.push(Flag {
                    student_id: sid,
                    flag_type: "NO_REVIEWS_RECEIVED",
                    severity: "INFO",
                    details: json!({"pr_number": pr_number, "repo": repo}),
                });
            }
        }
    }
    Ok(flags)
}

fn author_mismatch(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name,
                pr.author_id, pr.github_author_login
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    let rows: Vec<PrAuthorRepoLogin> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut flags = Vec::new();
    for (pr_id, pr_number, repo, author_id, gh_author) in rows {
        let mut pr_author_login = gh_author;
        if pr_author_login.is_none() {
            if let Some(aid) = &author_id {
                pr_author_login = conn
                    .query_row(
                        "SELECT github_login FROM students WHERE id = ?",
                        [aid],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten();
            }
        }
        let pr_author_lower = match pr_author_login {
            Some(l) => l.to_lowercase(),
            None => continue,
        };

        // T-P1.4: prefer pr_pre_squash_authors (captured at collect time
        // before any future force-push erases per-commit history). If the
        // pre-squash table has rows for this PR, treat them as authoritative;
        // otherwise fall back to pr_commits.
        let pre_squash: Vec<Option<String>> = {
            let mut stmt =
                conn.prepare("SELECT author_login FROM pr_pre_squash_authors WHERE pr_id = ?")?;
            let rows = stmt
                .query_map([&pr_id], |r| r.get::<_, Option<String>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            rows
        };
        let commits: Vec<Option<String>> = if pre_squash.is_empty() {
            let mut stmt = conn.prepare("SELECT author_login FROM pr_commits WHERE pr_id = ?")?;
            let rows = stmt
                .query_map([&pr_id], |r| r.get::<_, Option<String>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            rows
        } else {
            pre_squash
        };
        let mismatched: std::collections::BTreeSet<String> = commits
            .into_iter()
            .flatten()
            .filter(|a| a.to_lowercase() != pr_author_lower)
            .collect();
        if !mismatched.is_empty() {
            let student_id = author_id.unwrap_or_else(|| "UNKNOWN".to_string());
            flags.push(Flag {
                student_id,
                flag_type: "AUTHOR_MISMATCH",
                severity: "WARNING",
                details: json!({
                    "pr_number": pr_number,
                    "repo": repo,
                    "pr_author": pr_author_lower,
                    "commit_authors": mismatched,
                }),
            });
        }
    }
    Ok(flags)
}

fn orphan_pr(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.repo_full_name FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    let repos: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, Option<String>>(0))?
        .filter_map(Result::ok)
        .flatten()
        .collect();
    drop(stmt);
    if repos.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: String = std::iter::repeat("?")
        .take(repos.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT pr.id, pr.pr_number, pr.repo_full_name, pr.author_id
         FROM pull_requests pr
         WHERE pr.repo_full_name IN ({placeholders}) AND pr.merged = 1
           AND pr.id NOT IN (SELECT pr_id FROM task_pull_requests)"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params_vec: Vec<&dyn rusqlite::ToSql> =
        repos.iter().map(|r| r as &dyn rusqlite::ToSql).collect();
    let rows: Vec<PrAuthorRepoNum> = stmt
        .query_map(&params_vec[..], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut flags = Vec::new();
    for (_id, pr_number, repo, author_id) in rows {
        if let Some(sid) = author_id {
            flags.push(Flag {
                student_id: sid,
                flag_type: "ORPHAN_PR",
                severity: "INFO",
                details: json!({
                    "pr_number": pr_number,
                    "repo": repo,
                    "message": "Merged PR not linked to any task in TrackDev",
                }),
            });
        }
    }
    Ok(flags)
}

fn foreign_merge(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT t.task_key, t.assignee_id,
                pr.pr_number, pr.repo_full_name, pr.author_id
         FROM tasks t
         JOIN task_pull_requests tpr ON tpr.task_id = t.id
         JOIN pull_requests pr ON pr.id = tpr.pr_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND t.status = 'DONE' AND pr.merged = 1",
    )?;
    let rows: Vec<PrFingerprintRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (task_key, assignee_id, pr_number, repo, author_id) in rows {
        if let (Some(aid), Some(authoraid)) = (assignee_id.as_ref(), author_id.as_ref()) {
            if aid != authoraid {
                flags.push(Flag {
                    student_id: aid.clone(),
                    flag_type: "FOREIGN_MERGE",
                    severity: "INFO",
                    details: json!({
                        "task_key": task_key,
                        "pr_number": pr_number,
                        "repo": repo,
                        "task_owner": aid,
                        "pr_author": authoraid,
                    }),
                });
            }
        }
    }
    Ok(flags)
}

fn unknown_contributor(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    use std::collections::{BTreeMap, BTreeSet};
    // Known logins.
    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut stmt =
        conn.prepare("SELECT github_login FROM students WHERE github_login IS NOT NULL")?;
    for s in stmt.query_map([], |r| r.get::<_, String>(0))?.flatten() {
        known.insert(s.to_lowercase());
    }
    drop(stmt);
    let mut stmt = conn.prepare("SELECT login FROM github_users WHERE student_id IS NOT NULL")?;
    for s in stmt.query_map([], |r| r.get::<_, String>(0))?.flatten() {
        known.insert(s.to_lowercase());
    }
    drop(stmt);

    let mut unknowns: BTreeMap<String, Vec<Value>> = BTreeMap::new();

    // Commit authors.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pc.author_login, pr.repo_full_name, pr.pr_number
         FROM pr_commits pc
         JOIN pull_requests pr ON pr.id = pc.pr_id
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND pc.author_login IS NOT NULL",
    )?;
    for row in stmt.query_map([sprint_id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })? {
        let (login, repo, pr_number) = row?;
        if let Some(login) = login {
            if !known.contains(&login.to_lowercase()) {
                unknowns
                    .entry(login)
                    .or_default()
                    .push(json!({"repo": repo, "pr_number": pr_number, "role": "commit_author"}));
            }
        }
    }
    drop(stmt);

    // PR authors + mergers.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.github_author_login, pr.merged_by_login,
                pr.repo_full_name, pr.pr_number
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    for row in stmt.query_map([sprint_id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })? {
        let (gh_login, merged_by, repo, pr_number) = row?;
        for (login, role) in [(gh_login, "pr_author"), (merged_by, "merger")] {
            if let Some(login) = login {
                if !known.contains(&login.to_lowercase()) {
                    unknowns
                        .entry(login)
                        .or_default()
                        .push(json!({"repo": repo, "pr_number": pr_number, "role": role}));
                }
            }
        }
    }
    drop(stmt);

    let mut flags = Vec::new();
    for (login, mut occs) in unknowns {
        occs.truncate(5);
        let mut details = serde_json::Map::new();
        details.insert("github_login".into(), json!(login));
        details.insert("occurrences".into(), Value::Array(occs));
        let profile: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT name, email FROM github_users WHERE login = ?",
                [&login],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .ok();
        if let Some((name, email)) = profile {
            if let Some(n) = name {
                details.insert("github_name".into(), json!(n));
            }
            if let Some(e) = email {
                details.insert("github_email".into(), json!(e));
            }
        }
        flags.push(Flag {
            student_id: "UNKNOWN".to_string(),
            flag_type: "UNKNOWN_CONTRIBUTOR",
            severity: "WARNING",
            details: Value::Object(details),
        });
    }
    Ok(flags)
}

fn low_survival_rate(
    conn: &Connection,
    sprint_id: i64,
    thresh: &ThresholdConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let project_id: Option<i64> = conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get(0),
        )
        .ok();
    let project_id = match project_id {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let mut stmt = conn.prepare(
        "SELECT sss.student_id, sss.survival_rate_normalized
         FROM student_sprint_survival sss
         JOIN students s ON s.id = sss.student_id
         WHERE sss.sprint_id = ? AND s.team_project_id = ?",
    )?;
    let rows: Vec<(String, f64)> = stmt
        .query_map(params![sprint_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if rows.len() < 2 {
        return Ok(Vec::new());
    }
    let rates: Vec<f64> = rows.iter().map(|(_, r)| *r).collect();
    let m = rates.iter().sum::<f64>() / rates.len() as f64;
    let std = stddev_pop(&rates);
    let mut flags = Vec::new();
    if std > 0.0 {
        for (sid, rate) in rows {
            let z = (m - rate) / std;
            if z > thresh.low_survival_rate_stddev && rate < thresh.low_survival_absolute_floor {
                flags.push(Flag {
                    student_id: sid,
                    flag_type: "LOW_SURVIVAL_RATE",
                    severity: "WARNING",
                    details: json!({
                        "rate": round3(rate),
                        "team_avg": round3(m),
                        "z_score": round2(z),
                        "absolute_floor": thresh.low_survival_absolute_floor,
                    }),
                });
            }
        }
    }
    Ok(flags)
}

fn raw_normalized_divergence(
    conn: &Connection,
    sprint_id: i64,
    thresh: &ThresholdConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT student_id, survival_rate_raw, survival_rate_normalized
         FROM student_sprint_survival WHERE sprint_id = ?",
    )?;
    let rows: Vec<(String, f64, f64)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, raw, norm) in rows {
        let divergence = norm - raw;
        if divergence > thresh.raw_normalized_divergence_threshold {
            flags.push(Flag {
                student_id: sid,
                flag_type: "RAW_NORMALIZED_DIVERGENCE",
                severity: "INFO",
                details: json!({
                    "raw_rate": round3(raw),
                    "normalized_rate": round3(norm),
                    "divergence": round3(divergence),
                    "threshold": thresh.raw_normalized_divergence_threshold,
                }),
            });
        }
    }
    Ok(flags)
}

fn cosmetic_rewrite(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT original_author_id, rewriter_id, statements_affected, change_type,
                file_path, repo_full_name
         FROM cosmetic_rewrites WHERE sprint_id = ?",
    )?;
    let rows = stmt.query_map([sprint_id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    // Emit two flags per rewrite (T-P1.2):
    //   VICTIM (INFO)   — original author is told their code was rewritten.
    //   ACTOR  (WARNING) — rewriter is told to avoid churn-only changes.
    // Legacy COSMETIC_REWRITE rows in old DBs are still rendered via a
    // fallback in report::flag_details; this detector no longer emits them.
    let mut flags = Vec::new();
    for r in rows {
        let (orig, rewriter, affected, change, file, repo) = r?;
        if let (Some(orig), Some(rewriter)) = (orig.clone(), rewriter.clone()) {
            flags.push(Flag {
                student_id: orig.clone(),
                flag_type: "COSMETIC_REWRITE_VICTIM",
                severity: "INFO",
                details: json!({
                    "file": file,
                    "repo": repo,
                    "counterpart_user_id": rewriter.clone(),
                    "statements_affected": affected,
                    "change_type": change,
                }),
            });
            flags.push(Flag {
                student_id: rewriter,
                flag_type: "COSMETIC_REWRITE_ACTOR",
                severity: "WARNING",
                details: json!({
                    "file": file,
                    "repo": repo,
                    "counterpart_user_id": orig,
                    "statements_affected": affected,
                    "change_type": change,
                }),
            });
        } else if let Some(orig) = orig {
            // Rewriter unknown — keep the victim notice only, INFO severity.
            flags.push(Flag {
                student_id: orig,
                flag_type: "COSMETIC_REWRITE_VICTIM",
                severity: "INFO",
                details: json!({
                    "file": file,
                    "repo": repo,
                    "counterpart_user_id": rewriter,
                    "statements_affected": affected,
                    "change_type": change,
                }),
            });
        }
    }
    Ok(flags)
}

fn cross_team_similarity(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT team_a_project_id, team_b_project_id, file_path_a, file_path_b,
                method_name, fingerprint
         FROM cross_team_matches WHERE sprint_id = ?",
    )?;
    let rows: Vec<CrossTeamRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (a, b, file_a, file_b, method, fp) in rows {
        for (pid, other) in [(a, b), (b, a)] {
            let fp_preview = if fp.len() > 16 {
                format!("{}...", &fp[..16])
            } else {
                fp.clone()
            };
            flags.push(Flag {
                student_id: format!("PROJECT_{pid}"),
                flag_type: "CROSS_TEAM_SIMILARITY",
                severity: "CRITICAL",
                details: json!({
                    "method": method,
                    "other_team_project_id": other,
                    "file_a": file_a,
                    "file_b": file_b,
                    "fingerprint": fp_preview,
                }),
            });
        }
    }
    Ok(flags)
}

fn bulk_rename_pr(
    conn: &Connection,
    sprint_id: i64,
    dt: &DetectorThresholdsConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.author_id,
                pr.additions, pr.deletions
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY' AND pr.merged = 1",
    )?;
    let rows: Vec<PrReviewRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (pr_id, pr_number, repo, author_id, adds, dels) in rows {
        if adds < 10 || dels < 10 {
            continue;
        }
        let max_ad = adds.max(dels);
        let ratio = if max_ad > 0 {
            adds.min(dels) as f64 / max_ad as f64
        } else {
            0.0
        };
        if ratio > dt.bulk_rename_adds_dels_ratio && adds + dels > dt.bulk_rename_line_floor {
            let surv: Option<(i64, i64, i64, i64)> = conn
                .query_row(
                    "SELECT statements_surviving_raw, statements_added_raw,
                            statements_surviving_normalized, statements_added_normalized
                     FROM pr_survival WHERE pr_id = ? AND sprint_id = ?",
                    params![&pr_id, sprint_id],
                    |r| {
                        Ok((
                            r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                            r.get::<_, Option<i64>>(1)?.unwrap_or(1),
                            r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                            r.get::<_, Option<i64>>(3)?.unwrap_or(1),
                        ))
                    },
                )
                .ok();
            if let Some((sr, ar, sn, an)) = surv {
                let raw_rate = sr as f64 / ar.max(1) as f64;
                let norm_rate = sn as f64 / an.max(1) as f64;
                if norm_rate - raw_rate > 0.3 {
                    if let Some(sid) = author_id {
                        flags.push(Flag {
                            student_id: sid,
                            flag_type: "BULK_RENAME_PR",
                            severity: "INFO",
                            details: json!({
                                "pr_number": pr_number,
                                "repo": repo,
                                "additions": adds,
                                "deletions": dels,
                                "add_del_ratio": round2(ratio),
                            }),
                        });
                    }
                }
            }
        }
    }
    Ok(flags)
}

fn cosmetic_heavy_pr(
    conn: &Connection,
    sprint_id: i64,
    threshold: f64,
) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT plm.pr_id, plm.lat, plm.lar, pr.pr_number, pr.repo_full_name, pr.author_id
         FROM pr_line_metrics plm
         JOIN pull_requests pr ON pr.id = plm.pr_id
         WHERE plm.sprint_id = ? AND plm.lat > 0",
    )?;
    let rows: Vec<PrCommitsRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (_pr_id, lat, lar, pr_number, repo, author_id) in rows {
        let lar = lar.unwrap_or(0);
        let cosmetic_share = 1.0 - (lar as f64 / lat as f64);
        if cosmetic_share > threshold {
            if let Some(sid) = author_id {
                flags.push(Flag {
                    student_id: sid,
                    flag_type: "COSMETIC_HEAVY_PR",
                    severity: "WARNING",
                    details: json!({
                        "pr_number": pr_number,
                        "repo": repo,
                        "lat": lat,
                        "lar": lar,
                        "cosmetic_share": round3(cosmetic_share),
                        "threshold": threshold,
                    }),
                });
            }
        }
    }
    Ok(flags)
}

fn low_doc_score(
    conn: &Connection,
    sprint_id: i64,
    thresh: &ThresholdConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT student_id, avg_doc_score FROM student_sprint_metrics
         WHERE sprint_id = ? AND avg_doc_score IS NOT NULL",
    )?;
    let rows: Vec<(String, f64)> = stmt
        .query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, score) in rows {
        if score < thresh.low_doc_score as f64 {
            flags.push(Flag {
                student_id: sid,
                flag_type: "LOW_DOC_SCORE",
                severity: "INFO",
                details: json!({
                    "avg_score": round2(score),
                    "threshold": thresh.low_doc_score,
                }),
            });
        }
    }
    Ok(flags)
}

fn linked_tasks_for_pr(
    conn: &Connection,
    sprint_id: i64,
    pr_id: &str,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.task_key, t.name, COALESCE(t.estimation_points, 0), t.assignee_id
         FROM tasks t
         JOIN task_pull_requests tpr ON tpr.task_id = t.id
         WHERE tpr.pr_id = ? AND t.sprint_id = ? AND t.type != 'USER_STORY'
         ORDER BY t.task_key, t.id",
    )?;
    let rows = stmt
        .query_map(params![pr_id, sprint_id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "key": r.get::<_, Option<String>>(1)?,
                "name": r.get::<_, Option<String>>(2)?,
                "points": r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                "assignee_id": r.get::<_, Option<String>>(4)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn linked_prs_for_task(
    conn: &Connection,
    sprint_id: i64,
    task_id: i64,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT pr.id, pr.pr_number, pr.repo_full_name, pr.title, pr.url, pr.author_id,
                pr.additions, pr.deletions, plm.lat, plm.lar, plm.ls, plm.ld
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         LEFT JOIN pr_line_metrics plm ON plm.pr_id = pr.id AND plm.sprint_id = ?
         WHERE tpr.task_id = ?
         ORDER BY pr.repo_full_name, pr.pr_number, pr.id",
    )?;
    let rows = stmt
        .query_map(params![sprint_id, task_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "number": r.get::<_, Option<i64>>(1)?,
                "repo": r.get::<_, Option<String>>(2)?,
                "title": r.get::<_, Option<String>>(3)?,
                "url": r.get::<_, Option<String>>(4)?,
                "author_id": r.get::<_, Option<String>>(5)?,
                "additions": r.get::<_, Option<i64>>(6)?,
                "deletions": r.get::<_, Option<i64>>(7)?,
                "lat": r.get::<_, Option<f64>>(8)?.map(round2),
                "lar": r.get::<_, Option<f64>>(9)?.map(round2),
                "ls": r.get::<_, Option<f64>>(10)?.map(round2),
                "ld": r.get::<_, Option<f64>>(11)?.map(round2),
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn task_evidence_for_member(
    conn: &Connection,
    sprint_id: i64,
    student_id: &str,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_key, name, status, COALESCE(estimation_points, 0)
         FROM tasks
         WHERE sprint_id = ? AND assignee_id = ? AND status = 'DONE' AND type != 'USER_STORY'
         ORDER BY task_key, id",
    )?;
    let rows: Vec<DoneTaskRow> = stmt
        .query_map(params![sprint_id, student_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut tasks = Vec::new();
    for (id, key, name, status, points) in rows {
        tasks.push(json!({
            "id": id,
            "key": key,
            "name": name,
            "status": status,
            "points": points,
            "pull_requests": linked_prs_for_task(conn, sprint_id, id)?,
        }));
    }
    Ok(tasks)
}

fn pr_evidence_for_author(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    student_id: &str,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.title, pr.url,
                pr.additions, pr.deletions, pr.changed_files,
                plm.lat, plm.lar, plm.ls, plm.ld,
                (SELECT COUNT(DISTINCT pc.sha) FROM pr_commits pc WHERE pc.pr_id = pr.id)
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         JOIN students s ON s.id = pr.author_id
         LEFT JOIN pr_line_metrics plm ON plm.pr_id = pr.id AND plm.sprint_id = ?
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND pr.author_id = ? AND s.team_project_id = ?
         ORDER BY pr.repo_full_name, pr.pr_number, pr.id",
    )?;
    let rows: Vec<DonePrFullRow> = stmt
        .query_map(params![sprint_id, sprint_id, student_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<f64>>(8)?,
                r.get::<_, Option<f64>>(9)?,
                r.get::<_, Option<f64>>(10)?,
                r.get::<_, Option<f64>>(11)?,
                r.get::<_, i64>(12)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut prs = Vec::new();
    for (
        id,
        number,
        repo,
        title,
        url,
        additions,
        deletions,
        changed_files,
        lat,
        lar,
        ls,
        ld,
        commit_count,
    ) in rows
    {
        prs.push(json!({
            "id": id,
            "number": number,
            "repo": repo,
            "title": title,
            "url": url,
            "additions": additions,
            "deletions": deletions,
            "changed_files": changed_files,
            "commit_count": commit_count,
            "lat": lat.map(round2),
            "lar": lar.map(round2),
            "ls": ls.map(round2),
            "ld": ld.map(round2),
            "linked_tasks": linked_tasks_for_pr(conn, sprint_id, &id)?,
        }));
    }
    Ok(prs)
}

fn review_evidence_for_member(
    conn: &Connection,
    sprint_id: i64,
    student_id: &str,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT rv.pr_id, rv.state, rv.submitted_at,
                pr.pr_number, pr.repo_full_name, pr.title, pr.url,
                pr.author_id, plm.lat, plm.lar, plm.ls, plm.ld
         FROM students s
         JOIN pr_reviews rv ON LOWER(rv.reviewer_login) = LOWER(s.github_login)
         JOIN pull_requests pr ON pr.id = rv.pr_id
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         LEFT JOIN pr_line_metrics plm ON plm.pr_id = pr.id AND plm.sprint_id = ?
         WHERE s.id = ? AND t.sprint_id = ? AND t.type != 'USER_STORY'
         ORDER BY rv.submitted_at, rv.pr_id",
    )?;
    let rows: Vec<FlagDetailRow> = stmt
        .query_map(params![sprint_id, student_id, sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<f64>>(8)?,
                r.get::<_, Option<f64>>(9)?,
                r.get::<_, Option<f64>>(10)?,
                r.get::<_, Option<f64>>(11)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut reviews = Vec::new();
    for (id, state, submitted_at, number, repo, title, url, author_id, lat, lar, ls, ld) in rows {
        reviews.push(json!({
            "pr_id": id,
            "state": state,
            "submitted_at": submitted_at,
            "number": number,
            "repo": repo,
            "title": title,
            "url": url,
            "author_id": author_id,
            "lat": lat.map(round2),
            "lar": lar.map(round2),
            "ls": ls.map(round2),
            "ld": ld.map(round2),
            "linked_tasks": linked_tasks_for_pr(conn, sprint_id, &id)?,
        }));
    }
    Ok(reviews)
}

fn team_inequality_member_value(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    metric_name: &str,
    student_id: &str,
) -> rusqlite::Result<f64> {
    match metric_name {
        "points_delivered" => conn
            .query_row(
                "SELECT COALESCE(SUM(estimation_points), 0)
                 FROM tasks
                 WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'
                   AND assignee_id = ?",
                params![sprint_id, student_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v as f64),
        "commit_count" => conn
            .query_row(
                "SELECT COUNT(DISTINCT pc.sha)
                 FROM pull_requests pr
                 JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                 JOIN tasks t ON t.id = tpr.task_id
                 JOIN pr_commits pc ON pc.pr_id = pr.id
                 JOIN students s ON s.id = pr.author_id
                 WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
                   AND pr.author_id = ? AND s.team_project_id = ?",
                params![sprint_id, student_id, project_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v as f64),
        "reviews_given" => conn
            .query_row(
                "SELECT COUNT(DISTINCT rv.pr_id || rv.submitted_at)
                 FROM students s
                 JOIN pr_reviews rv ON LOWER(rv.reviewer_login) = LOWER(s.github_login)
                 JOIN pull_requests pr ON pr.id = rv.pr_id
                 JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                 JOIN tasks t ON t.id = tpr.task_id
                 WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
                   AND s.id = ? AND s.team_project_id = ?",
                params![sprint_id, student_id, project_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v as f64),
        "pr_lines" => conn
            .query_row(
                "SELECT COALESCE(weighted_pr_lines, 0)
                 FROM student_sprint_metrics
                 WHERE sprint_id = ? AND student_id = ?",
                params![sprint_id, student_id],
                |r| r.get::<_, Option<f64>>(0),
            )
            .map(|v| v.unwrap_or(0.0)),
        _ => Ok(0.0),
    }
}

fn team_inequality_evidence_for_member(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    metric_name: &str,
    student_id: &str,
) -> rusqlite::Result<Value> {
    let value = team_inequality_member_value(conn, sprint_id, project_id, metric_name, student_id)?;
    let student_name: Option<String> = conn
        .query_row(
            "SELECT full_name FROM students WHERE id = ?",
            [student_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let tasks = if metric_name == "points_delivered" {
        task_evidence_for_member(conn, sprint_id, student_id)?
    } else {
        Vec::new()
    };
    let pull_requests = if matches!(metric_name, "commit_count" | "pr_lines") {
        pr_evidence_for_author(conn, sprint_id, project_id, student_id)?
    } else {
        Vec::new()
    };
    let reviews_given = if metric_name == "reviews_given" {
        review_evidence_for_member(conn, sprint_id, student_id)?
    } else {
        Vec::new()
    };

    Ok(json!({
        "student_id": student_id,
        "student_name": student_name,
        "value": round2(value),
        "tasks": tasks,
        "pull_requests": pull_requests,
        "reviews_given": reviews_given,
    }))
}

fn team_inequality(
    conn: &Connection,
    sprint_id: i64,
    dt: &DetectorThresholdsConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let gini_warn = dt.gini_warn;
    let gini_crit = dt.gini_crit;
    let outlier_deviation = dt.team_inequality_outlier_deviation;
    let mut stmt = conn.prepare(
        "SELECT project_id, metric_name, gini, hoover, cv
         FROM team_sprint_inequality WHERE sprint_id = ?",
    )?;
    let rows: Vec<StudentMetricRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (project_id, metric_name, gini, hoover, cv) in rows {
        let g = gini.unwrap_or(0.0);
        let severity: &'static str =
            if matches!(metric_name.as_str(), "commit_count" | "reviews_given") {
                if g >= gini_warn {
                    "WARNING"
                } else {
                    continue;
                }
            } else if g >= gini_crit {
                "CRITICAL"
            } else if g >= gini_warn {
                "WARNING"
            } else {
                continue;
            };
        let project_name: String = conn
            .query_row(
                "SELECT name FROM projects WHERE id = ?",
                [project_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
            .unwrap_or_else(|| project_id.to_string());
        let mut stmt = conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
        let members: Vec<String> = stmt
            .query_map([project_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let member_evidence = members
            .iter()
            .map(|m| {
                team_inequality_evidence_for_member(conn, sprint_id, project_id, &metric_name, m)
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let values: Vec<f64> = member_evidence
            .iter()
            .filter_map(|m| {
                m.get("value").and_then(|v| match v {
                    Value::Number(n) => n.as_f64(),
                    _ => None,
                })
            })
            .collect();
        if values.is_empty() {
            continue;
        }
        let average = values.iter().sum::<f64>() / values.len() as f64;

        for m in members {
            let Some(member) = member_evidence.iter().find(|member| {
                member.get("student_id").and_then(Value::as_str) == Some(m.as_str())
            }) else {
                continue;
            };
            let value = member
                .get("value")
                .and_then(|v| match v {
                    Value::Number(n) => n.as_f64(),
                    _ => None,
                })
                .unwrap_or(0.0);
            if !team_inequality_is_material_outlier(value, average, outlier_deviation) {
                continue;
            }
            let details = json!({
                "dimension": &metric_name,
                "gini": round3(g),
                "hoover": round3(hoover.unwrap_or(0.0)),
                "cv": round3(cv.unwrap_or(0.0)),
                "threshold_warning": gini_warn,
                "threshold_critical": gini_crit,
                "project": &project_name,
                "flagged_student": &m,
                "members": member_evidence.clone(),
            });
            flags.push(Flag {
                student_id: m,
                flag_type: "TEAM_INEQUALITY",
                severity,
                details,
            });
        }
    }
    Ok(flags)
}

fn low_composite_score(
    conn: &Connection,
    sprint_id: i64,
    dt: &DetectorThresholdsConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let warn = dt.composite_warn;
    let crit = dt.composite_crit;
    let mut stmt = conn.prepare(
        "SELECT student_id, composite_score, code_signal, review_signal,
                task_signal, process_signal
         FROM student_sprint_contribution WHERE sprint_id = ?",
    )?;
    let rows: Vec<StudentFloatsRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<f64>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, score, code, review, task, process) in rows {
        let s = match score {
            Some(x) => x,
            None => continue,
        };
        let severity: &'static str = if s < crit {
            "CRITICAL"
        } else if s < warn {
            "WARNING"
        } else {
            continue;
        };
        flags.push(Flag {
            student_id: sid,
            flag_type: "LOW_COMPOSITE_SCORE",
            severity,
            details: json!({
                "composite": round3(s),
                "code": round3(code.unwrap_or(0.0)),
                "review": round3(review.unwrap_or(0.0)),
                "task": round3(task.unwrap_or(0.0)),
                "process": round3(process.unwrap_or(0.0)),
            }),
        });
    }
    Ok(flags)
}

fn ghost_contributor(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT student_id, composite_score, code_signal
         FROM student_sprint_contribution WHERE sprint_id = ?",
    )?;
    let rows: Vec<(String, Option<f64>, Option<f64>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<f64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, composite, code) in rows {
        let (c, k) = match (composite, code) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        let task_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE sprint_id = ? AND assignee_id = ? AND type != 'USER_STORY'",
                params![sprint_id, &sid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if task_count >= 1 && c < 0.15 && k < 0.10 {
            flags.push(Flag {
                student_id: sid,
                flag_type: "GHOST_CONTRIBUTOR",
                severity: "WARNING",
                details: json!({
                    "tasks_assigned": task_count,
                    "composite": round3(c),
                    "code_signal": round3(k),
                }),
            });
        }
    }
    Ok(flags)
}

fn hidden_contributor(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT student_id, code_signal, task_signal
         FROM student_sprint_contribution WHERE sprint_id = ?",
    )?;
    let rows: Vec<(String, Option<f64>, Option<f64>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<f64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, code, task) in rows {
        let (c, t) = match (code, task) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        if c >= 0.75 && t <= 0.25 {
            flags.push(Flag {
                student_id: sid,
                flag_type: "HIDDEN_CONTRIBUTOR",
                severity: "INFO",
                details: json!({
                    "code_signal": round3(c),
                    "task_signal": round3(t),
                    "interpretation": "contributing code but not reflected in task board",
                }),
            });
        }
    }
    Ok(flags)
}

fn pr_does_not_compile(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT pc.pr_number, pc.author_id, pr.title, pr.url, pr.repo_full_name
         FROM pr_compilation pc
         JOIN pull_requests pr ON pr.id = pc.pr_id
         WHERE pc.sprint_id = ? AND pc.compiles = 0 AND pr.merged = 1",
    )?;
    let rows: Vec<CompilationRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (pr_number, author_id, title, url, repo) in rows {
        if let Some(sid) = author_id {
            flags.push(Flag {
                student_id: sid,
                flag_type: "PR_DOES_NOT_COMPILE",
                severity: "WARNING",
                details: json!({
                    "pr_number": pr_number,
                    "pr_title": title,
                    "pr_url": url,
                    "repo": repo,
                }),
            });
        }
    }
    Ok(flags)
}

fn approved_broken_pr(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT pc.pr_id, pc.reviewer_ids, pc.author_id, pc.pr_number
         FROM pr_compilation pc
         WHERE pc.sprint_id = ? AND pc.compiles = 0",
    )?;
    let rows: Vec<ApprovedBrokenRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (pr_id, reviewer_json, author_id, pr_number) in rows {
        let reviewers: Vec<String> =
            serde_json::from_str(&reviewer_json.unwrap_or_default()).unwrap_or_default();
        for rid in reviewers {
            let approved: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM pr_reviews WHERE pr_id = ? AND state = 'APPROVED'
                     AND reviewer_login IN (SELECT github_login FROM students WHERE id = ?)",
                    params![&pr_id, &rid],
                    |r| r.get::<_, i64>(0),
                )
                .ok();
            if approved.is_some() {
                flags.push(Flag {
                    student_id: rid,
                    flag_type: "APPROVED_BROKEN_PR",
                    severity: "INFO",
                    details: json!({
                        "pr_id": pr_id,
                        "pr_number": pr_number,
                        "author": author_id,
                    }),
                });
            }
        }
    }
    Ok(flags)
}

fn high_compile_failure_rate(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT author_id, COUNT(*) AS total,
                SUM(CASE WHEN compiles = 0 THEN 1 ELSE 0 END) AS failed
         FROM pr_compilation WHERE sprint_id = ? AND author_id IS NOT NULL
         GROUP BY author_id HAVING total >= 3",
    )?;
    let rows: Vec<(String, i64, i64)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, total, failed) in rows {
        let rate = failed as f64 / total as f64;
        if rate >= 0.50 {
            flags.push(Flag {
                student_id: sid,
                flag_type: "HIGH_COMPILE_FAILURE_RATE",
                severity: "WARNING",
                details: json!({
                    "total": total,
                    "failed": failed,
                    "fail_rate": round3(rate),
                }),
            });
        }
    }
    Ok(flags)
}

fn last_minute_pr(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let mut stmt = conn.prepare(
        "SELECT pr.pr_number, pr.title, pr.url, pr.repo_full_name,
                prr.student_id, prr.hours_before_deadline, prr.merged_at
         FROM pr_regularity prr
         JOIN pull_requests pr ON pr.id = prr.pr_id
         WHERE prr.sprint_id = ? AND prr.regularity_band = 'last_minute'",
    )?;
    let rows: Vec<SuspectFastTaskRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (pr_number, title, url, repo, sid, hours, merged_at) in rows {
        if let Some(sid) = sid {
            flags.push(Flag {
                student_id: sid,
                flag_type: "LAST_MINUTE_PR",
                severity: "WARNING",
                details: json!({
                    "pr_number": pr_number,
                    "pr_title": title,
                    "pr_url": url,
                    "repo": repo,
                    "hours_before_deadline": round_half_even(hours.unwrap_or(0.0), 1),
                    "merged_at": merged_at,
                }),
            });
        }
    }
    Ok(flags)
}

fn all_prs_late(
    conn: &Connection,
    sprint_id: i64,
    dt: &DetectorThresholdsConfig,
) -> rusqlite::Result<Vec<Flag>> {
    let late_threshold = dt.late_regularity;
    let mut stmt = conn.prepare(
        "SELECT student_id, avg_regularity, pr_count, prs_in_last_24h, prs_in_last_3h
         FROM student_sprint_regularity WHERE sprint_id = ? AND pr_count >= 2",
    )?;
    let rows: Vec<(String, Option<f64>, i64, i64, i64)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, avg, pr_count, last_24h, last_3h) in rows {
        if let Some(a) = avg {
            if a < late_threshold {
                flags.push(Flag {
                    student_id: sid,
                    flag_type: "ALL_PRS_LATE",
                    severity: "WARNING",
                    details: json!({
                        "avg_regularity": round3(a),
                        "pr_count": pr_count,
                        "prs_in_last_24h": last_24h,
                        "prs_in_last_3h": last_3h,
                    }),
                });
            }
        }
    }
    Ok(flags)
}

fn regularity_declining(
    conn: &Connection,
    sprint_id: i64,
    dt: &DetectorThresholdsConfig,
) -> rusqlite::Result<Vec<Flag>> {
    // Below 3 PRs in either sprint, avg_regularity is noise: a single
    // late merge dominates. Skip the comparison entirely. Kept as a Rust
    // constant per T-P1.3 scope (not a tunable threshold).
    const MIN_PR_COUNT: i64 = 3;

    let mut stmt = conn.prepare(
        "SELECT student_id, avg_regularity, pr_count
         FROM student_sprint_regularity WHERE sprint_id = ?",
    )?;
    let rows: Vec<(String, Option<f64>, Option<i64>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut flags = Vec::new();
    for (sid, avg, cur_pr_count) in rows {
        let a = match avg {
            Some(x) => x,
            None => continue,
        };
        let cur_pr_count = cur_pr_count.unwrap_or(0);
        if cur_pr_count < MIN_PR_COUNT {
            continue;
        }
        let prev: Option<(Option<f64>, Option<i64>)> = conn
            .query_row(
                "SELECT sr.avg_regularity, sr.pr_count FROM student_sprint_regularity sr
                 JOIN sprints sp ON sp.id = sr.sprint_id
                 JOIN sprints sp_curr ON sp_curr.id = ?
                 WHERE sr.student_id = ? AND sp.start_date < sp_curr.start_date
                 ORDER BY sp.start_date DESC LIMIT 1",
                params![sprint_id, &sid],
                |r| Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<i64>>(1)?)),
            )
            .ok();
        if let Some((Some(p), prev_pr_count)) = prev {
            let prev_pr_count = prev_pr_count.unwrap_or(0);
            if prev_pr_count < MIN_PR_COUNT {
                continue;
            }
            let delta = a - p;
            if delta < dt.regularity_declining_delta {
                flags.push(Flag {
                    student_id: sid,
                    flag_type: "REGULARITY_DECLINING",
                    severity: "INFO",
                    details: json!({
                        "current": round3(a),
                        "previous": round3(p),
                        "delta": round3(delta),
                        "pr_count": cur_pr_count,
                        "previous_pr_count": prev_pr_count,
                    }),
                });
            }
        }
    }
    Ok(flags)
}

/// ARCHITECTURE_DRIFT (T-P2.2). For each project covered by this sprint,
/// compare the count of `architecture_violations` rows against the most
/// recent prior sprint. Fire WARNING when the current count is strictly
/// higher — i.e., the team regressed against the layered model.
///
/// The flag is project-scoped; we attribute it to a synthetic
/// `PROJECT_<id>` student, matching `cross_team_similarity`'s convention
/// so the renderer treats it as a team-level note rather than punishing
/// any single member.
fn architecture_drift(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    let project_id: Option<i64> = conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get(0),
        )
        .ok();
    let project_id = match project_id {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let current: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM architecture_violations av
             JOIN sprints s ON s.id = av.sprint_id
             WHERE s.project_id = ? AND av.sprint_id = ?",
            params![project_id, sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let prev: Option<(i64, i64)> = conn
        .query_row(
            "SELECT s.id, COUNT(av.sprint_id)
             FROM sprints s
             LEFT JOIN architecture_violations av ON av.sprint_id = s.id
             JOIN sprints curr ON curr.id = ?
             WHERE s.project_id = curr.project_id
                   AND s.start_date < curr.start_date
             GROUP BY s.id
             ORDER BY s.start_date DESC LIMIT 1",
            [sprint_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok();
    let mut flags = Vec::new();
    if let Some((prev_sprint_id, prev_count)) = prev {
        if current > prev_count {
            flags.push(Flag {
                student_id: format!("PROJECT_{project_id}"),
                flag_type: "ARCHITECTURE_DRIFT",
                severity: "WARNING",
                details: json!({
                    "current": current,
                    "previous": prev_count,
                    "previous_sprint_id": prev_sprint_id,
                    "delta": current - prev_count,
                }),
            });
        }
    }
    Ok(flags)
}

/// ESTIMATION_BIAS (T-P2.1). Per-student calibration flag based on the
/// `student_estimation_bias` posterior fitted by the `estimation` crate.
/// Fires WARNING when the 95 % credible interval for `β_u` excludes 0
/// by more than 0.5 logits AND the student has at least 5 estimated
/// tasks (the plan's small-sample mitigation; the prior keeps the
/// posterior defined below 5, but we don't fire on it).
///
/// Note: the bias table is keyed by `(student_id, project_id)`, so for
/// each sprint we resolve its project_id once and query all qualifying
/// students for that project. The same student fires once per sprint
/// they were active in — matching how the trajectory and inequality
/// flags scope their per-sprint commentary.
fn estimation_bias(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<Flag>> {
    const LOGIT_MARGIN: f64 = 0.5;
    const MIN_TASKS: i64 = 5;
    let project_id: Option<i64> = conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get(0),
        )
        .ok();
    let project_id = match project_id {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let mut stmt = conn.prepare(
        "SELECT seb.student_id, seb.beta_mean, seb.beta_lower95, seb.beta_upper95, seb.n_tasks
         FROM student_estimation_bias seb
         WHERE seb.project_id = ? AND seb.n_tasks >= ?",
    )?;
    let rows = stmt
        .query_map(params![project_id, MIN_TASKS], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut flags = Vec::new();
    for (student_id, beta_mean, lower95, upper95, n_tasks) in rows {
        let direction = if lower95 > LOGIT_MARGIN {
            "over"
        } else if upper95 < -LOGIT_MARGIN {
            "under"
        } else {
            continue;
        };
        flags.push(Flag {
            student_id,
            flag_type: "ESTIMATION_BIAS",
            severity: "WARNING",
            details: json!({
                "beta_mean": beta_mean,
                "beta_lower95": lower95,
                "beta_upper95": upper95,
                "n_tasks": n_tasks,
                "direction": direction,
            }),
        });
    }
    Ok(flags)
}

// ---- Dispatcher ----

fn persist_flags(conn: &Connection, sprint_id: i64, flags: &[Flag]) -> rusqlite::Result<()> {
    for f in flags {
        conn.execute(
            "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
             VALUES (?, ?, ?, ?, ?)",
            params![
                f.student_id,
                sprint_id,
                f.flag_type,
                f.severity,
                f.details.to_string(),
            ],
        )?;
    }
    Ok(())
}

/// Run every flag detector for one sprint. Mirrors `flags._detect_for_sprint_id`.
pub fn detect_flags_for_sprint_id(
    conn: &Connection,
    sprint_id: i64,
    config: &Config,
) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM flags WHERE sprint_id = ?", [sprint_id])?;
    let t = &config.thresholds;
    let dt = &config.detector_thresholds;

    // Each detector is allowed to fail independently — matches Python's
    // try/except around the dispatcher loop.
    macro_rules! run {
        ($name:expr, $e:expr) => {{
            match $e {
                Ok(v) => {
                    let n = v.len();
                    if n > 0 {
                        info!("  {}: {} flags", $name, n);
                    }
                    persist_flags(conn, sprint_id, &v)?;
                    n
                }
                Err(e) => {
                    warn!(flag = $name, error = %e, "detector failed");
                    0
                }
            }
        }};
    }

    let mut total = 0usize;
    total += run!("ZERO_TASKS", zero_tasks(conn, sprint_id));
    total += run!("CARRYING_TEAM", carrying_team(conn, sprint_id, t));
    total += run!(
        "CONTRIBUTION_IMBALANCE",
        contribution_imbalance(conn, sprint_id, t)
    );
    total += run!(
        "LOW_CODE_HIGH_POINTS",
        low_code_high_points(conn, sprint_id)
    );
    total += run!("POINT_CODE_MISMATCH", point_code_mismatch(conn, sprint_id));
    total += run!("CRAMMING", cramming(conn, sprint_id, t));
    total += run!("MICRO_PRS", micro_prs(conn, sprint_id, t));
    total += run!("SINGLE_COMMIT_DUMP", single_commit_dump(conn, sprint_id, t));
    total += run!("NO_REVIEWS_RECEIVED", no_reviews_received(conn, sprint_id));
    total += run!("AUTHOR_MISMATCH", author_mismatch(conn, sprint_id));
    total += run!("ORPHAN_PR", orphan_pr(conn, sprint_id));
    total += run!("FOREIGN_MERGE", foreign_merge(conn, sprint_id));
    total += run!("UNKNOWN_CONTRIBUTOR", unknown_contributor(conn, sprint_id));
    total += run!("LOW_SURVIVAL_RATE", low_survival_rate(conn, sprint_id, t));
    total += run!(
        "RAW_NORMALIZED_DIVERGENCE",
        raw_normalized_divergence(conn, sprint_id, t)
    );
    total += run!(
        "COSMETIC_REWRITE_VICTIM/ACTOR",
        cosmetic_rewrite(conn, sprint_id)
    );
    total += run!(
        "CROSS_TEAM_SIMILARITY",
        cross_team_similarity(conn, sprint_id)
    );
    total += run!("BULK_RENAME_PR", bulk_rename_pr(conn, sprint_id, dt));
    total += run!("LOW_DOC_SCORE", low_doc_score(conn, sprint_id, t));
    total += run!("TEAM_INEQUALITY", team_inequality(conn, sprint_id, dt));
    total += run!(
        "LOW_COMPOSITE_SCORE",
        low_composite_score(conn, sprint_id, dt)
    );
    total += run!("GHOST_CONTRIBUTOR", ghost_contributor(conn, sprint_id));
    total += run!("HIDDEN_CONTRIBUTOR", hidden_contributor(conn, sprint_id));
    total += run!("PR_DOES_NOT_COMPILE", pr_does_not_compile(conn, sprint_id));
    total += run!("APPROVED_BROKEN_PR", approved_broken_pr(conn, sprint_id));
    total += run!(
        "HIGH_COMPILE_FAILURE_RATE",
        high_compile_failure_rate(conn, sprint_id)
    );
    total += run!("LAST_MINUTE_PR", last_minute_pr(conn, sprint_id));
    total += run!("ALL_PRS_LATE", all_prs_late(conn, sprint_id, dt));
    total += run!(
        "REGULARITY_DECLINING",
        regularity_declining(conn, sprint_id, dt)
    );
    total += run!("ARCHITECTURE_DRIFT", architecture_drift(conn, sprint_id));
    total += run!("ESTIMATION_BIAS", estimation_bias(conn, sprint_id));

    // Config-dependent detector.
    total += run!(
        "COSMETIC_HEAVY_PR",
        cosmetic_heavy_pr(
            conn,
            sprint_id,
            config.repo_analysis.cosmetic_share_threshold
        )
    );

    info!(sprint_id, total, "Flag detection complete");
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mk_dt() -> DetectorThresholdsConfig {
        DetectorThresholdsConfig::default()
    }

    fn mk_thresh() -> ThresholdConfig {
        ThresholdConfig {
            carrying_team_pct: 0.40,
            cramming_hours: 48,
            cramming_commit_pct: 0.70,
            single_commit_dump_lines: 200,
            micro_pr_max_lines: 10,
            low_doc_score: 2,
            contribution_imbalance_stddev: 1.5,
            low_survival_rate_stddev: 1.5,
            low_survival_absolute_floor: 0.85,
            raw_normalized_divergence_threshold: 0.20,
        }
    }

    fn mk_survival_conn(rates: &[(&str, f64)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sprints (id INTEGER PRIMARY KEY, project_id INTEGER);
             CREATE TABLE students (id TEXT PRIMARY KEY, team_project_id INTEGER);
             CREATE TABLE student_sprint_survival (
                student_id TEXT, sprint_id INTEGER,
                survival_rate_normalized REAL,
                PRIMARY KEY (student_id, sprint_id));
             INSERT INTO sprints(id, project_id) VALUES (10, 1);",
        )
        .unwrap();
        for (sid, rate) in rates {
            conn.execute(
                "INSERT INTO students(id, team_project_id) VALUES (?, 1)",
                [sid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO student_sprint_survival(student_id, sprint_id, survival_rate_normalized)
                 VALUES (?, 10, ?)",
                params![sid, rate],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn low_survival_rate_silent_when_team_all_high() {
        let conn = mk_survival_conn(&[("a", 0.99), ("b", 0.99), ("c", 0.99), ("d", 0.95)]);
        let flags = low_survival_rate(&conn, 10, &mk_thresh()).unwrap();
        assert!(
            flags.is_empty(),
            "team uniformly high: must not flag — got {flags:?}"
        );
    }

    #[test]
    fn low_survival_rate_fires_when_absolute_low_and_relative_low() {
        let conn = mk_survival_conn(&[("a", 0.99), ("b", 0.99), ("c", 0.99), ("d", 0.50)]);
        let flags = low_survival_rate(&conn, 10, &mk_thresh()).unwrap();
        assert_eq!(flags.len(), 1, "expected exactly one flag, got {flags:?}");
        assert_eq!(flags[0].student_id, "d");
        assert_eq!(flags[0].flag_type, "LOW_SURVIVAL_RATE");
        assert_eq!(
            flags[0].details["absolute_floor"].as_f64(),
            Some(0.85),
            "details must record the floor used"
        );
    }

    fn mk_regularity_conn(rows: &[(i64, &str, f64, i64)]) -> Connection {
        // rows: (sprint_id, student_id, avg_regularity, pr_count).
        // Sprint 10 = current, sprint 9 = previous (start_date earlier).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sprints (id INTEGER PRIMARY KEY, start_date TEXT);
             CREATE TABLE student_sprint_regularity (
                student_id TEXT, sprint_id INTEGER,
                avg_regularity REAL, pr_count INTEGER,
                PRIMARY KEY (student_id, sprint_id));
             INSERT INTO sprints(id, start_date) VALUES (9, '2026-01-01');
             INSERT INTO sprints(id, start_date) VALUES (10, '2026-02-01');",
        )
        .unwrap();
        for (sid, student, avg, count) in rows {
            conn.execute(
                "INSERT INTO student_sprint_regularity(student_id, sprint_id, avg_regularity, pr_count)
                 VALUES (?, ?, ?, ?)",
                params![student, sid, avg, count],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn regularity_declining_silent_when_pr_count_low() {
        // delta = 0.20 - 0.70 = -0.50 (well past -0.30) but pr_count is only 2.
        let conn = mk_regularity_conn(&[
            (9, "a", 0.70, 5),
            (10, "a", 0.20, 2),
            (9, "b", 0.70, 2),
            (10, "b", 0.20, 5),
        ]);
        let flags = regularity_declining(&conn, 10, &mk_dt()).unwrap();
        assert!(
            flags.is_empty(),
            "low pr_count in either sprint must suppress the flag — got {flags:?}"
        );
    }

    #[test]
    fn regularity_declining_fires_when_count_sufficient() {
        // Both sprints have pr_count = 5; delta = 0.30 - 0.70 = -0.40.
        let conn = mk_regularity_conn(&[(9, "a", 0.70, 4), (10, "a", 0.30, 5)]);
        let flags = regularity_declining(&conn, 10, &mk_dt()).unwrap();
        assert_eq!(flags.len(), 1, "expected exactly one flag, got {flags:?}");
        assert_eq!(flags[0].student_id, "a");
        assert_eq!(flags[0].flag_type, "REGULARITY_DECLINING");
        assert_eq!(flags[0].details["pr_count"].as_i64(), Some(5));
        assert_eq!(flags[0].details["previous_pr_count"].as_i64(), Some(4));
    }

    fn mk_temporal_conn(rows: &[(&str, f64)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE student_sprint_temporal (
                student_id TEXT NOT NULL,
                sprint_id INTEGER NOT NULL,
                cramming_ratio REAL,
                PRIMARY KEY (student_id, sprint_id));",
        )
        .unwrap();
        for (sid, ratio) in rows {
            conn.execute(
                "INSERT INTO student_sprint_temporal(student_id, sprint_id, cramming_ratio)
                 VALUES (?, 10, ?)",
                params![sid, ratio],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn cramming_attributes_to_commit_author_not_task_assignee() {
        // Scenario: Alice (teammate) made all the late-night commits on Bob's
        // task. student_sprint_temporal is per-author (it joins pr_commits on
        // pc.author_login), so Alice's cramming_ratio is high and Bob's is 0.
        // Pre-T-P1.1 the detector read student_sprint_metrics.temporal_spread,
        // which is task-assignee-keyed, and flagged Bob.
        let conn = mk_temporal_conn(&[("alice", 0.85), ("bob", 0.0)]);
        let flags = cramming(&conn, 10, &mk_thresh()).unwrap();
        assert_eq!(flags.len(), 1, "expected exactly one flag, got {flags:?}");
        assert_eq!(flags[0].student_id, "alice");
        assert_eq!(flags[0].flag_type, "CRAMMING");
        assert_eq!(flags[0].severity, "WARNING");
        assert_eq!(
            flags[0].details["threshold"].as_f64(),
            Some(mk_thresh().cramming_commit_pct),
        );
    }

    fn mk_cosmetic_conn(rows: &[(&str, &str, i64)]) -> Connection {
        // rows: (original_author_id, rewriter_id, statements_affected).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE cosmetic_rewrites (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sprint_id INTEGER,
                file_path TEXT,
                repo_full_name TEXT,
                original_author_id TEXT,
                rewriter_id TEXT,
                statements_affected INTEGER,
                change_type TEXT);",
        )
        .unwrap();
        for (orig, rewriter, affected) in rows {
            conn.execute(
                "INSERT INTO cosmetic_rewrites
                    (sprint_id, file_path, repo_full_name, original_author_id,
                     rewriter_id, statements_affected, change_type)
                 VALUES (10, 'src/A.java', 'org/repo', ?, ?, ?, 'rename_local')",
                params![orig, rewriter, affected],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn cosmetic_rewrite_emits_victim_info_and_actor_warning() {
        // Alice rewrote 50 statements originally authored by Bob.
        let conn = mk_cosmetic_conn(&[("bob", "alice", 50)]);
        let flags = cosmetic_rewrite(&conn, 10).unwrap();
        assert_eq!(flags.len(), 2, "expected exactly two flags, got {flags:?}");

        let victim = flags
            .iter()
            .find(|f| f.flag_type == "COSMETIC_REWRITE_VICTIM")
            .expect("victim flag missing");
        assert_eq!(victim.student_id, "bob");
        assert_eq!(victim.severity, "INFO");
        assert_eq!(
            victim.details["counterpart_user_id"].as_str(),
            Some("alice")
        );
        assert_eq!(victim.details["statements_affected"].as_i64(), Some(50));

        let actor = flags
            .iter()
            .find(|f| f.flag_type == "COSMETIC_REWRITE_ACTOR")
            .expect("actor flag missing");
        assert_eq!(actor.student_id, "alice");
        assert_eq!(actor.severity, "WARNING");
        assert_eq!(actor.details["counterpart_user_id"].as_str(), Some("bob"));

        // Severity-aggregation invariant: rewriter accumulates WARNING+CRITICAL,
        // victim does not.
        let warn_or_crit = |sid: &str| -> usize {
            flags
                .iter()
                .filter(|f| {
                    f.student_id == sid && (f.severity == "WARNING" || f.severity == "CRITICAL")
                })
                .count()
        };
        assert_eq!(warn_or_crit("alice"), 1);
        assert_eq!(warn_or_crit("bob"), 0);
    }

    #[test]
    fn cramming_silent_below_threshold() {
        let conn = mk_temporal_conn(&[("alice", 0.50), ("bob", 0.10)]);
        let flags = cramming(&conn, 10, &mk_thresh()).unwrap();
        assert!(
            flags.is_empty(),
            "below threshold must not flag — got {flags:?}"
        );
    }

    // T-P1.3 wiring tests: prove `DetectorThresholdsConfig` overrides reach
    // each migrated literal. Each test uses a fixture sized so the default
    // threshold produces no flag and a custom override produces one (or
    // vice versa).

    #[test]
    fn regularity_declining_delta_threshold_is_wired() {
        // delta = 0.50 - 0.70 = -0.20. Default threshold (-0.30) → no flag.
        // Lower the bar to -0.10 and the flag fires.
        let conn = mk_regularity_conn(&[(9, "a", 0.70, 4), (10, "a", 0.50, 5)]);
        let default_flags = regularity_declining(&conn, 10, &mk_dt()).unwrap();
        assert!(default_flags.is_empty(), "default delta must not fire");

        let mut dt = mk_dt();
        dt.regularity_declining_delta = -0.10;
        let lenient = regularity_declining(&conn, 10, &dt).unwrap();
        assert_eq!(lenient.len(), 1, "loosened threshold must fire");
    }

    #[test]
    fn team_inequality_outlier_deviation_threshold_is_wired() {
        // Helper used by team_inequality to filter spurious outliers. With
        // value=100, average=80, ratio = 0.25. Default 0.35 → not an outlier.
        // Override to 0.20 → counts as an outlier.
        assert!(!team_inequality_is_material_outlier(100.0, 80.0, 0.35));
        assert!(team_inequality_is_material_outlier(100.0, 80.0, 0.20));
    }

    fn mk_author_mismatch_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sprints (id INTEGER PRIMARY KEY);
             CREATE TABLE students (id TEXT PRIMARY KEY, github_login TEXT);
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, sprint_id INTEGER, type TEXT);
             CREATE TABLE pull_requests (id TEXT PRIMARY KEY, pr_number INTEGER,
                repo_full_name TEXT, author_id TEXT, github_author_login TEXT);
             CREATE TABLE task_pull_requests (task_id INTEGER, pr_id TEXT);
             CREATE TABLE pr_commits (pr_id TEXT, sha TEXT, author_login TEXT);
             CREATE TABLE pr_pre_squash_authors (
                pr_id TEXT NOT NULL, sha TEXT NOT NULL,
                author_login TEXT, author_email TEXT, captured_at TEXT,
                PRIMARY KEY (pr_id, sha));
             INSERT INTO sprints(id) VALUES (10);
             INSERT INTO students(id, github_login) VALUES ('alice', 'alice-gh');
             INSERT INTO students(id, github_login) VALUES ('bob', 'bob-gh');
             INSERT INTO tasks(id, sprint_id, type) VALUES (1, 10, 'TASK');
             INSERT INTO pull_requests(id, pr_number, repo_full_name, author_id, github_author_login)
                VALUES ('pr-1', 42, 'org/repo', 'alice', 'alice-gh');
             INSERT INTO task_pull_requests(task_id, pr_id) VALUES (1, 'pr-1');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn author_mismatch_prefers_pr_pre_squash_authors() {
        // After a squash, pr_commits collapses to the squasher (alice).
        // pr_pre_squash_authors retains the original commit authors (bob).
        // Detector must read from pr_pre_squash_authors and flag the mismatch.
        let conn = mk_author_mismatch_conn();
        conn.execute(
            "INSERT INTO pr_commits(pr_id, sha, author_login)
             VALUES ('pr-1', 'squash-sha', 'alice-gh')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pr_pre_squash_authors(pr_id, sha, author_login, captured_at)
             VALUES ('pr-1', 'orig-sha-1', 'bob-gh', '2026-04-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let flags = author_mismatch(&conn, 10).unwrap();
        assert_eq!(flags.len(), 1, "expected mismatch flag, got {flags:?}");
        assert_eq!(flags[0].flag_type, "AUTHOR_MISMATCH");
        // Detail should name the original committer (bob), not the squasher.
        let commit_authors = flags[0].details["commit_authors"]
            .as_array()
            .expect("commit_authors array");
        assert!(
            commit_authors.iter().any(|v| v.as_str() == Some("bob-gh")),
            "must include original committer; got {commit_authors:?}"
        );
        assert!(
            !commit_authors
                .iter()
                .any(|v| v.as_str() == Some("alice-gh")),
            "must not include the PR author (squasher) from pr_commits fallback"
        );
    }

    #[test]
    fn author_mismatch_falls_back_to_pr_commits_when_pre_squash_empty() {
        // No pr_pre_squash_authors rows → fall back to pr_commits, which
        // contains the same author as the PR — no flag.
        let conn = mk_author_mismatch_conn();
        conn.execute(
            "INSERT INTO pr_commits(pr_id, sha, author_login)
             VALUES ('pr-1', 'sha-1', 'alice-gh')",
            [],
        )
        .unwrap();
        let flags = author_mismatch(&conn, 10).unwrap();
        assert!(
            flags.is_empty(),
            "matching authors must not flag — got {flags:?}"
        );
    }

    #[test]
    fn trajectory_thresholds_are_wired() {
        use crate::trajectory::classify_trajectory;
        // Steady fixture: cv ~ 0.05 → "steady" under default; raise
        // trajectory_cv_low past 0.05 should still be steady; lower it
        // below 0.05 should push the same scores past the steady cutoff.
        let scores = [10.0, 11.0, 10.5, 11.5];
        let dt_default = mk_dt();
        let r_default = classify_trajectory(&scores, &dt_default);
        assert_eq!(r_default.class, "steady");

        let mut dt_strict = mk_dt();
        dt_strict.trajectory_cv_low = 0.001;
        dt_strict.trajectory_cv_high = 0.01;
        let r_strict = classify_trajectory(&scores, &dt_strict);
        assert_ne!(
            r_strict.class, "steady",
            "tightened cv bands must reclassify the same scores"
        );
    }
}
