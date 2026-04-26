//! Excel (.xlsx) report generation.
//!
//! Per-team workbook: `team_{project_name}.xlsx`
//!   - Members (1 row per student)
//!   - PRs     (1 row per PR in the sprint)
//!   - Flags   (1 row per flag, filtered to the team + cross-team marker ids)
//!   - Estimation Quality (4 columns on survival / density)
//!   - Estimation Analysis (task_similarity_groups summary + per-member detail)
//!   - PR Submission Timing (counts per tier per student)
//!
//! Cross-team workbook: `all_teams_summary.xlsx`
//!   - Flags Summary (all flags, all sprints)
//!   - Team Comparison (one row per team)
//!   - Cross-team Matches (cross_team_matches rows)
//!
//! Layouts mirror `src/report/generate.py` column-for-column.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Url, Workbook, Worksheet};
use serde_json::Value;
use tracing::info;

use crate::flag_details::{enrich_flag_details, render_flag_details, render_flag_severity};

const TEMPORAL_TIERS: &[&str] = &["Regular", "Late", "Critical", "Fix"];

fn canonical_timing_tier(tier: &str) -> Option<&'static str> {
    match tier {
        "Regular" | "Green" => Some("Regular"),
        "Late" | "Orange" => Some("Late"),
        "Critical" | "Red" | "Cramming" => Some("Critical"),
        "Fix" => Some("Fix"),
        _ => None,
    }
}

fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xD9E2F3))
        .set_border(FormatBorder::Thin)
        .set_align(FormatAlign::Center)
        .set_text_wrap()
}

fn pct_format() -> Format {
    // `0.#%` drops the trailing `.0` when the value is a whole percent
    // (e.g. 50% instead of 50.0%). Non-integer percents still show one
    // decimal place.
    Format::new().set_num_format("0.#%")
}

fn dec2_format() -> Format {
    // `0.##` shows up to two decimals and trims trailing zeros/decimal
    // point, so 5.0 → "5", 5.5 → "5.5", 5.55 → "5.55".
    Format::new().set_num_format("0.##")
}

fn link_format() -> Format {
    Format::new()
        .set_font_color(Color::RGB(0x0563C1))
        .set_underline(rust_xlsxwriter::FormatUnderline::Single)
}

fn critical_fill() -> Format {
    Format::new().set_background_color(Color::RGB(0xFFC7CE))
}
fn warning_fill() -> Format {
    Format::new().set_background_color(Color::RGB(0xFFEB9C))
}

/// Write a row of string headers starting at (row, col), returning the next row.
fn write_headers(
    ws: &mut Worksheet,
    row: u32,
    headers: &[&str],
) -> Result<(), rust_xlsxwriter::XlsxError> {
    let fmt = header_format();
    for (i, h) in headers.iter().enumerate() {
        ws.write_string_with_format(row, i as u16, *h, &fmt)?;
    }
    ws.set_row_height(row, 28.0)?;
    Ok(())
}

