//! Dual-run DB comparison — compares two `grading.db` SQLite files table by
//! table by checksumming every row (ordered by `PRIMARY KEY`, falling back
//! to all columns when none is declared).
//!
//! Replaces the earlier `tools/diff_db.py` one-file Python helper. Wired as
//! the `sprint-grader diff-db` CLI subcommand and reused by the integration
//! test under `tests/parity_harness.rs`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{types::Value, Connection};
use sha2::{Digest, Sha256};

/// Tables that hold only *derived* pipeline output. The dual-run harness
/// typically targets these — `--derived-only` on the CLI.
pub const DERIVED_TABLES: &[&str] = &[
    "pr_line_metrics",
    "pr_survival",
    "pr_behavioral_signals",
    "pr_ai_probability",
    "pr_doc_evaluation",
    "pr_compilation",
    "pr_submission_tiers",
    "pr_regularity",
    "pr_workflow_metrics",
    "fingerprints",
    "cosmetic_rewrites",
    "cross_team_matches",
    "student_sprint_survival",
    "student_sprint_metrics",
    "student_sprint_contribution",
    "student_sprint_quality",
    "student_sprint_regularity",
    "student_sprint_temporal",
    "student_sprint_ai_usage",
    "student_trajectory",
    "student_style_baseline",
    "student_text_profile",
    "flags",
    "method_metrics",
    "satd_items",
    "task_similarity_groups",
    "task_group_members",
    "task_description_evaluation",
    "team_sprint_inequality",
    "team_sprint_collaboration",
    "sprint_planning_quality",
    "compilation_failure_summary",
    "curriculum_concepts",
    "curriculum_violations",
    "file_style_features",
    "file_ai_probability",
    "text_consistency_scores",
    "code_practices_evaluation",
];

/// Collection-stage tables that populate from TrackDev + GitHub. Compared
/// alongside derived tables on a full dual-run.
pub const COLLECTION_TABLES: &[&str] = &[
    "projects",
    "students",
    "github_users",
    "sprints",
    "tasks",
    "task_pull_requests",
    "pull_requests",
    "pr_commits",
    "pr_reviews",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableStatus {
    Ok,
    CountDiffers,
    ChecksumDiffers,
    OnlyA,
    OnlyB,
}

impl TableStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, TableStatus::Ok)
    }
    pub fn label(&self) -> &'static str {
        match self {
            TableStatus::Ok => "OK",
            TableStatus::CountDiffers => "COUNT",
            TableStatus::ChecksumDiffers => "CHECKSUM",
            TableStatus::OnlyA => "ONLY-A",
            TableStatus::OnlyB => "ONLY-B",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TableReport {
    pub table: String,
    pub count_a: Option<usize>,
    pub count_b: Option<usize>,
    pub status: TableStatus,
    /// Row-level diffs, populated when `DiffOptions::dump_diffs` is set and
    /// the status is `ChecksumDiffers`.
    pub row_diffs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    /// Optional table whitelist. Empty = use `DERIVED_TABLES + COLLECTION_TABLES`.
    pub tables: Vec<String>,
    /// Skip collection-stage tables (`projects`, `students`, ...).
    pub derived_only: bool,
    /// Columns to exclude from the checksum keyed by table name.
    pub ignore_cols: HashMap<String, BTreeSet<String>>,
    /// Emit up to this many row-level diffs per mismatched table.
    pub dump_diffs: bool,
    pub row_limit: usize,
    /// Float tolerance for row-level dump (checksum stays byte-exact).
    pub float_tol: f64,
}

impl DiffOptions {
    pub fn with_defaults() -> Self {
        Self {
            row_limit: 10,
            ..Default::default()
        }
    }
}

fn list_tables(conn: &Connection) -> rusqlite::Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    Ok(names.into_iter().collect())
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(`{}`)", table))?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    Ok(rows)
}

/// Return the PK columns in order. Falls back to *all* columns when the
/// table has no declared primary key — matches the Python behaviour so
/// tables without a PK still produce a deterministic ordering.
fn pk_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(`{}`)", table))?;
    let info: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(5)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut pk: Vec<(String, i64)> = info.iter().filter(|(_, p)| *p > 0).cloned().collect();
    if pk.is_empty() {
        return Ok(info.into_iter().map(|(n, _)| n).collect());
    }
    pk.sort_by_key(|(_, p)| *p);
    Ok(pk.into_iter().map(|(n, _)| n).collect())
}

