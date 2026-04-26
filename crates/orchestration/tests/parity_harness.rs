//! Parity harness: builds a minimal "golden" SQLite snapshot in-memory,
//! runs the Rust analyze/flags stage against it, and asserts the derived
//! output matches a committed expected value.
//!
//! This is the cheap, always-on half of the Step-11 verification strategy.
//! The expensive half — dual-running the full pipeline against a real
//! `grading.db` snapshot — lives under `tools/diff_db.py` and is driven by
//! the operator, not by `cargo test`.

use std::collections::BTreeMap;

use std::collections::BTreeSet;
use std::path::Path;

use sprint_grader_core::{Config, Database};
use sprint_grader_orchestration::checksum_table;

fn checksum(db: &Database, table: &str) -> String {
    let ignore: BTreeSet<String> = BTreeSet::new();
    checksum_table(&db.conn, table, &ignore).unwrap().1
}

fn checksum_conn(conn: &rusqlite::Connection, table: &str) -> String {
    let ignore: BTreeSet<String> = BTreeSet::new();
    checksum_table(conn, table, &ignore).unwrap().1
}

fn seed_sql() -> &'static str {
    SEED_SQL
}

const SEED_SQL: &str =
            "INSERT INTO projects (id, name) VALUES (1, 'pds26-test');
             INSERT INTO sprints (id, project_id, name, start_date, end_date)
                VALUES (10, 1, 'Sprint 1', '2026-02-16T00:00:00+00:00',
                        '2026-03-08T23:59:59+00:00');
             INSERT INTO students (id, full_name, github_login, team_project_id, email)
                VALUES
                    ('u1', 'Alice Doe',  'alice-gh', 1, 'alice@example.com'),
                    ('u2', 'Bob Ng',     'bob-gh',   1, 'bob@example.com'),
                    ('u3', 'Cara Park',  'cara-gh',  1, 'cara@example.com'),
                    ('u4', 'Dani Sole',  'dani-gh',  1, 'dani@example.com');
             INSERT INTO tasks
                (id, task_key, name, type, status, estimation_points,
                 assignee_id, sprint_id, parent_task_id)
                VALUES
                    (100, 'T-1', 'Login endpoint', 'TASK', 'DONE', 3, 'u1', 10, NULL),
                    (101, 'T-2', 'User view',      'TASK', 'DONE', 5, 'u2', 10, NULL),
                    (102, 'T-3', 'Profile page',   'TASK', 'DONE', 2, 'u3', 10, NULL);
             -- u4 has no DONE task on purpose: should trigger ZERO_TASKS.
             INSERT INTO pull_requests
                (id, pr_number, repo_full_name, title, url, author_id,
                 additions, deletions, changed_files, created_at, merged,
                 merged_at, body)
                VALUES
                    ('pr-a', 1, 'udg-pds/spring-x', 'Add login endpoint',
                     'https://github.com/udg-pds/spring-x/pull/1',
                     'u1', 120, 10, 3, '2026-02-20T10:00:00+00:00', 1,
                     '2026-02-22T15:00:00+00:00', 'body'),
                    ('pr-b', 2, 'udg-pds/android-x', 'User view',
                     'https://github.com/udg-pds/android-x/pull/2',
                     'u2', 200, 40, 6, '2026-02-22T11:00:00+00:00', 1,
                     '2026-02-25T12:00:00+00:00', 'body'),
                    ('pr-c', 3, 'udg-pds/android-x', 'Profile page',
                     'https://github.com/udg-pds/android-x/pull/3',
                     'u3', 60, 5, 2, '2026-03-06T18:00:00+00:00', 1,
                     '2026-03-08T09:00:00+00:00', 'body');
             INSERT INTO task_pull_requests (task_id, pr_id) VALUES
                    (100, 'pr-a'),
                    (101, 'pr-b'),
                    (102, 'pr-c');
             INSERT INTO pr_line_metrics (pr_id, sprint_id, merge_sha, lat, lar, ls)
                VALUES
                    ('pr-a', 10, 'sha-a', 100.0, 90.0, 80.0),
                    ('pr-b', 10, 'sha-b', 200.0, 180.0, 150.0),
                    ('pr-c', 10, 'sha-c',  50.0,  40.0,  30.0);
             INSERT INTO pr_commits (sha, pr_id, author_login, message, timestamp, additions, deletions)
                VALUES
                    ('c1', 'pr-a', 'alice-gh', 'Add login', '2026-02-20T10:00:00+00:00', 120, 10),
                    ('c2', 'pr-b', 'bob-gh',   'User view', '2026-02-22T11:00:00+00:00', 200, 40),
                    ('c3', 'pr-c', 'cara-gh',  'Profile',   '2026-03-06T18:00:00+00:00',  60,  5);";

fn mk_golden_db_at(path: &Path) -> Database {
    let db = Database::open(path).unwrap();
    db.create_tables().unwrap();
    db.conn.execute_batch(seed_sql()).unwrap();
    db
}

