//! Per-team Markdown report — replaces the Python `.docx` generator.
//!
//! Layout (matches REPORT.md sections A/B/C):
//!   A. Team snapshot: per-student summary table + stacked-bar SVG for PR
//!      submission timing + flag counts per severity.
//!   B. Student dashboards: one subsection per student with their PR table,
//!      task table, composite score, and flag details.
//!   C. Peer-group analysis: task_similarity_groups with outliers highlighted.
//!
//! Inline `<svg>` blocks keep the document self-contained (GitHub, GitLab,
//! and most Markdown viewers render SVG). No external image assets.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use chrono::{Datelike, TimeZone, Timelike};
use chrono_tz::Europe::Madrid;
use chrono_tz::OffsetName;
use rusqlite::{params_from_iter, types::Value, Connection};
use sprint_grader_core::time::parse_iso;
use tracing::info;

use crate::charts::{html_escape, sparkline_svg, stacked_bars_svg, StackedRow};
use crate::flag_details::{enrich_flag_details, render_flag_details, render_flag_severity};

/// Render an ISO-8601 / minute-precision timestamp as a Catalan-academic
/// human form anchored to UDG's local time (Europe/Madrid). Falls back to
/// the raw string when parsing fails so we never silently swallow data.
///
/// The DB stores TrackDev's UTC `YYYY-MM-DDThh:mmZ`; students see the
/// sprint window in their own clock with the live tz abbreviation
/// (CET in winter, CEST in summer) — which is what the deadlines on
/// the platform UI used.
fn humanize_local_dt(ts: &str) -> String {
    let Some(utc) = parse_iso(ts) else {
        return ts.to_string();
    };
    let local = utc.with_timezone(&Madrid);
    let offset = Madrid.offset_from_utc_datetime(&local.naive_utc());
    let abbr = offset.abbreviation();
    let month = month_name_en(local.month());
    format!(
        "{day} {month} {year}, {h:02}:{m:02} {tz}",
        day = local.day(),
        month = month,
        year = local.year(),
        h = local.hour(),
        m = local.minute(),
        tz = abbr,
    )
}

fn month_name_en(m: u32) -> &'static str {
    match m {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "",
    }
}

type DoneTaskRow = (
    i64,
    Option<String>,
    Option<String>,
    Option<f64>,
    Option<String>,
);

type DonePrRow = (
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    Option<String>,
);

type GroupMemberRow = (
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<f64>,
);

/// Aggregated per-student LS and LD for the team. LS captures surviving new
/// code; LD (cosmetic-filtered) captures legitimate cleanup/refactor value
/// that LS alone misses. Both are distributed across shared PRs using the
/// same task-points weighting as the xlsx report.
#[derive(Debug, Clone, Copy, Default)]
struct StudentLsLd {
    ls: f64,
    ls_per_pt: f64,
    ld: f64,
    points: f64,
}

