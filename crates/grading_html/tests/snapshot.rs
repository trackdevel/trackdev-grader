//! Phase 2 acceptance: the snapshot opens, table row counts equal the
//! corresponding `WorkbookData` vec lengths, reference rows match the graded
//! results, and the `v_student` / `v_team` views are queryable.

use rusqlite::{params, Connection};
use sprint_grader_core::Database;
use sprint_grader_grading_html::build_snapshot_bytes;
use sprint_grader_grading_xlsx::{load_workbook_data, GradingConfig};
use tempfile::{tempdir, NamedTempFile};

const PROJECT_ID: i64 = 1;
const SPRINT_ID: i64 = 10;

fn make_db() -> Database {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("grading.db");
    let db = Database::open(&path).expect("open db");
    db.create_tables().expect("schema");
    std::mem::forget(dir); // keep alive; process exit cleans up
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
    // One sprint flag + one artifact flag for alice → exercises the `flag`
    // table union (source = 'sprint' | 'artifact').
    conn.execute(
        "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
         VALUES ('alice', ?, 'SOME_FLAG', 'CRITICAL', 'd')",
        params![SPRINT_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO student_artifact_flags (student_id, project_id, flag_type, severity, details)
         VALUES ('alice', ?, 'ARTIFACT_FLAG', 'CRITICAL', NULL)",
        params![PROJECT_ID],
    )
    .unwrap();
}

fn open_snapshot(bytes: &[u8]) -> (Connection, NamedTempFile) {
    let tf = NamedTempFile::new().unwrap();
    std::fs::write(tf.path(), bytes).unwrap();
    let conn = Connection::open(tf.path()).unwrap();
    (conn, tf)
}

#[test]
fn snapshot_table_counts_match_workbook_data() {
    let db = make_db();
    seed_worked_example(&db);
    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();

    let bytes = build_snapshot_bytes(&data, &cfg).unwrap();
    assert!(bytes.len() > 1024, "snapshot suspiciously small");
    let (snap, _tf) = open_snapshot(&bytes);

    let count = |t: &str| -> i64 {
        snap.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    };

    let students: usize = data.results.iter().map(|r| r.students.len()).sum();

    assert_eq!(count("project") as usize, data.results.len());
    assert_eq!(count("project_axis") as usize, data.project_axes.len());
    assert_eq!(count("task") as usize, data.tasks.len());
    assert_eq!(count("crit_flag") as usize, data.crit_flags.len());
    assert_eq!(
        count("flag") as usize,
        data.flag_rows.len() + data.artifact_flag_rows.len()
    );
    assert_eq!(count("ai_detect") as usize, data.ai_detect_rows.len());
    assert_eq!(count("llm_flag") as usize, data.llm_flag_rows.len());
    assert_eq!(count("student") as usize, students);
    assert_eq!(count("reference_student") as usize, students);
    assert_eq!(count("reference_project") as usize, data.results.len());

    // Knob tables.
    assert_eq!(count("weights"), 25);
    assert_eq!(count("models") as usize, cfg.ai_usage.models.len());
    assert_eq!(count("levels") as usize, cfg.ai_usage.levels.len());
}

#[test]
fn snapshot_flag_union_carries_both_sources() {
    let db = make_db();
    seed_worked_example(&db);
    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    let bytes = build_snapshot_bytes(&data, &cfg).unwrap();
    let (snap, _tf) = open_snapshot(&bytes);

    let by_source = |src: &str| -> i64 {
        snap.query_row(
            "SELECT COUNT(*) FROM flag WHERE source = ?",
            params![src],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(by_source("sprint"), 1);
    assert_eq!(by_source("artifact"), 1);
    // Artifact rows have NULL sprint_id; sprint rows do not.
    let null_sprint: i64 = snap
        .query_row(
            "SELECT COUNT(*) FROM flag WHERE source = 'artifact' AND sprint_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(null_sprint, 1);
}

#[test]
fn snapshot_meta_and_views_are_queryable() {
    let db = make_db();
    seed_worked_example(&db);
    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    let bytes = build_snapshot_bytes(&data, &cfg).unwrap();
    let (snap, _tf) = open_snapshot(&bytes);

    let penalty_mode: String = snap
        .query_row("SELECT penalty_mode FROM meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(penalty_mode, "subtractive");
    let decimals: i64 = snap
        .query_row("SELECT decimals FROM meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(decimals, 2);

    // Views resolve and join the reference rows.
    let v_students: i64 = snap
        .query_row("SELECT COUNT(*) FROM v_student", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v_students, 2);
    let v_teams: i64 = snap
        .query_row("SELECT COUNT(*) FROM v_team", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v_teams, 1);

    // The architecture counts column exists (live-knob support).
    let _arch_crit: i64 = snap
        .query_row(
            "SELECT COALESCE(SUM(arch_crit_count), 0) FROM project_axis",
            [],
            |r| r.get(0),
        )
        .unwrap();
}

#[test]
fn snapshot_views_use_human_readable_labels() {
    let db = make_db();
    seed_worked_example(&db);
    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    let bytes = build_snapshot_bytes(&data, &cfg).unwrap();
    let (snap, _tf) = open_snapshot(&bytes);

    let (team, student, sprint): (String, String, Option<String>) = snap
        .query_row(
            "SELECT team, student, sprint FROM v_flag WHERE source = 'sprint'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(team, "Team 01");
    assert_eq!(student, "Alice");
    assert_eq!(sprint.as_deref(), Some("1"));

    let (art_team, art_student, art_sprint): (String, String, Option<String>) = snap
        .query_row(
            "SELECT team, student, sprint FROM v_flag WHERE source = 'artifact'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(art_team, "Team 01");
    assert_eq!(art_student, "Alice");
    assert!(art_sprint.is_none());

    let grade_student: String = snap
        .query_row(
            "SELECT full_name FROM v_student WHERE student_id = 'alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(grade_student, "Alice");
}

#[test]
fn snapshot_task_carries_ai_model_and_level() {
    let db = make_db();
    seed_worked_example(&db);
    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    let bytes = build_snapshot_bytes(&data, &cfg).unwrap();
    let (snap, _tf) = open_snapshot(&bytes);

    let (model, level): (Option<String>, Option<String>) = snap
        .query_row(
            "SELECT ai_model, ai_level FROM task WHERE task_id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(model.as_deref(), Some("Cap"));
    assert_eq!(level.as_deref(), Some("A"));
}
