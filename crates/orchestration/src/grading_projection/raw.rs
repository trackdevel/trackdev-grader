//! Load a `grade_core::RawProject` from a grading.db connection.

use super::db_axis::{
    architecture_counts, architecture_scan_present, code_quality_raw, documentation_raw,
    project_repos, survival_raw,
};
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use grade_core::{
    has_gradable_artifact, hotspot_blame_magnitude, AxisInputs, RawProject, RawStudent, RawTask,
    RepoMetrics, StudentFlag,
};
use rusqlite::{params, Connection};
use sprint_grader_core::Database;

pub fn load_raw_project(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<RawProject> {
    let name: String = conn.query_row(
        "SELECT name FROM projects WHERE id = ?",
        params![project_id],
        |r| r.get(0),
    )?;
    let team_size: i64 = conn.query_row(
        "SELECT COUNT(*) FROM students WHERE team_project_id = ?",
        params![project_id],
        |r| r.get(0),
    )?;

    let doc = documentation_raw(conn, project_id, sprint_ids)?;
    let (cq, cc_pct, mutation) = code_quality_raw(conn, project_id, sprint_ids)?;
    let surv = survival_raw(conn, project_id, sprint_ids)?;
    let repos = project_repos(conn, project_id)?;
    let inventory_repos = repos_for_inventory(conn, project_id, &repos)?;
    let (arch_crit, arch_warn) = architecture_counts(conn, project_id)?;
    let arch_present = architecture_scan_present(conn, &repos)?;

    let axis = AxisInputs {
        documentation_raw: doc.raw_value.unwrap_or(0.0),
        doc_present: doc.present,
        code_quality_raw: cq.raw_value.unwrap_or(0.0),
        cc_pct: cc_pct.unwrap_or(0.0),
        mutation_score: mutation.unwrap_or(0.0),
        cq_present: cq.present,
        survival_raw: surv.raw_value.unwrap_or(0.0),
        surv_present: surv.present,
        arch_crit_count: arch_crit,
        arch_warn_count: arch_warn,
        arch_present,
    };

    Ok(RawProject {
        project_id,
        name,
        team_size,
        axis,
        inventory: load_inventory(conn, &inventory_repos)?,
        tasks: load_tasks(conn, project_id, sprint_ids)?,
        students: load_students(conn, project_id)?,
        crit_findings: vec![],
        student_flags: load_student_flags(conn, project_id, sprint_ids)?,
    })
}

fn inventory_table_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'repo_structural_metrics'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Union PR-linked repo names with inventory scan rows for this project.
fn repos_for_inventory(
    conn: &Connection,
    project_id: i64,
    pr_repos: &[String],
) -> rusqlite::Result<Vec<String>> {
    let mut out = pr_repos.to_vec();
    if !inventory_table_exists(conn) {
        return Ok(out);
    }
    let mut stmt = conn.prepare(
        "SELECT DISTINCT repo_full_name FROM project_inventory_runs
         WHERE project_id = ? AND metric_count > 0",
    )?;
    let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
    for row in rows {
        let name = row?;
        if !out.iter().any(|r| r == &name) {
            out.push(name);
        }
    }
    Ok(out)
}

fn load_inventory(conn: &Connection, repos: &[String]) -> rusqlite::Result<Vec<RepoMetrics>> {
    if !inventory_table_exists(conn) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for repo in repos {
        let mut stmt = conn.prepare(
            "SELECT metric_key, value FROM repo_structural_metrics WHERE repo_full_name = ?",
        )?;
        let rows = stmt.query_map(rusqlite::params![repo], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?;
        let mut metrics = BTreeMap::new();
        for row in rows {
            let (k, v) = row?;
            metrics.insert(k, v);
        }
        if !metrics.is_empty() {
            out.push(RepoMetrics {
                repo_full_name: repo.clone(),
                metrics,
            });
        }
    }
    Ok(out)
}

fn load_tasks(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<Vec<RawTask>> {
    if sprint_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT t.assignee_id, t.estimation_points,
                tai.model_value, tai.level_value, tai.declared
         FROM tasks t
         JOIN students s ON s.id = t.assignee_id
         LEFT JOIN task_ai_usage tai ON tai.task_id = t.id
         WHERE s.team_project_id = ?
           AND t.sprint_id IN ({placeholders})
           AND t.status = 'DONE'
           AND t.type != 'USER_STORY'
           AND t.assignee_id IS NOT NULL
           AND t.estimation_points IS NOT NULL"
    );
    let mut bind: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        bind.push((*sid).into());
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bind), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (assignee_id, raw_pts, model, level, declared_flag) = row?;
        out.push(RawTask {
            assignee_id,
            raw_points: raw_pts as f64,
            ai_model: model,
            ai_level: level,
            declared: declared_flag.unwrap_or(0) == 1,
        });
    }
    Ok(out)
}

