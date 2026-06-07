//! Idempotent persistence of grade rows (`DELETE WHERE project_id` then insert).

use rusqlite::{params, Connection};

use crate::grade::{GradingResult, ProjectGradeRow, StudentGradeRow};

pub fn persist_project_grades(conn: &Connection, result: &GradingResult) -> rusqlite::Result<()> {
    let pid = result.project.project_id;
    delete_project_rows(conn, pid)?;

    conn.execute(
        "INSERT INTO project_final_grade
         (project_id, quality_grade, project_penalty, quality_penalized, ai_factor,
          final_grade, team_size, review_gate, ai_strength, weights_version, generated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            pid,
            result.project.quality_grade,
            result.project.project_penalty,
            result.project.quality_penalized,
            result.project.ai_factor,
            result.project.final_grade,
            result.project.team_size,
            result.project.review_gate,
            result.ai_strength,
            result.weights_version,
            result.generated_at,
        ],
    )?;

    for c in &result.project.components {
        conn.execute(
            "INSERT INTO project_component_score
             (project_id, component_key, raw_value, score_0_10, present)
             VALUES (?, ?, ?, ?, ?)",
            params![
                pid,
                c.key,
                c.raw_value,
                c.score_0_10,
                if c.present { 1 } else { 0 },
            ],
        )?;
    }

    for s in &result.students {
        persist_student_row(conn, s, &result.weights_version, &result.generated_at)?;
    }

    Ok(())
}

fn persist_student_row(
    conn: &Connection,
    s: &StudentGradeRow,
    weights_version: &str,
    generated_at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO student_final_grade
         (student_id, project_id, raw_points, effective_points, ai_keep_factor,
          contribution_ratio, base_grade, student_penalty, final_grade,
          review_gate, weights_version, generated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            s.student_id,
            s.project_id,
            s.raw_points,
            s.effective_points,
            s.ai_keep_factor,
            s.contribution_ratio,
            s.base_grade,
            s.student_penalty,
            s.final_grade,
            s.review_gate,
            weights_version,
            generated_at,
        ],
    )?;
    Ok(())
}

fn delete_project_rows(conn: &Connection, project_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM project_final_grade WHERE project_id = ?",
        params![project_id],
    )?;
    conn.execute(
        "DELETE FROM project_component_score WHERE project_id = ?",
        params![project_id],
    )?;
    conn.execute(
        "DELETE FROM student_final_grade WHERE project_id = ?",
        params![project_id],
    )?;
    conn.execute(
        "DELETE FROM student_component_score WHERE project_id = ?",
        params![project_id],
    )?;
    Ok(())
}

/// All `project_id` values with a row in `project_final_grade`, in stable order.
pub fn list_graded_project_ids(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT project_id FROM project_final_grade ORDER BY project_id")?;
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn load_persisted_project(
    conn: &Connection,
    project_id: i64,
) -> rusqlite::Result<Option<ProjectGradeRow>> {
    let row = conn.query_row(
        "SELECT quality_grade, project_penalty, quality_penalized, ai_factor,
                final_grade, team_size, review_gate
         FROM project_final_grade WHERE project_id = ?",
        params![project_id],
        |r| {
            Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        },
    );

    match row {
        Ok((qg, pp, qp, af, fg, ts, rg)) => {
            let name: String = conn
                .query_row(
                    "SELECT name FROM projects WHERE id = ?",
                    params![project_id],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| format!("project-{project_id}"));
            let mut stmt = conn.prepare(
                "SELECT component_key, raw_value, score_0_10, present
                 FROM project_component_score WHERE project_id = ?",
            )?;
            let comps = stmt.query_map(params![project_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<f64>>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            let mut components = Vec::new();
            for c in comps {
                let (key, raw, score, present) = c?;
                components.push(crate::grade::ComponentScore {
                    key: match key.as_str() {
                        "documentation" => "documentation",
                        "code_quality" => "code_quality",
                        "survival" => "survival",
                        "architecture" => "architecture",
                        _ => "documentation",
                    },
                    raw_value: raw,
                    score_0_10: score,
                    present: present != 0,
                });
            }
            Ok(Some(ProjectGradeRow {
                project_id,
                name,
                components,
                quality_grade: qg,
                project_penalty: pp,
                quality_penalized: qp,
                ai_factor: af,
                final_grade: fg,
                team_size: ts,
                review_gate: rg,
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}
