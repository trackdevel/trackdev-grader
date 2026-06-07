//! AI_REVIEW gate: HIGH detected risk + low declared usage.

use rusqlite::Connection;
use sprint_grader_grading_xlsx::{grade_project, GradingConfig};

const PROJECT_ID: i64 = 1;
const SPRINT_ID: i64 = 10;

fn make_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open db");
    sprint_grader_core::db::apply_schema(&conn).expect("schema");
    conn
}

#[test]
fn high_detected_with_low_declaration_sets_ai_review() {
    let conn = make_db();
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (1, 't', 'T')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (10, 1, 'S', '2026-01-01', '2026-01-15')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id)
         VALUES ('alice', 'alice', 'alice', 'Alice', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (1, 'T-1', 'Task', 'TASK', 'DONE', 5, 'alice', 10)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
         VALUES (1, 'Cap', 'A', 1, '2026-01-01')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO student_sprint_ai_usage (student_id, sprint_id, project_id, risk_level)
         VALUES ('alice', 10, 1, 'HIGH')",
        [],
    )
    .unwrap();

    let cfg = GradingConfig::default();
    let result = grade_project(&conn, PROJECT_ID, "T", &[SPRINT_ID], &cfg).unwrap();
    let alice = &result.students[0];
    assert_eq!(alice.review_gate.as_deref(), Some("AI_REVIEW"));
    // Student-level only: project grade is not gated by one member's detection.
    assert!(result.project.review_gate.is_none());
}
