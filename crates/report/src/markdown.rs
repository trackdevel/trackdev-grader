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

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use rusqlite::{params_from_iter, types::Value, Connection};
use tracing::info;

use crate::charts::{html_escape, sparkline_svg, stacked_bars_svg, StackedRow};
use crate::flag_details::{enrich_flag_details, render_flag_details, render_flag_severity};

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
    avg_doc: Option<f64>,
    surv_norm: f64,
    density: f64,
    flag_count: i64,
    /// T-P2.1: per-student estimation bias (β_u). Only populated by
    /// `cumulative_student_summary`; per-sprint rows leave it `None`
    /// because bias is fitted across all sprints (the posterior wants
    /// the full per-student history).
    estimation_bias: Option<EstimationBiasCell>,
}

#[derive(Debug, Clone, Copy)]
struct EstimationBiasCell {
    beta_mean: f64,
    /// Half-width of the 95 % credible interval. Used to decide the
    /// directional symbol: when the CrI excludes 0 by more than the
    /// detector's 0.5-logit margin we render ▲/▼; otherwise ≈.
    half_width: f64,
}

fn student_summary_headers(include_bias: bool) -> Vec<&'static str> {
    let mut h = vec![
        "Student",
        "GitHub",
        "Points",
        "Share",
        "PR lines",
        "LS",
        "LD",
        "LS/pt",
        "Survival",
        "Density",
        "Doc score",
        "Flags",
    ];
    if include_bias {
        h.push("β_u");
    }
    h
}

/// Render the β_u cell. Symbol semantics mirror the ESTIMATION_BIAS
/// detector: ▲ over-estimator (CrI strictly above +0.5 logits), ▼
/// under-estimator (CrI strictly below −0.5), ≈ calibrated otherwise.
fn fmt_bias_cell(bias: Option<EstimationBiasCell>) -> String {
    let Some(b) = bias else {
        return String::new();
    };
    const MARGIN: f64 = 0.5;
    let lower = b.beta_mean - b.half_width;
    let upper = b.beta_mean + b.half_width;
    let symbol = if lower > MARGIN {
        "▲"
    } else if upper < -MARGIN {
        "▼"
    } else {
        "≈"
    };
    format!("{symbol} {:+.2}", b.beta_mean)
}

fn write_student_summary_table(buf: &mut String, students: &[StudentSummaryRow]) {
    write_student_summary_table_inner(buf, students, false);
}

fn write_cumulative_student_summary_table(buf: &mut String, students: &[StudentSummaryRow]) {
    write_student_summary_table_inner(buf, students, true);
}

