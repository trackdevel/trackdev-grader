//! Phase 3 acceptance: the JS engine reproduces the Rust-computed grades on a
//! golden snapshot. Gated behind the `node-tests` feature so the default
//! `cargo test --workspace` needs no Node toolchain.
//!
//! Run with: `cargo test -p sprint-grader-grading-html --features node-tests`.

use rusqlite::params;
use sprint_grader_core::Database;
use sprint_grader_grading_html::build_snapshot_bytes;
use sprint_grader_grading_xlsx::{load_workbook_data, GradingConfig, PenaltyConfig};
use tempfile::{tempdir, NamedTempFile};

const PROJECT_ID: i64 = 1;
const SPRINT_ID: i64 = 10;

fn make_db() -> Database {
    let dir = tempdir().expect("tempdir");
    let db = Database::open(&dir.path().join("grading.db")).expect("open db");
    db.create_tables().expect("schema");
    std::mem::forget(dir);
    db
}

/// A non-trivial worked example exercising: the both-present declared keep gate
/// (task 3 declares a model but no level → undeclared keep), the documentation
/// and (live) architecture axes, AI modulation, and a CRITICAL student penalty.
fn seed_rich_example(db: &Database) {
    let conn = &db.conn;
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, 'team-01', 'Team 01')",
        params![PROJECT_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (?, ?, 'S1', '2026-01-01', '2026-01-15')",
        params![SPRINT_ID, PROJECT_ID],
    )
    .unwrap();
    for (id, name) in [("alice", "Alice"), ("bob", "Bob")] {
        conn.execute(
            "INSERT INTO students (id, username, github_login, full_name, team_project_id)
             VALUES (?, ?, ?, ?, ?)",
            params![id, id, id, name, PROJECT_ID],
        )
        .unwrap();
    }
    // Tasks: alice Cap/A (keep 1.0), bob GPT-5.5/E (keep 0.2), alice Cursor/—
    // (declared but no level → falls to the undeclared keep).
    let task = |id: i64, key: &str, pts: i64, who: &str| {
        conn.execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
             VALUES (?, ?, ?, 'TASK', 'DONE', ?, ?, ?)",
            params![id, key, key, pts, who, SPRINT_ID],
        )
        .unwrap();
    };
    let ai = |task_id: i64, model: &str, level: Option<&str>| {
        conn.execute(
            "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
             VALUES (?, ?, ?, 1, '2026-01-01')",
            params![task_id, model, level],
        )
        .unwrap();
    };
    task(1, "T-1", 10, "alice");
    ai(1, "Cap", Some("A"));
    task(2, "T-2", 10, "bob");
    ai(2, "GPT-5.5", Some("E"));
    task(3, "T-3", 5, "alice");
    ai(3, "Cursor", None);

    // A merged PR (with a repo) authored by alice makes the architecture axis
    // present (zero violations → density 0 → score 10) and carries the doc score.
    conn.execute(
        "INSERT INTO pull_requests (id, pr_number, repo_full_name, url, title, state, merged)
         VALUES ('pr1', 1, 'org/repo', 'http://x', 't', 'MERGED', 1)",
        [],
    )
    .unwrap();
    // `pr_authors` is a VIEW over task↔PR links; author alice by linking pr1 to
    // her task 1.
    conn.execute(
        "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (1, 'pr1')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pr_doc_evaluation (pr_id, sprint_id, total_doc_score) VALUES ('pr1', ?, 4.0)",
        params![SPRINT_ID],
    )
    .unwrap();

    // CRITICAL sprint flag for bob → student penalty (0.75 under defaults).
    conn.execute(
        "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
         VALUES ('bob', ?, 'SOME_FLAG', 'CRITICAL', NULL)",
        params![SPRINT_ID],
    )
    .unwrap();
}

fn snapshot_for_mode(mode: &str) -> Vec<u8> {
    let db = make_db();
    seed_rich_example(&db);
    let cfg = GradingConfig {
        penalty: PenaltyConfig {
            mode: mode.to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    build_snapshot_bytes(&data, &cfg).unwrap()
}

fn run_harness(bytes: &[u8]) -> serde_json::Value {
    let tf = NamedTempFile::new().unwrap();
    std::fs::write(tf.path(), bytes).unwrap();
    let script = format!("{}/tests/parity.mjs", env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("node")
        .arg(&script)
        .arg(tf.path())
        .output()
        .expect("run node parity harness (is `node` on PATH?)");
    assert!(
        out.status.success(),
        "parity harness exited non-zero\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice(&out.stdout).expect("parse parity JSON from harness stdout")
}

/// The baked project final grade — used to assert the worked example is
/// non-trivial so parity can't silently pass on an all-zero snapshot.
fn reference_project_final(bytes: &[u8]) -> f64 {
    let tf = NamedTempFile::new().unwrap();
    std::fs::write(tf.path(), bytes).unwrap();
    let conn = rusqlite::Connection::open(tf.path()).unwrap();
    conn.query_row(
        "SELECT final_grade FROM reference_project WHERE project_id = ?",
        params![PROJECT_ID],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
#[cfg_attr(not(feature = "node-tests"), ignore)]
fn js_engine_matches_rust_default_subtractive() {
    let bytes = snapshot_for_mode("subtractive");
    assert!(
        reference_project_final(&bytes) > 1.0,
        "worked example should be non-trivial (axes present → q > 0)"
    );
    let res = run_harness(&bytes);
    assert_eq!(
        res["keepOk"],
        serde_json::json!(true),
        "keep unit checks failed"
    );
    assert_eq!(
        res["ok"],
        serde_json::json!(true),
        "parity offenders: {}",
        res["offenders"]
    );
    let max_delta = res["maxDelta"].as_f64().unwrap();
    assert!(max_delta <= 0.005, "maxDelta {max_delta} exceeds 0.5*10^-2");
}

#[test]
#[cfg_attr(not(feature = "node-tests"), ignore)]
fn js_engine_matches_rust_penalty_mode_off() {
    // Non-subtractive mode zeroes both penalties on the Rust AND JS sides; the
    // grades must still agree (exercises the penalty-mode gate).
    let res = run_harness(&snapshot_for_mode("off"));
    assert_eq!(
        res["ok"],
        serde_json::json!(true),
        "parity offenders: {}",
        res["offenders"]
    );
    assert!(res["maxDelta"].as_f64().unwrap() <= 0.005);
}
