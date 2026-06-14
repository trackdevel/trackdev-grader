//! Load a `grade_core::RawProject` from a grading.db connection (test helper).

#[path = "db_axis.rs"]
mod db_axis;

use db_axis::{
    architecture_counts, architecture_scan_present, code_quality_raw, documentation_raw,
    project_repos, survival_raw,
};
use std::collections::BTreeMap;

use grade_core::{
    AxisInputs, CritFinding, FindingKind, RawProject, RawStudent, RawTask, RepoMetrics,
    StudentFlag,
};
use rusqlite::{params, Connection};
use sprint_grader_core::finding::{RuleKind, Severity};
use sprint_grader_core::rule_attribution::load_attributed_findings_for_repo;

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
        inventory: load_inventory(conn, &repos)?,
        tasks: load_tasks(conn, project_id, sprint_ids)?,
        students: load_students(conn, project_id)?,
        crit_findings: load_crit_findings(conn, project_id)?,
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

fn load_inventory(
    conn: &Connection,
    repos: &[String],
) -> rusqlite::Result<Vec<RepoMetrics>> {
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

fn load_crit_findings(conn: &Connection, project_id: i64) -> rusqlite::Result<Vec<CritFinding>> {
    let mut out = Vec::new();
    for repo in project_repos(conn, project_id)? {
        let mut stmt = conn.prepare(
            "SELECT category FROM static_analysis_findings
             WHERE repo_full_name = ? AND severity = 'CRITICAL'",
        )?;
        let rows = stmt.query_map(params![repo], |r| r.get::<_, Option<String>>(0))?;
        for row in rows {
            out.push(CritFinding {
                kind: FindingKind::StaticAnalysis,
                category: row?,
            });
        }
        let findings = load_attributed_findings_for_repo(conn, &repo, RuleKind::Complexity)?;
        for af in findings {
            if af.finding.severity == Severity::Critical {
                out.push(CritFinding {
                    kind: FindingKind::Complexity,
                    category: None,
                });
            }
        }
    }
    Ok(out)
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
            "SELECT student_id, severity, flag_type FROM flags
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
            ))
        })?;
        for row in rows {
            let (student_id, severity, flag_type) = row?;
            if conn.query_row(
                "SELECT COUNT(*) FROM students WHERE id = ? AND team_project_id = ?",
                params![student_id, project_id],
                |r| r.get::<_, i64>(0),
            )? == 0
            {
                continue;
            }
            out.push(StudentFlag {
                student_id,
                severity,
                source: "sprint".to_string(),
                flag_type: flag_type.unwrap_or_default(),
                weighted: 0.0,
            });
        }
    }

    // T2.2 mirror: hotspot artifact flags carry the per-student blame
    // magnitude in their details JSON.
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
        let weighted = details
            .as_deref()
            .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
            .map(|v| grade_core::hotspot_magnitude(&flag_type, &v))
            .unwrap_or(0.0);
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
