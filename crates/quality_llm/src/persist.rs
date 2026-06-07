//! Idempotent `llm_quality_flag` persistence (`DELETE WHERE project_id` then insert).

use rusqlite::{params, Connection};

use crate::flag::LlmQualityFlagRow;

pub fn delete_project_flags(conn: &Connection, project_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM llm_quality_flag WHERE project_id = ?",
        params![project_id],
    )?;
    Ok(())
}

pub fn insert_flag(conn: &Connection, row: &LlmQualityFlagRow) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO llm_quality_flag
         (project_id, student_id, sprint_id, scope, target_ref, category, severity,
          summary, detail, backend, model_id, prompt_version, generated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            row.project_id,
            row.student_id,
            row.sprint_id,
            row.scope,
            row.target_ref,
            row.category,
            row.severity,
            row.summary,
            row.detail,
            row.backend,
            row.model_id,
            row.prompt_version,
            row.generated_at,
        ],
    )?;
    Ok(())
}

pub fn persist_project_flags(
    conn: &Connection,
    project_id: i64,
    rows: &[LlmQualityFlagRow],
) -> rusqlite::Result<()> {
    delete_project_flags(conn, project_id)?;
    for row in rows {
        insert_flag(conn, row)?;
    }
    Ok(())
}

/// Whether a file-tier row already exists for resume (same target + prompt + backend + model).
pub fn file_flag_exists(
    conn: &Connection,
    project_id: i64,
    target_ref: &str,
    backend: &str,
    model_id: &str,
    prompt_version: &str,
) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM llm_quality_flag
         WHERE project_id = ? AND scope = 'file' AND target_ref = ?
           AND backend = ? AND model_id = ? AND prompt_version = ?",
        params![project_id, target_ref, backend, model_id, prompt_version],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// All `project_id` values with at least one `llm_quality_flag` row.
pub fn list_flagged_project_ids(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT project_id FROM llm_quality_flag ORDER BY project_id")?;
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Every advisory flag row (merged workbook export).
pub fn list_all_flags(conn: &Connection) -> rusqlite::Result<Vec<LlmQualityFlagRow>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, student_id, sprint_id, scope, target_ref, category, severity,
                summary, detail, backend, model_id, prompt_version, generated_at
         FROM llm_quality_flag
         ORDER BY project_id, scope, target_ref, id",
    )?;
    let rows = stmt.query_map([], map_flag_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn list_flags_for_projects(
    conn: &Connection,
    project_ids: &[i64],
) -> rusqlite::Result<Vec<LlmQualityFlagRow>> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = project_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT project_id, student_id, sprint_id, scope, target_ref, category, severity,
                summary, detail, backend, model_id, prompt_version, generated_at
         FROM llm_quality_flag
         WHERE project_id IN ({placeholders})
         ORDER BY project_id, scope, target_ref, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<rusqlite::types::Value> = project_ids.iter().map(|id| (*id).into()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(params.drain(..)), map_flag_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn map_flag_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LlmQualityFlagRow> {
    Ok(LlmQualityFlagRow {
        project_id: row.get(0)?,
        student_id: row.get(1)?,
        sprint_id: row.get(2)?,
        scope: row.get(3)?,
        target_ref: row.get(4)?,
        category: row.get(5)?,
        severity: row.get(6)?,
        summary: row.get(7)?,
        detail: row.get(8)?,
        backend: row.get(9)?,
        model_id: row.get(10)?,
        prompt_version: row.get(11)?,
        generated_at: row.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(project_id: i64, summary: &str) -> LlmQualityFlagRow {
        LlmQualityFlagRow {
            project_id,
            student_id: None,
            sprint_id: None,
            scope: "file".into(),
            target_ref: Some(format!("org/r:{summary}")),
            category: "other".into(),
            severity: "INFO".into(),
            summary: summary.into(),
            detail: None,
            backend: "claude-cli".into(),
            model_id: "m".into(),
            prompt_version: "1".into(),
            generated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn subset_delete_leaves_other_projects() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        persist_project_flags(&conn, 1, &[row(1, "a")]).unwrap();
        persist_project_flags(&conn, 2, &[row(2, "b")]).unwrap();
        delete_project_flags(&conn, 2).unwrap();
        assert_eq!(list_flagged_project_ids(&conn).unwrap(), vec![1]);
        assert_eq!(list_all_flags(&conn).unwrap().len(), 1);
    }
}
