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

/// Sprint ordinal (1-based) from which AI usage counts toward the keep
/// discount. AI was forbidden in the course's first two sprints, so any
/// declaration there is void and those tasks keep 100% of their points.
/// The desktop projection (`apps/desktop/src/data/projection.ts`) mirrors
/// this value — keep them in sync.
pub const AI_ALLOWED_FROM_SPRINT_ORDINAL: u32 = 3;

/// No sprint restriction (every sprint counts AI) — used by reference fixtures.
const AI_ALLOWED_FROM_FIRST_SPRINT: u32 = 1;

pub fn load_raw_project(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<RawProject> {
    load_raw_project_gated(conn, project_id, sprint_ids, AI_ALLOWED_FROM_FIRST_SPRINT)
}

/// As [`load_raw_project`], but AI declarations in sprints before
/// `ai_allowed_from_ordinal` are ignored (the tasks keep 100%).
pub fn load_raw_project_gated(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
    ai_allowed_from_ordinal: u32,
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
        tasks: load_tasks(conn, project_id, sprint_ids, ai_allowed_from_ordinal)?,
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
    ai_allowed_from_ordinal: u32,
) -> rusqlite::Result<Vec<RawTask>> {
    if sprint_ids.is_empty() {
        return Ok(Vec::new());
    }
    // `sprint_ids` is ordered by start_date ascending, so the first
    // `ordinal - 1` entries are the AI-forbidden early sprints. Tasks there are
    // treated as undeclared (AI ignored) so they keep 100% of their points.
    let forbidden_count = ai_allowed_from_ordinal.saturating_sub(1) as usize;
    let ai_forbidden_sprints: std::collections::HashSet<i64> =
        sprint_ids.iter().take(forbidden_count).copied().collect();

    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT t.sprint_id, t.assignee_id, t.estimation_points,
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
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i64>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (sprint_id, assignee_id, raw_pts, model, level, declared_flag) = row?;
        let ai_forbidden = ai_forbidden_sprints.contains(&sprint_id);
        out.push(RawTask {
            assignee_id,
            raw_points: raw_pts as f64,
            ai_model: if ai_forbidden { None } else { model },
            ai_level: if ai_forbidden { None } else { level },
            declared: !ai_forbidden && declared_flag.unwrap_or(0) == 1,
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
        let raw =
            load_raw_project_gated(&db.conn, pid, &sprint_ids, AI_ALLOWED_FROM_SPRINT_ORDINAL)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Project with three ascending sprints and a declared-AI task in sprint 1
    /// (5 pts) and sprint 3 (7 pts). Returns the kept tempdir + open Database.
    fn seed_three_sprint_project() -> (tempfile::TempDir, Database) {
        let dir = tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("g.db")).expect("open db");
        db.create_tables().expect("schema");
        let c = &db.conn;
        c.execute(
            "INSERT INTO projects (id, slug, name) VALUES (1, 'team-01', 'Team 01')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO students (id, username, github_login, full_name, team_project_id)
             VALUES ('alice', 'alice', 'alice', 'Alice', 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES
               (100, 1, 'S1', '2026-01-01', '2026-01-15'),
               (200, 1, 'S2', '2026-02-01', '2026-02-15'),
               (300, 1, 'S3', '2026-03-01', '2026-03-15')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id) VALUES
               (1, 'T-1', 'a', 'TASK', 'DONE', 5, 'alice', 100),
               (2, 'T-2', 'b', 'TASK', 'DONE', 7, 'alice', 300)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at) VALUES
               (1, 'GPT-5.5', 'E', 1, '2026-01-02'),
               (2, 'GPT-5.5', 'E', 1, '2026-03-02')",
            [],
        )
        .unwrap();
        (dir, db)
    }

    #[test]
    fn ai_declarations_in_forbidden_early_sprints_are_ignored() {
        let (_dir, db) = seed_three_sprint_project();
        let sprint_ids = [100i64, 200, 300];

        // ordinal 3 → sprints 1 and 2 are AI-forbidden. The sprint-1 task (5 pts)
        // is forced undeclared; the sprint-3 task (7 pts) keeps its declaration.
        let tasks = load_tasks(&db.conn, 1, &sprint_ids, 3).expect("load tasks");
        let early = tasks
            .iter()
            .find(|t| t.raw_points == 5.0)
            .expect("sprint-1 task");
        let late = tasks
            .iter()
            .find(|t| t.raw_points == 7.0)
            .expect("sprint-3 task");
        assert!(early.ai_model.is_none(), "forbidden sprint clears model");
        assert!(early.ai_level.is_none(), "forbidden sprint clears level");
        assert!(!early.declared, "forbidden sprint marks task undeclared");
        assert_eq!(late.ai_model.as_deref(), Some("GPT-5.5"));
        assert_eq!(late.ai_level.as_deref(), Some("E"));
        assert!(late.declared);
    }

    #[test]
    fn ordinal_one_imposes_no_sprint_restriction() {
        let (_dir, db) = seed_three_sprint_project();
        let sprint_ids = [100i64, 200, 300];
        // ordinal 1 (the reference-fixture / no-restriction value): every task
        // keeps whatever it declared, regardless of sprint.
        let tasks = load_tasks(&db.conn, 1, &sprint_ids, 1).expect("load tasks");
        assert_eq!(tasks.len(), 2);
        assert!(tasks
            .iter()
            .all(|t| t.declared && t.ai_model.as_deref() == Some("GPT-5.5")));
    }
}