fn load_students(conn: &Connection, project_id: i64) -> rusqlite::Result<Vec<RawStudent>> {
    let mut stmt =
        conn.prepare("SELECT id, full_name FROM students WHERE team_project_id = ? ORDER BY id")?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok(RawStudent {
            student_id: r.get(0)?,
            full_name: r.get(1)?,
        })
    })?;
    rows.collect()
}

fn load_student_flags(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<Vec<StudentFlag>> {
    let mut out = Vec::new();
    if !sprint_ids.is_empty() {
        let placeholders = sprint_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT student_id, severity, flag_type, details FROM flags
             WHERE sprint_id IN ({placeholders})
               AND student_id NOT LIKE 'PROJECT_%'"
        );
        let mut bind: Vec<rusqlite::types::Value> = Vec::new();
        for sid in sprint_ids {
            bind.push((*sid).into());
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (student_id, severity, flag_type, details) = row?;
            if conn.query_row(
                "SELECT COUNT(*) FROM students WHERE id = ? AND team_project_id = ?",
                params![student_id, project_id],
                |r| r.get::<_, i64>(0),
            )? == 0
            {
                continue;
            }
            let flag_type = flag_type.unwrap_or_default();
            let weighted = flag_magnitude(&flag_type, details.as_deref());
            out.push(StudentFlag {
                student_id,
                severity,
                source: "sprint".to_string(),
                flag_type,
                weighted,
            });
        }
    }

    let mut stmt = conn.prepare(
        "SELECT student_id, severity, flag_type, details FROM student_artifact_flags
         WHERE project_id = ? AND student_id NOT LIKE 'PROJECT_%'",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (student_id, severity, flag_type, details) = row?;
        let Some(student_id) = student_id else {
            continue;
        };
        let flag_type = flag_type.unwrap_or_default();
        let weighted = flag_magnitude(&flag_type, details.as_deref());
        out.push(StudentFlag {
            student_id,
            severity: severity.unwrap_or_default(),
            source: "artifact".to_string(),
            flag_type,
            weighted,
        });
    }
    Ok(out)
}

fn flag_magnitude(flag_type: &str, details: Option<&str>) -> Option<f64> {
    let mag = hotspot_blame_magnitude(flag_type, details);
    if mag > 0.0 {
        Some(mag)
    } else {
        None
    }
}

/// Load every project in the DB (optionally filtered by name) as `RawProject` rows.
pub fn load_cohort_raw_projects(
    db: &Database,
    today: &str,
    project_filter: Option<&[String]>,
) -> Result<Vec<RawProject>> {
    let project_ids = resolve_project_ids(db, project_filter)?;
    let mut out = Vec::with_capacity(project_ids.len());
    for pid in project_ids {
        let sprint_ids = db
            .sprint_ids_up_to_current(pid, today)
            .with_context(|| format!("sprint_ids for project_id {pid}"))?;
        let raw = load_raw_project(&db.conn, pid, &sprint_ids)
            .with_context(|| format!("load_raw_project project_id {pid}"))?;
        if has_gradable_artifact(&raw) {
            out.push(raw);
        }
    }
    Ok(out)
}

fn resolve_project_ids(db: &Database, project_filter: Option<&[String]>) -> Result<Vec<i64>> {
    match project_filter {
        Some(names) if !names.is_empty() => {
            let mut ids = Vec::new();
            for name in names {
                let id: i64 = db
                    .conn
                    .query_row("SELECT id FROM projects WHERE name = ?", [name], |r| {
                        r.get(0)
                    })
                    .with_context(|| format!("project not found: {name}"))?;
                ids.push(id);
            }
            Ok(ids)
        }
        _ => {
            let mut stmt = db.conn.prepare("SELECT id FROM projects ORDER BY id")?;
            let ids = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            Ok(ids)
        }
    }
}
