//! T1 skeleton test. Verifies (1) the canonical schema creates the three
//! new `static_analysis_*` tables, and (2) the stub `scan_repo_to_db`
//! returns `Ok(0)` for an empty repo without touching the DB.

use rusqlite::Connection;
use sprint_grader_core::db::apply_schema;
use sprint_grader_static_analysis::{scan_repo_to_db, Rules};

#[test]
fn schema_creates_static_analysis_tables_and_scan_is_a_noop() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();

    let mut tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'static_analysis_%' \
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    tables.sort();
    assert_eq!(
        tables,
        vec![
            "static_analysis_finding_attribution".to_string(),
            "static_analysis_findings".to_string(),
            "static_analysis_runs".to_string(),
        ],
        "schema must create the three static-analysis tables"
    );

    // Index sanity-check.
    let indexes: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_sa_%' \
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        indexes,
        vec![
            "idx_sa_attr_sprint".to_string(),
            "idx_sa_findings_sprint".to_string(),
        ],
    );

    let tmp = tempfile::tempdir().unwrap();
    let rules = Rules::default();
    let n = scan_repo_to_db(&conn, tmp.path(), "udg-pds/empty", 1, &rules).unwrap();
    assert_eq!(n, 0, "T1 stub must be a no-op");

    // No rows must have been inserted.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM static_analysis_findings", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}