fn student_ls_totals(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<String, StudentLsLd>> {
    let mut stmt = conn.prepare(
        "SELECT t.assignee_id AS student_id,
                tpr.pr_id,
                COALESCE(t.estimation_points, 0) AS task_points,
                plm.ls AS ls,
                plm.ld AS ld
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
        ls: Option<f64>,
        ld: Option<f64>,
    }
    let rows: Vec<Row> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok(Row {
                student_id: r.get::<_, Option<String>>(0)?,
                pr_id: r.get::<_, String>(1)?,
                task_points: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                ls: r.get::<_, Option<f64>>(3)?,
                ld: r.get::<_, Option<f64>>(4)?,
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
    // student → (ls_sum, ld_sum, points_sum)
    let mut agg: HashMap<String, (f64, f64, f64)> = HashMap::new();
    for r in &rows {
        let (tot_pts, count) = pr_totals[&r.pr_id];
        let weight = if tot_pts > 0.0 {
            r.task_points / tot_pts
        } else if count > 0 {
            1.0 / count as f64
        } else {
            0.0
        };
        let Some(sid) = &r.student_id else { continue };
        let e = agg.entry(sid.clone()).or_insert((0.0, 0.0, 0.0));
        e.0 += r.ls.unwrap_or(0.0) * weight;
        e.1 += r.ld.unwrap_or(0.0) * weight;
        e.2 += r.task_points;
    }
    Ok(agg
        .into_iter()
        .map(|(sid, (ls, ld, pts))| {
            let ratio = if pts > 0.0 { ls / pts } else { 0.0 };
            (
                sid,
                StudentLsLd {
                    ls,
                    ls_per_pt: ratio,
                    ld,
                    points: pts,
                },
            )
        })
        .collect())
}

fn cumulative_student_ls_totals(
    conn: &Connection,
    sprint_ids: &[i64],
    project_id: i64,
) -> rusqlite::Result<HashMap<String, StudentLsLd>> {
    let mut totals: HashMap<String, StudentLsLd> = HashMap::new();
    for sprint_id in sprint_ids {
        for (student_id, stats) in student_ls_totals(conn, *sprint_id, project_id)? {
            let entry = totals.entry(student_id).or_default();
            entry.ls += stats.ls;
            entry.ld += stats.ld;
            entry.points += stats.points;
        }
    }
    for stats in totals.values_mut() {
        stats.ls_per_pt = if stats.points > 0.0 {
            stats.ls / stats.points
        } else {
            0.0
        };
    }
    Ok(totals)
}

/// Per-task (LS, LD), each weighted by task points over the PR's linked
/// task points. Returns `task_id → (ls, ld)`.
fn task_ls_for_team(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<i64, (f64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT t.id AS task_id,
                tpr.pr_id,
                COALESCE(t.estimation_points, 0) AS task_points,
                plm.ls AS ls,
                plm.ld AS ld
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         JOIN pr_line_metrics plm ON plm.pr_id = tpr.pr_id AND plm.sprint_id = t.sprint_id
         JOIN students s ON s.id = t.assignee_id
         WHERE t.sprint_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'
           AND s.team_project_id = ?",
    )?;
    struct Row {
        task_id: i64,
        pr_id: String,
        task_points: f64,
        ls: Option<f64>,
        ld: Option<f64>,
    }
    let rows: Vec<Row> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok(Row {
                task_id: r.get::<_, i64>(0)?,
                pr_id: r.get::<_, String>(1)?,
                task_points: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                ls: r.get::<_, Option<f64>>(3)?,
                ld: r.get::<_, Option<f64>>(4)?,
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
    let mut out: HashMap<i64, (f64, f64)> = HashMap::new();
    for r in &rows {
        let (tot_pts, count) = pr_totals[&r.pr_id];
        let weight = if tot_pts > 0.0 {
            r.task_points / tot_pts
        } else if count > 0 {
            1.0 / count as f64
        } else {
            0.0
        };
        let e = out.entry(r.task_id).or_insert((0.0, 0.0));
        e.0 += r.ls.unwrap_or(0.0) * weight;
        e.1 += r.ld.unwrap_or(0.0) * weight;
    }
    Ok(out)
}

/// Per-PR `(ls, ld, total_linked_points)` for the team in this sprint. LS/LD
/// are raw from `pr_line_metrics`; the third value sums task points linked
/// to the PR so the row can compute LS/pt.
fn pr_ls_for_team(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<String, (f64, f64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT tpr.pr_id, COALESCE(SUM(t.estimation_points), 0) AS pts
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         JOIN students s ON s.id = t.assignee_id
         WHERE t.sprint_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'
           AND s.team_project_id = ?
         GROUP BY tpr.pr_id",
    )?;
    let pts_rows: Vec<(String, f64)> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut stmt = conn.prepare(
        "SELECT DISTINCT plm.pr_id,
                COALESCE(plm.ls, 0) AS ls,
                COALESCE(plm.ld, 0) AS ld
         FROM pr_line_metrics plm
         JOIN task_pull_requests tpr ON tpr.pr_id = plm.pr_id
         JOIN tasks t ON t.id = tpr.task_id
         JOIN students s ON s.id = t.assignee_id
         WHERE plm.sprint_id = ?
           AND t.sprint_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'
           AND s.team_project_id = ?",
    )?;
    let ls_rows: Vec<(String, f64, f64)> = stmt
        .query_map(rusqlite::params![sprint_id, sprint_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut out: HashMap<String, (f64, f64, f64)> = pts_rows
        .into_iter()
        .map(|(pr_id, pts)| (pr_id, (0.0, 0.0, pts)))
        .collect();
    for (pr_id, ls, ld) in ls_rows {
        let entry = out.entry(pr_id).or_insert((0.0, 0.0, 0.0));
        entry.0 = ls;
        entry.1 = ld;
    }
    Ok(out)
}

/// Per-task weighted surviving normalised statements for the team in this
/// sprint. Mirrors `task_ls_for_team` but reads `pr_survival` instead of
/// `pr_line_metrics`, so the value is the AST-normalised statement count
/// (the numerator of the estimation-density signal). PR-level totals are
/// distributed across linked tasks proportionally to task points (falling
/// back to even split when total points are zero).
fn task_stmts_for_team(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<i64, f64>> {
    let mut stmt = conn.prepare(
        "SELECT t.id AS task_id,
                tpr.pr_id,
                COALESCE(t.estimation_points, 0) AS task_points,
                ps.statements_surviving_normalized AS surv
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         JOIN pr_survival ps ON ps.pr_id = tpr.pr_id AND ps.sprint_id = t.sprint_id
         JOIN students s ON s.id = t.assignee_id
         WHERE t.sprint_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'
           AND s.team_project_id = ?",
    )?;
    struct Row {
        task_id: i64,
        pr_id: String,
        task_points: f64,
        surv: f64,
    }
    let rows: Vec<Row> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok(Row {
                task_id: r.get::<_, i64>(0)?,
                pr_id: r.get::<_, String>(1)?,
                task_points: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                surv: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
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
    let mut out: HashMap<i64, f64> = HashMap::new();
    for r in &rows {
        let (tot_pts, count) = pr_totals[&r.pr_id];
        let weight = if tot_pts > 0.0 {
            r.task_points / tot_pts
        } else if count > 0 {
            1.0 / count as f64
        } else {
            0.0
        };
        *out.entry(r.task_id).or_insert(0.0) += r.surv * weight;
    }
    Ok(out)
}

/// Per-PR raw surviving normalised statements for the team in this sprint.
/// Pulls directly from `pr_survival` for PRs linked to at least one DONE
/// task assigned to a team member.
fn pr_stmts_for_team(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<String, f64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT ps.pr_id,
                COALESCE(ps.statements_surviving_normalized, 0) AS surv
         FROM pr_survival ps
         JOIN task_pull_requests tpr ON tpr.pr_id = ps.pr_id
         JOIN tasks t ON t.id = tpr.task_id
         JOIN students s ON s.id = t.assignee_id
         WHERE ps.sprint_id = ?
           AND t.sprint_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'
           AND s.team_project_id = ?",
    )?;
    let rows: Vec<(String, f64)> = stmt
        .query_map(rusqlite::params![sprint_id, sprint_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    Ok(rows.into_iter().collect())
}

// Reporting convention: every task-centric query filters with
// `t.type != 'USER_STORY' AND t.status = 'DONE'`. User stories are
// container rows that never carry estimation work, and not-yet-DONE items
// are deferred to the next sprint's report. The predicate is inlined into
// each SQL string below so every report query shares the same definition.

fn md_escape(s: &str) -> String {
    // Minimal escaping for table cells: pipes break tables, newlines break rows.
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn push_table_header(buf: &mut String, headers: &[&str]) {
    buf.push_str("| ");
    buf.push_str(&headers.join(" | "));
    buf.push_str(" |\n|");
    for _ in headers {
        buf.push_str("---|");
    }
    buf.push('\n');
}

fn push_table_row(buf: &mut String, cells: &[String]) {
    buf.push_str("| ");
    buf.push_str(
        &cells
            .iter()
            .map(|c| md_escape(c))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    buf.push_str(" |\n");
}

fn fmt_int(n: i64) -> String {
    n.to_string()
}
/// Render `v` with up to `digits` decimals, trimming the decimal part when
/// the value is (effectively) integer so e.g. 5.0 prints as "5".
fn fmt_trim(v: f64, digits: usize) -> String {
    let s = format!("{:.*}", digits, v);
    if s.contains('.') {
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    } else {
        s
    }
}
fn fmt_f1(v: f64) -> String {
    fmt_trim(v, 1)
}
fn fmt_f2(v: f64) -> String {
    fmt_trim(v, 2)
}
fn fmt_pct(v: f64) -> String {
    format!("{}%", fmt_trim(v * 100.0, 1))
}

/// Render the per-PR attribution-errors warning glyph for the markdown report
/// (T-P1.5). Returns an empty string when the column is NULL, empty, or the
/// literal `[]`. Otherwise returns `⚠ (kind1, kind2)` where the kinds come
/// from the JSON entries; if parsing fails we fall back to a bare ⚠ so a
/// stale legacy value still surfaces as a signal.
fn attribution_error_glyph(raw: Option<&str>) -> String {
    let raw = match raw {
        Some(s) if !s.trim().is_empty() && s.trim() != "[]" => s,
        _ => return String::new(),
    };
    let parsed: Option<serde_json::Value> = serde_json::from_str(raw).ok();
    let kinds: Vec<String> = match parsed.as_ref().and_then(|v| v.as_array()) {
        Some(arr) => {
            let mut out: Vec<String> = arr
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("kind")
                        .and_then(|k| k.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            out.sort();
            out.dedup();
            out
        }
        None => Vec::new(),
    };
    if kinds.is_empty() {
        "⚠".to_string()
    } else {
        format!("⚠ ({})", kinds.join(", "))
    }
}

fn compact_reason_numbers(reason: &str) -> String {
    let chars: Vec<char> = reason.chars().collect();
    let mut out = String::with_capacity(reason.len());
    let mut i = 0usize;

    while i < chars.len() {
        let sign = matches!(chars[i], '+' | '-');
        let starts_number = chars[i].is_ascii_digit()
            || (sign && chars.get(i + 1).is_some_and(char::is_ascii_digit));
        if !starts_number {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        let start = i;
        if sign {
            i += 1;
        }
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        let mut has_decimal = false;
        if i < chars.len() && chars[i] == '.' {
            has_decimal = true;
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        }

        let token: String = chars[start..i].iter().collect();
        if has_decimal {
            if let Ok(value) = token.parse::<f64>() {
                if token.starts_with('+') && value > 0.0 {
                    out.push('+');
                }
                out.push_str(&fmt_f2(value));
            } else {
                out.push_str(&token);
            }
        } else {
            out.push_str(&token);
        }
    }

    out
}

#[derive(Debug)]
struct StudentSummaryRow {
    id: String,
    full_name: String,
    github_login: Option<String>,
    pts: f64,
    share: f64,
    lines: f64,
    ls: f64,
    ld: f64,
    ls_per_pt: f64,
    surv_norm: f64,
    /// Raw stmts/points for this student (surviving statements per
    /// estimated point). Carried so the summary writer can compute a
    /// per-team median + MAD and render the Density Δ glyph.
    density: f64,
    flag_count: i64,
}

fn student_summary_headers() -> Vec<&'static str> {
    vec![
        "Student",
        "GitHub",
        "Points",
        "Share",
        "PR lines",
        "LS",
        "LD",
        "LS/pt",
        "Survival",
        "Density Δ",
        "Flags",
    ]
}

fn write_student_summary_table(buf: &mut String, students: &[StudentSummaryRow]) {
    let densities: Vec<f64> = students
        .iter()
        .map(|s| s.density)
        .filter(|d| *d > 0.0)
        .collect();
    let median = sprint_grader_core::stats::median(&densities);
    let mad = sprint_grader_core::stats::mad(&densities);
    push_table_header(buf, &student_summary_headers());
    for s in students {
        let density_value = if s.density > 0.0 {
            Some(s.density)
        } else {
            None
        };
        let cells = vec![
            s.full_name.clone(),
            s.github_login
                .as_deref()
                .map(github_cell)
                .unwrap_or_default(),
            fmt_f1(s.pts),
            fmt_pct(s.share),
            fmt_f1(s.lines),
            fmt_f1(s.ls),
            fmt_f1(s.ld),
            fmt_f2(s.ls_per_pt),
            fmt_pct(s.surv_norm),
            fmt_density_dev(density_mad_z(density_value, median, mad)),
            fmt_int(s.flag_count),
        ];
        push_table_row(buf, &cells);
    }
    buf.push('\n');
}

fn write_cumulative_student_summary_table(buf: &mut String, students: &[StudentSummaryRow]) {
    write_student_summary_table(buf, students);
}

fn sprint_placeholders(sprint_ids: &[i64]) -> String {
    std::iter::repeat("?")
        .take(sprint_ids.len())
        .collect::<Vec<_>>()
        .join(",")
}

fn sprint_params(prefix: impl IntoIterator<Item = Value>, sprint_ids: &[i64]) -> Vec<Value> {
    prefix
        .into_iter()
        .chain(sprint_ids.iter().map(|sid| Value::Integer(*sid)))
        .collect()
}

fn current_student_summary(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<Vec<StudentSummaryRow>> {
    let ls_per_student = student_ls_totals(conn, sprint_id, project_id)?;

    let mut stmt = conn.prepare(
        "SELECT s.id, s.full_name, s.github_login,
                COALESCE(sm.points_delivered, 0) AS pts,
                COALESCE(sm.points_share, 0) AS share,
                COALESCE(sm.weighted_pr_lines, 0) AS lines,
                COALESCE(ss.survival_rate_normalized, 0) AS surv_norm,
                COALESCE(ss.estimation_density, 0) AS density,
                (SELECT COUNT(*) FROM flags f WHERE f.student_id = s.id AND f.sprint_id = ?)
                    AS flag_count
         FROM students s
         LEFT JOIN student_sprint_metrics sm
                ON sm.student_id = s.id AND sm.sprint_id = ?
         LEFT JOIN student_sprint_survival ss
                ON ss.student_id = s.id AND ss.sprint_id = ?
         WHERE s.team_project_id = ?
         ORDER BY s.full_name",
    )?;
    let mut students: Vec<StudentSummaryRow> = stmt
        .query_map(
            rusqlite::params![sprint_id, sprint_id, sprint_id, project_id],
            |r| {
                let id = r.get::<_, String>(0)?;
                let stats = ls_per_student.get(&id).copied().unwrap_or_default();
                Ok(StudentSummaryRow {
                    id,
                    full_name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    github_login: r.get::<_, Option<String>>(2)?,
                    pts: r.get::<_, f64>(3)?,
                    share: r.get::<_, f64>(4)?,
                    lines: r.get::<_, f64>(5)?,
                    surv_norm: r.get::<_, f64>(6)?,
                    density: r.get::<_, f64>(7)?,
                    flag_count: r.get::<_, i64>(8)?,
                    ls: stats.ls,
                    ld: stats.ld,
                    ls_per_pt: stats.ls_per_pt,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for s in &mut students {
        s.github_login = effective_github_login(conn, &s.id, s.github_login.as_deref());
    }
    Ok(students)
}

fn cumulative_student_summary(
    conn: &Connection,
    sprint_ids: &[i64],
    project_id: i64,
) -> rusqlite::Result<Vec<StudentSummaryRow>> {
    if sprint_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = sprint_placeholders(sprint_ids);
    let ls_per_student = cumulative_student_ls_totals(conn, sprint_ids, project_id)?;

    let team_total_points: f64 = conn
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(sm.points_delivered), 0)
                 FROM student_sprint_metrics sm
                 JOIN students s ON s.id = sm.student_id
                 WHERE s.team_project_id = ? AND sm.sprint_id IN ({})",
                placeholders
            ),
            params_from_iter(sprint_params([Value::Integer(project_id)], sprint_ids)),
            |r| r.get::<_, Option<f64>>(0),
        )
        .unwrap_or(Some(0.0))
        .unwrap_or(0.0);

    let mut stmt = conn.prepare(
        "SELECT id, full_name, github_login
         FROM students
         WHERE team_project_id = ?
         ORDER BY full_name",
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

    let mut rows = Vec::with_capacity(students.len());
    for (id, full_name, github_login) in students {
        let github_login = effective_github_login(conn, &id, github_login.as_deref());
        let metrics: (f64, f64) = conn
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(points_delivered), 0),
                            COALESCE(SUM(weighted_pr_lines), 0)
                     FROM student_sprint_metrics
                     WHERE student_id = ? AND sprint_id IN ({})",
                    placeholders
                ),
                params_from_iter(sprint_params([Value::Text(id.clone())], sprint_ids)),
                |r| {
                    Ok((
                        r.get::<_, Option<f64>>(0)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    ))
                },
            )
            .unwrap_or((0.0, 0.0));

        let survival: (i64, i64, f64) = conn
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(total_stmts_normalized), 0),
                            COALESCE(SUM(surviving_stmts_normalized), 0),
                            COALESCE(SUM(estimation_points_total), 0)
                     FROM student_sprint_survival
                     WHERE student_id = ? AND sprint_id IN ({})",
                    placeholders
                ),
                params_from_iter(sprint_params([Value::Text(id.clone())], sprint_ids)),
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    ))
                },
            )
            .unwrap_or((0, 0, 0.0));

        let flag_count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*)
                     FROM flags
                     WHERE student_id = ? AND sprint_id IN ({})",
                    placeholders
                ),
                params_from_iter(sprint_params([Value::Text(id.clone())], sprint_ids)),
                |r| r.get(0),
            )
            .unwrap_or(0);

        let stats = ls_per_student.get(&id).copied().unwrap_or_default();
        let share = if team_total_points > 0.0 {
            metrics.0 / team_total_points
        } else {
            0.0
        };
        let surv_norm = if survival.0 > 0 {
            survival.1 as f64 / survival.0 as f64
        } else {
            0.0
        };
        let density = if survival.2 > 0.0 {
            survival.1 as f64 / survival.2
        } else {
            0.0
        };

        rows.push(StudentSummaryRow {
            id,
            full_name,
            github_login,
            pts: metrics.0,
            share,
            lines: metrics.1,
            ls: stats.ls,
            ld: stats.ld,
            ls_per_pt: stats.ls_per_pt,
            surv_norm,
            density,
            flag_count,
        });
    }

    Ok(rows)
}

/// TrackDev dashboard URL for a task. Matches the scheme used in `xlsx.rs`
/// (`https://trackdev.org/dashboard/tasks/{id}`) so both report artefacts
/// link to the same target.
fn trackdev_task_url(task_id: i64) -> String {
    format!("https://trackdev.org/dashboard/tasks/{}", task_id)
}

/// Render `[label](url)` with pipe/newline escaping suitable for a Markdown
/// table cell. `label` is escaped inside the link text; the URL itself is
/// trusted (it comes from the DB).
fn md_link(label: &str, url: &str) -> String {
    format!("[{}]({})", md_escape(label), url)
}

fn normalize_github_login(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('@');
    if trimmed.is_empty() {
        return None;
    }

    let path = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("https://www.github.com/"))
        .or_else(|| trimmed.strip_prefix("http://www.github.com/"))
        .or_else(|| trimmed.strip_prefix("github.com/"))
        .or_else(|| trimmed.strip_prefix("www.github.com/"))
        .unwrap_or(trimmed);
    let login = path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches(".git");
    (!login.is_empty()).then(|| login.to_string())
}

fn effective_github_login(
    conn: &Connection,
    student_id: &str,
    stored: Option<&str>,
) -> Option<String> {
    if let Some(login) = stored.and_then(normalize_github_login) {
        return Some(login);
    }

    conn.query_row(
        "SELECT github_author_login
         FROM pull_requests
         WHERE author_id = ?
           AND github_author_login IS NOT NULL
           AND github_author_login != ''
         GROUP BY github_author_login
         ORDER BY COUNT(*) DESC, github_author_login
         LIMIT 1",
        [student_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|login| normalize_github_login(&login))
}

fn github_url(login: &str) -> String {
    format!("https://github.com/{}", login)
}

fn github_cell(login: &str) -> String {
    md_link(login, &github_url(login))
}

fn github_inline(login: &str) -> String {
    format!("[`{}`]({})", md_escape(login), github_url(login))
}

/// Resolve each comma-separated owner identifier (typically a TrackDev
/// student_id, but may already be a GitHub login if no student matched
/// at ownership-aggregation time) to its human-readable display string,
/// preserving the input order.
fn humanize_owner_csv(conn: &Connection, owners_csv: &str) -> String {
    owners_csv
        .split(',')
        .map(|raw| raw.trim())
        .filter(|s| !s.is_empty())
        .map(|owner| humanize_owner(conn, owner))
        .collect::<Vec<_>>()
        .join(", ")
}

fn humanize_owner(conn: &Connection, owner: &str) -> String {
    if let Ok((full_name, github_login)) = conn.query_row(
        "SELECT full_name, github_login FROM students WHERE id = ?",
        [owner],
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        },
    ) {
        let name = full_name.unwrap_or_default();
        let login = github_login.and_then(|l| normalize_github_login(&l));
        return match (name.is_empty(), login) {
            (false, Some(l)) => format!("{} ({})", name, l),
            (false, None) => name,
            (true, Some(l)) => l,
            (true, None) => owner.to_string(),
        };
    }
    // Fallback: treat the raw value as a GitHub login if it looks like one.
    owner.to_string()
}

fn canonical_timing_tier(tier: &str) -> Option<&'static str> {
    match tier {
        "Regular" | "Green" => Some("Regular"),
        "Late" | "Orange" => Some("Late"),
        "Critical" | "Red" | "Cramming" => Some("Critical"),
        "Fix" => Some("Fix"),
        _ => None,
    }
}

fn timing_tier_labels() -> &'static [&'static str] {
    &["Regular", "Late", "Critical", "Fix"]
}

// ── Section A: team snapshot ─────────────────────────────────────────────────

fn write_section_a(
    buf: &mut String,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    project_name: &str,
    cumulative_sprint_ids: Option<&[i64]>,
    depth: usize,
) -> rusqlite::Result<()> {
    let h2 = "#".repeat(depth);
    let h3 = "#".repeat(depth + 1);

    // In single-sprint mode (depth=2) emit the full per-report banner.
    // In multi-sprint mode the outer caller owns titling; skip it.
    if depth <= 2 {
        let _ = writeln!(buf, "# Sprint report — {}\n", project_name);

        let sprint_info: Option<(Option<String>, Option<String>)> = conn
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
        if let Some((Some(s), Some(e))) = sprint_info {
            let _ = writeln!(
                buf,
                "_Sprint window: {} → {}_\n",
                humanize_local_dt(&s),
                humanize_local_dt(&e)
            );
        }
    }

    let _ = writeln!(buf, "{} A. Team snapshot\n", h2);

    let students = current_student_summary(conn, sprint_id, project_id)?;
    if cumulative_sprint_ids.is_some() {
        let _ = writeln!(buf, "{} This sprint\n", h3);
    }
    write_student_summary_table(buf, &students);

    if let Some(sprint_ids) = cumulative_sprint_ids {
        if !sprint_ids.is_empty() {
            let cumulative_students = cumulative_student_summary(conn, sprint_ids, project_id)?;
            let _ = writeln!(buf, "{} Cumulative through this sprint\n", h3);
            write_cumulative_student_summary_table(buf, &cumulative_students);
            buf.push_str(
                "_Legend:_ **Density Δ** = MAD-normalised deviation of this \
student's surviving statements per estimated point from the team median: \
▲ denser than typical (more code per point — under-pointed or unusually \
dense work), ▼ sparser than typical (over-pointed or light task), ≈ \
within ±1 MAD-z. Empty when MAD is zero or the student has no surviving code.\n\n",
            );
        }
    }

    // PR submission timing SVG (horizontal stacked bars, one row per student).
    // Crediting matches the XLSX timing sheet: each PR contributes to its
    // primary assignee only (max points, ties → max task count → student_id),
    // so the team-wide totals are conserved on multi-assignee PRs.
    let mut rows: Vec<StackedRow> = Vec::new();
    for s in &students {
        let mut raw_counts: HashMap<String, i64> = HashMap::new();
        let mut stmt = conn.prepare(
            "WITH primary_author AS (
                 SELECT pa.pr_id, pa.student_id,
                        ROW_NUMBER() OVER (
                            PARTITION BY pa.pr_id
                            ORDER BY pa.author_points DESC,
                                     pa.author_task_count DESC,
                                     pa.student_id
                        ) AS rn
                 FROM pr_authors pa
             )
             SELECT pst.tier, COUNT(*) FROM pr_submission_tiers pst
             JOIN primary_author p ON p.pr_id = pst.pr_id AND p.rn = 1
             WHERE pst.sprint_id = ? AND p.student_id = ?
             GROUP BY pst.tier",
        )?;
        let counts = stmt
            .query_map(rusqlite::params![sprint_id, s.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for (raw_tier, count) in counts {
            if let Some(label) = canonical_timing_tier(&raw_tier) {
                *raw_counts.entry(label.to_string()).or_insert(0) += count;
            }
        }

        let mut segs: Vec<(String, f64)> = Vec::new();
        let mut any_nonzero = false;
        for tier in timing_tier_labels() {
            let count = raw_counts.get(*tier).copied().unwrap_or(0);
            if count > 0 {
                any_nonzero = true;
            }
            segs.push((tier.to_string(), count as f64));
        }
        if any_nonzero {
            rows.push(StackedRow {
                label: s.full_name.clone(),
                segments: segs,
            });
        }
    }
    if !rows.is_empty() {
        let _ = writeln!(buf, "{} PR submission timing\n", h3);
        let svg = stacked_bars_svg("PRs per timing tier", &rows, 720, 26);
        buf.push_str(&svg);
        buf.push_str("\n\n");
    }

    // Truck factor headline (T-P2.3). The per-file ownership treemap was
    // removed: with realistic file counts the diagram is too crowded to read.
    let truck: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT truck_factor, owners_csv FROM team_sprint_ownership
             WHERE project_id = ? AND sprint_id = ?",
            rusqlite::params![project_id, sprint_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .ok();
    if let Some((tf, owners_csv)) = truck {
        let _ = writeln!(buf, "{} Code ownership\n", h3);
        let owners = owners_csv.unwrap_or_default();
        let owners_display = if owners.is_empty() {
            "—".to_string()
        } else {
            humanize_owner_csv(conn, &owners)
        };
        let _ = writeln!(
            buf,
            "**Truck factor:** {} (top owners: {})\n",
            tf, owners_display
        );
    }

    // T-P2.2: architecture conformance roll-up. Reads
    // `architecture_violations` totals + per-rule top-3 so the team sees
    // both the headline count and which rules are biting.
    let arch_total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM architecture_violations av
             JOIN sprints s ON s.id = av.sprint_id
             WHERE s.project_id = ? AND av.sprint_id = ?",
            rusqlite::params![project_id, sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if arch_total > 0 {
        let _ = writeln!(buf, "{} Architecture conformance\n", h3);
        // Severity breakdown across the whole project for this sprint.
        let mut sev_stmt = conn.prepare(
            "SELECT av.severity, COUNT(*) FROM architecture_violations av
             JOIN sprints s ON s.id = av.sprint_id
             WHERE s.project_id = ? AND av.sprint_id = ?
             GROUP BY av.severity",
        )?;
        let mut crit = 0i64;
        let mut warn = 0i64;
        let mut info_n = 0i64;
        for row in sev_stmt.query_map(rusqlite::params![project_id, sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (sev, n) = row?;
            match sev.to_ascii_uppercase().as_str() {
                "CRITICAL" => crit = n,
                "WARNING" => warn = n,
                "INFO" => info_n = n,
                _ => {}
            }
        }
        drop(sev_stmt);
        let _ = writeln!(
            buf,
            "**Total violations:** {} ({} critical · {} warning · {} info). \
Severity bands per `architecture.toml`. Per-student attribution \
(by dominant author of the offending file) follows in Section B.\n",
            arch_total, crit, warn, info_n
        );

        // Stub for violations whose offending region has no surviving
        // authorship — they appear nowhere in Section B because there is
        // no student to blame. Surface the count here so the team total
        // is reconcilable with the per-student breakdowns.
        let unattributed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM architecture_violations av
                 JOIN sprints s ON s.id = av.sprint_id
                 WHERE s.project_id = ? AND av.sprint_id = ?
                   AND NOT EXISTS (
                     SELECT 1 FROM architecture_violation_attribution a
                     WHERE a.violation_rowid = av.rowid
                       AND a.weight > 0
                   )",
                rusqlite::params![project_id, sprint_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if unattributed > 0 {
            let _ = writeln!(
                buf,
                "**Unattributed violations:** {} — offending lines have no \
surviving authorship in this sprint, so they are not assigned to any \
student in Section B.\n",
                unattributed
            );
        }
    }

    // Flag severity roll-up for the team
    let critical = count_severity(conn, sprint_id, project_id, "CRITICAL");
    let warning = count_severity(conn, sprint_id, project_id, "WARNING");
    let info_n = count_severity(conn, sprint_id, project_id, "INFO");
    let _ = writeln!(
        buf,
        "**Team flag totals:** {} critical · {} warning · {} info\n",
        critical, warning, info_n
    );

    Ok(())
}

fn count_severity(conn: &Connection, sprint_id: i64, project_id: i64, severity: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM flags WHERE sprint_id = ? AND severity = ?
         AND student_id IN (SELECT id FROM students WHERE team_project_id = ?)",
        rusqlite::params![sprint_id, severity, project_id],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

// ── Section B: per-student dashboards ────────────────────────────────────────

/// One architecture violation, attributed to whichever student owns the
/// majority of statements in the offending file (per `file_ownership_for_project`).
/// Unattributed rows (no fingerprints for the file, or owner login not
/// resolvable to a student in the team) are dropped from the per-student
/// view — the team-level total still counts them.
///
/// `repo_full_qualified` is the `<org>/<repo>` form pulled from the matching
/// fingerprint row. It survives the basename-based join so the renderer can
/// build a `https://github.com/<org>/<repo>/blob/HEAD/<file_path>` URL.
#[derive(Debug, Clone)]
struct AttributedArchViolation {
    rule_name: String,
    file_path: String,
    severity: String,
    offending_import: String,
    repo_full_qualified: String,
    start_line: Option<i64>,
    end_line: Option<i64>,
    explanation: Option<String>,
    /// Blame share in [0, 1] of the violation's line range owned by this
    /// student. Sourced from `architecture_violation_attribution.weight`.
    /// Rows with weight ≤ 0 are dropped before reaching this struct.
    weight: f64,
}

/// Plain-English explanations for the AST + forbidden-import rule keys
/// shipped in `config/architecture.toml`. Reports lead with the prose
/// and append the machine key in backticks for traceability. Custom
/// rules a course author adds without updating this map fall back to a
/// best-effort humanizer (`fragment-no-retrofit-field` → "fragment no
/// retrofit field"), which is readable even without a hand-crafted
/// description.
const KNOWN_RULE_DESCRIPTIONS: &[(&str, &str)] = &[
    // Spring backend (controllers / services / repositories).
    (
        "controller-no-repository-field",
        "Controllers must not hold a Repository as a field — repository access goes through a Service",
    ),
    (
        "controller-no-repository-ctor-param",
        "Controllers must not inject a Repository via constructor — repository access goes through a Service",
    ),
    (
        "controller-method-not-fat",
        "Controller methods should be thin (≤20 statements) — business logic belongs in a Service",
    ),
    // Android client (Activity / Fragment / ViewModel hygiene).
    (
        "activity-no-retrofit-field",
        "Activities must not hold a Retrofit / ApiService field — data access goes through a Repository",
    ),
    (
        "fragment-no-retrofit-field",
        "Fragments must not hold a Retrofit / ApiService field — data access goes through a Repository",
    ),
    (
        "viewmodel-no-retrofit-field",
        "ViewModels must not hold a Retrofit / ApiService field — data access goes through a Repository",
    ),
    (
        "viewmodel-no-retrofit-ctor-param",
        "ViewModels must not inject a Retrofit / ApiService via constructor — data access goes through a Repository",
    ),
    // Forbidden-import rules.
    (
        "domain-no-spring-web",
        "Domain / model classes must not depend on Spring web, Spring data, or javax.servlet",
    ),
    (
        "domain-no-jpa",
        "Domain / model classes must not depend on JPA (jakarta.persistence / javax.persistence)",
    ),
    // REST API design (LLM-judged, both Spring controllers and Retrofit interfaces).
    (
        "URL_CONTAINS_VERB",
        "REST endpoint paths should be plural nouns, not verbs — the HTTP method already encodes the action",
    ),
    (
        "NON_CANONICAL_CRUD_SHAPE",
        "CRUD on a resource should use the canonical 2 URLs × 5 verbs shape (`/items`, `/items/{id}` × GET/POST/PUT/PATCH/DELETE)",
    ),
    (
        "RELATIONSHIP_NOT_NESTED",
        "Nested resources should use the parent-path form `/<parent>/{parentId}/<child>`",
    ),
];

/// Best-effort humanizer for unknown rule keys: replaces `-` and `_` with
/// spaces and capitalizes the first letter, so `fragment-no-retrofit-field`
/// renders as "Fragment no retrofit field". The original key still appears
/// in backticks for traceability.
fn humanize_unknown_rule_key(rule: &str) -> String {
    let cleaned: String = rule
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return rule.to_string();
    }
    let mut iter = trimmed.chars();
    match iter.next() {
        Some(first) => {
            let mut s = first.to_uppercase().to_string();
            s.push_str(iter.as_str());
            s
        }
        None => trimmed.to_string(),
    }
}

/// Render a rule_name as human-readable prose.
///
/// Three cases:
/// 1. Layered-architecture rules stored as `"{from}->!{to}"` (e.g.
///    `"domain->!infrastructure"`) → "**from** must not depend on
///    **to**".
/// 2. AST and forbidden-import rules whose key appears in
///    `KNOWN_RULE_DESCRIPTIONS` → the prose description, with the
///    machine key appended in backticks for traceability.
/// 3. Anything else → a best-effort humanized form of the key, also
///    with the key appended in backticks.
fn humanize_rule_name(rule: &str) -> String {
    if let Some((from, to)) = rule.split_once("->!") {
        return format!(
            "**{}** must not depend on **{}**",
            md_escape(from),
            md_escape(to)
        );
    }
    if let Some((_, description)) = KNOWN_RULE_DESCRIPTIONS.iter().find(|(key, _)| *key == rule) {
        return md_escape(description);
    }
    md_escape(&humanize_unknown_rule_key(rule))
}

/// Build the per-student architecture-violation map from
/// `architecture_violation_attribution`. Rows are filtered to students
/// who fired `ARCHITECTURE_HOTSPOT` this sprint (consolidation: per-student
/// detail only appears when the flag is on) and to attributions whose
/// `weight > 0` (a 0-share row would render as "0% of lines", which is
/// the same as not authoring the offending region — pure noise).
///
/// `weight` here is a true blame share: `lines_authored / total_lines`
/// over the violation's start/end range, computed by the architecture
/// stage's blame attribution pass. A weight of 1.0 means the student owns
/// every line of the offending region; 0.5 means half of them.
fn architecture_violations_per_student(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<String, Vec<AttributedArchViolation>>> {
    let qualified_by_basename = qualified_repo_names_by_basename(conn);
    let mut stmt = conn.prepare(
        "SELECT a.student_id,
                av.rule_name, av.file_path, av.repo_full_name, av.severity,
                av.offending_import, av.start_line, av.end_line, av.explanation,
                a.weight
         FROM architecture_violation_attribution a
         JOIN architecture_violations av ON av.rowid = a.violation_rowid
         JOIN students s ON s.id = a.student_id
         WHERE a.sprint_id = ?
           AND s.team_project_id = ?
           AND a.weight > 0
           AND EXISTS (
             SELECT 1 FROM flags f
             WHERE f.sprint_id = a.sprint_id
               AND f.student_id = a.student_id
               AND f.flag_type = 'ARCHITECTURE_HOTSPOT'
           )
         ORDER BY a.weight DESC, av.file_path, av.start_line",
    )?;
    type Row = (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
        Option<String>,
        f64,
    );
    let rows: Vec<Row> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, f64>(9)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut out: HashMap<String, Vec<AttributedArchViolation>> = HashMap::new();
    for (sid, rule, file, repo, severity, import, start, end, expl, weight) in rows {
        let repo_fqn = qualify_repo_name(&repo, &qualified_by_basename);
        out.entry(sid).or_default().push(AttributedArchViolation {
            rule_name: rule,
            file_path: file,
            severity,
            offending_import: import,
            repo_full_qualified: repo_fqn,
            start_line: start,
            end_line: end,
            explanation: expl,
            weight,
        });
    }
    Ok(out)
}

/// Render a blame-share weight as ` · 72% of lines`. The leading
/// separator is part of the suffix so callers can append it directly to
/// any bullet without conditional spacing. Returns an empty string when
/// `weight ≤ 0` (which we already filter out at the SQL layer, but this
/// keeps the helper safe to call unconditionally).
fn format_blame_weight_suffix(weight: f64) -> String {
    if weight <= 0.0 {
        return String::new();
    }
    let pct = (weight * 100.0).round() as i64;
    let pct = pct.clamp(1, 100);
    format!(" · {}% of lines", pct)
}

/// Build a clickable GitHub URL for a file at the default branch (`HEAD`).
/// Returns `None` when `repo_full_qualified` is missing the `<org>/<repo>`
/// form (e.g. an old DB row that only stored the bare repo name).
fn github_file_url(repo_full_qualified: &str, file_path: &str) -> Option<String> {
    if !repo_full_qualified.contains('/') {
        return None;
    }
    Some(format!(
        "https://github.com/{}/blob/HEAD/{}",
        repo_full_qualified, file_path
    ))
}

/// Build a `bare-repo-name → <org>/<repo>` map by scanning
/// `pull_requests.repo_full_name`. Legacy `architecture_violations`
/// rows wrote only the bare directory name; the report rendering uses
/// this map to recover the qualified form so every file reference
/// becomes a clickable github link. Scoped globally because basenames
/// are unique across teams (per the `<stack>-<team>` convention) and
/// `pull_requests` has no direct project foreign key.
fn qualified_repo_names_by_basename(conn: &Connection) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT repo_full_name FROM pull_requests
         WHERE repo_full_name IS NOT NULL
           AND instr(repo_full_name, '/') > 0",
    ) else {
        return out;
    };
    let rows = stmt.query_map([], |r| r.get::<_, String>(0));
    if let Ok(rows) = rows {
        for row in rows.flatten() {
            let basename = row.rsplit('/').next().unwrap_or(&row).to_string();
            out.entry(basename).or_insert(row);
        }
    }
    out
}

/// If `repo` already looks qualified (`<org>/<repo>`) return it as-is.
/// Otherwise look up its qualified form in the project's basename map.
/// Falls back to the original string when no match exists.
fn qualify_repo_name(repo: &str, by_basename: &HashMap<String, String>) -> String {
    if repo.contains('/') {
        return repo.to_string();
    }
    by_basename
        .get(repo)
        .cloned()
        .unwrap_or_else(|| repo.to_string())
}

/// Render the line range as a `:Lstart-Lend` (or `:L<line>` when
/// start == end) suffix appended to the file basename. Returns empty
/// string when the violation has no line information.
fn format_line_suffix(start: Option<i64>, end: Option<i64>) -> String {
    match (start, end) {
        (Some(s), Some(e)) if e > s => format!(" :L{}-L{}", s, e),
        (Some(s), _) => format!(" :L{}", s),
        _ => String::new(),
    }
}

/// GitHub anchor (`#L<n>` or `#L<s>-L<e>`) so the file link jumps to
/// the offending range. Empty when no line information exists.
fn format_url_line_anchor(start: Option<i64>, end: Option<i64>) -> String {
    match (start, end) {
        (Some(s), Some(e)) if e > s => format!("#L{}-L{}", s, e),
        (Some(s), _) => format!("#L{}", s),
        _ => String::new(),
    }
}

/// MAD-based deviation of a density (stmts/points) from the project's
/// typical density. Matches `repo_analysis::task_similarity::mad_z`'s
/// convention `(value − median) / mad`. Returns `None` when MAD is zero
/// (density is constant across the project — no normalization possible)
/// or when the input value is unavailable.
fn density_mad_z(value: Option<f64>, median: f64, mad: f64) -> Option<f64> {
    let v = value?;
    if mad <= 0.0 {
        return None;
    }
    Some((v - median) / mad)
}

/// Render a density-deviation cell: ▲ above typical density (more
/// statements per point than the project median), ▼ below typical,
/// ≈ within ±1 MAD-z.
fn fmt_density_dev(z: Option<f64>) -> String {
    let Some(z) = z else { return String::new() };
    let symbol = if z > 1.0 {
        "▲"
    } else if z < -1.0 {
        "▼"
    } else {
        "≈"
    };
    format!("{symbol} {:+.2}", z)
}

fn write_section_b(
    buf: &mut String,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    depth: usize,
    sa_per_student: Option<(&HashMap<String, Vec<SaFinding>>, usize)>,
) -> rusqlite::Result<()> {
    let h2 = "#".repeat(depth);
    let h3 = "#".repeat(depth + 1);
    let _ = writeln!(buf, "{} B. Student dashboards\n", h2);

    let task_ls = task_ls_for_team(conn, sprint_id, project_id)?;
    let pr_ls = pr_ls_for_team(conn, sprint_id, project_id)?;
    let task_stmts = task_stmts_for_team(conn, sprint_id, project_id)?;
    let pr_stmts = pr_stmts_for_team(conn, sprint_id, project_id)?;
    let arch_per_student = architecture_violations_per_student(conn, sprint_id, project_id)?;
    let complexity_per_student = complexity_findings_per_student(conn, sprint_id, project_id)?;
    let qualified_by_basename = qualified_repo_names_by_basename(conn);

    // Per-PR documentation score (heuristic title+description rubric or
    // LLM judge). Populates a single column in the per-PR table below.
    // Silently returns an empty map when the table is absent so minimal
    // test fixtures keep working.
    let pr_doc_scores: HashMap<String, i64> = match conn.prepare(
        "SELECT pr_id, total_doc_score FROM pr_doc_evaluation
         WHERE sprint_id = ? AND total_doc_score IS NOT NULL",
    ) {
        Ok(mut stmt) => {
            let rows = stmt.query_map([sprint_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            let mut out = HashMap::new();
            for row in rows {
                let (pid, score) = row?;
                out.insert(pid, score);
            }
            out
        }
        Err(_) => HashMap::new(),
    };

    // Project-wide density baselines for the per-task and per-PR
    // "density Δ" columns. Density = stmts/points; we use median + MAD
    // (matching `task_similarity::mad_z`) so a single mass-edited PR
    // doesn't blow the gauge. Both are computed across this team for
    // this sprint — the same scope as Section B's tables.
    let mut task_densities: Vec<f64> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT t.id, COALESCE(t.estimation_points, 0)
             FROM tasks t JOIN students s ON s.id = t.assignee_id
             WHERE t.sprint_id = ? AND s.team_project_id = ?
               AND t.type != 'USER_STORY' AND t.status = 'DONE'",
        )?;
        let rows = stmt.query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
        })?;
        for row in rows {
            let (tid, pts) = row?;
            if pts <= 0.0 {
                continue;
            }
            let stmts = task_stmts.get(&tid).copied().unwrap_or(0.0);
            task_densities.push(stmts / pts);
        }
    }
    let task_density_median = sprint_grader_core::stats::median(&task_densities);
    let task_density_mad = sprint_grader_core::stats::mad(&task_densities);

    let pr_densities: Vec<f64> = pr_ls
        .iter()
        .filter_map(|(pr_id, (_ls, _ld, linked_pts))| {
            if *linked_pts <= 0.0 {
                return None;
            }
            let stmts = pr_stmts.get(pr_id).copied().unwrap_or(0.0);
            Some(stmts / linked_pts)
        })
        .collect();
    let pr_density_median = sprint_grader_core::stats::median(&pr_densities);
    let pr_density_mad = sprint_grader_core::stats::mad(&pr_densities);

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
    let students: Vec<(String, String, Option<String>)> = students
        .into_iter()
        .map(|(sid, name, github)| {
            let github = effective_github_login(conn, &sid, github.as_deref());
            (sid, name, github)
        })
        .collect();

    if let Some((sa_data, _)) = sa_per_student {
        let mut t_total = 0usize;
        let mut t_crit = 0usize;
        let mut t_warn = 0usize;
        let mut t_info = 0usize;
        for findings in sa_data.values() {
            for f in findings {
                t_total += 1;
                match f.severity.to_ascii_uppercase().as_str() {
                    "CRITICAL" => t_crit += 1,
                    "WARNING" => t_warn += 1,
                    "INFO" => t_info += 1,
                    _ => {}
                }
            }
        }
        if t_total > 0 {
            use sprint_grader_static_analysis::i18n as sai18n;
            let _ = writeln!(
                buf,
                "**{}:** {} ({} {} · {} {} · {} {})\n",
                sai18n::TEAM_TALLY_LABEL,
                t_total,
                t_crit,
                sai18n::SEVERITY_CRITICAL_PLURAL,
                t_warn,
                sai18n::SEVERITY_WARNING_PLURAL,
                t_info,
                sai18n::SEVERITY_INFO_PLURAL,
            );
        }
    }

    for (sid, name, github) in &students {
        let _ = writeln!(buf, "{} {}", h3, name);
        if let Some(g) = github {
            let _ = writeln!(buf, "_GitHub: {}_\n", github_inline(g));
        } else {
            buf.push('\n');
        }
        // Section A's per-student summary already carries a Density Δ
        // glyph (team-relative MAD-z). Per-row tables below carry their
        // own per-task / per-PR Density Δ so each row is interpretable
        // on its own.

        // Trajectory sparkline across sprints (composite_score from
        // student_trajectory; renders as a minimalist 120×28 SVG).
        let traj: Vec<f64> = conn
            .prepare(
                "SELECT composite_score FROM student_sprint_metrics
                 WHERE student_id = ? AND composite_score IS NOT NULL
                 ORDER BY sprint_id",
            )
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map([sid], |r| r.get::<_, Option<f64>>(0))
                    .ok()
                    .map(|it| it.filter_map(|r| r.ok()).flatten().collect())
            })
            .unwrap_or_default();
        if !traj.is_empty() {
            let _ = writeln!(
                buf,
                "_Trajectory:_ {}  ({} sprint{})\n",
                sparkline_svg(&traj, 120, 28),
                traj.len(),
                if traj.len() == 1 { "" } else { "s" }
            );
        }

        // Task table. Selects `t.id` too so the Key cell can hyperlink to
        // the TrackDev dashboard — same scheme the Excel PRs sheet uses.
        // Only DONE TASK/BUG rows count for the report.
        let mut stmt = conn.prepare(
            "SELECT t.id, t.task_key, t.name, t.estimation_points, t.status
             FROM tasks t
             WHERE t.sprint_id = ? AND t.assignee_id = ?
               AND t.type != 'USER_STORY' AND t.status = 'DONE'
             ORDER BY t.task_key",
        )?;
        let tasks: Vec<DoneTaskRow> = stmt
            .query_map(rusqlite::params![sprint_id, sid], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        if !tasks.is_empty() {
            buf.push_str("**Tasks**\n\n");
            push_table_header(
                buf,
                &[
                    "Key",
                    "Name",
                    "Points",
                    "LS",
                    "LD",
                    "LS/pt",
                    "Stmts",
                    "Stmts/pt",
                    "Density Δ",
                    "Status",
                ],
            );
            for (task_id, key, name, pts, status) in tasks {
                let key_str = key.unwrap_or_default();
                let key_cell = if key_str.is_empty() {
                    String::new()
                } else {
                    md_link(&key_str, &trackdev_task_url(task_id))
                };
                let (ls, ld) = task_ls.get(&task_id).copied().unwrap_or((0.0, 0.0));
                let stmts = task_stmts.get(&task_id).copied().unwrap_or(0.0);
                let pts_val = pts.unwrap_or(0.0);
                let ls_per_pt = if pts_val > 0.0 { ls / pts_val } else { 0.0 };
                let stmts_per_pt = if pts_val > 0.0 { stmts / pts_val } else { 0.0 };
                let density = if pts_val > 0.0 {
                    Some(stmts / pts_val)
                } else {
                    None
                };
                let density_cell = fmt_density_dev(density_mad_z(
                    density,
                    task_density_median,
                    task_density_mad,
                ));
                // push_table_row escapes pipes, so write by hand to keep the
                // [label](url) link intact.
                let _ = writeln!(
                    buf,
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    key_cell,
                    md_escape(&name.unwrap_or_default()),
                    pts.map(fmt_f1).unwrap_or_default(),
                    fmt_f1(ls),
                    fmt_f1(ld),
                    fmt_f2(ls_per_pt),
                    fmt_f1(stmts),
                    fmt_f2(stmts_per_pt),
                    density_cell,
                    md_escape(&status.unwrap_or_default()),
                );
            }
            buf.push('\n');
        }

        // PR table — only PRs whose linked task is a DONE TASK/BUG show up.
        // Membership is derived from task assignment (the canonical TrackDev
        // source) via task_pull_requests → tasks.assignee_id, NOT from
        // pull_requests.author_id. A student sees every PR linked to a DONE
        // task they're assigned to, even when `pr.author_id` is NULL or
        // points to a different github identity.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.title, pr.url,
                    pr.additions, pr.deletions, pr.attribution_errors
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.assignee_id = ?
               AND t.type != 'USER_STORY' AND t.status = 'DONE'
             ORDER BY pr.pr_number",
        )?;
        let prs: Vec<DonePrRow> = stmt
            .query_map(rusqlite::params![sprint_id, sid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    r.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        if !prs.is_empty() {
            buf.push_str("**Pull requests**\n\n");
            push_table_header(
                buf,
                &[
                    "#",
                    "Repo",
                    "Title",
                    "+/-",
                    "Est. points",
                    "LS",
                    "LD",
                    "LS/pt",
                    "Stmts",
                    "Stmts/pt",
                    "Density Δ",
                    "Doc score",
                ],
            );
            for (pr_id, num, repo, title, url, adds, dels, attr_errors) in prs {
                let repo_short = repo
                    .as_deref()
                    .and_then(|s| s.rsplit('/').next())
                    .unwrap_or("")
                    .to_string();
                let title_str = title.unwrap_or_default();
                let (num_cell, linked_title) = match url.as_deref() {
                    Some(u) if u.starts_with("http") => {
                        (md_link(&format!("#{}", num), u), md_link(&title_str, u))
                    }
                    _ => (format!("#{}", num), md_escape(&title_str)),
                };
                let attr_glyph = attribution_error_glyph(attr_errors.as_deref());
                let num_cell_with_glyph = if attr_glyph.is_empty() {
                    num_cell
                } else {
                    format!("{num_cell} {attr_glyph}")
                };
                let (ls, ld, linked_pts) = pr_ls.get(&pr_id).copied().unwrap_or((0.0, 0.0, 0.0));
                let stmts = pr_stmts.get(&pr_id).copied().unwrap_or(0.0);
                let ls_per_pt = if linked_pts > 0.0 {
                    ls / linked_pts
                } else {
                    0.0
                };
                let stmts_per_pt = if linked_pts > 0.0 {
                    stmts / linked_pts
                } else {
                    0.0
                };
                let pr_density = if linked_pts > 0.0 {
                    Some(stmts / linked_pts)
                } else {
                    None
                };
                let density_cell =
                    fmt_density_dev(density_mad_z(pr_density, pr_density_median, pr_density_mad));
                let doc_cell = pr_doc_scores
                    .get(&pr_id)
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                // push_table_row escapes pipes, but we want the link to stay
                // intact — emit by hand for this row.
                let _ = writeln!(
                    buf,
                    "| {} | {} | {} | +{} / -{} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    num_cell_with_glyph,
                    md_escape(&repo_short),
                    linked_title.replace('|', "\\|"),
                    adds,
                    dels,
                    fmt_f1(linked_pts),
                    fmt_f1(ls),
                    fmt_f1(ld),
                    fmt_f2(ls_per_pt),
                    fmt_f1(stmts),
                    fmt_f2(stmts_per_pt),
                    density_cell,
                    doc_cell,
                );
            }
            buf.push('\n');
        }

        // Flags for this student
        let mut stmt = conn.prepare(
            "SELECT flag_type, severity, details FROM flags
             WHERE student_id = ? AND sprint_id = ?
             ORDER BY severity DESC, flag_type",
        )?;
        let flags: Vec<(String, String, Option<String>)> = stmt
            .query_map(rusqlite::params![sid, sprint_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        if !flags.is_empty() {
            buf.push_str("**Flags**\n\n");
            for (flag_type, severity, details) in flags {
                let enriched_details =
                    enrich_flag_details(conn, sprint_id, sid, &flag_type, details.as_deref());
                let rendered = render_flag_details(
                    &flag_type,
                    enriched_details.as_deref().or(details.as_deref()),
                );
                let severity = render_flag_severity(&flag_type, &severity);
                let _ = writeln!(
                    buf,
                    "- **{}** _{}_ — {}",
                    flag_type, severity, rendered.markdown
                );
            }
            buf.push('\n');
        }

        if let Some(violations) = arch_per_student.get(sid) {
            if !violations.is_empty() {
                write_student_architecture_block(buf, violations);
            }
        }

        if let Some(findings) = complexity_per_student.get(sid) {
            if !findings.is_empty() {
                write_student_complexity_block(buf, findings, &qualified_by_basename);
            }
        }

        if let Some((sa_data, top_n)) = sa_per_student {
            if let Some(findings) = sa_data.get(sid) {
                if !findings.is_empty() {
                    write_student_static_analysis_block(buf, findings, top_n);
                }
            }
        }

        buf.push_str("---\n\n");
    }
    Ok(())
}

/// Per-student architecture-violation block. One bullet per attributed
/// violation, sorted by descending blame-share. Each bullet carries the
/// humanised rule prose, the violation's severity, and a `· N% of lines`
/// suffix derived from `architecture_violation_attribution.weight`.
/// Optional LLM-supplied explanation renders as a child bullet so the
/// reader sees the *why*, not just the tag.
fn write_student_architecture_block(buf: &mut String, violations: &[AttributedArchViolation]) {
    if violations.is_empty() {
        return;
    }
    let total = violations.len();
    let crit = violations
        .iter()
        .filter(|v| v.severity.eq_ignore_ascii_case("CRITICAL"))
        .count();
    let warn = violations
        .iter()
        .filter(|v| v.severity.eq_ignore_ascii_case("WARNING"))
        .count();
    let info_n = violations
        .iter()
        .filter(|v| v.severity.eq_ignore_ascii_case("INFO"))
        .count();

    let _ = writeln!(
        buf,
        "**Architecture violations:** {} ({} critical · {} warning · {} info)\n",
        total, crit, warn, info_n
    );

    let mut sorted: Vec<&AttributedArchViolation> = violations.iter().collect();
    sorted.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.offending_import.cmp(&b.offending_import))
    });

    for v in sorted {
        let basename = v.file_path.rsplit('/').next().unwrap_or(&v.file_path);
        let label = format!("`{}`", md_escape(basename));
        let line_suffix = format_line_suffix(v.start_line, v.end_line);
        let url_anchor = format_url_line_anchor(v.start_line, v.end_line);
        let file_cell = match github_file_url(&v.repo_full_qualified, &v.file_path) {
            Some(url) => format!(
                "[{}{}]({}{} \"{}\")",
                label,
                line_suffix,
                url,
                url_anchor,
                md_escape(&v.file_path)
            ),
            None => format!("{}{}", label, line_suffix),
        };
        let weight_suffix = format_blame_weight_suffix(v.weight);
        let _ = writeln!(
            buf,
            "- {} — {} _({})_{}",
            file_cell,
            humanize_rule_name(&v.rule_name),
            v.severity.to_lowercase(),
            weight_suffix
        );
        if let Some(prose) = v
            .explanation
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let _ = writeln!(buf, "  - {}", md_escape(prose));
        }
    }
    buf.push('\n');
}

// ── Static-analysis findings (T-SA) ──────────────────────────────────────────
//
// Findings are rendered inside each student's Section B block (after the
// architecture violations). The `static_analysis_per_student` function fetches
// all rows once; `write_section_b` distributes them per student via
// `write_student_static_analysis_block`. The block is gated by the
// `include_static_analysis` flag: `sync-reports --push` passes `false` so
// team-facing reports don't surface the findings.

struct SaFinding {
    analyzer: String,
    rule_id: String,
    severity: String,
    file_path: String,
    repo_full_name: String,
    start_line: Option<i64>,
    end_line: Option<i64>,
    message: String,
    weight: f64,
}

fn static_analysis_per_student(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<String, Vec<SaFinding>>> {
    let mut stmt = conn.prepare(
        "SELECT a.student_id,
                f.analyzer, f.rule_id, f.severity,
                f.file_path, f.repo_full_name,
                f.start_line, f.end_line, f.message,
                a.weight
         FROM static_analysis_finding_attribution a
         JOIN static_analysis_findings f ON f.id = a.finding_id
         JOIN students s ON s.id = a.student_id
         WHERE a.sprint_id = ? AND s.team_project_id = ?
         ORDER BY a.weight DESC, f.file_path, f.start_line",
    )?;
    let mut result: HashMap<String, Vec<SaFinding>> = HashMap::new();
    let rows = stmt.query_map(rusqlite::params![sprint_id, project_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            SaFinding {
                analyzer: r.get(1)?,
                rule_id: r.get(2)?,
                severity: r.get(3)?,
                file_path: r.get(4)?,
                repo_full_name: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                start_line: r.get(6)?,
                end_line: r.get(7)?,
                message: r.get(8)?,
                weight: r.get(9)?,
            },
        ))
    })?;
    for row in rows {
        let (student_id, finding) = row?;
        result.entry(student_id).or_default().push(finding);
    }
    Ok(result)
}

fn write_student_static_analysis_block(buf: &mut String, findings: &[SaFinding], top_n: usize) {
    use sprint_grader_static_analysis::i18n;

    let total = findings.len();
    let crit = findings
        .iter()
        .filter(|f| f.severity.eq_ignore_ascii_case("CRITICAL"))
        .count();
    let warn = findings
        .iter()
        .filter(|f| f.severity.eq_ignore_ascii_case("WARNING"))
        .count();
    let info_n = findings
        .iter()
        .filter(|f| f.severity.eq_ignore_ascii_case("INFO"))
        .count();
    let total_weight: f64 = findings.iter().map(|f| f.weight).sum();

    let _ = writeln!(
        buf,
        "**{}:** {} ({} {} · {} {} · {} {}) — {} {:.1}\n",
        i18n::BLOCK_HEADER,
        total,
        crit,
        i18n::SEVERITY_CRITICAL_PLURAL,
        warn,
        i18n::SEVERITY_WARNING_PLURAL,
        info_n,
        i18n::SEVERITY_INFO_PLURAL,
        i18n::WEIGHT_LABEL,
        total_weight,
    );

    let _ = top_n;
    let _ = i18n::MORE_LABEL;
    for f in findings.iter() {
        let basename = f.file_path.rsplit('/').next().unwrap_or(&f.file_path);
        let label = format!("`{}`", basename);
        let line_suffix = format_line_suffix(f.start_line, f.end_line);
        let url_anchor = format_url_line_anchor(f.start_line, f.end_line);
        let file_cell = match github_file_url(&f.repo_full_name, &f.file_path) {
            Some(url) => format!("[{}{}]({}{})", label, line_suffix, url, url_anchor),
            None => format!("{}{}", label, line_suffix),
        };
        let weight_suffix = format_blame_weight_suffix(f.weight);
        let _ = writeln!(
            buf,
            "- {} — `{}:{}` · _{}_ — {}{}",
            file_cell,
            f.analyzer,
            f.rule_id,
            f.severity.to_lowercase(),
            md_escape(f.message.lines().next().unwrap_or("")),
            weight_suffix,
        );
    }
    buf.push('\n');
}

// ── Per-student complexity & testability block ──────────────────────────────
//
// Folded into Section B alongside the architecture block: one consecutive
// per-student section covering Flags + Architecture + Complexity. Gated by
// `COMPLEXITY_HOTSPOT` firing for the student so the block only appears when
// the flag is on, and by `weight > 0` so we never render rows the student
// did not author.

struct ComplexityFinding {
    repo_full_name: String,
    file_path: String,
    class_name: Option<String>,
    method_name: String,
    start_line: i64,
    end_line: i64,
    rule_key: String,
    severity: String,
    measured_value: Option<f64>,
    threshold: Option<f64>,
    weight: f64,
}

fn complexity_findings_per_student(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<String, Vec<ComplexityFinding>>> {
    let mut stmt = conn.prepare(
        "SELECT a.student_id,
                f.repo_full_name, f.file_path, f.class_name, f.method_name,
                f.start_line, f.end_line, f.rule_key, f.severity,
                f.measured_value, f.threshold,
                a.weight
         FROM method_complexity_attribution a
         JOIN method_complexity_findings f ON f.id = a.finding_id
         JOIN students s ON s.id = a.student_id
         WHERE a.sprint_id = ?
           AND s.team_project_id = ?
           AND a.weight > 0
           AND EXISTS (
             SELECT 1 FROM flags fl
             WHERE fl.sprint_id = a.sprint_id
               AND fl.student_id = a.student_id
               AND fl.flag_type = 'COMPLEXITY_HOTSPOT'
           )
         ORDER BY a.weight DESC, f.file_path, f.start_line",
    )?;
    let mut result: HashMap<String, Vec<ComplexityFinding>> = HashMap::new();
    let rows = stmt.query_map(rusqlite::params![sprint_id, project_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            ComplexityFinding {
                repo_full_name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                file_path: r.get(2)?,
                class_name: r.get(3)?,
                method_name: r.get(4)?,
                start_line: r.get(5)?,
                end_line: r.get(6)?,
                rule_key: r.get(7)?,
                severity: r.get(8)?,
                measured_value: r.get(9)?,
                threshold: r.get(10)?,
                weight: r.get(11)?,
            },
        ))
    })?;
    for row in rows {
        let (sid, finding) = row?;
        result.entry(sid).or_default().push(finding);
    }
    Ok(result)
}

