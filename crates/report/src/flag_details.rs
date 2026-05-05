use rusqlite::{params, Connection};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderedFlagDetails {
    pub plain: String,
    pub markdown: String,
    pub url: Option<String>,
}

impl RenderedFlagDetails {
    fn new(plain: String, markdown: String, url: Option<String>) -> Self {
        Self {
            plain,
            markdown,
            url,
        }
    }
}

pub(crate) fn render_flag_details(flag_type: &str, details: Option<&str>) -> RenderedFlagDetails {
    let parsed = details.and_then(|d| serde_json::from_str::<Value>(d).ok());
    match (flag_type, parsed.as_ref()) {
        ("TEAM_INEQUALITY", Some(v)) => render_team_inequality(v),
        ("CONTRIBUTION_IMBALANCE", Some(v)) => render_contribution_imbalance(v),
        ("GHOST_CONTRIBUTOR", Some(v)) => render_ghost_contributor(v),
        ("LOW_COMPOSITE_SCORE", Some(v)) => render_low_composite_score(v),
        ("LOW_SURVIVAL_RATE", Some(v)) => render_low_survival_rate(v),
        ("SINGLE_COMMIT_DUMP", Some(v)) => render_single_commit_dump(v),
        ("PR_DOES_NOT_COMPILE", Some(v)) => render_pr_reference("Does not compile", v, false),
        ("APPROVED_BROKEN_PR", Some(v)) => render_pr_reference("Approved broken PR", v, false),
        ("LAST_MINUTE_PR", Some(v)) => render_last_minute_pr(v),
        ("COSMETIC_REWRITE_VICTIM", Some(v)) => render_cosmetic_rewrite_victim(v),
        ("COSMETIC_REWRITE_ACTOR", Some(v)) => render_cosmetic_rewrite_actor(v),
        // Legacy rows from pre-T-P1.2 DBs: the single COSMETIC_REWRITE type
        // was attributed to the original author (victim) but the detail
        // named the rewriter under "rewriter".
        ("COSMETIC_REWRITE", Some(v)) => render_cosmetic_rewrite_legacy(v),
        ("COMPLEXITY_HOTSPOT", Some(v)) => render_complexity_hotspot(v),
        ("STATIC_ANALYSIS_HOTSPOT", Some(v)) => render_static_analysis_hotspot(v),
        ("ARCHITECTURE_HOTSPOT", Some(v)) => render_architecture_hotspot(v),
        (_, Some(v)) => render_generic(v),
        (_, None) => {
            let text = details.unwrap_or_default().to_string();
            RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
        }
    }
}

pub(crate) fn render_flag_severity(flag_type: &str, severity: &str) -> String {
    if flag_type == "TEAM_INEQUALITY" {
        format!("TEAM-LEVEL {severity}")
    } else {
        severity.to_string()
    }
}

pub(crate) fn enrich_flag_details(
    conn: &Connection,
    sprint_id: i64,
    student_id: &str,
    flag_type: &str,
    details: Option<&str>,
) -> Option<String> {
    match flag_type {
        "TEAM_INEQUALITY" => enrich_team_inequality_details(conn, sprint_id, student_id, details?)
            .and_then(|v| serde_json::to_string(&v).ok()),
        "APPROVED_BROKEN_PR" => enrich_approved_broken_pr_details(conn, details?)
            .and_then(|v| serde_json::to_string(&v).ok()),
        _ => None,
    }
}

