//! Per-sprint curriculum snapshots (T-P2.5).
//!
//! `curriculum_concepts` is the live view of "what the slides teach right
//! now" — instructor edits to the LaTeX deck propagate immediately. Without
//! a snapshot, that means re-running the pipeline against a past sprint
//! would silently re-grade it under the *current* curriculum, which is a
//! teaching-credibility problem (Sprint 2's report shouldn't be allowed to
//! regress when Sprint 3's slides land).
//!
//! `freeze_curriculum_for_sprint` writes a per-sprint copy of the concepts
//! that were considered "taught by" that sprint. Subsequent calls for the
//! same sprint are no-ops — the snapshot is treated as immutable.
//!
//! Reads are routed through [`get_allowed_concepts_with_snapshot`], which
//! prefers snapshot rows when present and falls back to the live table
//! otherwise. The current/active sprint typically has no snapshot yet, so
//! it transparently uses the live curriculum until the instructor freezes.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection};
use tracing::info;

/// Returns true if a snapshot row already exists for this sprint.
pub fn snapshot_exists(conn: &Connection, sprint_id: i64) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM curriculum_concepts_snapshot WHERE sprint_id = ?",
        [sprint_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Snapshot every `curriculum_concepts` row that the given sprint should
/// see (i.e., `sprint_taught IS NULL OR sprint_taught <= sprint_number`)
/// into `curriculum_concepts_snapshot`. Idempotent: if the snapshot for
/// `sprint_id` already exists, returns 0 without touching the DB.
///
/// Returns the number of rows written.
pub fn freeze_curriculum_for_sprint(
    conn: &Connection,
    sprint_id: i64,
    sprint_number: i64,
) -> rusqlite::Result<usize> {
    if snapshot_exists(conn, sprint_id)? {
        info!(
            sprint_id,
            "curriculum: snapshot already exists, leaving immutable"
        );
        return Ok(0);
    }
    let n = conn.execute(
        "INSERT INTO curriculum_concepts_snapshot
            (sprint_id, category, value, source_file, sprint_taught)
         SELECT ?, category, value, source_file, sprint_taught
         FROM curriculum_concepts
         WHERE sprint_taught IS NULL OR sprint_taught <= ?",
        params![sprint_id, sprint_number],
    )?;
    info!(
        sprint_id,
        sprint_number,
        rows = n,
        "curriculum: snapshot written"
    );
    Ok(n)
}

/// Read the curriculum that applies to one sprint. Snapshot wins; live is
/// the fallback. Used by `ai_detect::scan_repo_curriculum` so past sprints
/// are graded against frozen concepts and the active sprint sees the live
/// table until the instructor freezes it.
pub fn get_allowed_concepts_with_snapshot(
    conn: &Connection,
    sprint_id: i64,
    sprint_number: i64,
) -> rusqlite::Result<HashMap<String, HashSet<String>>> {
    if snapshot_exists(conn, sprint_id)? {
        let mut stmt = conn.prepare(
            "SELECT category, value FROM curriculum_concepts_snapshot WHERE sprint_id = ?",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([sprint_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut map: HashMap<String, HashSet<String>> = HashMap::new();
        for (cat, val) in rows {
            map.entry(cat).or_default().insert(val);
        }
        return Ok(map);
    }
    crate::latex_parser::get_allowed_concepts(conn, sprint_number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        conn
    }

    fn seed_concepts(conn: &Connection, rows: &[(&str, &str, Option<i64>)]) {
        for (cat, val, taught) in rows {
            conn.execute(
                "INSERT OR IGNORE INTO curriculum_concepts
                 (category, value, source_file, sprint_taught) VALUES (?, ?, 'fake.tex', ?)",
                params![cat, val, taught],
            )
            .unwrap();
        }
    }

    #[test]
    fn freeze_writes_only_concepts_taught_up_to_sprint_number() {
        let conn = mk_db();
        seed_concepts(
            &conn,
            &[
                ("import", "java.util.List", Some(1)),
                ("annotation", "@Override", Some(2)),
                ("annotation", "@Service", Some(3)),
                ("import", "java.lang.Math", None), // ambient
            ],
        );
        let n = freeze_curriculum_for_sprint(&conn, 100, 2).unwrap();
        // Three rows: List (s=1), Override (s=2), Math (NULL). Service is excluded (s=3).
        assert_eq!(n, 3);
        let allowed = get_allowed_concepts_with_snapshot(&conn, 100, 99).unwrap();
        assert!(allowed["import"].contains("java.util.List"));
        assert!(allowed["annotation"].contains("@Override"));
        assert!(allowed["import"].contains("java.lang.Math"));
        assert!(!allowed
            .get("annotation")
            .is_some_and(|s| s.contains("@Service")));
    }

    #[test]
    fn second_freeze_is_a_noop() {
        let conn = mk_db();
        seed_concepts(&conn, &[("annotation", "@Override", Some(1))]);
        let first = freeze_curriculum_for_sprint(&conn, 50, 1).unwrap();
        // Mutate the live table after the first freeze — second freeze must
        // not pick up the change.
        seed_concepts(&conn, &[("annotation", "@Service", Some(1))]);
        let second = freeze_curriculum_for_sprint(&conn, 50, 1).unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 0);
        let allowed = get_allowed_concepts_with_snapshot(&conn, 50, 1).unwrap();
        assert!(allowed["annotation"].contains("@Override"));
        assert!(!allowed["annotation"].contains("@Service"));
    }

    #[test]
    fn read_falls_back_to_live_when_no_snapshot() {
        let conn = mk_db();
        seed_concepts(&conn, &[("annotation", "@Override", Some(1))]);
        // No snapshot for sprint_id=42 → reads live table.
        let allowed = get_allowed_concepts_with_snapshot(&conn, 42, 1).unwrap();
        assert!(allowed["annotation"].contains("@Override"));
    }

    #[test]
    fn snapshot_isolates_sprints_from_each_other() {
        let conn = mk_db();
        seed_concepts(&conn, &[("annotation", "@Override", Some(1))]);
        freeze_curriculum_for_sprint(&conn, 10, 1).unwrap();
        seed_concepts(&conn, &[("annotation", "@Service", Some(2))]);
        freeze_curriculum_for_sprint(&conn, 11, 2).unwrap();

        let s1 = get_allowed_concepts_with_snapshot(&conn, 10, 1).unwrap();
        let s2 = get_allowed_concepts_with_snapshot(&conn, 11, 2).unwrap();
        assert!(s1["annotation"].contains("@Override"));
        assert!(!s1["annotation"].contains("@Service"));
        assert!(s2["annotation"].contains("@Override"));
        assert!(s2["annotation"].contains("@Service"));
    }
}
