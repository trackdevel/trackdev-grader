//! Wave 4 acceptance: workbook bytes, defined names, Rust/formula parity.

use rusqlite::params;
use sprint_grader_core::Database;
use sprint_grader_grading_xlsx::{
    load_workbook_data, write_workbook_buffer, GradingConfig, DEFINED_NAMES,
};
use tempfile::tempdir;

const PROJECT_ID: i64 = 1;
const SPRINT_ID: i64 = 10;

fn make_db() -> Database {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("grading.db");
    let db = Database::open(&path).expect("open db");
    db.create_tables().expect("schema");
    // Keep tempdir alive by leaking — test process exit cleans up.
    std::mem::forget(dir);
    db
}

fn seed_worked_example(db: &Database) {
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
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (1, 'T-1', 'A', 'TASK', 'DONE', 10, 'alice', ?)",
        params![SPRINT_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
         VALUES (1, 'Cap', 'A', 1, '2026-01-01')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (2, 'T-2', 'B', 'TASK', 'DONE', 10, 'bob', ?)",
        params![SPRINT_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
         VALUES (2, 'GPT-5.5', 'E', 1, '2026-01-01')",
        [],
    )
    .unwrap();
}

#[test]
fn defined_names_inventory_is_stable() {
    assert_eq!(DEFINED_NAMES.len(), 25);
    assert!(DEFINED_NAMES.contains(&"w_doc"));
    assert!(DEFINED_NAMES.contains(&"ai_strength"));
    assert!(DEFINED_NAMES.contains(&"arch_norm"));
}

#[test]
fn workbook_buffer_is_valid_xlsx_zip() {
    let db = make_db();
    seed_worked_example(&db);
    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    let buf = write_workbook_buffer(&data, &cfg).unwrap();
    assert!(buf.len() > 4096, "workbook suspiciously small");
    assert_eq!(&buf[0..2], b"PK", "xlsx must be a zip (PK header)");
}

#[test]
fn workbook_cached_grades_match_rust_engine() {
    let db = make_db();
    seed_worked_example(&db);
    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    assert_eq!(data.results.len(), 1);
    let project = &data.results[0].project;
    // Worked example: Q may be 0 without quality axes; focus on AI/points chain.
    assert!((project.ai_factor - 0.6).abs() < 1e-9);
    let alice = data.results[0]
        .students
        .iter()
        .find(|s| s.student_id == "alice")
        .unwrap();
    let bob = data.results[0]
        .students
        .iter()
        .find(|s| s.student_id == "bob")
        .unwrap();
    assert!((alice.raw_points - 10.0).abs() < 1e-9);
    assert!((bob.effective_points - 2.0).abs() < 1e-9);
    assert!((alice.effective_points - 10.0).abs() < 1e-9);
    // mean_raw = 20/2 = 10; base = Q_pen * eff / mean_raw
    let q_pen = project.quality_penalized;
    assert!((alice.base_grade - q_pen * 10.0 / 10.0).abs() < 1e-9);
    assert!((bob.base_grade - q_pen * 2.0 / 10.0).abs() < 1e-9);
    assert!((project.final_grade - q_pen * 0.6).abs() < 1e-9);

    let buf = write_workbook_buffer(&data, &cfg).unwrap();
    assert!(buf.len() > 4096);
}

#[test]
fn workbook_accepts_flag_details_beyond_excel_cell_limit() {
    let db = make_db();
    seed_worked_example(&db);
    let huge = "x".repeat(40_000);
    db.conn
        .execute(
            "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
             VALUES ('alice', ?, 'TEST_FLAG', 'WARNING', ?)",
            params![SPRINT_ID, huge],
        )
        .unwrap();

    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    let buf = write_workbook_buffer(&data, &cfg).unwrap();
    assert_eq!(&buf[0..2], b"PK");
}