/// Canonical cell repr used for both checksumming and row-level diffs.
/// Integer-valued doubles render as e.g. `1.0` (not `1`) to preserve
/// distinction from INTEGER 1.
fn cell_repr(v: &Value) -> String {
    match v {
        Value::Null => "\0NULL".into(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => {
            if f.is_nan() {
                "\0NAN".into()
            } else if f.is_infinite() {
                if *f > 0.0 {
                    "\0+INF".into()
                } else {
                    "\0-INF".into()
                }
            } else if *f == f.trunc() && f.abs() < 1e16 {
                format!("{:.1}", f)
            } else {
                format!("{}", f)
            }
        }
        Value::Text(s) => s.clone(),
        Value::Blob(b) => hex_encode(b),
    }
}

fn hex_encode(b: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0f) as usize] as char);
    }
    s
}

fn fetch_rows(
    conn: &Connection,
    table: &str,
    pk_cols: &[String],
) -> rusqlite::Result<Vec<Vec<Value>>> {
    let order = if pk_cols.is_empty() {
        "ROWID".to_string()
    } else {
        pk_cols
            .iter()
            .map(|c| format!("`{}`", c))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let sql = format!("SELECT * FROM `{}` ORDER BY {}", table, order);
    let mut stmt = conn.prepare(&sql)?;
    let col_count = stmt.column_count();
    let rows: Vec<Vec<Value>> = stmt
        .query_map([], |r| {
            (0..col_count).map(|i| r.get::<_, Value>(i)).collect()
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    Ok(rows)
}

/// Checksum a single table — returns `(row_count, hex_sha256)`.
pub fn checksum_table(
    conn: &Connection,
    table: &str,
    ignore_cols: &BTreeSet<String>,
) -> rusqlite::Result<(usize, String)> {
    let col_names = columns(conn, table)?;
    let pk_cols = pk_columns(conn, table)?;
    let hashed_idx: Vec<usize> = col_names
        .iter()
        .enumerate()
        .filter(|(_, n)| !ignore_cols.contains(n.as_str()))
        .map(|(i, _)| i)
        .collect();
    let rows = fetch_rows(conn, table, &pk_cols)?;
    let mut hasher = Sha256::new();
    for row in &rows {
        for i in &hashed_idx {
            hasher.update(cell_repr(&row[*i]).as_bytes());
            hasher.update(b"\x1f");
        }
    }
    Ok((rows.len(), format!("{:x}", hasher.finalize())))
}

fn row_key(row: &[Value], pk_cols: &[String], col_names: &[String]) -> Vec<String> {
    if pk_cols.is_empty() {
        row.iter().map(cell_repr).collect()
    } else {
        pk_cols
            .iter()
            .map(|pk| {
                col_names
                    .iter()
                    .position(|c| c == pk)
                    .map(|idx| cell_repr(&row[idx]))
                    .unwrap_or_else(|| "\0NULL".into())
            })
            .collect()
    }
}

fn floats_within(a: &Value, b: &Value, tol: f64) -> bool {
    if tol <= 0.0 {
        return false;
    }
    if let (Value::Real(x), Value::Real(y)) = (a, b) {
        let diff = (x - y).abs();
        diff <= tol || diff <= tol * x.abs().max(y.abs())
    } else {
        false
    }
}

/// Emit up to `limit` per-row diff strings for a mismatched table.
pub fn row_level_diffs(
    conn_a: &Connection,
    conn_b: &Connection,
    table: &str,
    ignore_cols: &BTreeSet<String>,
    limit: usize,
    float_tol: f64,
) -> rusqlite::Result<Vec<String>> {
    let col_names = columns(conn_a, table)?;
    let pk_cols = pk_columns(conn_a, table)?;
    let rows_a = fetch_rows(conn_a, table, &pk_cols)?;
    let rows_b = fetch_rows(conn_b, table, &pk_cols)?;

    let map_a: BTreeMap<Vec<String>, &Vec<Value>> = rows_a
        .iter()
        .map(|r| (row_key(r, &pk_cols, &col_names), r))
        .collect();
    let map_b: BTreeMap<Vec<String>, &Vec<Value>> = rows_b
        .iter()
        .map(|r| (row_key(r, &pk_cols, &col_names), r))
        .collect();

    let mut out: Vec<String> = Vec::new();
    for (key, row_a) in &map_a {
        if out.len() >= limit {
            return Ok(out);
        }
        let Some(row_b) = map_b.get(key) else {
            out.push(format!("  ONLY-A pk={:?}", key));
            continue;
        };
        let mut diffs: Vec<String> = Vec::new();
        for (i, col) in col_names.iter().enumerate() {
            if ignore_cols.contains(col) {
                continue;
            }
            if row_a[i] == row_b[i] {
                continue;
            }
            if floats_within(&row_a[i], &row_b[i], float_tol) {
                continue;
            }
            diffs.push(format!(
                "{}={} vs {}",
                col,
                cell_repr(&row_a[i]),
                cell_repr(&row_b[i])
            ));
        }
        if !diffs.is_empty() {
            out.push(format!("  DIFF pk={:?}: {}", key, diffs.join("; ")));
        }
    }
    for key in map_b.keys() {
        if out.len() >= limit {
            return Ok(out);
        }
        if !map_a.contains_key(key) {
            out.push(format!("  ONLY-B pk={:?}", key));
        }
    }
    Ok(out)
}

/// Compare two DBs. Returns one `TableReport` per checked table.
pub fn diff_dbs(db_a: &Path, db_b: &Path, opts: &DiffOptions) -> Result<Vec<TableReport>> {
    let conn_a =
        Connection::open(db_a).with_context(|| format!("opening DB A at {}", db_a.display()))?;
    let conn_b =
        Connection::open(db_b).with_context(|| format!("opening DB B at {}", db_b.display()))?;
    let tables_a = list_tables(&conn_a)?;
    let tables_b = list_tables(&conn_b)?;

    let targets: Vec<String> = if !opts.tables.is_empty() {
        opts.tables.clone()
    } else if opts.derived_only {
        DERIVED_TABLES.iter().map(|s| (*s).to_string()).collect()
    } else {
        DERIVED_TABLES
            .iter()
            .chain(COLLECTION_TABLES.iter())
            .map(|s| (*s).to_string())
            .collect()
    };

    let empty_ignore = BTreeSet::new();
    let mut reports: Vec<TableReport> = Vec::new();
    for table in &targets {
        let in_a = tables_a.contains(table);
        let in_b = tables_b.contains(table);
        if !in_a && !in_b {
            continue;
        }
        if in_a && !in_b {
            reports.push(TableReport {
                table: table.clone(),
                count_a: None,
                count_b: None,
                status: TableStatus::OnlyA,
                row_diffs: Vec::new(),
            });
            continue;
        }
        if !in_a && in_b {
            reports.push(TableReport {
                table: table.clone(),
                count_a: None,
                count_b: None,
                status: TableStatus::OnlyB,
                row_diffs: Vec::new(),
            });
            continue;
        }
        let ignore = opts.ignore_cols.get(table).unwrap_or(&empty_ignore);
        let (count_a, hash_a) = checksum_table(&conn_a, table, ignore)?;
        let (count_b, hash_b) = checksum_table(&conn_b, table, ignore)?;
        let status = if count_a != count_b {
            TableStatus::CountDiffers
        } else if hash_a != hash_b {
            TableStatus::ChecksumDiffers
        } else {
            TableStatus::Ok
        };
        let row_diffs = if opts.dump_diffs && status == TableStatus::ChecksumDiffers {
            row_level_diffs(
                &conn_a,
                &conn_b,
                table,
                ignore,
                opts.row_limit,
                opts.float_tol,
            )?
        } else {
            Vec::new()
        };
        reports.push(TableReport {
            table: table.clone(),
            count_a: Some(count_a),
            count_b: Some(count_b),
            status,
            row_diffs,
        });
    }
    Ok(reports)
}

/// Parse `--ignore-cols` CLI values of the form `table:col1,col2`.
pub fn parse_ignore_cols(items: &[String]) -> Result<HashMap<String, BTreeSet<String>>> {
    let mut out: HashMap<String, BTreeSet<String>> = HashMap::new();
    for item in items {
        let (table, cols) = item
            .split_once(':')
            .with_context(|| format!("--ignore-cols expects table:col, got {:?}", item))?;
        let entry = out.entry(table.to_string()).or_default();
        for c in cols.split(',') {
            let c = c.trim();
            if !c.is_empty() {
                entry.insert(c.to_string());
            }
        }
    }
    Ok(out)
}

/// Format a `Vec<TableReport>` as the stdout layout the Python script used.
/// Returns `(output, mismatch_count)`.
pub fn format_report(reports: &[TableReport]) -> (String, usize) {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "{:40} {:>8} {:>8}  STATUS", "TABLE", "A", "B");
    let mut mismatches = 0usize;
    for r in reports {
        let a = r
            .count_a
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        let b = r
            .count_b
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        let _ = writeln!(
            out,
            "{:40} {:>8} {:>8}  {}",
            r.table,
            a,
            b,
            r.status.label()
        );
        if !r.status.is_ok() {
            mismatches += 1;
            for line in &r.row_diffs {
                let _ = writeln!(out, "{}", line);
            }
        }
    }
    (out, mismatches)
}

/// Entry point for the CLI subcommand. Emits a human report to stdout and
/// returns the number of mismatches.
pub fn run_diff(db_a: &PathBuf, db_b: &PathBuf, opts: &DiffOptions) -> Result<usize> {
    let reports = diff_dbs(db_a, db_b, opts)?;
    let (body, mismatches) = format_report(&reports);
    println!("# Comparing {} ⇄ {}", db_a.display(), db_b.display());
    print!("{}", body);
    if mismatches == 0 {
        println!("\nAll checked tables match.");
    } else {
        println!("\n{} table(s) differ.", mismatches);
    }
    Ok(mismatches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mk_pair(seed_a: &str, seed_b: &str) -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a.db");
        let b = tmp.path().join("b.db");
        {
            let conn = Connection::open(&a).unwrap();
            conn.execute_batch(seed_a).unwrap();
        }
        {
            let conn = Connection::open(&b).unwrap();
            conn.execute_batch(seed_b).unwrap();
        }
        (tmp, a, b)
    }

    #[test]
    fn identical_dbs_produce_ok_for_every_table() {
        let seed = "CREATE TABLE flags (flag_id INTEGER PRIMARY KEY, student_id TEXT,
                      sprint_id INTEGER, flag_type TEXT, severity TEXT, details TEXT);
                    INSERT INTO flags VALUES (1, 'u1', 10, 'CRAMMING', 'WARNING', '{}');";
        let (_tmp, a, b) = mk_pair(seed, seed);
        let opts = DiffOptions {
            tables: vec!["flags".into()],
            ..DiffOptions::with_defaults()
        };
        let reports = diff_dbs(&a, &b, &opts).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].status.is_ok());
    }

    #[test]
    fn drifted_row_flags_checksum_status() {
        let seed_a = "CREATE TABLE flags (flag_id INTEGER PRIMARY KEY, student_id TEXT,
                        sprint_id INTEGER, flag_type TEXT, severity TEXT, details TEXT);
                      INSERT INTO flags VALUES (1, 'u1', 10, 'CRAMMING', 'WARNING', '{}');";
        let seed_b = "CREATE TABLE flags (flag_id INTEGER PRIMARY KEY, student_id TEXT,
                        sprint_id INTEGER, flag_type TEXT, severity TEXT, details TEXT);
                      INSERT INTO flags VALUES (1, 'u1', 10, 'CRAMMING', 'CRITICAL', '{}');";
        let (_tmp, a, b) = mk_pair(seed_a, seed_b);
        let opts = DiffOptions {
            tables: vec!["flags".into()],
            dump_diffs: true,
            row_limit: 5,
            ..DiffOptions::with_defaults()
        };
        let reports = diff_dbs(&a, &b, &opts).unwrap();
        assert_eq!(reports[0].status, TableStatus::ChecksumDiffers);
        // Row diff mentions the severity column
        assert!(reports[0]
            .row_diffs
            .iter()
            .any(|l| l.contains("severity=WARNING vs CRITICAL")));
    }

    #[test]
    fn count_mismatch_short_circuits_checksum() {
        let seed_a = "CREATE TABLE flags (flag_id INTEGER PRIMARY KEY, details TEXT);
                      INSERT INTO flags VALUES (1, '{}'), (2, '{}');";
        let seed_b = "CREATE TABLE flags (flag_id INTEGER PRIMARY KEY, details TEXT);
                      INSERT INTO flags VALUES (1, '{}');";
        let (_tmp, a, b) = mk_pair(seed_a, seed_b);
        let opts = DiffOptions {
            tables: vec!["flags".into()],
            ..DiffOptions::with_defaults()
        };
        let reports = diff_dbs(&a, &b, &opts).unwrap();
        assert_eq!(reports[0].status, TableStatus::CountDiffers);
    }

    #[test]
    fn only_a_and_only_b_detected() {
        let seed_a = "CREATE TABLE flags (id INTEGER PRIMARY KEY);";
        let seed_b = "CREATE TABLE other (id INTEGER PRIMARY KEY);";
        let (_tmp, a, b) = mk_pair(seed_a, seed_b);
        let opts = DiffOptions {
            tables: vec!["flags".into(), "other".into()],
            ..DiffOptions::with_defaults()
        };
        let reports = diff_dbs(&a, &b, &opts).unwrap();
        let flags_r = reports.iter().find(|r| r.table == "flags").unwrap();
        let other_r = reports.iter().find(|r| r.table == "other").unwrap();
        assert_eq!(flags_r.status, TableStatus::OnlyA);
        assert_eq!(other_r.status, TableStatus::OnlyB);
    }

    #[test]
    fn ignore_cols_masks_specific_difference() {
        let seed_a = "CREATE TABLE pr_line_metrics (pr_id TEXT, sprint_id INTEGER,
                        merge_sha TEXT, lat REAL, PRIMARY KEY (pr_id, sprint_id));
                      INSERT INTO pr_line_metrics VALUES ('p1', 10, 'sha-a', 42.0);";
        let seed_b = "CREATE TABLE pr_line_metrics (pr_id TEXT, sprint_id INTEGER,
                        merge_sha TEXT, lat REAL, PRIMARY KEY (pr_id, sprint_id));
                      INSERT INTO pr_line_metrics VALUES ('p1', 10, 'different-sha', 42.0);";
        let (_tmp, a, b) = mk_pair(seed_a, seed_b);
        let mut ignore = HashMap::new();
        let mut cols = BTreeSet::new();
        cols.insert("merge_sha".into());
        ignore.insert("pr_line_metrics".into(), cols);
        let opts = DiffOptions {
            tables: vec!["pr_line_metrics".into()],
            ignore_cols: ignore,
            ..DiffOptions::with_defaults()
        };
        let reports = diff_dbs(&a, &b, &opts).unwrap();
        assert_eq!(reports[0].status, TableStatus::Ok);
    }

    #[test]
    fn parse_ignore_cols_accepts_comma_list() {
        let parsed = parse_ignore_cols(&["pr_line_metrics:merge_sha,created_at".into()]).unwrap();
        let set = parsed.get("pr_line_metrics").unwrap();
        assert!(set.contains("merge_sha"));
        assert!(set.contains("created_at"));
    }

    #[test]
    fn parse_ignore_cols_rejects_missing_colon() {
        assert!(parse_ignore_cols(&["no_colon_here".into()]).is_err());
    }

    #[test]
    fn float_tolerance_suppresses_dump_diff_only() {
        let seed_a = "CREATE TABLE t (id INTEGER PRIMARY KEY, v REAL);
                      INSERT INTO t VALUES (1, 0.1);";
        let seed_b = "CREATE TABLE t (id INTEGER PRIMARY KEY, v REAL);
                      INSERT INTO t VALUES (1, 0.10000000001);";
        let (_tmp, a, b) = mk_pair(seed_a, seed_b);
        let opts = DiffOptions {
            tables: vec!["t".into()],
            dump_diffs: true,
            row_limit: 5,
            float_tol: 1e-9,
            ..DiffOptions::with_defaults()
        };
        let reports = diff_dbs(&a, &b, &opts).unwrap();
        // Checksum still differs (bit-exact).
        assert_eq!(reports[0].status, TableStatus::ChecksumDiffers);
        // But the row-level dump hides the within-tolerance difference.
        assert!(reports[0].row_diffs.is_empty());
    }
}
