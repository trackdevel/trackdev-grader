//! Per-file LLM-response cache (T-P3.3).
//!
//! `architecture_llm_cache(file_sha, rubric_version, model_id, response_json,
//! evaluated_at)` — `response_json` is the raw model response for that
//! `(file, rubric, model)` triple. Look up before paying for a new API
//! call; persist on every fresh call.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

pub fn lookup(
    conn: &Connection,
    file_sha: &str,
    rubric_version: &str,
    model_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT response_json FROM architecture_llm_cache
         WHERE file_sha = ? AND rubric_version = ? AND model_id = ?",
        params![file_sha, rubric_version, model_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
}

pub fn insert(
    conn: &Connection,
    file_sha: &str,
    rubric_version: &str,
    model_id: &str,
    response_json: &str,
) -> rusqlite::Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO architecture_llm_cache
            (file_sha, rubric_version, model_id, response_json, evaluated_at)
         VALUES (?, ?, ?, ?, ?)",
        params![file_sha, rubric_version, model_id, response_json, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_grader_core::db::apply_schema;

    #[test]
    fn miss_then_hit_round_trips_response() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        assert!(lookup(&conn, "sha-a", "1:bh", "model-x").unwrap().is_none());
        insert(&conn, "sha-a", "1:bh", "model-x", "{\"violations\":[]}").unwrap();
        let got = lookup(&conn, "sha-a", "1:bh", "model-x").unwrap();
        assert_eq!(got.as_deref(), Some("{\"violations\":[]}"));
    }

    #[test]
    fn different_rubric_version_does_not_hit() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        insert(&conn, "sha-a", "1:bh", "model-x", "{\"violations\":[]}").unwrap();
        assert!(lookup(&conn, "sha-a", "2:bh", "model-x").unwrap().is_none());
        assert!(lookup(&conn, "sha-a", "1:other", "model-x").unwrap().is_none());
        assert!(lookup(&conn, "sha-a", "1:bh", "model-y").unwrap().is_none());
    }

    #[test]
    fn re_insert_on_same_key_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        insert(&conn, "sha-a", "1:bh", "model-x", "{\"v\":1}").unwrap();
        insert(&conn, "sha-a", "1:bh", "model-x", "{\"v\":2}").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM architecture_llm_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "INSERT OR REPLACE keeps a single row");
        let got = lookup(&conn, "sha-a", "1:bh", "model-x").unwrap();
        assert_eq!(got.as_deref(), Some("{\"v\":2}"));
    }
}