fn enrich_approved_broken_pr_details(conn: &Connection, details: &str) -> Option<Value> {
    let mut parsed = serde_json::from_str::<Value>(details).ok()?;
    if string_field(&parsed, "pr_url")
        .filter(|u| u.starts_with("http"))
        .is_some()
    {
        return None;
    }
    let pr_id = string_field(&parsed, "pr_id")?;
    let (url, repo): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT url, repo_full_name FROM pull_requests WHERE id = ? LIMIT 1",
            [&pr_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .ok()?;
    let obj = parsed.as_object_mut()?;
    if let Some(u) = url.filter(|u| u.starts_with("http")) {
        obj.insert("pr_url".into(), json!(u));
    }
    if let Some(r) = repo.filter(|r| !r.is_empty()) {
        obj.insert("repo_full_name".into(), json!(r));
    }
    Some(parsed)
}

fn enrich_team_inequality_details(
    conn: &Connection,
    sprint_id: i64,
    student_id: &str,
    details: &str,
) -> Option<Value> {
    let mut parsed = serde_json::from_str::<Value>(details).ok()?;
    let dimension = string_field(&parsed, "dimension")?;
    let project_id = team_inequality_project_id(conn, student_id, parsed.get("project"))?;
    let members = match parsed.get("members").and_then(Value::as_array) {
        Some(existing) if !existing.is_empty() => {
            enrich_team_inequality_member_names(conn, project_id, existing).ok()?
        }
        _ => team_inequality_member_values(conn, sprint_id, project_id, &dimension).ok()?,
    };
    if members.is_empty() {
        return None;
    }

    let obj = parsed.as_object_mut()?;
    obj.insert("flagged_student".into(), json!(student_id));
    obj.insert("members".into(), Value::Array(members));
    Some(parsed)
}

fn enrich_team_inequality_member_names(
    conn: &Connection,
    project_id: i64,
    members: &[Value],
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT id, full_name FROM students WHERE team_project_id = ?")?;
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([project_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let names = rows
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();

    Ok(members
        .iter()
        .map(|member| {
            let mut member = member.clone();
            if member.get("student_name").and_then(Value::as_str).is_none() {
                if let Some(student_id) = string_field(&member, "student_id") {
                    if let Some(Some(name)) = names.get(&student_id) {
                        if let Some(obj) = member.as_object_mut() {
                            obj.insert("student_name".into(), json!(name));
                        }
                    }
                }
            }
            member
        })
        .collect())
}

fn team_inequality_project_id(
    conn: &Connection,
    student_id: &str,
    project_value: Option<&Value>,
) -> Option<i64> {
    if let Some(project) = project_value
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        if let Ok(project_id) = conn.query_row(
            "SELECT id FROM projects WHERE name = ? OR slug = ? LIMIT 1",
            params![project, project],
            |r| r.get::<_, i64>(0),
        ) {
            return Some(project_id);
        }
    }

    conn.query_row(
        "SELECT team_project_id FROM students WHERE id = ?",
        [student_id],
        |r| r.get::<_, Option<i64>>(0),
    )
    .ok()
    .flatten()
}

fn team_inequality_member_values(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    dimension: &str,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, full_name FROM students WHERE team_project_id = ? ORDER BY full_name, id",
    )?;
    let students: Vec<(String, Option<String>)> = stmt
        .query_map([project_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    students
        .iter()
        .map(|(sid, full_name)| {
            Ok(json!({
                "student_id": sid,
                "student_name": full_name,
                "value": team_inequality_member_value(conn, sprint_id, project_id, dimension, sid)?,
            }))
        })
        .collect()
}

fn team_inequality_member_value(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
    dimension: &str,
    student_id: &str,
) -> rusqlite::Result<f64> {
    match dimension {
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

fn render_contribution_imbalance(v: &Value) -> RenderedFlagDetails {
    let share = number_field(v, "share");
    let expected = number_field(v, "expected");
    let text = match (share, expected) {
        (Some(s), Some(e)) => {
            let direction = if s >= e { "above" } else { "below" };
            format!(
                "Contribution share is {}, {} the equal team share of {}.",
                fmt_percent(s),
                direction,
                fmt_percent(e)
            )
        }
        (Some(s), None) => format!("Contribution share is {}.", fmt_percent(s)),
        _ => "Contribution differs noticeably from the rest of the team.".into(),
    };
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_ghost_contributor(v: &Value) -> RenderedFlagDetails {
    let tasks_assigned = number_field(v, "tasks_assigned").unwrap_or(0.0);
    let composite = number_field(v, "composite").unwrap_or(0.0);
    let code_signal = number_field(v, "code_signal").unwrap_or(0.0);

    let task_text = if (tasks_assigned - 1.0).abs() < 0.05 {
        "1 assigned task".to_string()
    } else {
        format!("{} assigned tasks", fmt_num(tasks_assigned))
    };

    let signal_text = if code_signal <= 0.001 && composite <= 0.001 {
        "the sprint data shows no visible contribution attached to that work"
    } else if code_signal <= 0.05 && composite <= 0.05 {
        "the sprint data shows almost no visible contribution attached to that work"
    } else {
        "the sprint data shows much less visible contribution than expected for that assigned work"
    };

    let text = format!("Student has {task_text}, but {signal_text}.");
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_low_composite_score(v: &Value) -> RenderedFlagDetails {
    let code = number_field(v, "code").unwrap_or(0.0);
    let review = number_field(v, "review").unwrap_or(0.0);
    let task = number_field(v, "task").unwrap_or(0.0);
    let process = number_field(v, "process").unwrap_or(0.0);
    let composite = number_field(v, "composite").unwrap_or(0.0);

    let text = if composite <= 0.001
        && code <= 0.001
        && review <= 0.001
        && task <= 0.001
        && process <= 0.001
    {
        "Overall contribution signal for this sprint is effectively absent: no meaningful activity appears in code, task delivery, reviews, or process data.".to_string()
    } else {
        let weak_areas = [
            ("code work", code),
            ("task delivery", task),
            ("reviews", review),
            ("process activity", process),
        ]
        .into_iter()
        .filter_map(|(label, value)| (value <= 0.05).then_some(label))
        .collect::<Vec<_>>();

        if weak_areas.is_empty() {
            "Overall contribution signal for this sprint is very low.".to_string()
        } else {
            format!(
                "Overall contribution signal for this sprint is very low, especially in {}.",
                weak_areas.join(", ")
            )
        }
    };

    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_low_survival_rate(v: &Value) -> RenderedFlagDetails {
    let rate = number_field(v, "rate");
    let team_avg = number_field(v, "team_avg");
    let text = match (rate, team_avg) {
        (Some(r), Some(avg)) => format!(
            "Only {} of this student's added code survived the sprint, versus {} for the team on average.",
            fmt_percent(r),
            fmt_percent(avg)
        ),
        (Some(r), None) => format!(
            "Only {} of this student's added code survived the sprint.",
            fmt_percent(r)
        ),
        _ => "A much smaller share of this student's added code survived the sprint than expected.".to_string(),
    };
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_pr_reference(prefix: &str, v: &Value, include_repo: bool) -> RenderedFlagDetails {
    let (plain_pr, md_pr, url) = pr_reference(v, include_repo);
    let plain = format!("{prefix}: {plain_pr}.");
    let markdown = format!("{}: {}.", md_escape(prefix), md_pr);
    RenderedFlagDetails::new(plain, markdown, url)
}

fn render_single_commit_dump(v: &Value) -> RenderedFlagDetails {
    let (plain_pr, md_pr, url) = pr_reference(v, true);
    let total_lines = number_field(v, "total_lines");
    let plain = match total_lines {
        Some(n) => format!(
            "Single-commit dump: {plain_pr} ({} total lines).",
            fmt_num(n)
        ),
        None => format!("Single-commit dump: {plain_pr}."),
    };
    let markdown = match total_lines {
        Some(n) => format!("Single-commit dump: {md_pr} ({} total lines).", fmt_num(n)),
        None => format!("Single-commit dump: {md_pr}."),
    };
    RenderedFlagDetails::new(plain, markdown, url)
}

fn render_last_minute_pr(v: &Value) -> RenderedFlagDetails {
    let (plain_pr, md_pr, url) = pr_reference(v, false);
    let hours = number_field(v, "hours_before_deadline");
    let merged_at = string_field(v, "merged_at");
    let plain = match (hours, merged_at.as_deref()) {
        (Some(h), Some(ts)) if !ts.is_empty() => {
            format!("Merged {} ({ts}): {plain_pr}.", deadline_delta(h))
        }
        (Some(h), _) => format!("Merged {}: {plain_pr}.", deadline_delta(h)),
        (_, Some(ts)) if !ts.is_empty() => format!("Merged at {ts}: {plain_pr}."),
        _ => format!("Last-minute merge: {plain_pr}."),
    };
    let markdown = match (hours, merged_at.as_deref()) {
        (Some(h), Some(ts)) if !ts.is_empty() => {
            format!(
                "Merged {} ({}): {}.",
                deadline_delta(h),
                md_escape(ts),
                md_pr
            )
        }
        (Some(h), _) => format!("Merged {}: {}.", deadline_delta(h), md_pr),
        (_, Some(ts)) if !ts.is_empty() => format!("Merged at {}: {}.", md_escape(ts), md_pr),
        _ => format!("Last-minute merge: {md_pr}."),
    };
    RenderedFlagDetails::new(plain, markdown, url)
}

fn deadline_delta(hours_before_deadline: f64) -> String {
    if hours_before_deadline < -0.05 {
        format!(
            "{} after the deadline",
            fmt_hours(hours_before_deadline.abs())
        )
    } else {
        format!("{} before the deadline", fmt_hours(hours_before_deadline))
    }
}

fn render_cosmetic_rewrite_victim(v: &Value) -> RenderedFlagDetails {
    let actor = string_field(v, "counterpart_user_id").unwrap_or_else(|| "A teammate".into());
    let stmts = number_field(v, "statements_affected").unwrap_or(0.0);
    let stmts_label = if (stmts - 1.0).abs() < 0.05 {
        "1 statement".to_string()
    } else {
        format!("{} statements", fmt_num(stmts))
    };
    let text = format!(
        "{actor} cosmetically rewrote {stmts_label} you originally authored. No action needed."
    );
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_cosmetic_rewrite_actor(v: &Value) -> RenderedFlagDetails {
    let victim = string_field(v, "counterpart_user_id").unwrap_or_else(|| "a teammate".into());
    let stmts = number_field(v, "statements_affected").unwrap_or(0.0);
    let stmts_label = if (stmts - 1.0).abs() < 0.05 {
        "1 statement".to_string()
    } else {
        format!("{} statements", fmt_num(stmts))
    };
    let text = format!(
        "Cosmetically rewrote {stmts_label} originally authored by {victim}. Avoid churn-only changes."
    );
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_cosmetic_rewrite_legacy(v: &Value) -> RenderedFlagDetails {
    let rewriter = string_field(v, "rewriter").unwrap_or_else(|| "a teammate".into());
    let stmts = number_field(v, "statements_affected").unwrap_or(0.0);
    let stmts_label = if (stmts - 1.0).abs() < 0.05 {
        "1 statement".to_string()
    } else {
        format!("{} statements", fmt_num(stmts))
    };
    let text = format!(
        "{rewriter} cosmetically rewrote {stmts_label} originally authored by this student."
    );
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_complexity_hotspot(v: &Value) -> RenderedFlagDetails {
    let score = number_field(v, "score");
    let warn = number_field(v, "warn_threshold");
    let crit = number_field(v, "crit_threshold");
    let band = match (score, warn, crit) {
        (Some(s), _, Some(c)) if s >= c => format!(
            "Complexity hotspot score {} crosses the critical band ({}).",
            fmt_num(s),
            fmt_num(c)
        ),
        (Some(s), Some(w), _) => format!(
            "Complexity hotspot score {} crosses the warning band ({}).",
            fmt_num(s),
            fmt_num(w)
        ),
        (Some(s), _, _) => format!("Complexity hotspot score {}.", fmt_num(s)),
        _ => "Complexity hotspot threshold reached.".to_string(),
    };
    let tail =
        " See the Complexity & testability block in this dashboard for the offending methods.";
    let text = format!("{band}{tail}");
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_architecture_hotspot(v: &Value) -> RenderedFlagDetails {
    let weighted = number_field(v, "weighted");
    let min_weighted = number_field(v, "min_weighted");
    let lead = match (weighted, min_weighted) {
        (Some(w), Some(m)) => format!(
            "Architecture weighted contribution reached {} (threshold {}).",
            fmt_num(w),
            fmt_num(m)
        ),
        (Some(w), None) => format!("Architecture weighted contribution reached {}.", fmt_num(w)),
        _ => "Architecture hotspot threshold reached.".to_string(),
    };
    let tail =
        " See the Architecture violations block in this dashboard for the attributed offenders.";
    let text = format!("{lead}{tail}");
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_static_analysis_hotspot(v: &Value) -> RenderedFlagDetails {
    let weighted = number_field(v, "weighted");
    let min_weighted = number_field(v, "min_weighted");
    let lead = match (weighted, min_weighted) {
        (Some(w), Some(m)) => format!(
            "Static-analysis weighted findings reached {} (threshold {}).",
            fmt_num(w),
            fmt_num(m)
        ),
        (Some(w), None) => format!("Static-analysis weighted findings reached {}.", fmt_num(w)),
        _ => "Static-analysis threshold reached.".to_string(),
    };
    let tail = " See the Static analysis block in this dashboard for the attributed findings.";
    let text = format!("{lead}{tail}");
    RenderedFlagDetails::new(text.clone(), md_escape(&text), None)
}

fn render_team_inequality(v: &Value) -> RenderedFlagDetails {
    let dimension = string_field(v, "dimension").unwrap_or_default();
    let project = string_field(v, "project");
    let flagged = string_field(v, "flagged_student");
    let (label, singular_unit) = dimension_label(&dimension);
    let members = v
        .get("members")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let flagged_member = flagged
        .as_deref()
        .and_then(|sid| {
            members
                .iter()
                .find(|m| string_field(m, "student_id").as_deref() == Some(sid))
        })
        .or_else(|| members.first());

    let student_value = flagged_member.and_then(|m| number_field(m, "value"));
    let mut plain_parts = Vec::new();
    let mut md_parts = Vec::new();

    let intro = team_distribution_summary(
        label,
        singular_unit,
        project.as_deref(),
        flagged.as_deref(),
        student_value,
        &members,
    )
    .unwrap_or_else(|| match project {
        Some(p) if !p.is_empty() => format!("Uneven distribution of {label} in {p}."),
        _ => format!("Uneven distribution of {label}."),
    });
    plain_parts.push(intro.clone());
    md_parts.push(md_escape(&intro));

    RenderedFlagDetails::new(plain_parts.join(" "), md_parts.join(" "), None)
}

fn team_distribution_summary(
    label: &str,
    unit: &str,
    project: Option<&str>,
    flagged_student: Option<&str>,
    student_value: Option<f64>,
    members: &[Value],
) -> Option<String> {
    let student_value = student_value?;
    let values = members
        .iter()
        .filter_map(|m| {
            Some((
                string_field(m, "student_id")?,
                member_label(m)?,
                number_field(m, "value")?,
            ))
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }

    let total: f64 = values.iter().map(|(_, _, value)| *value).sum();
    let average = total / values.len() as f64;
    let share = if total > 0.0 {
        Some(student_value / total)
    } else {
        None
    };
    let min = values
        .iter()
        .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    let max = values
        .iter()
        .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

    let project_part = project
        .filter(|p| !p.is_empty())
        .map(|p| format!(" in {p}"))
        .unwrap_or_default();
    let mut text = format!(
        "Team-level imbalance in {label}{project_part}: this student is {} ({} vs team average {}).",
        relative_position(student_value, average),
        fmt_unit(student_value, unit),
        fmt_unit(average, unit)
    );
    if let Some(s) = share {
        text.push_str(&format!(
            " They account for {} of the team total.",
            fmt_percent(s)
        ));
    }

    if let (Some((min_sid, min_label, min_value)), Some((max_sid, max_label, max_value))) =
        (min, max)
    {
        let flagged_is_min = flagged_student == Some(min_sid.as_str());
        let flagged_is_max = flagged_student == Some(max_sid.as_str());
        let role = if flagged_is_max {
            Some("highest")
        } else if flagged_is_min {
            Some("lowest")
        } else {
            None
        };
        match role {
            Some(role) => text.push_str(&format!(
                " This is the team {role}. Team spread: {} ({min_label}) to {} ({max_label}).",
                fmt_unit(*min_value, unit),
                fmt_unit(*max_value, unit)
            )),
            None => text.push_str(&format!(
                " Team spread: {} ({min_label}) to {} ({max_label}).",
                fmt_unit(*min_value, unit),
                fmt_unit(*max_value, unit)
            )),
        }
    }

    Some(text)
}

fn relative_position(value: f64, average: f64) -> &'static str {
    if average == 0.0 {
        return "at the team average";
    }
    let ratio = (value - average) / average;
    let magnitude = ratio.abs();
    let direction = if ratio >= 0.0 { "above" } else { "below" };
    if magnitude < 0.10 {
        "near the team average"
    } else if magnitude < 0.35 {
        if direction == "above" {
            "slightly above average"
        } else {
            "slightly below average"
        }
    } else if direction == "above" {
        "well above average"
    } else {
        "well below average"
    }
}

fn member_label(member: &Value) -> Option<String> {
    string_field(member, "student_name")
        .or_else(|| string_field(member, "full_name"))
        .filter(|name| !name.is_empty())
        .or_else(|| string_field(member, "student_id"))
}

fn render_generic(v: &Value) -> RenderedFlagDetails {
    match v {
        Value::Object(map) => {
            let mut parts = Vec::new();
            if let Some(message) = map.get("message").and_then(Value::as_str) {
                parts.push(message.to_string());
            }
            for (key, value) in map {
                if key == "message" || is_internal_key(key) {
                    continue;
                }
                let rendered = plain_value(value);
                if rendered.is_empty() {
                    continue;
                }
                parts.push(format!("{}: {rendered}", label_key(key)));
            }
            let plain = parts.join("; ");
            RenderedFlagDetails::new(plain.clone(), md_escape(&plain), first_url(v))
        }
        _ => {
            let plain = plain_value(v);
            RenderedFlagDetails::new(plain.clone(), md_escape(&plain), first_url(v))
        }
    }
}

fn is_internal_key(key: &str) -> bool {
    key.starts_with("threshold")
        || matches!(
            key,
            "z_score"
                | "gini"
                | "hoover"
                | "cv"
                | "regularity_score"
                | "stderr_preview"
                | "exit_code"
                | "pr_id"
                | "flagged_student"
        )
}

fn label_key(key: &str) -> String {
    match key {
        "pr_number" | "number" => "PR".into(),
        "repo" | "repo_full_name" => "Repository".into(),
        "points" | "points_delivered" => "Task points".into(),
        "team_total" => "Team task points".into(),
        "share" => "Share".into(),
        "expected" => "Equal team share".into(),
        "weighted_lines" | "weighted_pr_lines" => "Weighted changed lines".into(),
        "pr_lines" => "Changed PR lines".into(),
        "commit_count" => "Commits".into(),
        "reviews_given" => "Reviews given".into(),
        "hours_before_deadline" => "Time before deadline".into(),
        "merged_at" => "Merged at".into(),
        other => other.replace('_', " "),
    }
}

fn plain_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(values) => values
            .iter()
            .take(5)
            .map(plain_value)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(map) => {
            if looks_like_pr(v) {
                return pr_reference(v, true).0;
            }
            if map.contains_key("key") || map.contains_key("task_key") {
                return task_label(v);
            }
            map.iter()
                .filter(|(k, _)| !is_internal_key(k))
                .filter_map(|(k, v)| {
                    let rendered = plain_value(v);
                    if rendered.is_empty() {
                        None
                    } else {
                        Some(format!("{}: {rendered}", label_key(k)))
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

fn looks_like_pr(v: &Value) -> bool {
    v.get("pr_number").is_some()
        || v.get("number").is_some()
        || v.get("pr_title").is_some()
        || v.get("title").is_some() && (v.get("url").is_some() || v.get("pr_url").is_some())
}

fn pr_reference(v: &Value, include_repo: bool) -> (String, String, Option<String>) {
    let number = number_field(v, "pr_number").or_else(|| number_field(v, "number"));
    let title = string_field(v, "pr_title").or_else(|| string_field(v, "title"));
    let repo = string_field(v, "repo").or_else(|| string_field(v, "repo_full_name"));
    let url = string_field(v, "pr_url")
        .or_else(|| string_field(v, "url"))
        .or_else(|| {
            // Construct GitHub URL from repo + pr_number for rows that pre-date pr_url storage.
            match (repo.as_deref(), number) {
                (Some(r), Some(n)) if r.contains('/') => {
                    Some(format!("https://github.com/{r}/pull/{}", n.round() as i64))
                }
                _ => None,
            }
        });
    let mut label = match (number, title.as_deref()) {
        (Some(n), Some(t)) if !t.is_empty() => format!("PR #{}: {t}", fmt_num(n)),
        (Some(n), _) => format!("PR #{}", fmt_num(n)),
        (_, Some(t)) if !t.is_empty() => t.to_string(),
        _ => "PR".into(),
    };
    if include_repo {
        if let Some(repo) = repo.filter(|r| !r.is_empty()) {
            label = format!("{repo} {label}");
        }
    }
    let markdown = match url.as_deref() {
        Some(u) if u.starts_with("http") => format!("[{}]({u})", md_escape(&label)),
        _ => md_escape(&label),
    };
    (label, markdown, url)
}

fn task_label(v: &Value) -> String {
    let key = string_field(v, "key").or_else(|| string_field(v, "task_key"));
    let name = string_field(v, "name");
    let points = number_field(v, "points");
    let base = match (key, name) {
        (Some(k), Some(n)) if !k.is_empty() && !n.is_empty() => format!("{k} - {n}"),
        (Some(k), _) if !k.is_empty() => k,
        (_, Some(n)) if !n.is_empty() => n,
        _ => String::new(),
    };
    match (base.is_empty(), points) {
        (false, Some(p)) => format!("{base} ({} pts)", fmt_num(p)),
        (false, None) => base,
        (true, Some(p)) => format!("{} pts", fmt_num(p)),
        (true, None) => String::new(),
    }
}

fn first_url(v: &Value) -> Option<String> {
    string_field(v, "pr_url")
        .or_else(|| string_field(v, "url"))
        .filter(|u| u.starts_with("http"))
}

fn string_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| match x {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn number_field(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| match x {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    })
}

fn dimension_label(name: &str) -> (&'static str, &'static str) {
    match name {
        "points_delivered" => ("task points delivered", "points"),
        "reviews_given" => ("reviews given", "reviews"),
        "commit_count" => ("authored commits", "commits"),
        "pr_lines" => ("changed PR lines", "lines"),
        _ => ("team contribution", "units"),
    }
}

fn fmt_percent(v: f64) -> String {
    format!("{}%", fmt_num(v * 100.0))
}

fn fmt_hours(v: f64) -> String {
    if (v - 1.0).abs() < 0.05 {
        "1 hour".into()
    } else {
        format!("{} hours", fmt_num(v))
    }
}

fn fmt_unit(v: f64, unit: &str) -> String {
    format!("{} {unit}", fmt_num(v))
}

fn fmt_num(v: f64) -> String {
    let rounded = (v * 10.0).round() / 10.0;
    if rounded.fract().abs() < f64::EPSILON {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|")
        .replace(['\n', '\r'], " ")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{enrich_flag_details, render_flag_details};

    #[test]
    fn renders_compile_flag_as_pr_reference_only() {
        let details = r##"{"pr_number":42,"pr_title":"Fix build","pr_url":"https://example.test/pr/42","exit_code":1,"stderr_preview":"boom"}"##;
        let rendered = render_flag_details("PR_DOES_NOT_COMPILE", Some(details));
        assert_eq!(rendered.plain, "Does not compile: PR #42: Fix build.");
        assert_eq!(
            rendered.markdown,
            "Does not compile: [PR #42: Fix build](https://example.test/pr/42)."
        );
        assert_eq!(rendered.url.as_deref(), Some("https://example.test/pr/42"));
    }

    #[test]
    fn renders_contribution_imbalance_without_z_score() {
        let details = r#"{"share":0.6,"expected":0.25,"z_score":2.8}"#;
        let rendered = render_flag_details("CONTRIBUTION_IMBALANCE", Some(details));
        assert_eq!(
            rendered.plain,
            "Contribution share is 60%, above the equal team share of 25%."
        );
        assert!(!rendered.plain.contains("z"));
    }

    #[test]
    fn renders_ghost_contributor_as_human_sentence() {
        let details = r#"{"tasks_assigned":7,"composite":0.0,"code_signal":0.0}"#;
        let rendered = render_flag_details("GHOST_CONTRIBUTOR", Some(details));
        assert_eq!(
            rendered.plain,
            "Student has 7 assigned tasks, but the sprint data shows no visible contribution attached to that work."
        );
        assert!(!rendered.plain.contains("code signal"));
        assert!(!rendered.plain.contains("composite"));
    }

    #[test]
    fn renders_low_composite_score_as_human_sentence() {
        let details = r#"{"code":0.0,"composite":0.0,"process":0.0,"review":0.0,"task":0.0}"#;
        let rendered = render_flag_details("LOW_COMPOSITE_SCORE", Some(details));
        assert_eq!(
            rendered.plain,
            "Overall contribution signal for this sprint is effectively absent: no meaningful activity appears in code, task delivery, reviews, or process data."
        );
        assert!(!rendered.plain.contains("composite"));
        assert!(!rendered.plain.contains("process:"));
    }

    #[test]
    fn renders_low_survival_rate_as_human_sentence() {
        let details = r#"{"rate":0.0,"team_avg":0.833,"z_score":1.7}"#;
        let rendered = render_flag_details("LOW_SURVIVAL_RATE", Some(details));
        assert_eq!(
            rendered.plain,
            "Only 0% of this student's added code survived the sprint, versus 83.3% for the team on average."
        );
        assert!(!rendered.plain.contains("team avg"));
        assert!(!rendered.plain.contains("z"));
    }

    #[test]
    fn last_minute_pr_after_deadline_does_not_render_negative_before() {
        let details = r##"{
            "pr_number":16,
            "pr_title":"Late fix",
            "pr_url":"https://example.test/pr/16",
            "hours_before_deadline":-39.8,
            "merged_at":"2026-04-23T08:49:04Z"
        }"##;
        let rendered = render_flag_details("LAST_MINUTE_PR", Some(details));
        assert!(rendered
            .plain
            .contains("Merged 39.8 hours after the deadline"));
        assert!(!rendered.plain.contains("-39.8 hours before"));
    }

    #[test]
    fn renders_team_inequality_without_internal_metrics() {
        let details = r##"{
            "dimension":"pr_lines",
            "gini":0.5,
            "hoover":0.3,
            "cv":1.2,
            "threshold_warning":0.35,
            "project":"pds26-5a",
            "flagged_student":"s1",
            "members":[
                {"student_id":"s1","value":120,"pull_requests":[{"number":7,"title":"Feature","url":"https://example.test/pr/7"}]},
                {"student_id":"s2","value":10,"pull_requests":[]}
            ]
        }"##;
        let rendered = render_flag_details("TEAM_INEQUALITY", Some(details));
        assert!(rendered.plain.contains("changed PR lines"));
        assert!(rendered
            .plain
            .contains("Team-level imbalance in changed PR lines in pds26-5a: this student is well above average (120 lines vs team average 65 lines)."));
        assert!(rendered
            .plain
            .contains("They account for 92.3% of the team total."));
        assert!(rendered.plain.contains("This is the team highest"));
        assert!(!rendered.plain.contains("gini"));
        assert!(!rendered.plain.contains("threshold"));
        assert!(!rendered.plain.contains("Team values"));
    }

    #[test]
    fn team_inequality_explains_below_average_students() {
        let details = r##"{
            "dimension":"reviews_given",
            "project":"pds26-5a",
            "flagged_student":"s2",
            "members":[
                {"student_id":"s1","value":8,"reviews_given":[]},
                {"student_id":"s2","value":0,"reviews_given":[]}
            ]
        }"##;
        let rendered = render_flag_details("TEAM_INEQUALITY", Some(details));
        assert!(rendered.plain.contains("reviews given"));
        assert!(rendered
            .plain
            .contains("Team-level imbalance in reviews given in pds26-5a: this student is well below average (0 reviews vs team average 4 reviews)."));
        assert!(rendered
            .plain
            .contains("They account for 0% of the team total."));
        assert!(rendered.plain.contains("This is the team lowest"));
        assert!(!rendered.plain.contains("Team values"));
    }

    #[test]
    fn old_team_inequality_rows_are_enriched_from_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE projects (id INTEGER PRIMARY KEY, slug TEXT, name TEXT);
            CREATE TABLE students (id TEXT PRIMARY KEY, github_login TEXT, full_name TEXT, team_project_id INTEGER);
            CREATE TABLE tasks (id INTEGER PRIMARY KEY, type TEXT, status TEXT, estimation_points INTEGER, assignee_id TEXT, sprint_id INTEGER);
            CREATE TABLE pull_requests (id TEXT PRIMARY KEY, author_id TEXT, additions INTEGER, deletions INTEGER);
            CREATE TABLE task_pull_requests (task_id INTEGER, pr_id TEXT);
            CREATE TABLE pr_commits (sha TEXT, pr_id TEXT);
            CREATE TABLE pr_reviews (pr_id TEXT, reviewer_login TEXT, submitted_at TEXT);

            INSERT INTO projects VALUES (20, 'pds26-5a', 'pds26-5a');
            INSERT INTO students VALUES ('s1', 'gh1', 'Student 1', 20);
            INSERT INTO students VALUES ('s2', 'gh2', 'Student 2', 20);
            INSERT INTO tasks VALUES (1, 'TASK', 'DONE', 10, 's1', 43);
            INSERT INTO tasks VALUES (2, 'TASK', 'DONE', 2, 's2', 43);
            ",
        )
        .unwrap();

        let old_details = r#"{"cv":1.204,"dimension":"points_delivered","gini":0.615,"hoover":0.5,"project":"pds26-5a"}"#;
        let enriched =
            enrich_flag_details(&conn, 43, "s1", "TEAM_INEQUALITY", Some(old_details)).unwrap();
        let rendered = render_flag_details("TEAM_INEQUALITY", Some(&enriched));
        assert!(rendered
            .plain
            .contains("Team-level imbalance in task points delivered in pds26-5a: this student is well above average (10 points vs team average 6 points)."));
        assert!(rendered
            .plain
            .contains("Team spread: 2 points (Student 2) to 10 points (Student 1)."));
        assert!(!rendered.plain.contains("Team values"));
        assert!(!rendered.plain.contains("gini"));
    }

    #[test]
    fn pr_lines_enrichment_uses_weighted_pr_lines() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE projects (id INTEGER PRIMARY KEY, slug TEXT, name TEXT);
            CREATE TABLE students (id TEXT PRIMARY KEY, github_login TEXT, full_name TEXT, team_project_id INTEGER);
            CREATE TABLE student_sprint_metrics (student_id TEXT, sprint_id INTEGER, weighted_pr_lines REAL);
            CREATE TABLE tasks (id INTEGER PRIMARY KEY, type TEXT, status TEXT, estimation_points INTEGER, assignee_id TEXT, sprint_id INTEGER);
            CREATE TABLE pull_requests (id TEXT PRIMARY KEY, author_id TEXT, additions INTEGER, deletions INTEGER);
            CREATE TABLE task_pull_requests (task_id INTEGER, pr_id TEXT);
            CREATE TABLE pr_commits (sha TEXT, pr_id TEXT);
            CREATE TABLE pr_reviews (pr_id TEXT, reviewer_login TEXT, submitted_at TEXT);

            INSERT INTO projects VALUES (20, 'pds26-5a', 'pds26-5a');
            INSERT INTO students VALUES ('s1', 'gh1', 'Student 1', 20);
            INSERT INTO students VALUES ('s2', 'gh2', 'Student 2', 20);
            INSERT INTO student_sprint_metrics VALUES ('s1', 43, 1711.0);
            INSERT INTO student_sprint_metrics VALUES ('s2', 43, 995.0);
            INSERT INTO pull_requests VALUES ('pr-1', 's1', 8000, 425);
            ",
        )
        .unwrap();

        let old_details = r#"{"dimension":"pr_lines","gini":0.4,"project":"pds26-5a"}"#;
        let enriched =
            enrich_flag_details(&conn, 43, "s1", "TEAM_INEQUALITY", Some(old_details)).unwrap();
        let rendered = render_flag_details("TEAM_INEQUALITY", Some(&enriched));
        assert!(rendered
            .plain
            .contains("1711 lines vs team average 1353 lines"));
        assert!(!rendered.plain.contains("8425"));
    }

    #[test]
    fn approved_broken_pr_hides_author_id() {
        let details = r#"{"pr_id":"raw-123","pr_number":9,"threshold":0.5,"author":"student-1"}"#;
        let rendered = render_flag_details("APPROVED_BROKEN_PR", Some(details));
        assert_eq!(rendered.plain, "Approved broken PR: PR #9.");
        assert!(!rendered.plain.contains("raw-123"));
        assert!(!rendered.plain.contains("student-1"));
        assert!(!rendered.plain.contains("threshold"));
    }

    #[test]
    fn approved_broken_pr_embeds_url_when_present() {
        let details = r#"{"pr_id":"raw-123","pr_number":31,"author":"alice","pr_url":"https://github.com/org/repo/pull/31"}"#;
        let rendered = render_flag_details("APPROVED_BROKEN_PR", Some(details));
        assert_eq!(rendered.plain, "Approved broken PR: PR #31.");
        assert_eq!(
            rendered.markdown,
            "Approved broken PR: [PR #31](https://github.com/org/repo/pull/31)."
        );
        assert_eq!(
            rendered.url.as_deref(),
            Some("https://github.com/org/repo/pull/31")
        );
    }

    #[test]
    fn single_commit_dump_formats_with_url_and_repo() {
        let details = r#"{"pr_number":10,"repo":"udg-pds/android-pds26_4c","total_lines":352,"threshold":200,"pr_url":"https://github.com/udg-pds/android-pds26_4c/pull/10"}"#;
        let rendered = render_flag_details("SINGLE_COMMIT_DUMP", Some(details));
        assert_eq!(
            rendered.plain,
            "Single-commit dump: udg-pds/android-pds26_4c PR #10 (352 total lines)."
        );
        assert!(rendered.markdown.contains("[udg-pds/android-pds26_4c PR"));
        assert!(rendered.markdown.contains("(352 total lines)"));
        assert_eq!(
            rendered.url.as_deref(),
            Some("https://github.com/udg-pds/android-pds26_4c/pull/10")
        );
        assert!(!rendered.plain.contains("threshold"));
    }

    #[test]
    fn single_commit_dump_constructs_url_from_repo_when_pr_url_absent() {
        // Old DB rows have repo in <org>/<repo> format but no pr_url.
        let details = r#"{"pr_number":2,"repo":"udg-pds/android-pds26_3b","total_lines":666,"threshold":200}"#;
        let rendered = render_flag_details("SINGLE_COMMIT_DUMP", Some(details));
        assert_eq!(
            rendered.plain,
            "Single-commit dump: udg-pds/android-pds26_3b PR #2 (666 total lines)."
        );
        assert!(rendered
            .markdown
            .contains("https://github.com/udg-pds/android-pds26_3b/pull/2"));
        assert_eq!(
            rendered.url.as_deref(),
            Some("https://github.com/udg-pds/android-pds26_3b/pull/2")
        );
    }

    #[test]
    fn approved_broken_pr_enrichment_adds_url_from_db() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE pull_requests (id TEXT PRIMARY KEY, url TEXT, repo_full_name TEXT);
             INSERT INTO pull_requests VALUES ('pr-uuid-1', 'https://github.com/org/repo/pull/31', 'org/repo');",
        )
        .unwrap();
        let details = r#"{"pr_id":"pr-uuid-1","pr_number":31,"author":"alice"}"#;
        let enriched =
            super::enrich_flag_details(&conn, 1, "alice", "APPROVED_BROKEN_PR", Some(details));
        let rendered = render_flag_details("APPROVED_BROKEN_PR", enriched.as_deref());
        assert_eq!(
            rendered.markdown,
            "Approved broken PR: [PR #31](https://github.com/org/repo/pull/31)."
        );
        assert_eq!(
            rendered.url.as_deref(),
            Some("https://github.com/org/repo/pull/31")
        );
    }

    #[test]
    fn generic_rendering_hides_internal_pr_id_and_thresholds() {
        let details = r#"{"pr_id":"raw-123","pr_number":9,"threshold":0.5,"author":"student-1"}"#;
        let rendered = render_flag_details("SOME_FLAG", Some(details));
        assert_eq!(rendered.plain, "author: student-1; PR: 9");
        assert!(!rendered.plain.contains("raw-123"));
        assert!(!rendered.plain.contains("threshold"));
    }
}