fn write_student_summary_table_inner(
    buf: &mut String,
    students: &[StudentSummaryRow],
    include_bias: bool,
) {
    push_table_header(buf, &student_summary_headers(include_bias));
    for s in students {
        let mut cells = vec![
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
            fmt_f2(s.density),
            s.avg_doc.map(fmt_f1).unwrap_or_default(),
            fmt_int(s.flag_count),
        ];
        if include_bias {
            cells.push(fmt_bias_cell(s.estimation_bias));
        }
        push_table_row(buf, &cells);
    }
    buf.push('\n');
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
                sm.avg_doc_score,
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
                    avg_doc: r.get::<_, Option<f64>>(6)?,
                    surv_norm: r.get::<_, f64>(7)?,
                    density: r.get::<_, f64>(8)?,
                    flag_count: r.get::<_, i64>(9)?,
                    ls: stats.ls,
                    ld: stats.ld,
                    ls_per_pt: stats.ls_per_pt,
                    estimation_bias: None,
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
        let metrics: (f64, f64, Option<f64>) = conn
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(points_delivered), 0),
                            COALESCE(SUM(weighted_pr_lines), 0),
                            AVG(avg_doc_score)
                     FROM student_sprint_metrics
                     WHERE student_id = ? AND sprint_id IN ({})",
                    placeholders
                ),
                params_from_iter(sprint_params([Value::Text(id.clone())], sprint_ids)),
                |r| {
                    Ok((
                        r.get::<_, Option<f64>>(0)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(2)?,
                    ))
                },
            )
            .unwrap_or((0.0, 0.0, None));

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

        // T-P2.1: pull the per-student bias posterior for this project.
        // Falls back to None when the row is missing (project skipped or
        // student never had any estimated tasks). The half-width is
        // `(upper95 - lower95) / 2`, which equals 1.96 · σ_post for the
        // Gaussian posterior we fit.
        let bias: Option<EstimationBiasCell> = conn
            .query_row(
                "SELECT beta_mean, beta_lower95, beta_upper95
                 FROM student_estimation_bias
                 WHERE student_id = ? AND project_id = ?",
                rusqlite::params![id, project_id],
                |r| {
                    Ok((
                        r.get::<_, f64>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, f64>(2)?,
                    ))
                },
            )
            .ok()
            .map(|(mean, lo, hi)| EstimationBiasCell {
                beta_mean: mean,
                half_width: (hi - lo) / 2.0,
            });

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
            avg_doc: metrics.2,
            surv_norm,
            density,
            flag_count,
            estimation_bias: bias,
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
            let _ = writeln!(buf, "_Sprint window: {} → {}_\n", s, e);
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
                "_Legend:_ **Density** = surviving statements per estimated point \
(higher = more code per point delivered, a code-volume signal). \
**β_u** = per-student estimation bias in logits, posterior mean ± symbol \
(▲ over-estimator, ▼ under-estimator, ≈ calibrated within ±0.5 logits).\n\n",
            );
        }
    }

    // PR submission timing SVG (horizontal stacked bars, one row per student)
    let mut rows: Vec<StackedRow> = Vec::new();
    for s in &students {
        let mut raw_counts: HashMap<String, i64> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT pst.tier, COUNT(*) FROM pr_submission_tiers pst
             JOIN pull_requests pr ON pr.id = pst.pr_id
             WHERE pst.sprint_id = ? AND pr.author_id = ?
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
        for row in sev_stmt
            .query_map(rusqlite::params![project_id, sprint_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
        {
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
        let mut stmt = conn.prepare(
            "SELECT rule_name, COUNT(*) as n FROM architecture_violations av
             JOIN sprints s ON s.id = av.sprint_id
             WHERE s.project_id = ? AND av.sprint_id = ?
             GROUP BY rule_name ORDER BY n DESC, rule_name LIMIT 5",
        )?;
        let rows: Vec<(String, i64)> = stmt
            .query_map(rusqlite::params![project_id, sprint_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        if !rows.is_empty() {
            let _ = writeln!(buf, "Top rules:");
            for (rule, n) in rows {
                let _ = writeln!(buf, "- `{}` — {}", html_escape(&rule), n);
            }
            buf.push('\n');
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
}

/// Normalize a repo_full_name to its trailing component. The architecture
/// stage stores `<repo>` while survival/blame stores `<org>/<repo>`; this
/// strips any leading `<org>/` so both sources can be matched by basename.
fn repo_basename(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

fn architecture_violations_per_student(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<HashMap<String, Vec<AttributedArchViolation>>> {
    let ownership =
        sprint_grader_repo_analysis::file_ownership_for_project(conn, sprint_id, project_id)
            .unwrap_or_default();
    // (file_path, repo_basename) → (student_id, repo_full_qualified). The
    // ownership query already collapsed authors per file and resolved login
    // → student_id where possible, so we only have to index it. Keying by
    // repo basename keeps the lookup robust to the org-prefix mismatch
    // between `architecture_violations` and `fingerprints`; we keep the
    // org-qualified name alongside so URLs can be built later.
    let mut owner_by_file: HashMap<(String, String), (String, String)> = HashMap::new();
    for f in ownership {
        let repo = f.repo_full_name.unwrap_or_default();
        let key = (f.file_path, repo_basename(&repo).to_string());
        owner_by_file.insert(key, (f.dominant_author, repo));
    }

    // Limit to student_ids that actually belong to this team — keeps
    // anonymous logins or stale identities from leaking into the report.
    let mut team_stmt =
        conn.prepare("SELECT id FROM students WHERE team_project_id = ?")?;
    let team: HashSet<String> = team_stmt
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(team_stmt);

    let mut stmt = conn.prepare(
        "SELECT av.rule_name, av.file_path, av.repo_full_name, av.severity,
                av.offending_import
         FROM architecture_violations av
         JOIN sprints s ON s.id = av.sprint_id
         WHERE s.project_id = ? AND av.sprint_id = ?",
    )?;
    let rows: Vec<(String, String, String, String, String)> = stmt
        .query_map(rusqlite::params![project_id, sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut out: HashMap<String, Vec<AttributedArchViolation>> = HashMap::new();
    for (rule, file, repo, severity, import) in rows {
        let key = (file.clone(), repo_basename(&repo).to_string());
        let Some((owner, repo_fqn)) = owner_by_file.get(&key) else {
            continue;
        };
        if !team.contains(owner) {
            continue;
        }
        out.entry(owner.clone())
            .or_default()
            .push(AttributedArchViolation {
                rule_name: rule,
                file_path: file,
                severity,
                offending_import: import,
                repo_full_qualified: repo_fqn.clone(),
            });
    }
    Ok(out)
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

fn write_section_b(
    buf: &mut String,
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    depth: usize,
) -> rusqlite::Result<()> {
    let h2 = "#".repeat(depth);
    let h3 = "#".repeat(depth + 1);
    let _ = writeln!(buf, "{} B. Student dashboards\n", h2);

    let task_ls = task_ls_for_team(conn, sprint_id, project_id)?;
    let pr_ls = pr_ls_for_team(conn, sprint_id, project_id)?;
    let arch_per_student = architecture_violations_per_student(conn, sprint_id, project_id)?;

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

    for (sid, name, github) in &students {
        let _ = writeln!(buf, "{} {}", h3, name);
        if let Some(g) = github {
            let _ = writeln!(buf, "_GitHub: {}_\n", github_inline(g));
        } else {
            buf.push('\n');
        }

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
                &["Key", "Name", "Points", "LS", "LD", "LS/pt", "Status"],
            );
            for (task_id, key, name, pts, status) in tasks {
                let key_str = key.unwrap_or_default();
                let key_cell = if key_str.is_empty() {
                    String::new()
                } else {
                    md_link(&key_str, &trackdev_task_url(task_id))
                };
                let (ls, ld) = task_ls.get(&task_id).copied().unwrap_or((0.0, 0.0));
                let pts_val = pts.unwrap_or(0.0);
                let ls_per_pt = if pts_val > 0.0 { ls / pts_val } else { 0.0 };
                // push_table_row escapes pipes, so write by hand to keep the
                // [label](url) link intact.
                let _ = writeln!(
                    buf,
                    "| {} | {} | {} | {} | {} | {} | {} |",
                    key_cell,
                    md_escape(&name.unwrap_or_default()),
                    pts.map(fmt_f1).unwrap_or_default(),
                    fmt_f1(ls),
                    fmt_f1(ld),
                    fmt_f2(ls_per_pt),
                    md_escape(&status.unwrap_or_default()),
                );
            }
            buf.push('\n');
        }

        // PR table — only PRs whose linked task is a DONE TASK/BUG show up.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.title, pr.url,
                    pr.additions, pr.deletions, pr.attribution_errors
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND pr.author_id = ?
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
                let ls_per_pt = if linked_pts > 0.0 {
                    ls / linked_pts
                } else {
                    0.0
                };
                // push_table_row escapes pipes, but we want the link to stay
                // intact — emit by hand for this row.
                let _ = writeln!(
                    buf,
                    "| {} | {} | {} | +{} / -{} | {} | {} | {} | {} |",
                    num_cell_with_glyph,
                    md_escape(&repo_short),
                    linked_title.replace('|', "\\|"),
                    adds,
                    dels,
                    fmt_f1(linked_pts),
                    fmt_f1(ls),
                    fmt_f1(ld),
                    fmt_f2(ls_per_pt),
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

        buf.push_str("---\n\n");
    }
    Ok(())
}

/// Per-student architecture detail. Aggregates violations by rule, lists the
/// top files for each rule, and emits a compact severity breakdown so each
/// student sees their own architectural debt as an individual signal.
fn write_student_architecture_block(buf: &mut String, violations: &[AttributedArchViolation]) {
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

    // Group by rule_name. Each grouped row keeps the file path, the
    // org-qualified repo name (for URL building), and the offending import.
    // Rule order: descending count, tie-break alphabetical for determinism.
    type RuleExample = (String, String, String);
    let mut by_rule: HashMap<String, Vec<RuleExample>> = HashMap::new();
    for v in violations {
        by_rule.entry(v.rule_name.clone()).or_default().push((
            v.file_path.clone(),
            v.repo_full_qualified.clone(),
            v.offending_import.clone(),
        ));
    }
    let mut rules: Vec<(String, Vec<RuleExample>)> = by_rule.into_iter().collect();
    rules.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

    for (rule, files) in rules {
        let _ = writeln!(buf, "- `{}` — {} file(s)", html_escape(&rule), files.len());
        // Show up to 3 examples per rule with the offending import. The file
        // appears as a [basename](github-url) link with the full path in the
        // markdown title attribute (`[label](url "title")`), so the visible
        // text stays short while the source of truth is one click away.
        let mut shown: Vec<&RuleExample> = files.iter().collect();
        shown.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
        shown.dedup_by(|a, b| a.0 == b.0 && a.2 == b.2);
        for (file, repo_fqn, import) in shown.iter().take(3) {
            let basename = file.rsplit('/').next().unwrap_or(file);
            let label = format!("`{}`", basename);
            let file_cell = match github_file_url(repo_fqn, file) {
                Some(url) => format!("[{}]({} \"{}\")", label, url, md_escape(file)),
                None => label,
            };
            let _ = writeln!(buf, "  - {} ← `{}`", file_cell, html_escape(import));
        }
        if shown.len() > 3 {
            let _ = writeln!(buf, "  - … and {} more", shown.len() - 3);
        }
    }
    buf.push('\n');
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
    write_section_b(&mut buf, conn, sprint_id, project_id, 2)?;
    write_section_c(&mut buf, conn, sprint_id, project_id, 2)?;
    if let Some(sids) = cumulative_sprint_ids {
        if !sids.is_empty() {
            write_cumulative_summary(&mut buf, conn, project_id, sids, 2)?;
        }
    }

    std::fs::write(output_path, buf)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    info!(path = %output_path.display(), cumulative = cumulative_sprint_ids.is_some(), "Markdown report written");
    Ok(())
}

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
            format!(" ({} — {})", start, end)
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
        write_section_b(&mut buf, conn, *sid, project_id, 3)?;
        write_section_c(&mut buf, conn, *sid, project_id, 3)?;
    }

    if !sprint_ids_ordered.is_empty() {
        write_cumulative_summary(&mut buf, conn, project_id, sprint_ids_ordered, 2)?;
    }

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
                Some(v) => fmt_f2(v),
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

    fn mk_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
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
             CREATE TABLE flags (flag_id INTEGER PRIMARY KEY AUTOINCREMENT,
                student_id TEXT, sprint_id INTEGER, flag_type TEXT, severity TEXT,
                details TEXT);
             CREATE TABLE github_users (login TEXT PRIMARY KEY, name TEXT, email TEXT,
                student_id TEXT, fetched_at TEXT);
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
                PRIMARY KEY (repo_full_name, sprint_id, file_path, rule_name, offending_import));
             CREATE TABLE task_similarity_groups (group_id INTEGER PRIMARY KEY AUTOINCREMENT,
                sprint_id INTEGER, project_id INTEGER, representative_task_id INTEGER,
                group_label TEXT, stack TEXT, layer TEXT, action TEXT,
                member_count INTEGER, median_points REAL, median_lar REAL,
                median_ls REAL, median_ls_per_point REAL);
             CREATE TABLE task_group_members (group_id INTEGER, task_id INTEGER,
                sprint_id INTEGER, is_outlier INTEGER, outlier_reason TEXT,
                points_deviation REAL, lar_deviation REAL, ls_deviation REAL,
                ls_per_point_deviation REAL, PRIMARY KEY (group_id, task_id));
             INSERT INTO projects VALUES (1, 'pds26-1a');
             INSERT INTO sprints VALUES (10, 1, 'Sprint 1', '2026-02-16', '2026-03-08');
             INSERT INTO sprints VALUES (11, 1, 'Sprint 2', '2026-03-09', '2026-03-29');
             INSERT INTO students VALUES ('u1', 'Alice Bob', 'alice-gh', 1, 'a@ex.com');
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
             INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
                VALUES ('u1', 10, 'LOW_DOC_SCORE', 'WARNING',
                        '{\"message\":\"average doc score below threshold\"}');",
        )
        .unwrap();
        conn
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
        assert!(body.contains("Bob C (bob-gh)"), "missing humanized name for uuid-bob");
        assert!(!body.contains("uuid-bob"), "raw student_id leaked into output");
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
    fn architecture_section_renders_when_violations_exist() {
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
        assert!(body.contains("presentation-&gt;!infrastructure"));
    }

    #[test]
    fn per_student_architecture_attribution_matches_on_repo_basename() {
        // Regression: the architecture stage writes `repo_full_name` as a
        // bare repo (e.g. `spring-foo`) while fingerprints store `org/repo`
        // (e.g. `udg-pds/spring-foo`). Attribution must match on basename.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO github_users (login, student_id) VALUES ('alice-gh', 'u1');
             INSERT INTO fingerprints (file_path, repo_full_name, statement_index,
                raw_fingerprint, normalized_fingerprint, blame_author_login, sprint_id)
                VALUES ('A.java', 'udg-pds/spring-foo', 0, 'r0', 'n0', 'alice-gh', 10),
                       ('A.java', 'udg-pds/spring-foo', 1, 'r1', 'n1', 'alice-gh', 10);
             INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name,
                 violation_kind, offending_import, severity)
             VALUES
                ('spring-foo', 10, 'A.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repo.UserRepo', 'WARNING');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("**Architecture violations:** 1 (0 critical · 1 warning · 0 info)"),
            "violation must be attributed to Alice despite the org-prefix mismatch",
        );
    }

    #[test]
    fn per_student_architecture_block_attributes_to_dominant_owner() {
        // Two violated files. Alice (`u1`) owns A.java via fingerprints; the
        // C.java file has no fingerprints so it must stay unattributed and
        // therefore not show up in Alice's per-student block.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO github_users (login, student_id) VALUES ('alice-gh', 'u1');
             INSERT INTO fingerprints (file_path, repo_full_name, statement_index,
                raw_fingerprint, normalized_fingerprint, blame_author_login, sprint_id)
                VALUES ('A.java', 'udg/spring-foo', 0, 'r0', 'n0', 'alice-gh', 10),
                       ('A.java', 'udg/spring-foo', 1, 'r1', 'n1', 'alice-gh', 10);
             INSERT INTO architecture_violations
                (repo_full_name, sprint_id, file_path, rule_name,
                 violation_kind, offending_import, severity)
             VALUES
                ('udg/spring-foo', 10, 'A.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repo.UserRepo', 'WARNING'),
                ('udg/spring-foo', 10, 'A.java', 'domain-no-spring-web',
                 'forbidden_import', 'org.springframework.web.RestController', 'CRITICAL'),
                ('udg/spring-foo', 10, 'C.java', 'presentation->!infrastructure',
                 'layer_dependency', 'com.x.repo.OtherRepo', 'WARNING');",
        )
        .unwrap();
        let tmp = TempDir::new().unwrap();
        let path = generate_markdown_report(&conn, 10, 1, "pds26-1a", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Section B per-student block under Alice.
        assert!(
            body.contains("**Architecture violations:** 2 (1 critical · 1 warning · 0 info)"),
            "missing per-student arch headline; got: {}",
            body
        );
        assert!(
            body.contains("- `presentation-&gt;!infrastructure` — 1 file(s)"),
            "missing rule-grouped breakdown",
        );
        assert!(
            body.contains(
                "[`A.java`](https://github.com/udg/spring-foo/blob/HEAD/A.java \"A.java\") \
← `com.x.repo.UserRepo`"
            ),
            "missing linked example offending-import line; got body:\n{body}",
        );
        // Team-level severity breakdown reflects all three (including the
        // unattributed one).
        assert!(body.contains("**Total violations:** 3 (1 critical · 2 warning · 0 info)"));
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
    fn cumulative_table_carries_density_and_beta_legend() {
        let conn = mk_conn();
        let tmp = TempDir::new().unwrap();
        let path =
            generate_markdown_report_ex(&conn, 11, 1, "pds26-1a", tmp.path(), Some(&[10, 11]))
                .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("**Density**") && body.contains("**β_u**"),
            "missing legend explaining Density / β_u columns",
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
        assert!(body.contains(
            "| Alice Bob | [alice-gh](https://github.com/alice-gh) | 12 | 100% | 200 | 110 | 15 | 11 | 67.5% | 11.25 | 3.5 | 1 |"
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
        assert!(body.contains(
            "| Alice Bob | [alice-gh](https://github.com/alice-gh) | 5 | 100% | 120 | 60 | 10 | 20 | 85% | 17 | 3 | 1 |"
        ));
        assert!(body.contains(
            "| Alice Bob | [alice-gh](https://github.com/alice-gh) | 12 | 100% | 200 | 110 | 15 | 11 | 67.5% | 11.25 | 3.5 | 1 |"
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
            "INSERT INTO students VALUES ('u2', 'Fallback User', '', 1, 'f@ex.com');
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
}