fn write_student_complexity_block(
    buf: &mut String,
    findings: &[ComplexityFinding],
    qualified_by_basename: &HashMap<String, String>,
) {
    use sprint_grader_quality::i18n as cxi18n;

    if findings.is_empty() {
        return;
    }
    let total = findings.len();
    let crit = findings
        .iter()
        .filter(|f| f.severity.eq_ignore_ascii_case("CRITICAL"))
        .count();
    let warn = findings
        .iter()
        .filter(|f| f.severity.eq_ignore_ascii_case("WARNING"))
        .count();
    let info_n = findings
        .iter()
        .filter(|f| f.severity.eq_ignore_ascii_case("INFO"))
        .count();

    let _ = writeln!(
        buf,
        "**Complexity & testability:** {} ({} critical · {} warning · {} info)\n",
        total, crit, warn, info_n
    );

    for f in findings {
        let repo = qualify_repo_name(&f.repo_full_name, qualified_by_basename);
        let class_prefix = f
            .class_name
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "<unknown>")
            .map(|c| format!("{c}."))
            .unwrap_or_default();
        let label = format!("`{}{}()`", class_prefix, f.method_name);
        let url_anchor = format_url_line_anchor(Some(f.start_line), Some(f.end_line));
        let line_suffix = format_line_suffix(Some(f.start_line), Some(f.end_line));
        let file_cell = match github_file_url(&repo, &f.file_path) {
            Some(url) => format!(
                "[{}{}]({}{} \"{}\")",
                label,
                line_suffix,
                url,
                url_anchor,
                md_escape(&f.file_path)
            ),
            None => format!("{}{}", label, line_suffix),
        };
        let prose = cxi18n::rule_prose(&f.rule_key);
        let measured_tail = match (f.measured_value, f.threshold) {
            (Some(m), Some(t)) => format!(
                " ({} > {})",
                round_to_int_if_integer(m),
                round_to_int_if_integer(t)
            ),
            _ => String::new(),
        };
        let weight_suffix = format_blame_weight_suffix(f.weight);
        let _ = writeln!(
            buf,
            "- {} — {} _({})_{}{}",
            file_cell,
            prose,
            f.severity.to_lowercase(),
            measured_tail,
            weight_suffix,
        );
    }
    buf.push('\n');
}