fn auto_width(ws: &mut Worksheet, n_cols: u16, min_width: f64, max_width: f64) {
    // rust_xlsxwriter doesn't have a real auto-fit at write time; set a
    // reasonable default and bump a few known-wide columns elsewhere.
    for c in 0..n_cols {
        let _ = ws.set_column_width(c, min_width.max(max_width.min(18.0)));
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn get_opt_f64(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Option<f64> {
    conn.query_row(
        sql,
        rusqlite::params_from_iter(params.iter().copied()),
        |r| r.get::<_, Option<f64>>(0),
    )
    .ok()
    .flatten()
}

fn count_i64(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> i64 {
    conn.query_row(
        sql,
        rusqlite::params_from_iter(params.iter().copied()),
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

/// Sum of LS and LAT per student for the sprint, distributed proportionally.
/// Mirrors `generate.py::_student_ls_lat_totals`.
fn student_ls_lat_totals(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<String, (f64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT t.assignee_id AS student_id,
                tpr.pr_id,
                COALESCE(t.estimation_points, 0) AS task_points,
                plm.lat AS lat, plm.ls AS ls
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         JOIN pr_line_metrics plm ON plm.pr_id = tpr.pr_id AND plm.sprint_id = t.sprint_id
         JOIN students s ON s.id = t.assignee_id
         WHERE t.sprint_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'
           AND s.team_project_id = ?",
    )?;

    struct Row {
        student_id: Option<String>,
        pr_id: String,
        task_points: f64,
        lat: Option<f64>,
        ls: Option<f64>,
    }
    let rows: Vec<Row> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok(Row {
                student_id: r.get::<_, Option<String>>(0)?,
                pr_id: r.get::<_, String>(1)?,
                task_points: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                lat: r.get::<_, Option<f64>>(3)?,
                ls: r.get::<_, Option<f64>>(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut pr_totals: HashMap<String, (f64, i64)> = HashMap::new();
    for r in &rows {
        let e = pr_totals.entry(r.pr_id.clone()).or_insert((0.0, 0));
        e.0 += r.task_points;
        e.1 += 1;
    }
    let mut out: HashMap<String, (f64, f64)> = HashMap::new();
    for r in &rows {
        let (tot_pts, count) = pr_totals[&r.pr_id];
        let weight = if tot_pts > 0.0 {
            r.task_points / tot_pts
        } else if count > 0 {
            1.0 / count as f64
        } else {
            0.0
        };
        let lat = r.lat.unwrap_or(0.0) * weight;
        let ls = r.ls.unwrap_or(0.0) * weight;
        let Some(sid) = &r.student_id else { continue };
        let e = out.entry(sid.clone()).or_insert((0.0, 0.0));
        e.0 += ls;
        e.1 += lat;
    }
    Ok(out)
}

// ── sheet writers ────────────────────────────────────────────────────────────

const MEMBERS_HEADERS: &[&str] = &[
    "Student",
    "GitHub",
    "Points",
    "Points %",
    "PR Lines",
    "Raw Stmts (added)",
    "Raw Stmts (surv)",
    "Raw Rate",
    "Norm Stmts (added)",
    "Norm Stmts (surv)",
    "Norm Rate",
    "Methods (added)",
    "Methods (surv)",
    "Est. Density",
    "Team Avg Density",
    "Commits",
    "Files",
    "Reviews",
    "Early %",
    "Mid %",
    "Late %",
    "Cramming %",
    "Doc Score",
    "Flags",
    "LS",
    "LAT",
    "Retention %",
    "LS/pt",
];

fn write_members_sheet(
    ws: &mut Worksheet,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<()> {
    write_headers(ws, 0, MEMBERS_HEADERS).map_err(to_rusqlite)?;

    let ls_lat = student_ls_lat_totals(conn, sprint_id, project_id)?;

    let mut stmt = conn.prepare(
        "SELECT id, full_name, github_login FROM students
         WHERE team_project_id = ? ORDER BY full_name",
    )?;
    let students: Vec<(String, String, Option<String>)> = stmt
        .query_map([project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let team_avg_density = get_opt_f64(
        conn,
        "SELECT AVG(estimation_density) FROM student_sprint_survival
         WHERE sprint_id = ? AND student_id IN
         (SELECT id FROM students WHERE team_project_id = ?)",
        &[&sprint_id, &project_id],
    )
    .unwrap_or(0.0);

    let pct = pct_format();
    let dec = dec2_format();

    for (idx, (sid, full_name, github)) in students.iter().enumerate() {
        let row = (idx + 1) as u32;

        let metrics: Option<(f64, f64, f64, i64, i64, i64, Option<f64>, Option<String>)> = conn
            .query_row(
                "SELECT points_delivered, points_share, weighted_pr_lines,
                        commit_count, files_touched, reviews_given,
                        avg_doc_score, temporal_spread
                 FROM student_sprint_metrics WHERE student_id = ? AND sprint_id = ?",
                rusqlite::params![sid, sprint_id],
                |r| {
                    Ok((
                        r.get::<_, Option<f64>>(0)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                        r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                        r.get::<_, Option<f64>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .ok();

        let survival: Option<(i64, i64, f64, i64, i64, f64, i64, i64, f64)> = conn
            .query_row(
                "SELECT total_stmts_raw, surviving_stmts_raw, survival_rate_raw,
                        total_stmts_normalized, surviving_stmts_normalized,
                        survival_rate_normalized,
                        total_methods, surviving_methods, estimation_density
                 FROM student_sprint_survival WHERE student_id = ? AND sprint_id = ?",
                rusqlite::params![sid, sprint_id],
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                        r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                        r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                        r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                    ))
                },
            )
            .ok();

        let flag_count = count_i64(
            conn,
            "SELECT COUNT(*) FROM flags WHERE student_id = ? AND sprint_id = ?",
            &[sid, &sprint_id],
        );

        // Parse temporal_spread JSON
        let spread: Value = metrics
            .as_ref()
            .and_then(|m| m.7.as_deref())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let spread_count =
            |k: &str| -> f64 { spread.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) };
        let spread_total = spread_count("early")
            + spread_count("mid")
            + spread_count("late")
            + spread_count("cramming");
        let spread_pct = |k: &str| -> f64 {
            if spread_total > 0.0 {
                spread_count(k) / spread_total
            } else {
                0.0
            }
        };

        let (ls_total, lat_total) = ls_lat.get(sid).copied().unwrap_or((0.0, 0.0));
        let retention = if lat_total > 0.0 {
            ls_total / lat_total
        } else {
            0.0
        };
        let pts_delivered = metrics.as_ref().map(|m| m.0).unwrap_or(0.0);
        let ls_per_pt = if pts_delivered > 0.0 {
            ls_total / pts_delivered
        } else {
            0.0
        };

        let points_delivered = metrics.as_ref().map(|m| m.0).unwrap_or(0.0);
        let points_share = metrics.as_ref().map(|m| m.1).unwrap_or(0.0);
        let weighted_lines = metrics.as_ref().map(|m| m.2).unwrap_or(0.0);
        let commits = metrics.as_ref().map(|m| m.3).unwrap_or(0);
        let files = metrics.as_ref().map(|m| m.4).unwrap_or(0);
        let reviews = metrics.as_ref().map(|m| m.5).unwrap_or(0);
        let avg_doc = metrics.as_ref().and_then(|m| m.6);

        let s = survival.unwrap_or_default();

        ws.write_string(row, 0, full_name).map_err(to_rusqlite)?;
        ws.write_string(row, 1, github.as_deref().unwrap_or(""))
            .map_err(to_rusqlite)?;
        ws.write_number(row, 2, points_delivered)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 3, points_share, &pct)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 4, (weighted_lines * 10.0).round() / 10.0)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 5, s.0 as f64).map_err(to_rusqlite)?;
        ws.write_number(row, 6, s.1 as f64).map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 7, s.2, &pct)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 8, s.3 as f64).map_err(to_rusqlite)?;
        ws.write_number(row, 9, s.4 as f64).map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 10, s.5, &pct)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 11, s.6 as f64).map_err(to_rusqlite)?;
        ws.write_number(row, 12, s.7 as f64).map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 13, (s.8 * 100.0).round() / 100.0, &dec)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 14, (team_avg_density * 100.0).round() / 100.0, &dec)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 15, commits as f64)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 16, files as f64)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 17, reviews as f64)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 18, spread_pct("early"), &pct)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 19, spread_pct("mid"), &pct)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 20, spread_pct("late"), &pct)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 21, spread_pct("cramming"), &pct)
            .map_err(to_rusqlite)?;
        if let Some(v) = avg_doc {
            ws.write_number(row, 22, (v * 10.0).round() / 10.0)
                .map_err(to_rusqlite)?;
        }
        ws.write_number(row, 23, flag_count as f64)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 24, (ls_total * 10.0).round() / 10.0)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 25, (lat_total * 10.0).round() / 10.0)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 26, retention, &pct)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 27, (ls_per_pt * 100.0).round() / 100.0, &dec)
            .map_err(to_rusqlite)?;
    }

    auto_width(ws, MEMBERS_HEADERS.len() as u16, 10.0, 40.0);
    Ok(())
}

const PRS_HEADERS: &[&str] = &[
    "PR #",
    "Repo",
    "Title",
    "URL",
    "Tasks",
    "Author",
    "Adds",
    "Dels",
    "Files",
    "Commits",
    "Raw (added)",
    "Raw (surv)",
    "Norm (added)",
    "Norm (surv)",
    "Methods (added)",
    "Methods (surv)",
    "Title Score",
    "Desc Score",
    "Doc Total",
    "Justification",
    "Flags",
    "LAT",
    "LS",
    "Retention %",
    "Cosmetic?",
];