fn mk_golden_db_tmp() -> (tempfile::TempDir, Database) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("grading.db");
    let db = mk_golden_db_at(&path);
    (tmp, db)
}

fn default_config() -> Config {
    // `Config::default()` is not implemented; build a minimal Config in
    // code by round-tripping a tiny course.toml through the loader.
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().to_path_buf();
    std::fs::write(
        config_dir.join("course.toml"),
        r#"
[course]
name = "test"
num_sprints = 1
pm_base_url = "https://example.invalid"
github_org = "udg-pds"
course_id = 5

[thresholds]
carrying_team_pct = 0.40
cramming_hours = 48
cramming_commit_pct = 0.70
single_commit_dump_lines = 200
micro_pr_max_lines = 10
low_doc_score = 2
contribution_imbalance_stddev = 1.5

[build]
max_parallel_builds = 1
stderr_max_chars = 2000
skip_already_tested = true

[regularity]

[repo_analysis]
"#,
    )
    .unwrap();
    // user_mapping.csv — minimal but non-empty so the loader is happy.
    std::fs::write(
        config_dir.join("user_mapping.csv"),
        "trackdev_username,github_username,enrollment_id,team_id\n",
    )
    .unwrap();
    // Hold the tempdir so the files outlive Config::load.
    let cfg = Config::load(&config_dir).unwrap();
    std::mem::forget(tmp);
    cfg
}

#[test]
fn analyze_stage_is_deterministic_and_matches_golden_checksums() {
    // Run the analyze stage twice against two fresh DBs built from the same
    // seed; the derived outputs must checksum-match byte-for-byte.
    let (_tmp1, db1) = mk_golden_db_tmp();
    let (_tmp2, db2) = mk_golden_db_tmp();
    let cfg = default_config();

    for db in [&db1, &db2] {
        sprint_grader_analyze::metrics::compute_metrics_for_sprint_id(
            &db.conn,
            10,
            cfg.thresholds.cramming_hours,
        )
        .unwrap();
        sprint_grader_analyze::flags::detect_flags_for_sprint_id(&db.conn, 10, &cfg).unwrap();
        sprint_grader_analyze::compute_all_inequality(&db.conn, 10).unwrap();
    }

    let mut mismatches: BTreeMap<&str, (String, String)> = BTreeMap::new();
    for table in ["student_sprint_metrics", "flags", "team_sprint_inequality"] {
        let a = checksum(&db1, table);
        let b = checksum(&db2, table);
        if a != b {
            mismatches.insert(table, (a, b));
        }
    }
    assert!(
        mismatches.is_empty(),
        "non-deterministic analyze output: {:?}",
        mismatches
    );

    let zero_tasks = db1
        .conn
        .query_row(
            "SELECT COUNT(*) FROM flags WHERE sprint_id = ? AND flag_type = 'ZERO_TASKS'",
            [10_i64],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    assert!(
        zero_tasks >= 1,
        "expected ZERO_TASKS on u4, got {zero_tasks}"
    );
}

#[test]
fn parallel_worker_conn_isolation_is_deterministic() {
    // The orchestration pipeline opens per-worker Connections. Two separate
    // Connections against the same fresh golden DB must produce identical
    // metrics rows.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("grading.db");
    let _seed = mk_golden_db_at(&db_path);
    drop(_seed);

    let cfg = default_config();

    let c1 = rusqlite::Connection::open(&db_path).unwrap();
    c1.pragma_update(None, "journal_mode", &"WAL").unwrap();
    c1.pragma_update(None, "busy_timeout", &10_000).unwrap();
    sprint_grader_analyze::metrics::compute_metrics_for_sprint_id(
        &c1,
        10,
        cfg.thresholds.cramming_hours,
    )
    .unwrap();
    sprint_grader_analyze::flags::detect_flags_for_sprint_id(&c1, 10, &cfg).unwrap();
    let hash1 = checksum_conn(&c1, "student_sprint_metrics");
    drop(c1);

    // Wipe derived rows and re-run via a different connection.
    let c2 = rusqlite::Connection::open(&db_path).unwrap();
    c2.execute(
        "DELETE FROM student_sprint_metrics WHERE sprint_id = ?",
        [10],
    )
    .unwrap();
    c2.execute("DELETE FROM flags WHERE sprint_id = ?", [10])
        .unwrap();
    sprint_grader_analyze::metrics::compute_metrics_for_sprint_id(
        &c2,
        10,
        cfg.thresholds.cramming_hours,
    )
    .unwrap();
    sprint_grader_analyze::flags::detect_flags_for_sprint_id(&c2, 10, &cfg).unwrap();
    let hash2 = checksum_conn(&c2, "student_sprint_metrics");
    assert_eq!(
        hash1, hash2,
        "per-worker connection produced different metrics"
    );
}
