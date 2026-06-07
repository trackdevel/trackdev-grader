//! Wave 3 acceptance: grade arithmetic, gates, and idempotent persist.

use rusqlite::{params, Connection};
use sprint_grader_grading_xlsx::{
    aggregate_team_points, grade_project, keep, load_persisted_project, load_task_points,
    persist_project_grades, GradingConfig,
};

const PROJECT_ID: i64 = 1;
const SPRINT_ID: i64 = 10;
fn make_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open db");
    sprint_grader_core::db::apply_schema(&conn).expect("schema");
    conn
}

fn seed_project(conn: &Connection) {
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
}

fn seed_student(conn: &Connection, id: &str, name: &str) {
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id)
         VALUES (?, ?, ?, ?, ?)",
        params![id, id, id, name, PROJECT_ID],
    )
    .unwrap();
}

fn seed_done_task(
    conn: &Connection,
    task_id: i64,
    assignee: &str,
    points: i64,
    model: Option<&str>,
    level: Option<&str>,
) {
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (?, ?, ?, 'TASK', 'DONE', ?, ?, ?)",
        params![task_id, format!("T-{task_id}"), format!("Task {task_id}"), points, assignee, SPRINT_ID],
    )
    .unwrap();
    let declared = model.is_some() && level.is_some();
    conn.execute(
        "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
         VALUES (?, ?, ?, ?, '2026-01-10')",
        params![task_id, model, level, if declared { 1 } else { 0 }],
    )
    .unwrap();
}

#[test]
fn per_task_keep_and_effective_points() {
    let conn = make_db();
    seed_project(&conn);
    seed_student(&conn, "alice", "Alice");
    seed_done_task(&conn, 1, "alice", 10, Some("Cap"), Some("A"));
    seed_done_task(&conn, 2, "alice", 10, Some("GPT-5.5"), Some("E"));

    let cfg = GradingConfig::default();
    let tasks = load_task_points(&conn, PROJECT_ID, &[SPRINT_ID], &cfg).unwrap();
    assert_eq!(tasks.len(), 2);

    let cap_a = tasks.iter().find(|t| t.task_id == 1).unwrap();
    assert!((cap_a.keep - 1.0).abs() < 1e-9);
    assert!((cap_a.effective - 10.0).abs() < 1e-9);

    let frontier_e = tasks.iter().find(|t| t.task_id == 2).unwrap();
    assert!((frontier_e.keep - 0.2).abs() < 1e-9);
    assert!((frontier_e.effective - 2.0).abs() < 1e-9);
}

#[test]
fn worked_example_alice_bob() {
    let conn = make_db();
    seed_project(&conn);
    seed_student(&conn, "alice", "Alice");
    seed_student(&conn, "bob", "Bob");
    seed_done_task(&conn, 1, "alice", 10, Some("Cap"), Some("A"));
    seed_done_task(&conn, 2, "bob", 10, Some("GPT-5.5"), Some("E"));

    let cfg = GradingConfig::default();
    let team = aggregate_team_points(&conn, PROJECT_ID, &[SPRINT_ID], &cfg).unwrap();
    assert!((team.sum_raw - 20.0).abs() < 1e-9);
    assert!((team.sum_effective - 12.0).abs() < 1e-9);
    assert_eq!(team.team_size, 2);
    assert!((team.mean_raw - 10.0).abs() < 1e-9);

    let ai_factor = team.sum_effective / team.sum_raw;
    assert!((ai_factor - 0.6).abs() < 1e-9);

    let q_pen = 8.0;
    let alice = team
        .students
        .iter()
        .find(|s| s.student_id == "alice")
        .unwrap();
    let bob = team
        .students
        .iter()
        .find(|s| s.student_id == "bob")
        .unwrap();
    let alice_base = q_pen * alice.effective / team.mean_raw;
    let bob_base = q_pen * bob.effective / team.mean_raw;
    assert!((alice_base - 8.0).abs() < 1e-9);
    assert!((bob_base - 1.6).abs() < 1e-9);
    assert!((q_pen * ai_factor - 4.8).abs() < 1e-9);
}

#[test]
fn undeclared_task_uses_assumed_discount() {
    let conn = make_db();
    seed_project(&conn);
    seed_student(&conn, "carol", "Carol");
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (99, 'T-99', 'Undeclared', 'TASK', 'DONE', 10, 'carol', ?)",
        params![SPRINT_ID],
    )
    .unwrap();

    let cfg = GradingConfig::default();
    let expected_keep = keep(
        cfg.ai_usage.undeclared_model_m,
        cfg.ai_usage.undeclared_level_l,
        cfg.ai_usage.strength,
        cfg.ai_usage.floor_keep,
    );
    let tasks = load_task_points(&conn, PROJECT_ID, &[SPRINT_ID], &cfg).unwrap();
    assert_eq!(tasks.len(), 1);
    assert!((tasks[0].keep - expected_keep).abs() < 1e-9);
    assert!((tasks[0].effective - 10.0 * expected_keep).abs() < 1e-9);
}

#[test]
fn gates_no_delivery_plagiarism_ai_review() {
    let conn = make_db();
    seed_project(&conn);
    seed_student(&conn, "alice", "Alice");
    seed_student(&conn, "bob", "Bob");
    seed_done_task(&conn, 1, "alice", 10, Some("Cap"), Some("A"));
    // bob has no tasks → no delivery

    conn.execute(
        "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
         VALUES ('PROJECT_1', ?, 'CROSS_TEAM_SIMILARITY', 'CRITICAL', '{{}}')",
        params![SPRINT_ID],
    )
    .unwrap();

    let cfg = GradingConfig::default();
    let result = grade_project(&conn, PROJECT_ID, "Team 01", &[SPRINT_ID], &cfg).unwrap();

    let alice = result
        .students
        .iter()
        .find(|s| s.student_id == "alice")
        .unwrap();
    assert_eq!(alice.review_gate.as_deref(), Some("PLAGIARISM"));

    let bob = result
        .students
        .iter()
        .find(|s| s.student_id == "bob")
        .unwrap();
    assert_eq!(bob.review_gate.as_deref(), Some("NO_DELIVERY"));
    assert!((bob.final_grade - 0.0).abs() < 1e-9);
    assert_eq!(result.project.review_gate.as_deref(), Some("PLAGIARISM"));
}

#[test]
fn persist_is_idempotent() {
    let conn = make_db();
    seed_project(&conn);
    seed_student(&conn, "alice", "Alice");
    seed_done_task(&conn, 1, "alice", 10, Some("Cap"), Some("A"));

    let cfg = GradingConfig::default();
    let result = grade_project(&conn, PROJECT_ID, "Team 01", &[SPRINT_ID], &cfg).unwrap();
    persist_project_grades(&conn, &result).unwrap();
    let first: String = conn
        .query_row(
            "SELECT weights_version FROM project_final_grade WHERE project_id = ?",
            params![PROJECT_ID],
            |r| r.get(0),
        )
        .unwrap();

    persist_project_grades(&conn, &result).unwrap();
    let second: String = conn
        .query_row(
            "SELECT weights_version FROM project_final_grade WHERE project_id = ?",
            params![PROJECT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(first, second);

    let loaded = load_persisted_project(&conn, PROJECT_ID)
        .unwrap()
        .expect("row");
    assert!((loaded.final_grade - result.project.final_grade).abs() < 1e-9);
}