/// Professor-only attribution + flag block (T-CX, step 7). Renders
/// per-student weighted contribution across this sprint's complexity
/// findings, sorted by descending hotspot score (Σ weight ×
/// severity_rank), and inlines the COMPLEXITY_HOTSPOT flag detail row
/// for every student that crossed the threshold. Skipped entirely when
/// no findings or no attribution exist for the project.
fn write_section_complexity_attribution(
    buf: &mut String,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    depth: usize,
) -> rusqlite::Result<()> {
    use sprint_grader_quality::i18n as cxi18n;

    let h2 = "#".repeat(depth);
    let h3 = "#".repeat(depth + 1);

    type Row = (
        String,
        String,
        String,
        f64,
        String,
        String,
        Option<String>,
        String,
        i64,
        i64,
        String,
    );
    let mut stmt = conn.prepare(
        "SELECT a.student_id, COALESCE(s.full_name, s.id),
                f.severity, a.weight, f.rule_key, f.file_path,
                f.class_name, f.method_name, f.start_line, f.end_line,
                f.repo_full_name
         FROM method_complexity_attribution a
         JOIN method_complexity_findings f ON f.id = a.finding_id
         JOIN students s ON s.id = a.student_id
         WHERE a.sprint_id = ? AND s.team_project_id = ?
         ORDER BY s.full_name, a.weight DESC, f.file_path, f.start_line",
    )?;
    let rows: Vec<Row> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, String>(10)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    if rows.is_empty() {
        return Ok(());
    }

    let _ = writeln!(
        buf,
        "{} {} — grading attribution (instructor view)\n",
        h2,
        cxi18n::SECTION_HEADER
    );
    let _ = writeln!(buf, "{}", cxi18n::PROF_DISCLAIMER);
    let _ = writeln!(buf, "{} {}\n", h3, cxi18n::PROF_PER_STUDENT_HEADER);

    fn rank(sev: &str) -> u8 {
        match sev {
            "CRITICAL" => 3,
            "WARNING" => 2,
            "INFO" => 1,
            _ => 0,
        }
    }

    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Acc {
        full_name: String,
        score: f64,
        crit: usize,
        warn: usize,
        info: usize,
        weight_sum: f64,
        offenders: Vec<usize>,
    }
    let mut by_student: BTreeMap<String, Acc> = BTreeMap::new();
    for (idx, r) in rows.iter().enumerate() {
        let acc = by_student.entry(r.0.clone()).or_default();
        if acc.full_name.is_empty() {
            acc.full_name = r.1.clone();
        }
        let r_rank = rank(&r.2) as f64;
        acc.score += r.3 * r_rank;
        acc.weight_sum += r.3;
        match r.2.as_str() {
            "CRITICAL" => acc.crit += 1,
            "WARNING" => acc.warn += 1,
            "INFO" => acc.info += 1,
            _ => {}
        }
        acc.offenders.push(idx);
    }

    let mut entries: Vec<(String, Acc)> = by_student.into_iter().collect();
    entries.sort_by(|a, b| {
        b.1.score
            .partial_cmp(&a.1.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    type FlagRow = (String, String, String);
    let mut flag_stmt = conn.prepare(
        "SELECT student_id, severity, COALESCE(details, '')
         FROM flags WHERE sprint_id = ? AND flag_type = 'COMPLEXITY_HOTSPOT'",
    )?;
    let flags: BTreeMap<String, (String, String)> = flag_stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .map(|t: FlagRow| (t.0, (t.1, t.2)))
        .collect();
    drop(flag_stmt);

    let qualified_by_basename = qualified_repo_names_by_basename(conn);
    for (sid, acc) in entries {
        let band = flags
            .get(&sid)
            .map(|(sev, _)| sev.as_str())
            .unwrap_or("(below threshold)");
        let _ = writeln!(
            buf,
            "- **{}** — {} {:.2}, {} {:.2} · {} {}, {} {}, {} {} · {}: `{}`",
            acc.full_name,
            cxi18n::PROF_SCORE_LABEL,
            acc.score,
            cxi18n::PROF_WEIGHT_LABEL,
            acc.weight_sum,
            acc.crit,
            cxi18n::SEVERITY_CRITICAL_PLURAL,
            acc.warn,
            cxi18n::SEVERITY_WARNING_PLURAL,
            acc.info,
            cxi18n::SEVERITY_INFO_PLURAL,
            cxi18n::PROF_FLAG_SUMMARY_HEADER,
            band,
        );
        for &idx in acc.offenders.iter().take(5) {
            let r = &rows[idx];
            let repo = qualify_repo_name(&r.10, &qualified_by_basename);
            let basename = r.5.rsplit('/').next().unwrap_or(&r.5);
            let class_prefix =
                r.6.as_deref()
                    .filter(|s| !s.is_empty() && *s != "<unknown>")
                    .map(|c| format!("{c}."))
                    .unwrap_or_default();
            let method_label = format!("`{}{}()`", class_prefix, r.7);
            let url_anchor = format_url_line_anchor(Some(r.8), Some(r.9));
            let method_link = match github_file_url(&repo, &r.5) {
                Some(url) => format!("[{}]({}{})", method_label, url, url_anchor),
                None => method_label,
            };
            let prose = cxi18n::rule_prose(&r.4);
            let _ = writeln!(
                buf,
                "  - {} ({}, {} {:.2}) — {} — `{}`",
                method_link,
                r.2,
                cxi18n::PROF_WEIGHT_LABEL,
                r.3,
                md_escape(prose),
                basename,
            );
        }
        if acc.offenders.len() > 5 {
            let _ = writeln!(
                buf,
                "  - … {} {}",
                acc.offenders.len() - 5,
                cxi18n::MORE_LABEL
            );
        }
    }
    buf.push('\n');
    Ok(())
}

/// Render a metric value as an integer when it has no fractional part,
/// otherwise `:.1`.
fn round_to_int_if_integer(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value as i64)
    } else {
        format!("{:.1}", value)
    }
}

// ── Annex: orphan PRs (no linked tasks) ──────────────────────────────────────

/// Lists PRs that have no linked tasks and therefore have no TrackDev
/// authors under the task-assignee-derived authorship model. These PRs
/// contribute zero evidence to identity resolution and are excluded from
/// every author-keyed metric/flag — surfacing them here lets professors
/// follow up.
fn write_orphan_pr_annex(
    buf: &mut String,
    conn: &Connection,
    project_id: i64,
    sprint_ids_ordered: &[i64],
    depth: usize,
) -> rusqlite::Result<()> {
    if sprint_ids_ordered.is_empty() {
        return Ok(());
    }
    let h2 = "#".repeat(depth);
    let placeholders: String = std::iter::repeat("?")
        .take(sprint_ids_ordered.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.title, pr.url,
                pr.merged_at
         FROM pull_requests pr
         WHERE pr.repo_full_name IN (
                 SELECT DISTINCT pr2.repo_full_name FROM pull_requests pr2
                 JOIN task_pull_requests tpr ON tpr.pr_id = pr2.id
                 JOIN tasks t ON t.id = tpr.task_id
                 JOIN students s ON s.id = t.assignee_id
                 WHERE s.team_project_id = ?
                   AND t.sprint_id IN ({placeholders})
               )
           AND pr.id NOT IN (SELECT pr_id FROM task_pull_requests)
         ORDER BY pr.merged_at, pr.pr_number"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> =
        Vec::with_capacity(1 + sprint_ids_ordered.len());
    params_vec.push(Box::new(project_id));
    for sid in sprint_ids_ordered {
        params_vec.push(Box::new(*sid));
    }
    let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(&params_refs[..], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    if rows.is_empty() {
        return Ok(());
    }
    let _ = writeln!(buf, "{} Ghost contributor — orphan pull requests\n", h2);
    let _ = writeln!(
        buf,
        "These PRs touch this team's repos but link to **no TrackDev tasks**. \
Under the task-assignee author model they have no student to attribute to, \
so they're collected here as if produced by a synthetic 'ghost' team \
member: visible for review, excluded from every author-keyed metric. \
Each row is a hint to either link a task or mark the PR as out-of-scope.\n"
    );
    push_table_header(buf, &["#", "Repo", "Title", "Merged"]);
    for (_id, num, repo, title, url, merged_at) in rows {
        let repo_short = repo
            .as_deref()
            .and_then(|s| s.rsplit('/').next())
            .unwrap_or("")
            .to_string();
        let title_str = title.unwrap_or_default();
        let (num_cell, linked_title) = match url.as_deref() {
            Some(u) if u.starts_with("http") => {
                (md_link(&format!("#{}", num), u), md_link(&title_str, u))
            }
            _ => (format!("#{}", num), md_escape(&title_str)),
        };
        let merged = merged_at
            .as_deref()
            .map(humanize_local_dt)
            .unwrap_or_default();
        let _ = writeln!(
            buf,
            "| {} | {} | {} | {} |",
            num_cell,
            md_escape(&repo_short),
            linked_title.replace('|', "\\|"),
            md_escape(&merged),
        );
    }
    buf.push('\n');
    Ok(())
}

// ── Section C: peer-group analysis ───────────────────────────────────────────

fn write_section_c(
    buf: &mut String,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    depth: usize,
) -> rusqlite::Result<()> {
    let h2 = "#".repeat(depth);
    let h3 = "#".repeat(depth + 1);
    let mut stmt = conn.prepare(
        "SELECT group_id, group_label, member_count, median_points,
                median_ls, median_ls_per_point
         FROM task_similarity_groups
         WHERE sprint_id = ? AND (project_id = ? OR project_id IS NULL)
         ORDER BY stack, layer, action, group_id",
    )?;
    struct G {
        group_id: i64,
        label: String,
        member_count: i64,
        median_points: Option<f64>,
        median_ls: Option<f64>,
        median_ls_per_point: Option<f64>,
    }
    let groups: Vec<G> = stmt
        .query_map(rusqlite::params![sprint_id, project_id], |r| {
            Ok(G {
                group_id: r.get::<_, i64>(0)?,
                label: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                member_count: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                median_points: r.get::<_, Option<f64>>(3)?,
                median_ls: r.get::<_, Option<f64>>(4)?,
                median_ls_per_point: r.get::<_, Option<f64>>(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    if groups.is_empty() {
        let _ = writeln!(
            buf,
            "{} C. Peer-group analysis\n\n_No task similarity groups for this sprint._\n",
            h2
        );
        return Ok(());
    }

    let _ = writeln!(buf, "{} C. Peer-group analysis\n", h2);
    for g in &groups {
        let _ = writeln!(
            buf,
            "{} {} ({} member{})",
            h3,
            g.label,
            g.member_count,
            if g.member_count == 1 { "" } else { "s" }
        );
        let mut bullets: Vec<String> = Vec::new();
        if let Some(v) = g.median_points {
            bullets.push(format!("median points: {}", fmt_f2(v)));
        }
        if let Some(v) = g.median_ls {
            bullets.push(format!("median LS: {}", fmt_f2(v)));
        }
        if let Some(v) = g.median_ls_per_point {
            bullets.push(format!("median LS/pt: {}", fmt_f2(v)));
        }
        if !bullets.is_empty() {
            let _ = writeln!(buf, "_{}_\n", bullets.join(" · "));
        } else {
            buf.push('\n');
        }

        let mut stmt = conn.prepare(
            "SELECT t.id, t.task_key, t.name, s.full_name,
                    tgm.is_outlier, tgm.outlier_reason,
                    t.estimation_points
             FROM task_group_members tgm
             JOIN tasks t ON t.id = tgm.task_id
             LEFT JOIN students s ON s.id = t.assignee_id
             WHERE tgm.group_id = ?
               AND t.type != 'USER_STORY' AND t.status = 'DONE'
             ORDER BY tgm.is_outlier DESC, t.task_key",
        )?;
        let members: Vec<GroupMemberRow> = stmt
            .query_map([g.group_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<f64>>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        push_table_header(buf, &["Task", "Assignee", "Points", "Outlier?", "Reason"]);
        for (task_id, key, name, assignee, is_outlier, reason, pts) in members {
            let marker = if is_outlier == 1 { "**outlier**" } else { "" };
            let url = trackdev_task_url(task_id);
            let name_part = name.unwrap_or_default();
            // Build a linked Task cell. The push_table_row helper escapes
            // pipes in every cell, which would break `[...](...)`; write the
            // row by hand so the link renders.
            let task_cell = match key.as_deref() {
                Some(k) if !k.is_empty() && !name_part.is_empty() => {
                    format!("{} — {}", md_link(k, &url), md_escape(&name_part))
                }
                Some(k) if !k.is_empty() => md_link(k, &url),
                _ => md_escape(&name_part),
            };
            let _ = writeln!(
                buf,
                "| {} | {} | {} | {} | {} |",
                task_cell,
                md_escape(&assignee.unwrap_or_default()),
                pts.map(fmt_f1).unwrap_or_default(),
                marker,
                md_escape(&compact_reason_numbers(&reason.unwrap_or_default())),
            );
        }
        buf.push('\n');
    }
    // Suppress a warning in release builds where html_escape is unused here.
    let _ = html_escape;
    Ok(())
}

/// Default TOC depth: include H2 (sections / sprint headers) and H3
/// (students, peer groups, A/B/C subsections in multi-sprint mode). Going
/// deeper would push student-detail subsections (e.g. Tasks/PRs) into the
/// TOC even though they're not separate headings, and shallower would lose
/// the per-student jump targets students actually want.
const TOC_MAX_DEPTH: usize = 3;

/// Compute a GitHub-style anchor slug for `text`, mutating `used` so repeated
/// headings get suffixed `-1`, `-2`, … in the order they appear. Mirrors the
/// algorithm used by GitHub/GitLab/cmark-gfm renderers: lowercase the text,
/// drop everything that isn't alphanumeric / `-` / `_`, and turn whitespace
/// into hyphens. Backslash-escapes (e.g. `\|` produced by `md_escape`) drop
/// out the same way they do at render time, so the slug stays in sync.
fn slugify_anchor(text: &str, used: &mut HashMap<String, usize>) -> String {
    let mut s = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for low in ch.to_lowercase() {
                s.push(low);
            }
        } else if ch.is_whitespace() {
            s.push('-');
        } else if ch == '-' || ch == '_' {
            s.push(ch);
        }
    }
    let count = used.entry(s.clone()).or_insert(0);
    let result = if *count == 0 {
        s.clone()
    } else {
        format!("{}-{}", s, *count)
    };
    *count += 1;
    result
}

/// Heading text prefix (case-insensitive) used to recognise the cumulative
/// per-student summary section. Per-student rows under this section are
/// suppressed in the TOC because the section already lists every student
/// linearly — duplicating them in the index just doubles the noise.
const CUMULATIVE_SECTION_PREFIX: &str = "d. cumulative";

/// Heading text prefix (case-insensitive) used to recognise the glossary.
/// The glossary's H3 subsections are pedagogical groupings and don't add
/// real navigation value over the H2 entry; collapse them in the TOC.
const GLOSSARY_SECTION_PREFIX: &str = "0. glossary";

/// Heading text prefix (case-insensitive) used to recognise the per-student
/// dashboards section. Students under this section are surfaced in the TOC
/// regardless of nesting depth — in multi-sprint reports section B is at
/// H3 and student names sit at H4, which would otherwise fall outside
/// `TOC_MAX_DEPTH` and disappear from the navigation index.
const STUDENT_SECTION_PREFIX: &str = "b. student";

/// Build a Markdown table of contents from `body` by scanning every line that
/// starts with `#`. H1 is treated as the document title and is skipped; the
/// TOC entries cover depths 2..=`max_depth`. Indentation is two spaces per
/// nesting level so GitHub renders a proper nested list. Returns an empty
/// string when the body has no in-scope headings.
///
/// All headings (including ones we don't render in the TOC) are pushed
/// through `slugify_anchor` so the duplicate-counter stays aligned with what
/// the rendering engine will assign — without that, e.g. a multi-sprint
/// report's repeated "A. Team snapshot" anchors would drift off-by-one.
///
/// H3 entries whose nearest H2 ancestor is the cumulative per-student
/// summary are intentionally dropped from the TOC: the section already
/// enumerates every student in document order, so listing them in the
/// index too just bloats the navigation header.
fn build_toc(body: &str, max_depth: usize) -> String {
    if max_depth < 2 {
        return String::new();
    }
    let mut entries: Vec<(usize, String, String)> = Vec::new();
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut current_h2_text: Option<String> = None;
    let mut current_h3_text: Option<String> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes == 0 || hashes > 6 {
            continue;
        }
        let rest = &trimmed[hashes..];
        if !rest.starts_with(' ') && !rest.starts_with('\t') {
            continue;
        }
        let text = rest.trim();
        if text.is_empty() {
            continue;
        }
        let anchor = slugify_anchor(text, &mut used);
        // Maintain ancestor pointers so children can interrogate them.
        // A new H2 invalidates the H3 pointer; an H3 sets it.
        if hashes == 2 {
            current_h2_text = Some(text.to_string());
            current_h3_text = None;
        } else if hashes == 3 {
            current_h3_text = Some(text.to_string());
        }
        // Section B per-student rows live at H4 in multi-sprint reports
        // and at H3 in single-sprint reports. Surface them in the TOC
        // either way: if the immediate H3 ancestor is "B. Student
        // dashboards", an H4 row is allowed through even past max_depth.
        let in_student_section_overflow = hashes == max_depth + 1
            && current_h3_text
                .as_deref()
                .map(|s| s.to_lowercase().starts_with(STUDENT_SECTION_PREFIX))
                .unwrap_or(false);

        if hashes < 2 || (hashes > max_depth && !in_student_section_overflow) {
            continue;
        }
        // Drop H3 rows under sections that already enumerate their
        // children inline (the cumulative summary lists every student;
        // the glossary's subsections are pedagogical groupings, not
        // navigation targets).
        if hashes > 2 {
            if let Some(parent) = &current_h2_text {
                let lower = parent.to_lowercase();
                if lower.starts_with(CUMULATIVE_SECTION_PREFIX)
                    || lower.starts_with(GLOSSARY_SECTION_PREFIX)
                {
                    continue;
                }
            }
        }
        entries.push((hashes, text.to_string(), anchor));
    }
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(entries.len() * 64 + 64);
    out.push_str("## Table of contents\n\n");
    for (depth, text, anchor) in &entries {
        let indent = "  ".repeat(depth - 2);
        let _ = writeln!(out, "{}- [{}](#{})", indent, text, anchor);
    }
    out.push('\n');
    out
}

/// Insert a TOC block in `buf` right before the first H2 heading. Picks up
/// after the H1 banner (and any leading blockquote / italic line such as
/// `_Sprint window: …_`) so the TOC sits between the title and the first
/// real section. No-op when the body has no H2.
fn insert_toc(buf: &mut String, max_depth: usize) {
    let toc = build_toc(buf, max_depth);
    if toc.is_empty() {
        return;
    }
    if buf.starts_with("## ") {
        buf.insert_str(0, &toc);
        return;
    }
    if let Some(i) = buf.find("\n## ") {
        buf.insert_str(i + 1, &toc);
    }
}

/// Static glossary explaining every metric, signal and severity students
/// will encounter in the rest of the report. Rendered as section "0." so
/// it precedes A/B/C/D and shows up at the top of the TOC. Edit this
/// string when introducing a new metric column or flag family — the
/// glossary is the canonical student-facing reference.
const GLOSSARY_BODY: &str = "\
## 0. Glossary — how to read this report\n\
\n\
This report aggregates four kinds of evidence per sprint: **task delivery** \
(TrackDev), **code authorship** (Git blame on merged PRs), **code quality \
& process** (commits, reviews, build outcomes), and **architectural \
conformance** (static analysis of the cloned repos). The terms below are \
used throughout the tables, charts and flags.\n\
\n\
### Code volume\n\
\n\
- **LAT** — *Lines Added Total*. Raw `+` lines in the merged diff (before \
any filtering). The most permissive measure of code volume; includes \
formatting changes, generated code and renames.\n\
- **LAR** — *Lines Added Real*. LAT after stripping blank lines and \
comment-only lines. The diff line count we treat as \"actual code\" \
without any survival or cosmetic-rewrite filter applied.\n\
- **LD** — *Lines Deleted*. Lines removed by the PR's diff, used to \
recognise legitimate refactor/cleanup work that LS alone would miss.\n\
- **LS** — *Lines Surviving*. Statements introduced by the student that \
still exist (after AST-fingerprint-aware blame) at the report's reference \
date. This is the volume figure used in Section B and in flags. LS \
attributes at the *statement* level, not the line level — whitespace \
changes and identifier renames don't inflate or deflate it.\n\
- **LS/pt** — LS divided by the task's estimation points. A density \
measure: high values mean the task delivered more surviving code per \
estimated point than typical; low values mean the opposite.\n\
- **Stmts** — *Surviving statements (normalised)*. AST-normalised \
statements introduced by the student that survive at the report's \
reference date, taken from `pr_survival.statements_surviving_normalized`. \
Less sensitive to verbosity, formatting, identifier renames and cosmetic \
rewrites than LS, because the AST normaliser collapses whitespace, masks \
identifiers and masks literals before counting. For tasks the value is \
distributed across a PR's linked tasks weighted by estimation-point \
share; for PRs the value is the raw PR-level total.\n\
- **Stmts/pt** — Stmts divided by the task's (or the PR's linked tasks') \
estimation points. The same density idea as LS/pt but on the statement \
unit, which is the better red flag for over- or under-estimation: it is \
harder to inflate by reformatting, harder to deflate by refactor, and is \
the unit used in the per-student `estimation_density` aggregate.\n\
- **Weighted PR Lines** — each PR's `+` and `-` lines distributed across \
its linked tasks weighted by estimation-point share. Used in the \
cumulative summary so multi-task PRs don't double-count.\n\
\n\
### Estimation calibration\n\
\n\
- **Density Δ** — MAD-normalised deviation of this row's `stmts/points` \
from the median density across the relevant scope: \
`(stmts_per_pt − median) / MAD`. In the per-student summary the median \
is computed across teammates; in the per-task and per-PR tables it is \
computed across the project's tasks/PRs in the sprint. **▲** denser \
than typical (more code per point — under-pointed or unusually dense \
work), **▼** sparser than typical (over-pointed or light task), **≈** \
within ±1 MAD-z. Empty when MAD is zero (density is uniform — no \
normalisation possible) or the row has zero estimation points.\n\
\n\
### PR submission timing\n\
\n\
The horizontal stacked bar in section A classifies each merged PR by \
when it landed relative to the sprint deadline:\n\
\n\
- **Regular** — submitted with at least a working day of runway.\n\
- **Late** — close to the deadline but not at the wire.\n\
- **Critical / Cramming** — pushed in the final hours; correlates with \
weak review and rushed integration.\n\
- **Fix** — explicit fix-up PR (title or linked task type).\n\
\n\
### Code ownership\n\
\n\
- **Truck factor** — the smallest number of authors whose combined \
ownership covers ≥ 50 % of the team's surviving code. Higher is better \
(less concentrated). The owners list shows top contributors by share.\n\
\n\
### Architecture conformance\n\
\n\
The cloned repos are scanned against the project rubric \
(`config/architecture.toml`). Every offending Java file produces a \
**violation** with three pieces of metadata:\n\
\n\
- **Rule** — what was violated. Layered rules read as `<from>` *must \
not depend on* `<to>` (e.g. *presentation must not depend on \
infrastructure*); AST rules document a structural constraint such as \
\"fragments must not hold Retrofit fields\".\n\
- **Severity** — *CRITICAL*, *WARNING* or *INFO*, set by the rule itself.\n\
- **Attribution** — the dominant author of the offending file (by \
surviving statements). That author owns the violation in section B.\n\
\n\
### Static analysis (per student)\n\
\n\
Each student dashboard ends with a **Static analysis** block summarising \
findings from PMD, Checkstyle and SpotBugs that `git blame` attributes \
to that student. **Informational only — these findings do not affect \
the assignment grade.** The headline reports total findings broken down \
by severity, plus a `weight` figure: weight reflects how much of each \
offending region the student authored, so a 1-line typo fix on a 30-line \
method weighs ~0.03, not 0.50. Findings are listed top-`weight` first; \
the team-wide tally (when present) appears just under the section B \
heading.\n\
\n\
### Flags\n\
\n\
Flags are signal triggers: each one fires when a specific evidence \
threshold is crossed. They appear under **Flags** in each student \
dashboard with a severity tag:\n\
\n\
- **CRITICAL** — strong evidence of an issue that warrants direct \
discussion (e.g. *Ghost contributor*, *PR does not compile*).\n\
- **WARNING** — meaningful concern that the student should be aware of \
(e.g. *Low survival rate*, *Cramming*, *Architecture drift*).\n\
- **INFO** — informational signal, often paired contextually with a \
warning elsewhere (e.g. the *Cosmetic rewrite — victim* row pairs with \
the *actor* row).\n\
\n\
The **⚠** glyph next to a PR number means the data pipeline recorded an \
attribution error (base-SHA fallback, missing author, GitHub HTTP \
hiccup). It is observability metadata, never a grading penalty.\n\
\n\
### Doc-score signals\n\
\n\
- **Avg Doc Score** — quality of PR descriptions and titles, scored on \
0–6 (title 0–2 + description 0–4). Computed by an LLM rubric when an \
Anthropic key is configured, otherwise by deterministic heuristics \
(empty body / generic title).\n\
\n\
### Peer-group analysis (section C)\n\
\n\
Tasks are bucketed into peer groups by **stack × layer × action** (e.g. \
*spring · controller · create*). Within each group, tasks whose \
points / LS / LS-per-point fall outside a MAD-based band are marked as \
**outliers**. An outlier is a discussion seed, not a verdict.\n\
\n";

/// Inject the static glossary at the top of `buf`, just after the H1
/// banner (and any leading blockquote / italic line). The function is a
/// no-op when there is no H2 heading to anchor before — the glossary is
/// only useful in the context of a full report.
fn insert_glossary(buf: &mut String) {
    if buf.starts_with("## ") {
        buf.insert_str(0, GLOSSARY_BODY);
        return;
    }
    if let Some(i) = buf.find("\n## ") {
        buf.insert_str(i + 1, GLOSSARY_BODY);
    }
}

/// Build a "Team identity map" markdown section linking each TrackDev
/// student to their resolved GitHub identity. Returns `None` when the
/// project has no enrolled students. The caller is responsible for
/// splicing the returned block into the right place in the report.
fn render_team_identity_map(
    conn: &Connection,
    project_id: i64,
    depth: usize,
) -> rusqlite::Result<Option<String>> {
    let mut buf = String::new();
    write_team_identity_map_into(&mut buf, conn, project_id, depth)?;
    Ok(if buf.is_empty() { None } else { Some(buf) })
}

/// Splice the team identity map immediately before the first H2 heading
/// in `buf`, mirroring `insert_glossary`. Order of operations: call this
/// **after** `insert_glossary` so the map ends up before the glossary
/// (i.e. is the first level-2 section the reader sees).
fn insert_team_identity_map(buf: &mut String, conn: &Connection, project_id: i64) {
    let block = match render_team_identity_map(conn, project_id, 2) {
        Ok(Some(b)) => b,
        _ => return,
    };
    if buf.starts_with("## ") {
        buf.insert_str(0, &block);
        return;
    }
    if let Some(i) = buf.find("\n## ") {
        buf.insert_str(i + 1, &block);
    }
}

fn write_team_identity_map_into(
    buf: &mut String,
    conn: &Connection,
    project_id: i64,
    depth: usize,
) -> rusqlite::Result<()> {
    // Source of truth for resolved identities is `student_github_identity`
    // (populated by collect::identity_resolver). `github_users` and
    // `students.github_login` are cold-start fallbacks only — kept here
    // for the boot path where the resolver hasn't run yet, mirroring
    // the precedence in survival::blame::build_email_to_student_map.
    //
    // Per kind we pick the heaviest-weight, then highest-confidence row
    // (correlated subquery; six-row teams make cost trivial).
    let mut stmt = conn.prepare(
        "SELECT s.username, s.full_name, s.email,
                COALESCE(
                    (SELECT identity_value FROM student_github_identity
                     WHERE student_id = s.id AND identity_kind = 'login'
                     ORDER BY weight DESC, confidence DESC, identity_value
                     LIMIT 1),
                    gu.login,
                    s.github_login
                ),
                COALESCE(
                    (SELECT identity_value FROM student_github_identity
                     WHERE student_id = s.id AND identity_kind = 'email'
                     ORDER BY weight DESC, confidence DESC, identity_value
                     LIMIT 1),
                    gu.email
                )
         FROM students s
         LEFT JOIN github_users gu ON gu.student_id = s.id
         WHERE s.team_project_id = ?
         ORDER BY COALESCE(s.full_name, s.username, s.id)",
    )?;
    type Row = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<Row> = stmt
        .query_map([project_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    if rows.is_empty() {
        return Ok(());
    }

    let h = "#".repeat(depth);
    let _ = writeln!(buf, "{} Team identity map\n", h);

    // Cross-project guard. By workspace invariant a PR's task-assignee set
    // must lie entirely within one project; if it ever spans two, the team
    // mapping itself is corrupt and every downstream metric is suspect.
    // List offenders here before the identity table so the warning is
    // unmissable. SELECT counts assignees by team_project_id over the
    // pr_authors view; the OFFENDERS subquery then surfaces the actual PRs.
    let mut offender_stmt = conn.prepare(
        "SELECT pr.id, pr.pr_number, pr.repo_full_name, pr.title, pr.url,
                GROUP_CONCAT(DISTINCT p.name) AS projects
         FROM pull_requests pr
         JOIN pr_authors pa ON pa.pr_id = pr.id
         JOIN students s ON s.id = pa.student_id
         JOIN projects p ON p.id = s.team_project_id
         WHERE pr.id IN (
             SELECT pa2.pr_id FROM pr_authors pa2
             JOIN students s2 ON s2.id = pa2.student_id
             WHERE s2.team_project_id = ?
         )
         GROUP BY pr.id
         HAVING COUNT(DISTINCT s.team_project_id) > 1
         ORDER BY pr.pr_number",
    )?;
    type OffenderRow = (
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let offenders: Vec<OffenderRow> = offender_stmt
        .query_map([project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(offender_stmt);
    if offenders.is_empty() {
        let _ = writeln!(
            buf,
            "_All PRs that touch this team are scoped to it (no cross-project PRs)._\n"
        );
    } else {
        let _ = writeln!(
            buf,
            "**⚠ Cross-project PRs detected.** Every PR that touches this \
team should have all its task assignees in the same project. The PRs below \
are linked to tasks across multiple projects, which means a downstream \
metric is reading a corrupted team mapping. Resolve before trusting this \
report.\n"
        );
        for (_id, num, repo, title, url, projects_csv) in &offenders {
            let title_str = title.clone().unwrap_or_default();
            let repo_short = repo
                .as_deref()
                .and_then(|s| s.rsplit('/').next())
                .unwrap_or("")
                .to_string();
            let pr_label = match num {
                Some(n) => format!("#{}", n),
                None => "(unnumbered)".to_string(),
            };
            let pr_cell = match url.as_deref() {
                Some(u) if u.starts_with("http") => md_link(&pr_label, u),
                _ => pr_label.clone(),
            };
            let projects_label = projects_csv.clone().unwrap_or_default();
            let _ = writeln!(
                buf,
                "- {} in `{}` — {}  \n  _projects on this PR: {}_",
                pr_cell,
                md_escape(&repo_short),
                md_escape(&title_str),
                md_escape(&projects_label),
            );
        }
        buf.push('\n');
    }

    push_table_header(
        buf,
        &[
            "TrackDev user",
            "Full name",
            "TrackDev email",
            "GitHub",
            "GitHub email",
        ],
    );
    for (username, full_name, td_email, gh_login, gh_email) in rows {
        let user_cell = username.unwrap_or_default();
        let name_cell = full_name.unwrap_or_default();
        let td_email_cell = td_email.unwrap_or_default();
        let gh_cell = match gh_login.as_deref().filter(|s| !s.is_empty()) {
            Some(login) => github_cell(login),
            None => "—".to_string(),
        };
        let gh_email_cell = gh_email
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "—".to_string());
        push_table_row(
            buf,
            &[
                md_escape(&user_cell),
                md_escape(&name_cell),
                md_escape(&td_email_cell),
                gh_cell,
                md_escape(&gh_email_cell),
            ],
        );
    }
    buf.push('\n');
    Ok(())
}

pub fn generate_markdown_report(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    project_name: &str,
    output_dir: &Path,
) -> rusqlite::Result<PathBuf> {
    generate_markdown_report_ex(conn, sprint_id, project_id, project_name, output_dir, None)
}

/// Extended entry point that renders the A/B/C sections for `sprint_id` and,
/// when `cumulative_sprint_ids` is `Some`, adds cumulative team-snapshot
/// totals and appends a per-student cumulative summary (one row per sprint
/// in the chain) mirroring Python's
/// `_render_cumulative_summary_per_student` in `word_report.py`.
///
/// Excel reports stay per-sprint regardless — matching Python's design,
/// where `--cumulative` only alters the narrative document.
pub fn generate_markdown_report_ex(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    project_name: &str,
    output_dir: &Path,
    cumulative_sprint_ids: Option<&[i64]>,
) -> rusqlite::Result<PathBuf> {
    let path = output_dir.join(format!("report_{}.md", project_name));
    generate_markdown_report_to_path_ex(
        conn,
        sprint_id,
        project_id,
        project_name,
        &path,
        cumulative_sprint_ids,
    )?;
    Ok(path)
}

pub fn generate_markdown_report_to_path_ex(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    project_name: &str,
    output_path: &Path,
    cumulative_sprint_ids: Option<&[i64]>,
) -> rusqlite::Result<()> {
    // Default: instructor-friendly (include the static-analysis section).
    // `sync-reports --push` calls the `_ex2` variant with `false`.
    generate_markdown_report_to_path_ex2(
        conn,
        sprint_id,
        project_id,
        project_name,
        output_path,
        cumulative_sprint_ids,
        true,
    )
}

/// Like `generate_markdown_report_to_path_ex` but takes
/// `include_static_analysis`. T-SA: `false` strips the "Static code
/// analysis" section so reports pushed to team repos don't surface the
/// findings (instructor-only by default).
pub fn generate_markdown_report_to_path_ex2(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    project_name: &str,
    output_path: &Path,
    cumulative_sprint_ids: Option<&[i64]>,
    include_static_analysis: bool,
) -> rusqlite::Result<()> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    }

    let mut buf = String::with_capacity(16 * 1024);
    write_section_a(
        &mut buf,
        conn,
        sprint_id,
        project_id,
        project_name,
        cumulative_sprint_ids,
        2,
    )?;
    let sa_data = if include_static_analysis {
        Some(static_analysis_per_student(conn, sprint_id, project_id)?)
    } else {
        None
    };
    write_section_b(
        &mut buf,
        conn,
        sprint_id,
        project_id,
        2,
        sa_data.as_ref().map(|d| (d, DEFAULT_TOP_N_PER_STUDENT)),
    )?;
    write_section_c(&mut buf, conn, sprint_id, project_id, 2)?;
    if let Some(sids) = cumulative_sprint_ids {
        if !sids.is_empty() {
            write_cumulative_summary(&mut buf, conn, project_id, sids, 2)?;
        }
    }
    // Glossary must land before the TOC builder runs so its heading is
    // surfaced as the first TOC entry. Team identity map is injected
    // **after** the glossary so it ends up before it in the final order
    // (each insert places itself before the current first H2).
    insert_glossary(&mut buf);
    insert_team_identity_map(&mut buf, conn, project_id);
    insert_toc(&mut buf, TOC_MAX_DEPTH);

    std::fs::write(output_path, buf)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    info!(path = %output_path.display(), cumulative = cumulative_sprint_ids.is_some(), include_static_analysis, "Markdown report written");
    Ok(())
}

/// Default cap on findings listed per student in the report. Kept here
/// rather than threading `Rules` all the way through the report API —
/// callers that want a different cap can use the `_ex2` variant once we
/// extend it to take a `RenderOptions`. The phase-1 default of 5 is the
/// recommendation in `static_analysis.toml.example::[reporting] top_n_per_student`.
const DEFAULT_TOP_N_PER_STUDENT: usize = 5;

/// Multi-sprint project report: one `# Project report` header, then one
/// `## Sprint K: {name} ({start} — {end})` section per sprint in
/// `sprint_ids_ordered` (chronological ascending), each containing the full
/// A/B/C content at depth=3. A trailing
/// cumulative per-student summary covers the entire chain.
pub fn generate_markdown_report_multi(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    sprint_ids_ordered: &[i64],
    output_dir: &Path,
) -> rusqlite::Result<PathBuf> {
    let path = output_dir.join(format!("report_{}.md", project_name));
    generate_markdown_report_multi_to_path(
        conn,
        project_id,
        project_name,
        sprint_ids_ordered,
        &path,
    )?;
    Ok(path)
}

pub fn generate_markdown_report_multi_to_path(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    sprint_ids_ordered: &[i64],
    output_path: &Path,
) -> rusqlite::Result<()> {
    // Default: instructor-friendly. `sync-reports --push` uses the `_ex`
    // variant with `false` to strip the static-analysis section before
    // committing to team repos.
    generate_markdown_report_multi_to_path_ex(
        conn,
        project_id,
        project_name,
        sprint_ids_ordered,
        output_path,
        true,
    )
}

/// Optional knobs the multi-sprint Markdown renderer accepts. Defaults
/// (`MultiReportOptions::default()`) match the historical
/// instructor-friendly preset: static analysis on, professor view off.
#[derive(Debug, Clone, Copy, Default)]
pub struct MultiReportOptions {
    /// T-SA: when `false`, the static-analysis section is stripped from
    /// the rendered markdown.
    pub include_static_analysis: bool,
    /// T-CX (step 7): when `true`, append the per-student
    /// "Code complexity & testability — grading attribution" block
    /// (weighted contribution + COMPLEXITY_HOTSPOT band) to every
    /// sprint section. The student-facing complexity-hotspots section
    /// always renders regardless; this flag ONLY toggles the
    /// instructor-only attribution block beneath it.
    pub professor_view: bool,
}

impl MultiReportOptions {
    pub fn instructor() -> Self {
        Self {
            include_static_analysis: true,
            professor_view: false,
        }
    }

    pub fn team_facing() -> Self {
        Self {
            include_static_analysis: false,
            professor_view: false,
        }
    }
}

/// Like `generate_markdown_report_multi_to_path` but takes
/// `include_static_analysis`. T-SA: pass `false` from `sync-reports --push`
/// so reports pushed to team repos don't surface the findings.
pub fn generate_markdown_report_multi_to_path_ex(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    sprint_ids_ordered: &[i64],
    output_path: &Path,
    include_static_analysis: bool,
) -> rusqlite::Result<()> {
    generate_markdown_report_multi_to_path_with_opts(
        conn,
        project_id,
        project_name,
        sprint_ids_ordered,
        output_path,
        MultiReportOptions {
            include_static_analysis,
            professor_view: false,
        },
    )
}

/// New canonical entry point for multi-sprint markdown rendering. T-CX
/// (step 7) added this shape so callers can request the instructor-only
/// professor view (per-student weighted attribution + COMPLEXITY_HOTSPOT
/// band) on top of the always-on student-facing complexity section.
pub fn generate_markdown_report_multi_to_path_with_opts(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    sprint_ids_ordered: &[i64],
    output_path: &Path,
    opts: MultiReportOptions,
) -> rusqlite::Result<()> {
    generate_markdown_report_multi_to_path_inner(
        conn,
        project_id,
        project_name,
        sprint_ids_ordered,
        output_path,
        opts.include_static_analysis,
        opts.professor_view,
    )
}

fn generate_markdown_report_multi_to_path_inner(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    sprint_ids_ordered: &[i64],
    output_path: &Path,
    include_static_analysis: bool,
    professor_view: bool,
) -> rusqlite::Result<()> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    }

    let mut buf = String::with_capacity(64 * 1024);
    let _ = writeln!(buf, "# Project report — {}\n", project_name);

    for (idx, sid) in sprint_ids_ordered.iter().enumerate() {
        let (sprint_name, start, end) = conn
            .query_row(
                "SELECT COALESCE(name, ''), COALESCE(start_date, ''), COALESCE(end_date, '')
                 FROM sprints WHERE id = ?",
                [sid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap_or_else(|_| (String::new(), String::new(), String::new()));
        let ordinal = ordinal_for_sprint_id_via_conn(conn, *sid).unwrap_or(0);

        let banner_num = if ordinal > 0 {
            format!("Sprint {}", ordinal)
        } else {
            format!("Sprint (id {})", sid)
        };
        let window = if start.is_empty() && end.is_empty() {
            String::new()
        } else {
            format!(
                " ({} — {})",
                humanize_local_dt(&start),
                humanize_local_dt(&end)
            )
        };
        let heading = if sprint_name.is_empty() || sprint_name == banner_num {
            banner_num
        } else {
            format!("{}: {}", banner_num, sprint_name)
        };
        let _ = writeln!(buf, "## {}{}\n", heading, window);

        write_section_a(
            &mut buf,
            conn,
            *sid,
            project_id,
            project_name,
            Some(&sprint_ids_ordered[..=idx]),
            3,
        )?;
        let sa_data = if include_static_analysis {
            Some(static_analysis_per_student(conn, *sid, project_id)?)
        } else {
            None
        };
        write_section_b(
            &mut buf,
            conn,
            *sid,
            project_id,
            3,
            sa_data.as_ref().map(|d| (d, DEFAULT_TOP_N_PER_STUDENT)),
        )?;
        if professor_view {
            write_section_complexity_attribution(&mut buf, conn, *sid, project_id, 3)?;
        }
        write_section_c(&mut buf, conn, *sid, project_id, 3)?;
    }

    if !sprint_ids_ordered.is_empty() {
        write_cumulative_summary(&mut buf, conn, project_id, sprint_ids_ordered, 2)?;
    }
    write_orphan_pr_annex(&mut buf, conn, project_id, sprint_ids_ordered, 2)?;
    insert_glossary(&mut buf);
    insert_team_identity_map(&mut buf, conn, project_id);
    insert_toc(&mut buf, TOC_MAX_DEPTH);

    std::fs::write(output_path, &buf)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    info!(
        path = %output_path.display(),
        sprints = sprint_ids_ordered.len(),
        "Multi-sprint Markdown report written"
    );
    Ok(())
}

/// 1-based ordinal (by `start_date ASC`) of `sprint_id` within its project.
/// Connection-level variant of `survival::ordinal_for_sprint_id`.
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

// ── Section D: cumulative per-student summary ────────────────────────────────

/// Per-student table with one row per sprint in the chain.
/// Columns: Sprint | Points (DONE) | PRs | Commits | Files | Weighted PR Lines | Doc.
fn write_cumulative_summary(
    buf: &mut String,
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
    depth: usize,
) -> rusqlite::Result<()> {
    let h2 = "#".repeat(depth);
    let h3 = "#".repeat(depth + 1);
    let _ = writeln!(buf, "\n{} D. Cumulative per-student summary\n", h2);
    buf.push_str(
        "Totals per sprint across the DONE tasks assigned to each student. \
         Weighted PR Lines distributes each PR's additions+deletions across \
         its linked tasks by estimation-point share.\n\n",
    );

    // Resolve sprint display names once; keep the caller's ordering.
    let sprint_labels: Vec<(i64, String)> = sprint_ids
        .iter()
        .filter_map(|sid| {
            conn.query_row("SELECT name FROM sprints WHERE id = ?", [sid], |r| {
                r.get::<_, Option<String>>(0)
            })
            .ok()
            .map(|name| (*sid, name.unwrap_or_else(|| format!("sprint-{}", sid))))
        })
        .collect();

    // Each student on the project gets one subsection.
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(full_name, github_login, id)
         FROM students WHERE team_project_id = ?
         ORDER BY COALESCE(full_name, github_login, id)",
    )?;
    let students: Vec<(String, String)> = stmt
        .query_map([project_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    for (sid, full_name) in &students {
        let _ = writeln!(buf, "{} {}\n", h3, md_escape(full_name));
        push_table_header(
            buf,
            &[
                "Sprint",
                "Points (DONE)",
                "PRs",
                "Commits",
                "Files",
                "Weighted PR Lines",
                "Avg Doc Score",
            ],
        );

        let mut any_row = false;
        for (sprint_id, sprint_label) in &sprint_labels {
            let points: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
                     WHERE sprint_id = ? AND assignee_id = ?
                       AND type != 'USER_STORY' AND status = 'DONE'",
                    rusqlite::params![sprint_id, sid],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let metrics: Option<(f64, i64, i64, Option<f64>)> = conn
                .query_row(
                    "SELECT weighted_pr_lines, commit_count, files_touched, avg_doc_score
                     FROM student_sprint_metrics
                     WHERE student_id = ? AND sprint_id = ?",
                    rusqlite::params![sid, sprint_id],
                    |r| {
                        Ok((
                            r.get::<_, Option<f64>>(0)?.unwrap_or(0.0),
                            r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                            r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                            r.get::<_, Option<f64>>(3)?,
                        ))
                    },
                )
                .ok();
            let pr_count: i64 = conn
                .query_row(
                    "SELECT COUNT(DISTINCT pr.id)
                     FROM pull_requests pr
                     JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                     JOIN tasks t ON t.id = tpr.task_id
                     WHERE t.sprint_id = ? AND t.assignee_id = ?
                       AND t.type != 'USER_STORY' AND t.status = 'DONE'",
                    rusqlite::params![sprint_id, sid],
                    |r| r.get(0),
                )
                .unwrap_or(0);

            let (wpl, commits, files, doc) = metrics.unwrap_or((0.0, 0, 0, None));
            // Skip empty sprints entirely? Python's table keeps them; preserve that.
            any_row = true;
            let doc_str = match doc {
                Some(v) => format!("{:.1}", v),
                None => "—".to_string(),
            };
            push_table_row(
                buf,
                &[
                    sprint_label.clone(),
                    fmt_int(points),
                    fmt_int(pr_count),
                    fmt_int(commits),
                    fmt_int(files),
                    fmt_f1(wpl),
                    doc_str,
                ],
            );
        }
        if !any_row {
            buf.push_str("_(no sprint data)_\n");
        }
        buf.push('\n');
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    #[test]
    fn attribution_error_glyph_empty_for_null_or_empty_array() {
        assert_eq!(attribution_error_glyph(None), "");
        assert_eq!(attribution_error_glyph(Some("")), "");
        assert_eq!(attribution_error_glyph(Some("[]")), "");
        assert_eq!(attribution_error_glyph(Some("   ")), "");
    }

    #[test]
    fn attribution_error_glyph_lists_kinds() {
        let raw = r#"[{"kind":"base_sha_fallback","detail":"x","observed_at":"t"},
                       {"kind":"null_author_login","detail":"y","observed_at":"t"}]"#;
        let s = attribution_error_glyph(Some(raw));
        assert!(s.starts_with("⚠ ("));
        assert!(s.contains("base_sha_fallback"));
        assert!(s.contains("null_author_login"));
    }

    #[test]
    fn attribution_error_glyph_dedupes_repeated_kinds() {
        let raw = r#"[{"kind":"github_http_error","detail":"a","observed_at":"t"},
                       {"kind":"github_http_error","detail":"b","observed_at":"t"}]"#;
        let s = attribution_error_glyph(Some(raw));
        assert_eq!(s, "⚠ (github_http_error)");
    }

    #[test]
    fn attribution_error_glyph_falls_back_for_garbage() {
        // Unparseable / non-array → bare glyph still signals.
        assert_eq!(attribution_error_glyph(Some("not-json")), "⚠");
    }

    #[test]
    fn humanize_rule_name_translates_layer_arrow_to_prose() {
        let s = humanize_rule_name("domain->!infrastructure");
        assert!(
            s.contains("**domain**") && s.contains("**infrastructure**"),
            "should bold both layer names: {s}"
        );
        assert!(
            s.contains("must not depend on"),
            "should read as a sentence: {s}"
        );
        assert!(
            !s.contains("->!"),
            "raw arrow form must not leak into output: {s}"
        );
    }

    #[test]
    fn humanize_rule_name_uses_known_description_for_shipped_ast_rules() {
        let s = humanize_rule_name("fragment-no-retrofit-field");
        assert!(
            s.contains("Fragments must not hold"),
            "shipped AST rule must render its prose description: {s}"
        );
        assert!(
            !s.contains("fragment-no-retrofit-field"),
            "machine key must NOT leak into student-facing prose: {s}"
        );
        assert!(
            !s.contains('`'),
            "no backticks (machine-key syntax) in student-facing prose: {s}"
        );
    }

    #[test]
    fn humanize_rule_name_falls_back_for_unknown_rule_key() {
        // Custom AST rule a course author added without updating the
        // descriptions map: the renderer humanises the key on a
        // best-effort basis.
        let s = humanize_rule_name("custom_team_specific_check");
        assert!(
            s.starts_with("Custom team specific check"),
            "fallback must humanise dashes/underscores and capitalise: {s}"
        );
        assert!(
            !s.contains("custom_team_specific_check"),
            "raw key must NOT leak into student-facing prose: {s}"
        );
        assert!(
            !s.contains('`'),
            "no backticks in student-facing prose: {s}"
        );
    }

    fn mk_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sprints (id INTEGER PRIMARY KEY, project_id INTEGER, name TEXT,
                start_date TEXT, end_date TEXT);
             CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE students (id TEXT PRIMARY KEY, username TEXT, full_name TEXT,
                github_login TEXT, team_project_id INTEGER, email TEXT);
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, task_key TEXT, name TEXT, type TEXT,
                status TEXT, estimation_points INTEGER, assignee_id TEXT, sprint_id INTEGER,
                parent_task_id INTEGER);
             CREATE TABLE pull_requests (id TEXT PRIMARY KEY, pr_number INTEGER,
                repo_full_name TEXT, title TEXT, url TEXT, author_id TEXT,
                additions INTEGER, deletions INTEGER, changed_files INTEGER,
                created_at TEXT, merged INTEGER, merged_at TEXT, body TEXT,
                attribution_errors TEXT);
             CREATE TABLE task_pull_requests (task_id INTEGER, pr_id TEXT,
                PRIMARY KEY (task_id, pr_id));
             CREATE TABLE student_sprint_metrics (student_id TEXT, sprint_id INTEGER,
                points_delivered REAL, points_share REAL, weighted_pr_lines REAL,
                commit_count INTEGER, files_touched INTEGER, reviews_given INTEGER,
                avg_doc_score REAL, temporal_spread TEXT, composite_score REAL,
                PRIMARY KEY (student_id, sprint_id));
             CREATE TABLE student_sprint_survival (student_id TEXT, sprint_id INTEGER,
                survival_rate_normalized REAL, estimation_density REAL,
                estimation_points_total REAL, surviving_stmts_normalized INTEGER,
                total_stmts_normalized INTEGER,
                PRIMARY KEY (student_id, sprint_id));
             CREATE TABLE pr_submission_tiers (sprint_id INTEGER, pr_id TEXT,
                merged_at TEXT, hours_before_deadline REAL, tier TEXT, pr_kind TEXT,
                PRIMARY KEY (sprint_id, pr_id));
             CREATE TABLE pr_line_metrics (pr_id TEXT, sprint_id INTEGER,
                merge_sha TEXT, lat REAL, lar REAL, ls REAL, ld REAL,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE pr_survival (pr_id TEXT, sprint_id INTEGER,
                statements_added_raw INTEGER, statements_surviving_raw INTEGER,
                statements_added_normalized INTEGER,
                statements_surviving_normalized INTEGER,
                methods_added INTEGER, methods_surviving INTEGER,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE flags (flag_id INTEGER PRIMARY KEY AUTOINCREMENT,
                student_id TEXT, sprint_id INTEGER, flag_type TEXT, severity TEXT,
                details TEXT);
             CREATE TABLE github_users (login TEXT PRIMARY KEY, name TEXT, email TEXT,
                student_id TEXT, fetched_at TEXT);
             CREATE TABLE student_github_identity (student_id TEXT NOT NULL,
                identity_kind TEXT NOT NULL, identity_value TEXT NOT NULL,
                weight REAL NOT NULL, confidence REAL NOT NULL,
                first_seen_pr TEXT, last_seen_pr TEXT,
                PRIMARY KEY (student_id, identity_kind, identity_value));
             CREATE TABLE fingerprints (id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT, repo_full_name TEXT, statement_index INTEGER,
                method_name TEXT, raw_fingerprint TEXT, normalized_fingerprint TEXT,
                method_fingerprint TEXT, blame_commit TEXT, blame_author_login TEXT,
                sprint_id INTEGER);
             CREATE TABLE team_sprint_ownership (project_id INTEGER, sprint_id INTEGER,
                truck_factor INTEGER, owners_csv TEXT,
                PRIMARY KEY (project_id, sprint_id));
             CREATE TABLE architecture_violations (repo_full_name TEXT, sprint_id INTEGER,
                file_path TEXT, rule_name TEXT, violation_kind TEXT,
                offending_import TEXT, severity TEXT,
                start_line INTEGER, end_line INTEGER,
                rule_kind TEXT, rule_version TEXT, explanation TEXT,
                PRIMARY KEY (repo_full_name, sprint_id, file_path, rule_name, offending_import));
             CREATE TABLE architecture_violation_attribution (
                violation_rowid INTEGER NOT NULL, student_id TEXT NOT NULL,
                lines_authored INTEGER NOT NULL, total_lines INTEGER NOT NULL,
                weight REAL NOT NULL, sprint_id INTEGER NOT NULL,
                PRIMARY KEY (violation_rowid, student_id));
             CREATE TABLE static_analysis_findings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repo_full_name TEXT NOT NULL, sprint_id INTEGER NOT NULL,
                analyzer TEXT NOT NULL, analyzer_version TEXT,
                rule_id TEXT NOT NULL, category TEXT, severity TEXT NOT NULL,
                file_path TEXT NOT NULL, start_line INTEGER, end_line INTEGER,
                message TEXT NOT NULL, help_uri TEXT,
                fingerprint TEXT NOT NULL, head_sha TEXT,
                UNIQUE (repo_full_name, sprint_id, fingerprint));
             CREATE TABLE static_analysis_finding_attribution (
                finding_id INTEGER NOT NULL, student_id TEXT NOT NULL,
                lines_authored INTEGER NOT NULL, total_lines INTEGER NOT NULL,
                weight REAL NOT NULL, sprint_id INTEGER NOT NULL,
                PRIMARY KEY (finding_id, student_id));
             CREATE TABLE static_analysis_runs (
                repo_full_name TEXT NOT NULL, sprint_id INTEGER NOT NULL,
                analyzer TEXT NOT NULL, status TEXT NOT NULL,
                findings_count INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER, head_sha TEXT, diagnostics TEXT,
                ran_at TEXT NOT NULL,
                PRIMARY KEY (repo_full_name, sprint_id, analyzer));
             CREATE TABLE task_similarity_groups (group_id INTEGER PRIMARY KEY AUTOINCREMENT,
                sprint_id INTEGER, project_id INTEGER, representative_task_id INTEGER,
                group_label TEXT, stack TEXT, layer TEXT, action TEXT,
                member_count INTEGER, median_points REAL, median_lar REAL,
                median_ls REAL, median_ls_per_point REAL);
             CREATE TABLE task_group_members (group_id INTEGER, task_id INTEGER,
                sprint_id INTEGER, is_outlier INTEGER, outlier_reason TEXT,
                points_deviation REAL, lar_deviation REAL, ls_deviation REAL,
                ls_per_point_deviation REAL, PRIMARY KEY (group_id, task_id));
             CREATE TABLE method_complexity_findings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sprint_id INTEGER NOT NULL, project_id INTEGER NOT NULL,
                repo_full_name TEXT NOT NULL, file_path TEXT NOT NULL,
                class_name TEXT, method_name TEXT NOT NULL,
                start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                rule_key TEXT NOT NULL, severity TEXT NOT NULL,
                measured_value REAL, threshold REAL, detail TEXT);
             CREATE TABLE method_complexity_attribution (
                finding_id INTEGER NOT NULL, student_id TEXT NOT NULL,
                lines_attributed INTEGER NOT NULL, weighted_lines REAL NOT NULL,
                weight REAL NOT NULL, sprint_id INTEGER NOT NULL,
                PRIMARY KEY (finding_id, student_id));
             CREATE VIEW IF NOT EXISTS pr_authors AS
                SELECT pr.id AS pr_id, t.assignee_id AS student_id,
                       SUM(COALESCE(t.estimation_points, 0)) AS author_points,
                       COUNT(*) AS author_task_count
                FROM pull_requests pr
                JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
                JOIN tasks t ON t.id = tpr.task_id
                WHERE t.type != 'USER_STORY' AND t.assignee_id IS NOT NULL
                GROUP BY pr.id, t.assignee_id;
             INSERT INTO projects VALUES (1, 'pds26-1a');
             INSERT INTO sprints VALUES (10, 1, 'Sprint 1', '2026-02-16', '2026-03-08');
             INSERT INTO sprints VALUES (11, 1, 'Sprint 2', '2026-03-09', '2026-03-29');
             INSERT INTO students (id, full_name, github_login, team_project_id, email)
                VALUES ('u1', 'Alice Bob', 'alice-gh', 1, 'a@ex.com');
             INSERT INTO student_sprint_metrics
                (student_id, sprint_id, points_delivered, points_share,
                 weighted_pr_lines, commit_count, files_touched, reviews_given,
                 avg_doc_score, temporal_spread, composite_score)
                VALUES ('u1', 10, 5, 0.5, 120, 10, 5, 2, 3.0,
                        '{\"early\":1,\"mid\":2,\"late\":1,\"cramming\":0}', 0.75);
             INSERT INTO student_sprint_metrics
                (student_id, sprint_id, points_delivered, points_share,
                 weighted_pr_lines, commit_count, files_touched, reviews_given,
                 avg_doc_score, temporal_spread, composite_score)
                VALUES ('u1', 11, 7, 1.0, 80, 8, 4, 1, 4.0,
                        '{\"early\":1,\"mid\":1,\"late\":1,\"cramming\":0}', 0.80);
             INSERT INTO student_sprint_survival
                (student_id, sprint_id, survival_rate_normalized, estimation_density,
                 estimation_points_total, surviving_stmts_normalized, total_stmts_normalized)
                VALUES ('u1', 10, 0.85, 17.0, 5.0, 85, 100);
             INSERT INTO student_sprint_survival
                (student_id, sprint_id, survival_rate_normalized, estimation_density,
                 estimation_points_total, surviving_stmts_normalized, total_stmts_normalized)
                VALUES ('u1', 11, 0.50, 7.14, 7.0, 50, 100);
             INSERT INTO tasks VALUES
                (100, 'T-1', 'Login endpoint', 'TASK', 'DONE', 3, 'u1', 10, NULL);
             INSERT INTO tasks VALUES
                (101, 'T-2', 'Profile page', 'TASK', 'DONE', 7, 'u1', 11, NULL);
             INSERT INTO pull_requests
                (id, pr_number, repo_full_name, title, url, author_id,
                 additions, deletions, changed_files, created_at, merged, merged_at, body)
                VALUES ('pr-1', 42, 'udg-pds/spring-foo', 'Add login endpoint',
                        'https://github.com/udg-pds/spring-foo/pull/42',
                        'u1', 120, 5, 4, '2026-02-20T10:00:00+00:00', 1,
                        '2026-02-22T15:00:00+00:00', 'body');
             INSERT INTO pull_requests
                (id, pr_number, repo_full_name, title, url, author_id,
                 additions, deletions, changed_files, created_at, merged, merged_at, body)
                VALUES ('pr-2', 43, 'udg-pds/spring-foo', 'Add profile page',
                        'https://github.com/udg-pds/spring-foo/pull/43',
                        'u1', 80, 10, 3, '2026-03-12T10:00:00+00:00', 1,
                        '2026-03-13T15:00:00+00:00', 'body');
             INSERT INTO task_pull_requests VALUES (100, 'pr-1');
             INSERT INTO task_pull_requests VALUES (101, 'pr-2');
             INSERT INTO pr_line_metrics
                (pr_id, sprint_id, merge_sha, lat, lar, ls, ld)
                VALUES ('pr-1', 10, 'sha1', 120, 60, 60, 10);
             INSERT INTO pr_line_metrics
                (pr_id, sprint_id, merge_sha, lat, lar, ls, ld)
                VALUES ('pr-2', 11, 'sha2', 80, 50, 50, 5);
             INSERT INTO pr_survival
                (pr_id, sprint_id, statements_added_raw, statements_surviving_raw,
                 statements_added_normalized, statements_surviving_normalized,
                 methods_added, methods_surviving)
                VALUES ('pr-1', 10, 18, 15, 18, 15, 3, 3);
             INSERT INTO pr_survival
                (pr_id, sprint_id, statements_added_raw, statements_surviving_raw,
                 statements_added_normalized, statements_surviving_normalized,
                 methods_added, methods_surviving)
                VALUES ('pr-2', 11, 12, 9, 12, 9, 2, 2);
             INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
                VALUES ('u1', 10, 'LOW_DOC_SCORE', 'WARNING',
                        '{\"message\":\"average doc score below threshold\"}');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn team_identity_map_renders_after_h1_before_glossary() {
        // The team identity map must surface the GitHub login + GitHub-side
        // email resolved by `github_users` for each enrolled student, and
        // must land directly after the H1 banner — before the glossary so
        // it's the first section the reader sees.
        let conn = mk_conn();
        conn.execute_batch(
            "UPDATE students SET username = 'alice-td' WHERE id = 'u1';
             INSERT INTO github_users (login, name, email, student_id)
                VALUES ('alice-gh', 'Alice on GitHub', 'alice@gh.example', 'u1');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let map_idx = body
            .find("## Team identity map")
            .expect("team identity map heading missing");
        let glossary_idx = body
            .find("## 0. Glossary")
            .expect("glossary heading missing");
        assert!(
            map_idx < glossary_idx,
            "team identity map must precede the glossary: {body}"
        );
        let h1_idx = body.find("# Sprint report").expect("H1 banner missing");
        assert!(
            h1_idx < map_idx,
            "H1 banner must precede the team identity map: {body}"
        );
        // GitHub identity is shown via the embedded github_cell helper
        // (link + login text).
        assert!(
            body.contains("[alice-gh](https://github.com/alice-gh)"),
            "GitHub login link missing from identity map: {body}"
        );
        assert!(
            body.contains("alice@gh.example"),
            "GitHub-side email missing from identity map: {body}"
        );
        assert!(
            body.contains("alice-td"),
            "TrackDev username missing from identity map: {body}"
        );
        assert!(
            body.contains("Alice Bob"),
            "full name missing from identity map: {body}"
        );
    }

    #[test]
    fn team_identity_map_uses_student_github_identity_when_github_users_is_empty() {
        // Regression: pds26-5a had 5/6 teammates resolved in
        // student_github_identity but absent from github_users +
        // students.github_login. The renderer was silently rendering
        // them as "—". Source-of-truth precedence must be:
        //   1. student_github_identity (resolver-fed)
        //   2. github_users           (cold-start backstop)
        //   3. students.github_login  (cold-start backstop)
        let conn = mk_conn();
        // Wipe the cold-start tables so this test exercises *only* the
        // resolver-fed path. Then seed a high-weight login + email row
        // for u1 in student_github_identity.
        conn.execute_batch(
            "UPDATE students SET github_login = NULL WHERE id = 'u1';
             DELETE FROM github_users;
             INSERT INTO student_github_identity
                (student_id, identity_kind, identity_value, weight, confidence,
                 first_seen_pr, last_seen_pr)
              VALUES
                ('u1', 'login', 'alice-resolved', 91.0, 1.0, 'pr-1', 'pr-7'),
                ('u1', 'email', '147317731+alice@users.noreply.github.com',
                 2.0, 1.0, 'pr-1', 'pr-1'),
                ('u1', 'email', 'alice@campus.example', 84.0, 1.0, 'pr-1', 'pr-7');",
        )
        .unwrap();

        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();

        assert!(
            body.contains("[alice-resolved](https://github.com/alice-resolved)"),
            "resolver-fed login missing from identity map: {body}"
        );
        // The two emails have weight 84 and 2 — the heavier one wins.
        assert!(
            body.contains("alice@campus.example"),
            "highest-weight email must win over the noreply fallback: {body}"
        );
        assert!(
            !body.contains("147317731+alice"),
            "lower-weight noreply email leaked into the identity map: {body}"
        );
    }

    #[test]
    fn markdown_report_contains_all_three_sections() {
        let conn = mk_conn();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        assert!(path.exists());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("# Sprint report"));
        assert!(body.contains("## A. Team snapshot"));
        assert!(body.contains("## B. Student dashboards"));
        assert!(body.contains("## C. Peer-group analysis"));
        // Student section renders
        assert!(body.contains("### Alice Bob"));
        // PR link survives
        assert!(body.contains("https://github.com/udg-pds/spring-foo/pull/42"));
        // Flag bullet
        assert!(body.contains("LOW_DOC_SCORE"));
    }

    #[test]
    fn markdown_report_emits_table_of_contents_before_first_section() {
        let conn = mk_conn();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let toc_idx = body
            .find("## Table of contents")
            .expect("TOC heading missing");
        let section_a = body.find("## A. Team snapshot").expect("section A missing");
        assert!(
            toc_idx < section_a,
            "TOC must precede section A so it acts as the document index"
        );
        // TOC entries link to section anchors students will jump to.
        assert!(body.contains("[A. Team snapshot](#a-team-snapshot)"));
        assert!(body.contains("[B. Student dashboards](#b-student-dashboards)"));
        assert!(body.contains("[C. Peer-group analysis](#c-peer-group-analysis)"));
        // Student-level entry is nested one level beyond section B.
        assert!(body.contains("  - [Alice Bob](#alice-bob)"));
    }

    #[test]
    fn humanize_local_dt_renders_winter_and_summer_with_local_abbrev() {
        // March 1 23:00 UTC lands at 00:00 CET on March 2 in Madrid.
        // The student-facing string must show local clock + tz abbrev.
        assert_eq!(
            humanize_local_dt("2026-03-01T23:00Z"),
            "2 March 2026, 00:00 CET"
        );
        // June 5 21:59 UTC is during DST, so Madrid reads 23:59 CEST.
        assert_eq!(
            humanize_local_dt("2026-06-05T21:59Z"),
            "5 June 2026, 23:59 CEST"
        );
        // Garbage input is passed through verbatim — never silently dropped.
        assert_eq!(humanize_local_dt("not a date"), "not a date");
    }

    #[test]
    fn markdown_report_includes_glossary_before_section_a_with_toc_link() {
        let conn = mk_conn();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let glossary_idx = body
            .find("## 0. Glossary")
            .expect("glossary heading missing");
        let section_a = body.find("## A. Team snapshot").expect("section A missing");
        assert!(
            glossary_idx < section_a,
            "Glossary must precede section A so it acts as the report preface"
        );
        // Glossary is reachable from the TOC under its own anchor.
        assert!(
            body.contains(
                "[0. Glossary — how to read this report](#0-glossary--how-to-read-this-report)"
            ),
            "TOC must link to the glossary heading: {body}"
        );
        // A few canonical glossary terms students will look up.
        for term in [
            "**LAT**",
            "**LAR**",
            "**LS**",
            "**Density Δ**",
            "Truck factor",
            "Severity",
        ] {
            assert!(
                body.contains(term),
                "glossary missing required term {term}: {body}"
            );
        }
    }

    #[test]
    fn build_toc_surfaces_student_dashboards_children_in_multi_sprint_mode() {
        // Multi-sprint structure: A/B/C live at H3 under '## Sprint K',
        // students under section B sit at H4. They must still appear in
        // the TOC, indented one level deeper than section B itself, even
        // though H4 is past the default depth-3 cutoff.
        let body = "# Project report\n\
                    \n\
                    ## Sprint 1\n\
                    \n\
                    ### A. Team snapshot\n\
                    \n\
                    ### B. Student dashboards\n\
                    \n\
                    #### Alice Bob\n\
                    \n\
                    #### Carol Dee\n\
                    \n\
                    ### C. Peer-group analysis\n\
                    \n\
                    #### group-1\n";
        let toc = build_toc(body, 3);
        // Students under B are present, and indented two levels (4 spaces)
        // because they live at H4 under H3 section B.
        assert!(
            toc.contains("    - [Alice Bob](#alice-bob)"),
            "section B students must appear indented under section B in multi-sprint TOC: {toc}"
        );
        assert!(
            toc.contains("    - [Carol Dee](#carol-dee)"),
            "all section B students must appear: {toc}"
        );
        // Section C children stay collapsed — only section B got the
        // depth-overflow exception.
        assert!(
            !toc.contains("[group-1]"),
            "non-section-B H4 entries must remain hidden: {toc}"
        );
    }

    #[test]
    fn build_toc_collapses_glossary_subsections() {
        // Glossary H3 subsections are pedagogical groupings, not navigation
        // anchors — the TOC should list the H2 entry and stop.
        let body = "# Sprint report\n\
                    \n\
                    ## 0. Glossary — how to read this report\n\
                    \n\
                    ### Code volume\n\
                    \n\
                    ### Flags\n\
                    \n\
                    ## A. Team snapshot\n";
        let toc = build_toc(body, 3);
        assert!(toc.contains("[0. Glossary"));
        assert!(
            !toc.contains("[Code volume]"),
            "glossary subsection leaked into TOC: {toc}"
        );
        assert!(
            !toc.contains("[Flags]"),
            "glossary subsection leaked into TOC: {toc}"
        );
        assert!(toc.contains("[A. Team snapshot](#a-team-snapshot)"));
    }

    #[test]
    fn build_toc_keeps_section_b_students_but_drops_cumulative_students() {
        // Section B students must remain navigable; the cumulative summary
        // already lists every student linearly, so its H3 rows are dropped
        // from the TOC. The H2 cumulative heading itself still appears.
        let body = "# Sprint report\n\
                    \n\
                    ## B. Student dashboards\n\
                    \n\
                    ### Alice Bob\n\
                    \n\
                    ### Carol Dee\n\
                    \n\
                    ## D. Cumulative per-student summary\n\
                    \n\
                    ### Alice Bob\n\
                    \n\
                    ### Carol Dee\n";
        let toc = build_toc(body, 3);
        assert!(toc.contains("[B. Student dashboards](#b-student-dashboards)"));
        assert!(toc.contains("  - [Alice Bob](#alice-bob)"));
        assert!(toc.contains("  - [Carol Dee](#carol-dee)"));
        assert!(
            toc.contains("[D. Cumulative per-student summary](#d-cumulative-per-student-summary)")
        );
        // The collision-suffixed slugs (alice-bob-1, carol-dee-1) belong to
        // students rendered under the cumulative section — they must not
        // surface as their own TOC rows.
        assert!(
            !toc.contains("(#alice-bob-1)"),
            "cumulative-section per-student row leaked into TOC: {toc}"
        );
        assert!(
            !toc.contains("(#carol-dee-1)"),
            "cumulative-section per-student row leaked into TOC: {toc}"
        );
    }

    #[test]
    fn build_toc_disambiguates_repeated_headings_with_numeric_suffix() {
        let body = "# Project\n\n## Sprint 1\n\n### A. Team snapshot\n\n## Sprint 2\n\n### A. Team snapshot\n";
        let toc = build_toc(body, 3);
        // First "A. Team snapshot" gets the bare slug; the second collides
        // and earns the GitHub-style `-1` suffix.
        assert!(toc.contains("[A. Team snapshot](#a-team-snapshot)"));
        assert!(toc.contains("[A. Team snapshot](#a-team-snapshot-1)"));
    }

    #[test]
    fn build_toc_skips_h1_and_returns_empty_when_no_h2_headings() {
        // H1-only body has no in-scope entries; we should not emit a stub TOC.
        assert_eq!(build_toc("# Title only\n", 3), "");
    }

    #[test]
    fn slugify_anchor_lowercases_unicode_and_drops_punctuation() {
        let mut used = HashMap::new();
        // Accented letters are alphanumeric — they survive lowercasing.
        // Punctuation (period, parens, em-dash) is stripped; whitespace
        // becomes a hyphen.
        assert_eq!(
            slugify_anchor("José Núñez (Sprint 1)", &mut used),
            "josé-núñez-sprint-1"
        );
    }

    #[test]
    fn architecture_hotspot_flag_pointer_text_references_per_student_block() {
        // ARCHITECTURE_HOTSPOT flag bullet renders as a one-liner pointing
        // the reader to the per-student Architecture violations sub-block.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name,
                 violation_kind, offending_import, severity, start_line, end_line)
             VALUES
                ('udg/spring-foo', 10, 'X.java', 'SERVICE_RETURNS_ENTITY',
                 'llm', 'X@L13', 'CRITICAL', 13, 13);
             INSERT INTO architecture_violation_attribution
                (violation_rowid, student_id, lines_authored, total_lines, weight, sprint_id)
                SELECT rowid, 'u1', 1, 1, 1.0, 10 FROM architecture_violations
                WHERE sprint_id = 10;
             INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
                VALUES ('u1', 10, 'ARCHITECTURE_HOTSPOT', 'CRITICAL',
                        '{\"weighted\":2.0,\"min_weighted\":1.5}');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains(
                "Architecture weighted contribution reached 2 (threshold 1.5). \
See the Architecture violations block in this dashboard for the attributed offenders."
            ),
            "flag pointer line missing or stale: {body}"
        );
        // The standalone "## Architecture hotspots" section is gone.
        assert!(
            !body.contains("## Architecture hotspots"),
            "standalone arch hotspot section must be removed: {body}"
        );
    }

    #[test]
    fn per_student_architecture_block_silent_when_hotspot_flag_did_not_fire() {
        // Violations and attribution exist but no ARCHITECTURE_HOTSPOT flag
        // → the per-student arch block must not render at all.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name,
                 violation_kind, offending_import, severity)
             VALUES
                ('udg/spring-foo', 10, 'X.java', 'SOMETHING',
                 'llm', 'X@L1', 'WARNING');
             INSERT INTO architecture_violation_attribution
                (violation_rowid, student_id, lines_authored, total_lines, weight, sprint_id)
                SELECT rowid, 'u1', 1, 1, 1.0, 10 FROM architecture_violations
                WHERE sprint_id = 10;",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains("**Architecture violations:**"),
            "per-student arch block must be gated by ARCHITECTURE_HOTSPOT firing: {body}"
        );
    }

    #[test]
    fn truck_factor_humanizes_owner_csv_to_full_names() {
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO students (id, full_name, github_login, team_project_id, email)
                VALUES ('uuid-bob', 'Bob C', 'bob-gh', 1, 'b@ex.com');
             INSERT INTO team_sprint_ownership (project_id, sprint_id, truck_factor, owners_csv)
                VALUES (1, 10, 2, 'u1,uuid-bob');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Names + GitHub logins, not raw UUIDs.
        assert!(
            body.contains("Alice Bob (alice-gh)"),
            "missing humanized name for u1 in: {body}",
        );
        assert!(
            body.contains("Bob C (bob-gh)"),
            "missing humanized name for uuid-bob"
        );
        assert!(
            !body.contains("uuid-bob"),
            "raw student_id leaked into output"
        );
    }

    #[test]
    fn code_ownership_section_renders_truck_factor_only() {
        let conn = mk_conn();
        // The per-file treemap was removed because realistic file counts make
        // it unreadable; only the truck-factor headline survives.
        conn.execute_batch(
            "INSERT INTO github_users (login, student_id) VALUES ('alice-gh', 'u1');
             INSERT INTO students (id, full_name, github_login, team_project_id, email)
                VALUES ('u2', 'Bob C', 'bob-gh', 1, 'b@ex.com');
             INSERT INTO team_sprint_ownership (project_id, sprint_id, truck_factor, owners_csv)
                VALUES (1, 10, 2, 'u1,u2');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("### Code ownership"),
            "missing ownership heading"
        );
        assert!(
            body.contains("Truck factor:** 2"),
            "missing truck factor line"
        );
        assert!(
            !body.contains("Files (sized by statement count"),
            "treemap should no longer be rendered",
        );
    }

    #[test]
    fn architecture_section_a_renders_total_only_no_per_rule_listing() {
        // Section A summarises architecture conformance with a heading and
        // a total-count line; the per-rule + per-file listing now lives
        // exclusively in Section B's per-student block.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name,
                 violation_kind, offending_import, severity)
             VALUES
                ('udg/spring-foo', 10, 'A.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repository.UserRepository', 'WARNING'),
                ('udg/spring-foo', 10, 'B.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repository.UserRepository', 'WARNING'),
                ('udg/spring-foo', 10, 'C.java', 'domain-no-spring-web',
                 'forbidden_import', 'org.springframework.web.RestController', 'WARNING');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("### Architecture conformance"));
        assert!(body.contains("**Total violations:** 3"));
        assert!(
            !body.contains("**Violations by rule:**"),
            "per-rule block must no longer render in Section A: {body}"
        );
        // With no fingerprints seeded, no Section B per-student block exists,
        // so file-level links must not leak from a removed Section A renderer.
        assert!(
            !body.contains("[`A.java`](https://github.com/udg/spring-foo/blob/HEAD/A.java"),
            "no per-file listing must appear in Section A anymore: {body}"
        );
    }

    #[test]
    fn per_student_architecture_block_lists_every_attributed_violation_with_weight_and_explanation()
    {
        // Section B per-student arch block fires once ARCHITECTURE_HOTSPOT
        // is on for the student. Every (student, violation) attribution row
        // with weight > 0 renders as one bullet with: clickable file link
        // (line anchor where known), humanised rule prose, severity badge,
        // and a `· N% of lines` suffix derived from the blame weight.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name,
                 violation_kind, offending_import, severity,
                 start_line, end_line, rule_kind, explanation)
             VALUES
                ('udg/spring-foo', 10, 'A.java', 'HARDCODED_API_URL',
                 'llm', 'HARDCODED_API_URL@L13', 'WARNING', 13, 13, 'llm',
                 'API URL should use BuildConfig instead of a literal string.'),
                ('udg/spring-foo', 10, 'B.java', 'HARDCODED_API_URL',
                 'llm', 'HARDCODED_API_URL@L26', 'WARNING', 26, 49, 'llm',
                 'Repository hits the network without checking the cache.'),
                ('udg/spring-foo', 10, 'C.java', 'HARDCODED_API_URL',
                 'llm', 'HARDCODED_API_URL@L7', 'WARNING', 7, 7, 'llm',
                 'Third hard-coded URL.'),
                ('udg/spring-foo', 10, 'D.java', 'HARDCODED_API_URL',
                 'llm', 'HARDCODED_API_URL@L9', 'WARNING', 9, 9, 'llm',
                 'Fourth hard-coded URL.');
             INSERT INTO architecture_violation_attribution
                (violation_rowid, student_id, lines_authored, total_lines, weight, sprint_id)
                SELECT rowid, 'u1',
                       CASE file_path WHEN 'A.java' THEN 1 WHEN 'B.java' THEN 12 ELSE 1 END,
                       CASE file_path WHEN 'A.java' THEN 1 WHEN 'B.java' THEN 24 ELSE 1 END,
                       CASE file_path WHEN 'A.java' THEN 1.0 WHEN 'B.java' THEN 0.5 ELSE 1.0 END,
                       10
                FROM architecture_violations WHERE sprint_id = 10;
             INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
                VALUES ('u1', 10, 'ARCHITECTURE_HOTSPOT', 'WARNING',
                        '{\"weighted\":3.5,\"min_weighted\":2.0}');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Single-line violation: `:L13` suffix and `#L13` anchor + 100% suffix.
        assert!(
            body.contains("[`A.java` :L13](https://github.com/udg/spring-foo/blob/HEAD/A.java#L13"),
            "single-line link must carry :L13 suffix and #L13 anchor; got:\n{body}"
        );
        // Range violation: `:L26-L49` suffix + 50% suffix from weight 0.5.
        assert!(
            body.contains(
                "[`B.java` :L26-L49](https://github.com/udg/spring-foo/blob/HEAD/B.java#L26-L49"
            ),
            "range link must carry :L26-L49 suffix and #L26-L49 anchor; got:\n{body}"
        );
        assert!(
            body.contains(" · 50% of lines"),
            "blame weight 0.5 must render as 50% suffix; got:\n{body}"
        );
        assert!(
            body.contains(" · 100% of lines"),
            "blame weight 1.0 must render as 100% suffix; got:\n{body}"
        );
        // Every attributed violation renders — no top-N cap.
        assert!(
            body.contains("[`C.java` :L7](https://github.com/udg/spring-foo/blob/HEAD/C.java#L7"),
            "third violation must render — no top-N cap; got:\n{body}"
        );
        assert!(
            body.contains("[`D.java` :L9](https://github.com/udg/spring-foo/blob/HEAD/D.java#L9"),
            "fourth violation must render — no top-N cap; got:\n{body}"
        );
        // Explanation prose surfaces as a nested bullet.
        assert!(
            body.contains("  - API URL should use BuildConfig instead of a literal string."),
            "explanation must render as a nested bullet; got:\n{body}"
        );
        // Per-student headline reports the four attributed violations.
        assert!(
            body.contains("**Architecture violations:** 4 (0 critical · 4 warning · 0 info)"),
            "per-student headline missing or wrong: {body}"
        );
        // The legacy rule-grouped phrasing must not reappear.
        assert!(
            !body.contains("occurrence(s) across"),
            "rule grouping must be gone — bullet per violation now: {body}"
        );
    }

    #[test]
    fn per_student_architecture_attribution_resolves_bare_repo_basename_to_qualified_url() {
        // Regression: the architecture stage may write `repo_full_name` as a
        // bare repo (e.g. `spring-foo`) on legacy rows. The per-student arch
        // block must resolve the qualified `<org>/<repo>` from `pull_requests`
        // so the file URL is clickable.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO pull_requests (id, repo_full_name)
                VALUES ('pr1', 'udg-pds/spring-foo');
             INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name,
                 violation_kind, offending_import, severity)
             VALUES
                ('spring-foo', 10, 'A.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repo.UserRepo', 'WARNING');
             INSERT INTO architecture_violation_attribution
                (violation_rowid, student_id, lines_authored, total_lines, weight, sprint_id)
                SELECT rowid, 'u1', 1, 1, 1.0, 10 FROM architecture_violations
                WHERE sprint_id = 10;
             INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
                VALUES ('u1', 10, 'ARCHITECTURE_HOTSPOT', 'WARNING',
                        '{\"weighted\":1.0,\"min_weighted\":0.5}');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("**Architecture violations:** 1 (0 critical · 1 warning · 0 info)"),
            "missing per-student arch headline; got: {body}"
        );
        assert!(
            body.contains("[`A.java`](https://github.com/udg-pds/spring-foo/blob/HEAD/A.java"),
            "bare repo name must be resolved against pull_requests so the file link is clickable: {body}"
        );
    }

    #[test]
    fn per_student_architecture_block_uses_attribution_table_and_skips_zero_weight() {
        // Two violated files. u1 has attribution rows for A.java (weight=1.0
        // for two violations) but no row for C.java → C.java is unattributed
        // and falls into Section A's "Unattributed violations" stub instead.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name,
                 violation_kind, offending_import, severity)
             VALUES
                ('udg/spring-foo', 10, 'A.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repo.UserRepo', 'WARNING'),
                ('udg/spring-foo', 10, 'A.java', 'domain-no-spring-web',
                 'forbidden_import', 'org.springframework.web.RestController', 'CRITICAL'),
                ('udg/spring-foo', 10, 'C.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repo.OtherRepo', 'WARNING');
             INSERT INTO architecture_violation_attribution
                (violation_rowid, student_id, lines_authored, total_lines, weight, sprint_id)
                SELECT rowid, 'u1', 1, 1, 1.0, 10 FROM architecture_violations
                WHERE sprint_id = 10 AND file_path = 'A.java';
             INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
                VALUES ('u1', 10, 'ARCHITECTURE_HOTSPOT', 'WARNING',
                        '{\"weighted\":2.0,\"min_weighted\":1.0}');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Section B per-student block under Alice.
        assert!(
            body.contains("**Architecture violations:** 2 (1 critical · 1 warning · 0 info)"),
            "missing per-student arch headline; got: {body}"
        );
        // Layer-rule prose still humanises.
        assert!(
            body.contains("**presentation** must not depend on **infrastructure**"),
            "layer rule must humanise; got:\n{body}"
        );
        // Team-level severity breakdown reflects all three rows.
        assert!(body.contains("**Total violations:** 3 (1 critical · 2 warning · 0 info)"));
        // Unattributed stub picks up C.java.
        assert!(
            body.contains("**Unattributed violations:** 1"),
            "unattributed stub missing; got:\n{body}"
        );
        // C.java does not appear under any per-student block.
        assert!(
            !body.contains("[`C.java`]"),
            "unattributed C.java must not surface in Section B; got:\n{body}"
        );
    }

    #[test]
    fn md_escape_preserves_pipes_as_backslash() {
        let s = md_escape("a|b\nc");
        assert_eq!(s, "a\\|b c");
    }

    #[test]
    fn compact_reason_numbers_trims_full_precision_metrics() {
        let reason = "LS=243.0769230769231 vs median=74.38888888888889 (z=+4.8); LS/pt=30.20 vs median=12.5600 (z=-6.0)";
        assert_eq!(
            compact_reason_numbers(reason),
            "LS=243.08 vs median=74.39 (z=+4.8); LS/pt=30.2 vs median=12.56 (z=-6)"
        );
    }

    #[test]
    fn cumulative_table_carries_density_delta_legend() {
        let conn = mk_conn();
        let tmp = TempDir::new().unwrap();
        let path =
            generate_markdown_report_ex(&conn, 11, 1, "pds26-1a", tmp.path(), Some(&[10, 11]))
                .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("**Density Δ**"),
            "missing legend explaining Density Δ column",
        );
    }

    #[test]
    fn cumulative_markdown_adds_section_a_student_totals() {
        let conn = mk_conn();
        let tmp = TempDir::new().unwrap();
        let path =
            generate_markdown_report_ex(&conn, 11, 1, "pds26-1a", tmp.path(), Some(&[10, 11]))
                .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("### This sprint"));
        assert!(body.contains("### Cumulative through this sprint"));
        // Single-student team → MAD = 0 → Density Δ cell is empty.
        assert!(body.contains(
            "| Alice Bob | [alice-gh](https://github.com/alice-gh) | 12 | 100% | 200 | 110 | 15 | 11 | 67.5% |  | 1 |"
        ));
    }

    #[test]
    fn multi_report_adds_cumulative_table_to_each_sprint_snapshot() {
        let conn = mk_conn();
        let tmp = TempDir::new().unwrap();
        let path =
            generate_markdown_report_multi(&conn, 1, "pds26-1a", &[10, 11], tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.matches("#### This sprint").count(), 2);
        assert_eq!(
            body.matches("#### Cumulative through this sprint").count(),
            2
        );
        // Single-student team → MAD = 0 → Density Δ cell is empty.
        assert!(body.contains(
            "| Alice Bob | [alice-gh](https://github.com/alice-gh) | 5 | 100% | 120 | 60 | 10 | 20 | 85% |  | 1 |"
        ));
        assert!(body.contains(
            "| Alice Bob | [alice-gh](https://github.com/alice-gh) | 12 | 100% | 200 | 110 | 15 | 11 | 67.5% |  | 1 |"
        ));
    }

    #[test]
    fn github_logins_are_normalized_and_can_fall_back_to_pr_author() {
        let conn = mk_conn();
        conn.execute(
            "UPDATE students SET github_login = 'https://github.com/alice-gh/' WHERE id = 'u1'",
            [],
        )
        .unwrap();
        conn.execute(
            "ALTER TABLE pull_requests ADD COLUMN github_author_login TEXT",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO students (id, full_name, github_login, team_project_id, email)
                VALUES ('u2', 'Fallback User', '', 1, 'f@ex.com');
             INSERT INTO pull_requests
                (id, pr_number, repo_full_name, title, url, author_id,
                 additions, deletions, changed_files, created_at, merged, merged_at, body,
                 github_author_login)
                VALUES ('pr-fallback', 44, 'udg-pds/spring-foo', 'Fallback PR',
                        'https://github.com/udg-pds/spring-foo/pull/44',
                        'u2', 10, 2, 1, '2026-02-20T10:00:00+00:00', 1,
                        '2026-02-22T15:00:00+00:00', 'body', 'fallback-gh');",
        )
        .unwrap();

        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("| Alice Bob | [alice-gh](https://github.com/alice-gh) |"));
        assert!(body.contains("| Fallback User | [fallback-gh](https://github.com/fallback-gh) |"));
        assert!(body.contains("_GitHub: [`fallback-gh`](https://github.com/fallback-gh)_"));
    }

    #[test]
    fn pr_rows_keep_linked_points_when_line_metrics_are_missing() {
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO tasks VALUES
                (102, 'T-3', 'No metrics task', 'TASK', 'DONE', 4, 'u1', 10, NULL);
             INSERT INTO pull_requests
                (id, pr_number, repo_full_name, title, url, author_id,
                 additions, deletions, changed_files, created_at, merged, merged_at, body)
                VALUES ('pr-3', 44, 'udg-pds/spring-foo', 'No metric PR',
                        'https://github.com/udg-pds/spring-foo/pull/44',
                        'u1', 10, 2, 1, '2026-02-20T10:00:00+00:00', 1,
                        '2026-02-22T15:00:00+00:00', 'body');
             INSERT INTO task_pull_requests VALUES (102, 'pr-3');",
        )
        .unwrap();

        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("| [#44](https://github.com/udg-pds/spring-foo/pull/44) | spring-foo | [No metric PR](https://github.com/udg-pds/spring-foo/pull/44) | +10 / -2 | 4 | 0 | 0 | 0 |"));
    }

    // --- T-SA: static-analysis section -------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn seed_static_analysis_finding(
        conn: &Connection,
        finding_id: i64,
        student: &str,
        rule_id: &str,
        analyzer: &str,
        severity: &str,
        file_path: &str,
        start: i64,
        end: i64,
        weight: f64,
    ) {
        conn.execute(
            "INSERT INTO static_analysis_findings
                (id, repo_full_name, sprint_id, analyzer, rule_id, severity, category,
                 file_path, start_line, end_line, message, fingerprint)
             VALUES (?, 'udg-pds/spring-foo', 10, ?, ?, ?, 'bug', ?, ?, ?, ?, ?)",
            rusqlite::params![
                finding_id,
                analyzer,
                rule_id,
                severity,
                file_path,
                start,
                end,
                format!("Possible {} issue", rule_id),
                format!("fp-{}-{}", finding_id, rule_id),
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO static_analysis_finding_attribution
                (finding_id, student_id, lines_authored, total_lines, weight, sprint_id)
             VALUES (?, ?, 1, 1, ?, 10)",
            rusqlite::params![finding_id, student, weight],
        )
        .unwrap();
    }

    #[test]
    fn static_analysis_section_renders_blob_url_with_line_anchor() {
        use sprint_grader_static_analysis::i18n as sai18n;
        let conn = mk_conn();
        seed_static_analysis_finding(
            &conn,
            1,
            "u1",
            "UnusedPrivateField",
            "pmd",
            "WARNING",
            "src/main/java/com/x/UserController.java",
            26,
            49,
            0.8,
        );
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains(
                "blob/HEAD/src/main/java/com/x/UserController.java#L26-L49"
            ),
            "static-analysis block must emit a clickable blob URL with the line anchor; got:\n{body}"
        );
        assert!(
            body.contains("`pmd:UnusedPrivateField`"),
            "finding line must include `analyzer:rule_id`; got:\n{body}"
        );
        let block_marker = format!("**{}:**", sai18n::BLOCK_HEADER);
        assert!(
            body.contains(&block_marker),
            "per-student static-analysis block header must surface ({block_marker}); got:\n{body}"
        );
        let team_marker = format!("**{}:**", sai18n::TEAM_TALLY_LABEL);
        assert!(
            body.contains(&team_marker),
            "team-level static-analysis tally must surface ({team_marker}); got:\n{body}"
        );
        assert!(
            body.contains(sai18n::SEVERITY_INFO_PLURAL),
            "headline must include the info-severity bucket; got:\n{body}"
        );
    }

    #[test]
    fn static_analysis_section_omitted_when_disabled() {
        use sprint_grader_static_analysis::i18n as sai18n;
        let conn = mk_conn();
        seed_static_analysis_finding(
            &conn,
            2,
            "u1",
            "UnusedPrivateField",
            "pmd",
            "WARNING",
            "Foo.java",
            7,
            7,
            1.0,
        );
        let tmp = TempDir::new().unwrap();
        let report_path = tmp.path().join("report.md");
        // _ex2 with `false` is the path `sync-reports --push` takes.
        generate_markdown_report_to_path_ex2(&conn, 10, 1, "pds26-1a", &report_path, None, false)
            .unwrap();
        let body = std::fs::read_to_string(&report_path).unwrap();
        let block_marker = format!("**{}:**", sai18n::BLOCK_HEADER);
        assert!(
            !body.contains(&block_marker),
            "static-analysis block must be stripped when include_static_analysis = false; got:\n{body}"
        );
        let team_marker = format!("**{}:**", sai18n::TEAM_TALLY_LABEL);
        assert!(
            !body.contains(&team_marker),
            "team tally must be stripped when include_static_analysis = false; got:\n{body}"
        );
        assert!(
            !body.contains("UnusedPrivateField"),
            "no finding must surface when disabled; got:\n{body}"
        );
    }

    #[test]
    fn static_analysis_lists_every_finding_per_student_no_cap() {
        let conn = mk_conn();
        // Seed 8 findings for one student — every one of them must render;
        // the legacy "… N more" rollup is gone.
        for i in 0..8 {
            seed_static_analysis_finding(
                &conn,
                100 + i,
                "u1",
                &format!("Rule{}", i),
                "pmd",
                "WARNING",
                &format!("F{}.java", i),
                1 + i,
                1 + i,
                1.0 - (i as f64) * 0.05,
            );
        }
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        for i in 0..8 {
            let needle = format!("`pmd:Rule{}`", i);
            assert!(
                body.contains(&needle),
                "Rule{} must appear — no top-N cap; got:\n{}",
                i,
                body
            );
        }
        assert!(
            !body.contains("- … "),
            "no '… N more' rollup may remain; got:\n{body}"
        );
    }

    // ── Complexity / testability section (T-CX, steps 6 + 7) ───────────

    #[allow(clippy::too_many_arguments)]
    fn seed_complexity_finding(
        conn: &Connection,
        finding_id: i64,
        repo_full_name: &str,
        file_path: &str,
        class_name: &str,
        method_name: &str,
        rule_key: &str,
        severity: &str,
        start: i64,
        end: i64,
        measured: Option<f64>,
        threshold: Option<f64>,
    ) {
        conn.execute(
            "INSERT INTO method_complexity_findings
                (id, sprint_id, project_id, repo_full_name, file_path, class_name,
                 method_name, start_line, end_line, rule_key, severity,
                 measured_value, threshold, detail)
             VALUES (?, 10, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '')",
            rusqlite::params![
                finding_id,
                repo_full_name,
                file_path,
                class_name,
                method_name,
                start,
                end,
                rule_key,
                severity,
                measured,
                threshold,
            ],
        )
        .unwrap();
    }

    fn seed_complexity_attribution(
        conn: &Connection,
        finding_id: i64,
        student_id: &str,
        weight: f64,
    ) {
        conn.execute(
            "INSERT INTO method_complexity_attribution
                (finding_id, student_id, lines_attributed, weighted_lines, weight, sprint_id)
             VALUES (?, ?, 5, 10.0, ?, 10)",
            rusqlite::params![finding_id, student_id, weight],
        )
        .unwrap();
    }

    fn seed_complexity_flag(conn: &Connection, student_id: &str, severity: &str) {
        conn.execute(
            "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
             VALUES (?, 10, 'COMPLEXITY_HOTSPOT', ?, '{}')",
            rusqlite::params![student_id, severity],
        )
        .unwrap();
    }

    #[test]
    fn per_student_complexity_block_silent_on_empty_db() {
        let conn = mk_conn();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains("## Code complexity & testability"),
            "standalone complexity section must be removed: {body}"
        );
        assert!(
            !body.contains("**Complexity & testability:**"),
            "per-student complexity block must not render on empty DB: {body}"
        );
    }

    #[test]
    fn per_student_complexity_block_renders_method_link_with_weight_and_anchor() {
        // COMPLEXITY_HOTSPOT fires for u1 → the per-student complexity block
        // surfaces every finding attributed to u1 with its method link, line
        // anchor, severity, threshold tail, and `· N% of lines` blame suffix.
        let conn = mk_conn();
        seed_complexity_finding(
            &conn,
            1,
            "udg-pds/spring-foo",
            "src/main/java/com/x/UserController.java",
            "UserController",
            "register",
            "cyclomatic",
            "WARNING",
            42,
            90,
            Some(17.0),
            Some(15.0),
        );
        seed_complexity_attribution(&conn, 1, "u1", 0.6);
        seed_complexity_flag(&conn, "u1", "WARNING");
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("**Complexity & testability:** 1"));
        assert!(body.contains("`UserController.register()`"));
        assert!(body.contains("blob/HEAD/src/main/java/com/x/UserController.java#L42-L90"));
        assert!(body.contains("Cyclomatic complexity above the ceiling"));
        assert!(body.contains("(17 > 15)"));
        assert!(
            body.contains(" · 60% of lines"),
            "blame weight 0.6 must render as 60% suffix; got:\n{body}"
        );
    }

    #[test]
    fn per_student_complexity_block_one_bullet_per_finding_no_grouping() {
        // Two findings on the same method must render as TWO bullets in the
        // per-student block — one per (file, method, rule) tuple — because
        // weights can differ per rule.
        let conn = mk_conn();
        seed_complexity_finding(
            &conn,
            1,
            "udg-pds/spring-foo",
            "src/main/java/com/x/A.java",
            "A",
            "f",
            "cyclomatic",
            "CRITICAL",
            10,
            50,
            Some(22.0),
            Some(15.0),
        );
        seed_complexity_finding(
            &conn,
            2,
            "udg-pds/spring-foo",
            "src/main/java/com/x/A.java",
            "A",
            "f",
            "broad-catch",
            "WARNING",
            10,
            50,
            None,
            None,
        );
        seed_complexity_attribution(&conn, 1, "u1", 1.0);
        seed_complexity_attribution(&conn, 2, "u1", 0.5);
        seed_complexity_flag(&conn, "u1", "CRITICAL");
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let method_occurrences = body.matches("`A.f()`").count();
        assert_eq!(
            method_occurrences, 2,
            "one bullet per finding (no grouping); got:\n{body}"
        );
        assert!(body.contains("Cyclomatic complexity above the ceiling"));
        assert!(body.contains("Catches `Exception`/`Throwable` without rethrowing"));
        assert!(body.contains(" · 100% of lines"));
        assert!(body.contains(" · 50% of lines"));
    }

    #[test]
    fn professor_view_appends_attribution_block_with_flag_band() {
        let conn = mk_conn();
        conn.execute(
            "INSERT INTO pull_requests (id, pr_number, repo_full_name, title, url,
                author_id, additions, deletions, changed_files, created_at, merged, merged_at, body)
             VALUES ('pr1', 1, 'udg-pds/spring-foo', 't', 'u', 'u1', 0, 0, 0,
                     '2026-02-20', 1, '2026-02-22', '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                assignee_id, sprint_id) VALUES (1, 'T-1', 'x', 'TASK', 'DONE', 3, 'u1', 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (1, 'pr1')",
            [],
        )
        .unwrap();
        seed_complexity_finding(
            &conn,
            1,
            "udg-pds/spring-foo",
            "src/main/java/com/x/UserController.java",
            "UserController",
            "register",
            "cyclomatic",
            "WARNING",
            42,
            90,
            Some(17.0),
            Some(15.0),
        );
        seed_complexity_attribution(&conn, 1, "u1", 1.0);
        seed_complexity_flag(&conn, "u1", "WARNING");

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("REPORT_PROFESSOR.md");
        generate_markdown_report_multi_to_path_with_opts(
            &conn,
            1,
            "pds26-1a",
            &[10],
            &path,
            MultiReportOptions {
                include_static_analysis: true,
                professor_view: true,
            },
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // The standalone "## Code complexity & testability" section is gone
        // (folded into Section B per-student blocks). The professor-only
        // "### Code complexity & testability — grading attribution" block
        // still renders. Use a newline anchor so the H2 check is not
        // satisfied by the surviving H3.
        assert!(
            !body.contains("\n## Code complexity & testability\n"),
            "standalone H2 complexity section must be removed even in professor view: {body}"
        );
        assert!(body.contains("grading attribution"));
        assert!(body.contains("Alice Bob") && body.contains("score"));
        assert!(body.contains("COMPLEXITY_HOTSPOT band: `WARNING`"));
        let prof_section_start = body.find("grading attribution").unwrap();
        let prof_section = &body[prof_section_start..];
        assert!(prof_section.contains("blob/HEAD/src/main/java/com/x/UserController.java#L42-L90"));
    }

    #[test]
    fn professor_view_off_by_default_does_not_render_attribution_block() {
        let conn = mk_conn();
        conn.execute(
            "INSERT INTO pull_requests (id, pr_number, repo_full_name, title, url,
                author_id, additions, deletions, changed_files, created_at, merged, merged_at, body)
             VALUES ('pr1', 1, 'udg-pds/spring-foo', 't', 'u', 'u1', 0, 0, 0,
                     '2026-02-20', 1, '2026-02-22', '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                assignee_id, sprint_id) VALUES (1, 'T-1', 'x', 'TASK', 'DONE', 3, 'u1', 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (1, 'pr1')",
            [],
        )
        .unwrap();
        seed_complexity_finding(
            &conn,
            1,
            "udg-pds/spring-foo",
            "src/main/java/com/x/A.java",
            "A",
            "f",
            "cyclomatic",
            "WARNING",
            10,
            50,
            Some(12.0),
            Some(10.0),
        );
        seed_complexity_attribution(&conn, 1, "u1", 1.0);
        seed_complexity_flag(&conn, "u1", "WARNING");

        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report_multi(&conn, 1, "pds26-1a", &[10], tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains("\n## Code complexity & testability\n"),
            "standalone H2 complexity section is removed: {body}"
        );
        assert!(!body.contains("grading attribution"));
        assert!(!body.contains("COMPLEXITY_HOTSPOT band"));
    }
}
