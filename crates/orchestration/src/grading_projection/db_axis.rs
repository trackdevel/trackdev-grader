//! Axis raw loaders for test DB projection (mirrors grading_xlsx normalize SQL).

use rusqlite::{params, Connection};

#[derive(Debug, Clone, PartialEq)]
pub struct AxisRaw {
    pub raw_value: Option<f64>,
    pub present: bool,
}

pub fn documentation_raw(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<AxisRaw> {
    if sprint_ids.is_empty() {
        return Ok(AxisRaw {
            raw_value: None,
            present: false,
        });
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT AVG(pde.total_doc_score)
         FROM pr_doc_evaluation pde
         JOIN pull_requests pr ON pr.id = pde.pr_id
         JOIN pr_authors pa ON pa.pr_id = pr.id
         JOIN students s ON s.id = pa.student_id
         WHERE s.team_project_id = ?
           AND pde.sprint_id IN ({placeholders})
           AND pde.total_doc_score IS NOT NULL"
    );
    let mut params: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let avg: Option<f64> =
        conn.query_row(&sql, rusqlite::params_from_iter(params), |r| r.get(0))?;
    Ok(match avg {
        Some(v) => AxisRaw {
            raw_value: Some(v),
            present: true,
        },
        None => AxisRaw {
            raw_value: None,
            present: false,
        },
    })
}

pub fn code_quality_raw(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<(AxisRaw, Option<f64>, Option<f64>)> {
    if sprint_ids.is_empty() {
        return Ok((
            AxisRaw {
                raw_value: None,
                present: false,
            },
            None,
            None,
        ));
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT AVG(ssq.avg_maintainability), AVG(ssq.pct_methods_cc_over_10)
         FROM student_sprint_quality ssq
         JOIN students s ON s.id = ssq.student_id
         WHERE s.team_project_id = ?
           AND ssq.sprint_id IN ({placeholders})
           AND ssq.avg_maintainability IS NOT NULL"
    );
    let mut params: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let (mi, cc): (Option<f64>, Option<f64>) =
        conn.query_row(&sql, rusqlite::params_from_iter(params), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;

    let mutation_sql = format!(
        "SELECT AVG(pm.mutation_score)
         FROM pr_mutation pm
         JOIN pull_requests pr ON pr.id = pm.pr_id
         JOIN pr_authors pa ON pa.pr_id = pr.id
         JOIN students s ON s.id = pa.student_id
         WHERE s.team_project_id = ?
           AND pm.sprint_id IN ({placeholders})
           AND pm.mutation_score IS NOT NULL"
    );
    let mut mparams: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        mparams.push((*sid).into());
    }
    let mutation: Option<f64> =
        conn.query_row(&mutation_sql, rusqlite::params_from_iter(mparams), |r| {
            r.get(0)
        })?;

    Ok((
        match mi {
            Some(v) => AxisRaw {
                raw_value: Some(v),
                present: true,
            },
            None => AxisRaw {
                raw_value: None,
                present: false,
            },
        },
        cc,
        mutation,
    ))
}

pub fn survival_raw(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<AxisRaw> {
    if sprint_ids.is_empty() {
        return Ok(AxisRaw {
            raw_value: None,
            present: false,
        });
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT sss.survival_rate_normalized, sss.estimation_points_total
         FROM student_sprint_survival sss
         JOIN students s ON s.id = sss.student_id
         WHERE s.team_project_id = ?
           AND sss.sprint_id IN ({placeholders})
           AND sss.survival_rate_normalized IS NOT NULL"
    );
    let mut params: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((r.get::<_, f64>(0)?, r.get::<_, Option<i64>>(1)?))
    })?;
    let mut weighted_sum = 0.0;
    let mut weight_total = 0.0;
    let mut any = false;
    for row in rows {
        let (rate, pts) = row?;
        any = true;
        let w = pts.map(|p| p as f64).unwrap_or(1.0).max(0.0);
        if w > 0.0 {
            weighted_sum += rate * w;
            weight_total += w;
        }
    }
    if !any || weight_total <= 0.0 {
        return Ok(AxisRaw {
            raw_value: None,
            present: false,
        });
    }
    Ok(AxisRaw {
        raw_value: Some(weighted_sum / weight_total),
        present: true,
    })
}

pub fn project_repos(conn: &Connection, project_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.repo_full_name
         FROM pull_requests pr
         JOIN pr_authors pa ON pa.pr_id = pr.id
         JOIN students s ON s.id = pa.student_id
         WHERE s.team_project_id = ? AND pr.repo_full_name IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![project_id], |r| r.get(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn architecture_scan_present(conn: &Connection, repos: &[String]) -> rusqlite::Result<bool> {
    for repo in repos {
        // SKIPPED_HEAD_UNCHANGED means a prior OK scan's violations are still
        // valid (the cache gate found the same HEAD) — the data is present.
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM architecture_runs
             WHERE repo_full_name = ? AND status IN ('OK', 'SKIPPED_HEAD_UNCHANGED')",
            params![repo],
            |r| r.get(0),
        )?;
        if n > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Grading v4: the project quality axis sees only **high-level** architecture
/// — `layer_dependency` breaches (wrong package layering, a team-level design
/// decision). Every per-file AST rule (`FINDVIEWBYID_USAGE`,
/// `FRAGMENT_BYPASSES_VIEWMODEL`, …) is charged to the offending student via
/// the `*_HOTSPOT` artifact flags, not to the team.
pub fn architecture_counts(conn: &Connection, project_id: i64) -> rusqlite::Result<(f64, f64)> {
    let repos = project_repos(conn, project_id)?;
    let mut crit = 0.0;
    let mut warn = 0.0;
    for repo in &repos {
        let mut stmt = conn.prepare(
            "SELECT severity, COUNT(*) FROM architecture_violations
             WHERE repo_full_name = ? AND rule_kind = 'layer_dependency'
             GROUP BY severity",
        )?;
        let rows = stmt.query_map(params![repo], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (severity, n) = row?;
            match severity.to_ascii_uppercase().as_str() {
                "CRITICAL" | "ERROR" => crit += n as f64,
                "WARNING" => warn += n as f64,
                _ => {}
            }
        }
    }
    Ok((crit, warn))
}