fn write_prs_sheet(
    ws: &mut Worksheet,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<()> {
    write_headers(ws, 0, PRS_HEADERS).map_err(to_rusqlite)?;

    // pr_line_metrics snapshot
    let mut plm_by_pr: HashMap<String, (f64, f64, f64)> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT pr_id, lat, lar, ls FROM pr_line_metrics WHERE sprint_id = ?")?;
        for row in stmt
            .query_map([sprint_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                ))
            })?
            .filter_map(|r| r.ok())
        {
            plm_by_pr.insert(row.0, (row.1, row.2, row.3));
        }
    }

    // PRs for the team — only those linked to DONE TASK/BUG rows.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.title, pr.url,
                pr.author_id, pr.additions, pr.deletions, pr.changed_files
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         JOIN students s ON s.id = pr.author_id
         WHERE t.sprint_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'
           AND s.team_project_id = ?
         ORDER BY pr.pr_number",
    )?;
    struct PrRow {
        id: String,
        pr_number: i64,
        repo_full_name: Option<String>,
        title: Option<String>,
        url: Option<String>,
        author_id: Option<String>,
        additions: i64,
        deletions: i64,
        changed_files: i64,
    }
    let prs: Vec<PrRow> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok(PrRow {
                id: r.get::<_, String>(0)?,
                pr_number: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                repo_full_name: r.get::<_, Option<String>>(2)?,
                title: r.get::<_, Option<String>>(3)?,
                url: r.get::<_, Option<String>>(4)?,
                author_id: r.get::<_, Option<String>>(5)?,
                additions: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                deletions: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                changed_files: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let pct = pct_format();
    let lnk = link_format();

    for (idx, pr) in prs.iter().enumerate() {
        let row = (idx + 1) as u32;

        // Linked tasks — only DONE TASK/BUG rows.
        let mut task_stmt = conn.prepare(
            "SELECT t.id, t.task_key, t.estimation_points
             FROM tasks t JOIN task_pull_requests tpr ON tpr.task_id = t.id
             WHERE tpr.pr_id = ?
               AND t.type != 'USER_STORY' AND t.status = 'DONE'",
        )?;
        let tasks: Vec<(i64, Option<String>, Option<i64>)> = task_stmt
            .query_map([&pr.id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(task_stmt);
        let tasks_str = tasks
            .iter()
            .map(|(_, key, pts)| {
                format!(
                    "{} ({}pts)",
                    key.clone().unwrap_or_default(),
                    pts.unwrap_or(0)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        // Author name
        let author_name: String = pr
            .author_id
            .as_ref()
            .and_then(|aid| {
                conn.query_row(
                    "SELECT COALESCE(full_name, github_login, '') FROM students WHERE id = ?",
                    [aid],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
            .unwrap_or_default();

        let commits = count_i64(
            conn,
            "SELECT COUNT(*) FROM pr_commits WHERE pr_id = ?",
            &[&pr.id],
        );

        let surv: Option<(i64, i64, i64, i64, i64, i64)> = conn
            .query_row(
                "SELECT statements_added_raw, statements_surviving_raw,
                        statements_added_normalized, statements_surviving_normalized,
                        methods_added, methods_surviving
                 FROM pr_survival WHERE pr_id = ? AND sprint_id = ?",
                rusqlite::params![&pr.id, sprint_id],
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    ))
                },
            )
            .ok();

        let doc: Option<(Option<f64>, Option<f64>, Option<f64>, Option<String>)> = conn
            .query_row(
                "SELECT title_score, description_score, total_doc_score, justification
                 FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
                rusqlite::params![&pr.id, sprint_id],
                |r| {
                    Ok((
                        r.get::<_, Option<f64>>(0)?,
                        r.get::<_, Option<f64>>(1)?,
                        r.get::<_, Option<f64>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .ok();

        // Flag text
        let mut flag_stmt =
            conn.prepare("SELECT flag_type FROM flags WHERE sprint_id = ? AND details LIKE ?")?;
        let needle = format!("%\"pr_number\": {}%", pr.pr_number);
        let flag_types: Vec<String> = flag_stmt
            .query_map(rusqlite::params![sprint_id, needle], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(flag_stmt);
        let flags_str = flag_types.join(", ");

        let (lat_v, _lar_v, ls_v) = plm_by_pr.get(&pr.id).copied().unwrap_or((0.0, 0.0, 0.0));
        let retention = if lat_v > 0.0 { ls_v / lat_v } else { 0.0 };
        let cosmetic = if flags_str.contains("COSMETIC_HEAVY_PR") {
            "Yes"
        } else {
            ""
        };

        ws.write_number(row, 0, pr.pr_number as f64)
            .map_err(to_rusqlite)?;
        let repo_short = pr
            .repo_full_name
            .as_deref()
            .and_then(|s| s.rsplit('/').next())
            .unwrap_or("");
        ws.write_string(row, 1, repo_short).map_err(to_rusqlite)?;
        ws.write_string(row, 2, pr.title.as_deref().unwrap_or(""))
            .map_err(to_rusqlite)?;

        if let Some(u) = pr.url.as_deref() {
            if u.starts_with("http") {
                ws.write_url_with_format(row, 3, Url::new(u), &lnk)
                    .map_err(to_rusqlite)?;
            } else {
                ws.write_string(row, 3, u).map_err(to_rusqlite)?;
            }
        }
        ws.write_string(row, 4, &tasks_str).map_err(to_rusqlite)?;
        // hyperlink the Tasks cell when there's exactly one task, matching
        // Python's behaviour.
        if tasks.len() == 1 {
            let task_id = tasks[0].0;
            let url = format!("https://trackdev.org/dashboard/tasks/{}", task_id);
            let link = Url::new(url).set_text(&tasks_str);
            ws.write_url_with_format(row, 4, link, &lnk)
                .map_err(to_rusqlite)?;
        }
        ws.write_string(row, 5, &author_name).map_err(to_rusqlite)?;
        ws.write_number(row, 6, pr.additions as f64)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 7, pr.deletions as f64)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 8, pr.changed_files as f64)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 9, commits as f64)
            .map_err(to_rusqlite)?;
        if let Some(s) = surv {
            ws.write_number(row, 10, s.0 as f64).map_err(to_rusqlite)?;
            ws.write_number(row, 11, s.1 as f64).map_err(to_rusqlite)?;
            ws.write_number(row, 12, s.2 as f64).map_err(to_rusqlite)?;
            ws.write_number(row, 13, s.3 as f64).map_err(to_rusqlite)?;
            ws.write_number(row, 14, s.4 as f64).map_err(to_rusqlite)?;
            ws.write_number(row, 15, s.5 as f64).map_err(to_rusqlite)?;
        }
        if let Some((title_s, desc_s, total_s, just)) = doc {
            if let Some(v) = title_s {
                ws.write_number(row, 16, v).map_err(to_rusqlite)?;
            }
            if let Some(v) = desc_s {
                ws.write_number(row, 17, v).map_err(to_rusqlite)?;
            }
            if let Some(v) = total_s {
                ws.write_number(row, 18, v).map_err(to_rusqlite)?;
            }
            if let Some(j) = just {
                let trimmed = if j.chars().count() > 80 {
                    let truncated: String = j.chars().take(80).collect();
                    format!("{}...", truncated)
                } else {
                    j
                };
                ws.write_string(row, 19, &trimmed).map_err(to_rusqlite)?;
            }
        }
        ws.write_string(row, 20, &flags_str).map_err(to_rusqlite)?;
        ws.write_number(row, 21, lat_v).map_err(to_rusqlite)?;
        ws.write_number(row, 22, ls_v).map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 23, retention, &pct)
            .map_err(to_rusqlite)?;
        ws.write_string(row, 24, cosmetic).map_err(to_rusqlite)?;
    }

    auto_width(ws, PRS_HEADERS.len() as u16, 10.0, 40.0);
    Ok(())
}

const FLAGS_HEADERS: &[&str] = &["Student", "Flag", "Severity", "Details"];

fn write_flags_sheet(
    ws: &mut Worksheet,
    conn: &Connection,
    sprint_id: i64,
    project_id: Option<i64>,
    header_row: &mut u32,
) -> rusqlite::Result<()> {
    if *header_row == 0 {
        write_headers(ws, 0, FLAGS_HEADERS).map_err(to_rusqlite)?;
        *header_row = 1;
    }

    let sql = match project_id {
        Some(_) => {
            "SELECT f.student_id, f.flag_type, f.severity, f.details, s.full_name
             FROM flags f
             LEFT JOIN students s ON s.id = f.student_id
             WHERE f.sprint_id = ?
               AND (s.team_project_id = ? OR f.student_id LIKE 'PROJECT_%' OR f.student_id = 'UNKNOWN')
             ORDER BY f.severity DESC, f.flag_type"
        }
        None => {
            "SELECT f.student_id, f.flag_type, f.severity, f.details, s.full_name
             FROM flags f
             LEFT JOIN students s ON s.id = f.student_id
             WHERE f.sprint_id = ?
             ORDER BY f.severity DESC, f.flag_type"
        }
    };
    let mut stmt = conn.prepare(sql)?;

    let rows: Vec<(String, String, String, Option<String>, Option<String>)> = match project_id {
        Some(pid) => stmt
            .query_map(rusqlite::params![sprint_id, pid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?,
        None => stmt
            .query_map([sprint_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?,
    };
    drop(stmt);

    let critical = critical_fill();
    let warning = warning_fill();

    for (student_id, flag_type, severity, details, full_name) in rows {
        let row = *header_row;
        *header_row += 1;

        let enriched_details =
            enrich_flag_details(conn, sprint_id, &student_id, &flag_type, details.as_deref());
        let rendered = render_flag_details(
            &flag_type,
            enriched_details.as_deref().or(details.as_deref()),
        );
        let details_str = rendered.plain;
        let display_severity = render_flag_severity(&flag_type, &severity);

        let name_cell = full_name.unwrap_or(student_id);
        let fill = match severity.as_str() {
            "CRITICAL" => Some(&critical),
            "WARNING" => Some(&warning),
            _ => None,
        };
        match fill {
            Some(fmt) => {
                ws.write_string_with_format(row, 0, &name_cell, fmt)
                    .map_err(to_rusqlite)?;
                ws.write_string_with_format(row, 1, &flag_type, fmt)
                    .map_err(to_rusqlite)?;
                ws.write_string_with_format(row, 2, &display_severity, fmt)
                    .map_err(to_rusqlite)?;
                if let Some(url) = rendered.url.as_deref() {
                    let link = Url::new(url).set_text(&details_str);
                    ws.write_url_with_format(row, 3, link, fmt)
                        .map_err(to_rusqlite)?;
                } else {
                    ws.write_string_with_format(row, 3, &details_str, fmt)
                        .map_err(to_rusqlite)?;
                }
            }
            None => {
                ws.write_string(row, 0, &name_cell).map_err(to_rusqlite)?;
                ws.write_string(row, 1, &flag_type).map_err(to_rusqlite)?;
                ws.write_string(row, 2, &display_severity)
                    .map_err(to_rusqlite)?;
                if let Some(url) = rendered.url.as_deref() {
                    let link = Url::new(url).set_text(&details_str);
                    ws.write_url(row, 3, link).map_err(to_rusqlite)?;
                } else {
                    ws.write_string(row, 3, &details_str).map_err(to_rusqlite)?;
                }
            }
        }
    }

    auto_width(ws, FLAGS_HEADERS.len() as u16, 10.0, 80.0);
    Ok(())
}

fn write_estimation_quality_sheet(
    ws: &mut Worksheet,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<()> {
    let headers: &[&str] = &[
        "Student",
        "Est. Points",
        "Surviving Norm Stmts",
        "Density",
        "Team Avg",
        "Deviation",
    ];
    write_headers(ws, 0, headers).map_err(to_rusqlite)?;

    let mut stmt = conn.prepare(
        "SELECT s.full_name, sss.estimation_points_total, sss.surviving_stmts_normalized,
                sss.estimation_density
         FROM student_sprint_survival sss
         JOIN students s ON s.id = sss.student_id
         WHERE sss.sprint_id = ? AND s.team_project_id = ?
         ORDER BY s.full_name",
    )?;
    let rows: Vec<(Option<String>, Option<f64>, Option<i64>, Option<f64>)> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<f64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let densities: Vec<f64> = rows.iter().filter_map(|r| r.3).collect();
    let team_avg = if densities.is_empty() {
        0.0
    } else {
        densities.iter().sum::<f64>() / densities.len() as f64
    };

    let dec = dec2_format();
    for (i, (name, pts, surv, dens)) in rows.iter().enumerate() {
        let row = (i + 1) as u32;
        ws.write_string(row, 0, name.as_deref().unwrap_or(""))
            .map_err(to_rusqlite)?;
        ws.write_number(row, 1, pts.unwrap_or(0.0))
            .map_err(to_rusqlite)?;
        ws.write_number(row, 2, surv.unwrap_or(0) as f64)
            .map_err(to_rusqlite)?;
        let density = dens.unwrap_or(0.0);
        ws.write_number_with_format(row, 3, (density * 100.0).round() / 100.0, &dec)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 4, (team_avg * 100.0).round() / 100.0, &dec)
            .map_err(to_rusqlite)?;
        ws.write_number_with_format(row, 5, ((density - team_avg) * 100.0).round() / 100.0, &dec)
            .map_err(to_rusqlite)?;
    }

    auto_width(ws, headers.len() as u16, 10.0, 30.0);
    Ok(())
}

fn write_pr_timing_sheet(
    ws: &mut Worksheet,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<bool> {
    let any: i64 = count_i64(
        conn,
        "SELECT COUNT(*) FROM pr_submission_tiers pst
         JOIN pull_requests pr ON pr.id = pst.pr_id
         JOIN students s ON s.id = pr.author_id
         WHERE pst.sprint_id = ? AND s.team_project_id = ?",
        &[&sprint_id, &project_id],
    );
    if any == 0 {
        return Ok(false);
    }

    let mut headers: Vec<&str> = vec!["Student"];
    headers.extend_from_slice(TEMPORAL_TIERS);
    headers.push("Total");
    write_headers(ws, 0, &headers).map_err(to_rusqlite)?;

    let mut stmt = conn.prepare(
        "SELECT id, full_name FROM students WHERE team_project_id = ? ORDER BY full_name",
    )?;
    let students: Vec<(String, String)> = stmt
        .query_map([project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut wrote: u32 = 0;
    for (sid, name) in &students {
        let mut tier_stmt = conn.prepare(
            "SELECT pst.tier, COUNT(*) FROM pr_submission_tiers pst
             JOIN pull_requests pr ON pr.id = pst.pr_id
             WHERE pst.sprint_id = ? AND pr.author_id = ?
             GROUP BY pst.tier",
        )?;
        let mut counts: HashMap<String, i64> =
            TEMPORAL_TIERS.iter().map(|t| (t.to_string(), 0)).collect();
        for row in tier_stmt
            .query_map(rusqlite::params![sprint_id, sid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .filter_map(|r| r.ok())
        {
            if let Some(tier) = canonical_timing_tier(&row.0) {
                *counts.entry(tier.to_string()).or_insert(0) += row.1;
            }
        }
        drop(tier_stmt);
        let total: i64 = counts.values().sum();
        if total == 0 {
            continue;
        }
        let row = wrote + 1;
        wrote += 1;
        ws.write_string(row, 0, name).map_err(to_rusqlite)?;
        for (i, tier) in TEMPORAL_TIERS.iter().enumerate() {
            ws.write_number(row, (i + 1) as u16, *counts.get(*tier).unwrap_or(&0) as f64)
                .map_err(to_rusqlite)?;
        }
        ws.write_number(row, (TEMPORAL_TIERS.len() + 1) as u16, total as f64)
            .map_err(to_rusqlite)?;
    }

    if wrote == 0 {
        return Ok(false);
    }
    auto_width(ws, headers.len() as u16, 10.0, 28.0);
    Ok(true)
}

fn write_estimation_analysis_sheet(
    ws: &mut Worksheet,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT group_id, group_label, stack, layer, action,
                member_count, median_points, median_lar, median_ls, median_ls_per_point,
                representative_task_id
         FROM task_similarity_groups
         WHERE sprint_id = ? AND (project_id = ? OR project_id IS NULL)
         ORDER BY stack ASC, layer ASC, action ASC, group_id ASC",
    )?;
    struct G {
        group_id: i64,
        group_label: String,
        member_count: i64,
        median_points: Option<f64>,
        median_ls: Option<f64>,
        median_ls_per_point: Option<f64>,
        representative_task_id: i64,
    }
    let groups: Vec<G> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok(G {
                group_id: r.get::<_, i64>(0)?,
                group_label: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                member_count: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                median_points: r.get::<_, Option<f64>>(6)?,
                median_ls: r.get::<_, Option<f64>>(8)?,
                median_ls_per_point: r.get::<_, Option<f64>>(9)?,
                representative_task_id: r.get::<_, i64>(10)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    if groups.is_empty() {
        return Ok(false);
    }

    let summary_headers: &[&str] = &[
        "Group",
        "Label",
        "Members",
        "Median Points",
        "Median LS",
        "Median LS/pt",
        "Representative Task",
    ];
    write_headers(ws, 0, summary_headers).map_err(to_rusqlite)?;
    let dec = dec2_format();
    let lnk = link_format();

    for (i, g) in groups.iter().enumerate() {
        let row = (i + 1) as u32;
        let rep: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT task_key, name FROM tasks WHERE id = ?",
                [g.representative_task_id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .ok();
        let rep_label = match &rep {
            Some((Some(key), name)) => format!("{} — {}", key, name.clone().unwrap_or_default()),
            Some((None, Some(name))) => name.clone(),
            _ => String::new(),
        };
        ws.write_number(row, 0, g.group_id as f64)
            .map_err(to_rusqlite)?;
        ws.write_string(row, 1, &g.group_label)
            .map_err(to_rusqlite)?;
        ws.write_number(row, 2, g.member_count as f64)
            .map_err(to_rusqlite)?;
        if let Some(v) = g.median_points {
            ws.write_number_with_format(row, 3, (v * 100.0).round() / 100.0, &dec)
                .map_err(to_rusqlite)?;
        }
        if let Some(v) = g.median_ls {
            ws.write_number_with_format(row, 4, (v * 100.0).round() / 100.0, &dec)
                .map_err(to_rusqlite)?;
        }
        if let Some(v) = g.median_ls_per_point {
            ws.write_number_with_format(row, 5, (v * 100.0).round() / 100.0, &dec)
                .map_err(to_rusqlite)?;
        }
        if let Some((Some(_), _)) = &rep {
            let url = format!(
                "https://trackdev.org/dashboard/tasks/{}",
                g.representative_task_id
            );
            let link = Url::new(url).set_text(&rep_label);
            ws.write_url_with_format(row, 6, link, &lnk)
                .map_err(to_rusqlite)?;
        } else {
            ws.write_string(row, 6, &rep_label).map_err(to_rusqlite)?;
        }
    }

    auto_width(ws, summary_headers.len() as u16, 10.0, 40.0);
    Ok(true)
}

// ── public entry points ──────────────────────────────────────────────────────

fn to_rusqlite(e: rust_xlsxwriter::XlsxError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

pub fn generate_team_report(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    project_name: &str,
    output_dir: &Path,
) -> rusqlite::Result<PathBuf> {
    std::fs::create_dir_all(output_dir)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

    let mut wb = Workbook::new();

    {
        let ws = wb
            .add_worksheet()
            .set_name("Members")
            .map_err(to_rusqlite)?;
        write_members_sheet(ws, conn, sprint_id, project_id)?;
    }
    {
        let ws = wb.add_worksheet().set_name("PRs").map_err(to_rusqlite)?;
        write_prs_sheet(ws, conn, sprint_id, project_id)?;
    }
    {
        let ws = wb.add_worksheet().set_name("Flags").map_err(to_rusqlite)?;
        let mut h = 0u32;
        write_flags_sheet(ws, conn, sprint_id, Some(project_id), &mut h)?;
    }
    {
        let ws = wb
            .add_worksheet()
            .set_name("Estimation Quality")
            .map_err(to_rusqlite)?;
        write_estimation_quality_sheet(ws, conn, sprint_id, project_id)?;
    }
    {
        let ws = wb
            .add_worksheet()
            .set_name("Estimation Analysis")
            .map_err(to_rusqlite)?;
        let wrote = write_estimation_analysis_sheet(ws, conn, sprint_id, project_id)?;
        if !wrote {
            // No data — label the sheet so removing it isn't needed.
            write_headers(ws, 0, &["(no task similarity groups yet)"]).map_err(to_rusqlite)?;
        }
    }
    {
        let ws = wb
            .add_worksheet()
            .set_name("PR Submission Timing")
            .map_err(to_rusqlite)?;
        let wrote = write_pr_timing_sheet(ws, conn, sprint_id, project_id)?;
        if !wrote {
            write_headers(ws, 0, &["(no PR submission tiers yet)"]).map_err(to_rusqlite)?;
        }
    }

    let path = output_dir.join(format!("team_{}.xlsx", project_name));
    wb.save(&path).map_err(to_rusqlite)?;
    info!(path = %path.display(), "team Excel report written");
    Ok(path)
}

pub fn generate_summary_report(
    conn: &Connection,
    sprint_ids: &[i64],
    output_dir: &Path,
) -> rusqlite::Result<PathBuf> {
    std::fs::create_dir_all(output_dir)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

    let mut wb = Workbook::new();

    // Sheet 1: Flags Summary
    {
        let ws = wb
            .add_worksheet()
            .set_name("Flags Summary")
            .map_err(to_rusqlite)?;
        let mut h = 0u32;
        for sid in sprint_ids {
            write_flags_sheet(ws, conn, *sid, None, &mut h)?;
        }
        if h == 0 {
            write_headers(ws, 0, &["(no flags yet)"]).map_err(to_rusqlite)?;
        }
    }

    // Sheet 2: Team Comparison
    {
        let ws = wb
            .add_worksheet()
            .set_name("Team Comparison")
            .map_err(to_rusqlite)?;
        let headers: &[&str] = &[
            "Team",
            "Points",
            "Avg Surv Rate (norm)",
            "Avg Est Density",
            "Avg Doc Score",
            "Critical",
            "Warning",
            "Info",
        ];
        write_headers(ws, 0, headers).map_err(to_rusqlite)?;
        let dec = dec2_format();
        let mut next_row: u32 = 1;
        for sid in sprint_ids {
            let project_id: Option<i64> = conn
                .query_row("SELECT project_id FROM sprints WHERE id = ?", [sid], |r| {
                    r.get::<_, i64>(0)
                })
                .ok();
            let Some(pid) = project_id else { continue };
            let team_name: String = conn
                .query_row("SELECT name FROM projects WHERE id = ?", [pid], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| format!("Project {}", pid));

            let total_pts = get_opt_f64(
                conn,
                "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
                 WHERE sprint_id = ? AND status = 'DONE' AND type != 'USER_STORY'",
                &[sid],
            )
            .unwrap_or(0.0);

            let avg_surv = get_opt_f64(
                conn,
                "SELECT AVG(survival_rate_normalized) FROM student_sprint_survival
                 WHERE sprint_id = ? AND student_id IN
                 (SELECT id FROM students WHERE team_project_id = ?)",
                &[sid, &pid],
            )
            .unwrap_or(0.0);
            let avg_density = get_opt_f64(
                conn,
                "SELECT AVG(estimation_density) FROM student_sprint_survival
                 WHERE sprint_id = ? AND student_id IN
                 (SELECT id FROM students WHERE team_project_id = ?)",
                &[sid, &pid],
            )
            .unwrap_or(0.0);
            let avg_doc = get_opt_f64(
                conn,
                "SELECT AVG(avg_doc_score) FROM student_sprint_metrics
                 WHERE sprint_id = ? AND student_id IN
                 (SELECT id FROM students WHERE team_project_id = ?)
                 AND avg_doc_score IS NOT NULL",
                &[sid, &pid],
            );

            let critical = count_i64(
                conn,
                "SELECT COUNT(*) FROM flags WHERE sprint_id = ? AND severity = 'CRITICAL'
                 AND student_id IN (SELECT id FROM students WHERE team_project_id = ?)",
                &[sid, &pid],
            );
            let warning = count_i64(
                conn,
                "SELECT COUNT(*) FROM flags WHERE sprint_id = ? AND severity = 'WARNING'
                 AND student_id IN (SELECT id FROM students WHERE team_project_id = ?)",
                &[sid, &pid],
            );
            let info = count_i64(
                conn,
                "SELECT COUNT(*) FROM flags WHERE sprint_id = ? AND severity = 'INFO'
                 AND student_id IN (SELECT id FROM students WHERE team_project_id = ?)",
                &[sid, &pid],
            );

            ws.write_string(next_row, 0, &team_name)
                .map_err(to_rusqlite)?;
            ws.write_number(next_row, 1, total_pts)
                .map_err(to_rusqlite)?;
            ws.write_number_with_format(next_row, 2, (avg_surv * 1000.0).round() / 1000.0, &dec)
                .map_err(to_rusqlite)?;
            ws.write_number_with_format(next_row, 3, (avg_density * 100.0).round() / 100.0, &dec)
                .map_err(to_rusqlite)?;
            if let Some(v) = avg_doc {
                ws.write_number(next_row, 4, (v * 10.0).round() / 10.0)
                    .map_err(to_rusqlite)?;
            }
            ws.write_number(next_row, 5, critical as f64)
                .map_err(to_rusqlite)?;
            ws.write_number(next_row, 6, warning as f64)
                .map_err(to_rusqlite)?;
            ws.write_number(next_row, 7, info as f64)
                .map_err(to_rusqlite)?;
            next_row += 1;
        }
        auto_width(ws, headers.len() as u16, 10.0, 40.0);
    }

    // Sheet 3: Cross-team Matches
    {
        let ws = wb
            .add_worksheet()
            .set_name("Cross-team Matches")
            .map_err(to_rusqlite)?;
        let headers: &[&str] = &[
            "Team A",
            "Team B",
            "File A",
            "File B",
            "Method",
            "Fingerprint",
        ];
        write_headers(ws, 0, headers).map_err(to_rusqlite)?;
        let mut next_row: u32 = 1;
        for sid in sprint_ids {
            let mut stmt = conn.prepare(
                "SELECT team_a_project_id, team_b_project_id, file_path_a, file_path_b,
                        method_name, fingerprint
                 FROM cross_team_matches WHERE sprint_id = ?",
            )?;
            let rows: Vec<(i64, i64, String, String, Option<String>, String)> = stmt
                .query_map([sid], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for (a, b, file_a, file_b, method, fingerprint) in rows {
                let team_a: String = conn
                    .query_row("SELECT name FROM projects WHERE id = ?", [a], |r| {
                        r.get::<_, Option<String>>(0)
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| format!("Project {}", a));
                let team_b: String = conn
                    .query_row("SELECT name FROM projects WHERE id = ?", [b], |r| {
                        r.get::<_, Option<String>>(0)
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| format!("Project {}", b));
                ws.write_string(next_row, 0, &team_a).map_err(to_rusqlite)?;
                ws.write_string(next_row, 1, &team_b).map_err(to_rusqlite)?;
                ws.write_string(next_row, 2, &file_a).map_err(to_rusqlite)?;
                ws.write_string(next_row, 3, &file_b).map_err(to_rusqlite)?;
                ws.write_string(next_row, 4, method.as_deref().unwrap_or(""))
                    .map_err(to_rusqlite)?;
                let fp_short: String = fingerprint.chars().take(16).collect();
                ws.write_string(next_row, 5, &format!("{}...", fp_short))
                    .map_err(to_rusqlite)?;
                next_row += 1;
            }
        }
        auto_width(ws, headers.len() as u16, 10.0, 40.0);
    }

    let path = output_dir.join("all_teams_summary.xlsx");
    wb.save(&path).map_err(to_rusqlite)?;
    info!(path = %path.display(), "cross-team summary Excel written");
    Ok(path)
}

/// Generate per-team + cross-team Excel reports for a sprint. Mirrors
/// `generate.py::generate_reports`.
///
/// `project_filter` — when `Some`, only projects whose `projects.name` is in
/// the set get a `team_<project>.xlsx`. Cross-team summary still aggregates
/// across every sprint_id in `sprint_ids` (callers should pre-filter the
/// list if they want the summary scoped too).
pub fn generate_reports(
    conn: &Connection,
    sprint_ids: &[i64],
    output_dir: &Path,
    project_filter: Option<&std::collections::HashSet<String>>,
) -> rusqlite::Result<()> {
    if sprint_ids.is_empty() {
        info!("no sprint IDs — nothing to report");
        return Ok(());
    }
    for sid in sprint_ids {
        let project_id: Option<i64> = conn
            .query_row("SELECT project_id FROM sprints WHERE id = ?", [sid], |r| {
                r.get::<_, i64>(0)
            })
            .ok();
        let Some(pid) = project_id else { continue };
        let project_name: String = conn
            .query_row("SELECT name FROM projects WHERE id = ?", [pid], |r| {
                r.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("project_{}", pid));
        if let Some(filter) = project_filter {
            if !filter.contains(&project_name) {
                continue;
            }
        }
        // Group each sprint's team workbooks under `sprint_K/`, where K is
        // the sprint's chronological ordinal in its project.
        let k = ordinal_for_sprint_id_via_conn(conn, *sid).unwrap_or(0);
        let sprint_dir = if k > 0 {
            output_dir.join(format!("sprint_{}", k))
        } else {
            output_dir.join(format!("sprint_id_{}", sid))
        };
        std::fs::create_dir_all(&sprint_dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        generate_team_report(conn, *sid, pid, &project_name, &sprint_dir)?;
    }
    // Summary workbook covers every sprint in the slice — stays at the top
    // level since it's already aggregated.
    generate_summary_report(conn, sprint_ids, output_dir)?;
    Ok(())
}

/// 1-based ordinal (by `start_date ASC`) of `sprint_id` within its project.
fn ordinal_for_sprint_id_via_conn(conn: &Connection, sprint_id: i64) -> Option<u32> {
    let project_id: i64 = conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()?;
    let mut stmt = conn
        .prepare(
            "SELECT id FROM sprints
             WHERE project_id = ? AND start_date IS NOT NULL AND start_date != ''
             ORDER BY start_date ASC",
        )
        .ok()?;
    let rows = stmt.query_map([project_id], |r| r.get::<_, i64>(0)).ok()?;
    for (idx, r) in rows.enumerate() {
        if let Ok(sid) = r {
            if sid == sprint_id {
                return Some((idx + 1) as u32);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn mk_minimal_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Only create the tables the report reads from.
        conn.execute_batch(
            "CREATE TABLE sprints (id INTEGER PRIMARY KEY, project_id INTEGER, name TEXT,
                start_date TEXT, end_date TEXT);
             CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE students (id TEXT PRIMARY KEY, full_name TEXT, github_login TEXT,
                team_project_id INTEGER, email TEXT);
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, task_key TEXT, name TEXT, type TEXT,
                status TEXT, estimation_points INTEGER, assignee_id TEXT, sprint_id INTEGER,
                parent_task_id INTEGER);
             CREATE TABLE pull_requests (id TEXT PRIMARY KEY, pr_number INTEGER,
                repo_full_name TEXT, title TEXT, url TEXT, author_id TEXT,
                additions INTEGER, deletions INTEGER, changed_files INTEGER,
                created_at TEXT, merged INTEGER, merged_at TEXT, body TEXT);
             CREATE TABLE pr_commits (sha TEXT, pr_id TEXT, author_login TEXT,
                message TEXT, timestamp TEXT, additions INTEGER, deletions INTEGER,
                PRIMARY KEY (sha, pr_id));
             CREATE TABLE task_pull_requests (task_id INTEGER, pr_id TEXT,
                PRIMARY KEY (task_id, pr_id));
             CREATE TABLE pr_line_metrics (pr_id TEXT, sprint_id INTEGER, merge_sha TEXT,
                lat REAL, lar REAL, ls REAL, ld REAL, PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE student_sprint_metrics (student_id TEXT, sprint_id INTEGER,
                points_delivered REAL, points_share REAL, weighted_pr_lines REAL,
                commit_count INTEGER, files_touched INTEGER, reviews_given INTEGER,
                avg_doc_score REAL, temporal_spread TEXT,
                PRIMARY KEY (student_id, sprint_id));
             CREATE TABLE student_sprint_survival (student_id TEXT, sprint_id INTEGER,
                total_stmts_raw INTEGER, surviving_stmts_raw INTEGER, survival_rate_raw REAL,
                total_stmts_normalized INTEGER, surviving_stmts_normalized INTEGER,
                survival_rate_normalized REAL, total_methods INTEGER,
                surviving_methods INTEGER, estimation_density REAL,
                estimation_points_total REAL,
                PRIMARY KEY (student_id, sprint_id));
             CREATE TABLE pr_survival (pr_id TEXT, sprint_id INTEGER,
                statements_added_raw INTEGER, statements_surviving_raw INTEGER,
                statements_added_normalized INTEGER, statements_surviving_normalized INTEGER,
                methods_added INTEGER, methods_surviving INTEGER,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE pr_doc_evaluation (pr_id TEXT, sprint_id INTEGER,
                title_score REAL, description_score REAL, total_doc_score REAL,
                justification TEXT, PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE flags (flag_id INTEGER PRIMARY KEY AUTOINCREMENT,
                student_id TEXT, sprint_id INTEGER, flag_type TEXT, severity TEXT,
                details TEXT);
             CREATE TABLE task_similarity_groups (group_id INTEGER PRIMARY KEY AUTOINCREMENT,
                sprint_id INTEGER, project_id INTEGER,
                representative_task_id INTEGER, group_label TEXT,
                stack TEXT, layer TEXT, action TEXT,
                member_count INTEGER, median_points REAL, median_lar REAL,
                median_ls REAL, median_ls_per_point REAL);
             CREATE TABLE task_group_members (group_id INTEGER, task_id INTEGER,
                sprint_id INTEGER, is_outlier INTEGER, outlier_reason TEXT,
                points_deviation REAL, lar_deviation REAL, ls_deviation REAL,
                ls_per_point_deviation REAL,
                PRIMARY KEY (group_id, task_id));
             CREATE TABLE pr_submission_tiers (sprint_id INTEGER, pr_id TEXT,
                merged_at TEXT, hours_before_deadline REAL, tier TEXT, pr_kind TEXT,
                PRIMARY KEY (sprint_id, pr_id));
             CREATE TABLE cross_team_matches (sprint_id INTEGER,
                team_a_project_id INTEGER, team_b_project_id INTEGER,
                file_path_a TEXT, file_path_b TEXT, method_name TEXT,
                fingerprint TEXT);
             INSERT INTO projects VALUES (1, 'pds26-1a');
             INSERT INTO sprints VALUES (10, 1, 'Sprint 1', '2026-02-16', '2026-03-08');
             INSERT INTO students VALUES ('u1', 'Alice', 'alice-gh', 1, 'alice@example.com');
             INSERT INTO student_sprint_metrics
               (student_id, sprint_id, points_delivered, points_share, weighted_pr_lines,
                commit_count, files_touched, reviews_given, avg_doc_score, temporal_spread)
               VALUES ('u1', 10, 5.0, 0.5, 123.0, 12, 7, 3, 3.5,
                       '{\"early\":2,\"mid\":3,\"late\":1,\"cramming\":0}');
             INSERT INTO student_sprint_survival
               (student_id, sprint_id, total_stmts_raw, surviving_stmts_raw, survival_rate_raw,
                total_stmts_normalized, surviving_stmts_normalized, survival_rate_normalized,
                total_methods, surviving_methods, estimation_density, estimation_points_total)
               VALUES ('u1', 10, 100, 80, 0.8, 100, 85, 0.85, 10, 9, 17.0, 5.0);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn team_report_writes_all_sheets_without_error() {
        let conn = mk_minimal_conn();
        let tmp = TempDir::new().unwrap();
        let path = generate_team_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with("team_pds26-1a.xlsx"));
    }

    #[test]
    fn summary_report_writes_without_error() {
        let conn = mk_minimal_conn();
        let tmp = TempDir::new().unwrap();
        let path = generate_summary_report(&conn, &[10], tmp.path()).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with("all_teams_summary.xlsx"));
    }
}
