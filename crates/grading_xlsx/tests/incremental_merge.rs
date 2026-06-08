//! Incremental grading: subset persist + merged workbook export (option C).

use rusqlite::params;
use sprint_grader_core::Database;
use sprint_grader_grading_xlsx::{
    grade_project, list_graded_project_ids, load_workbook_data, persist_project_grades, run,
    GradingConfig, RunOpts,
};
use sprint_grader_quality_llm::{persist_project_flags, LlmQualityFlagRow};

fn make_db() -> Database {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("grading.db");
    let db = Database::open(&path).expect("open db");
    db.create_tables().expect("schema");
    std::mem::forget(dir);
    db
}

fn seed_minimal_project(db: &Database, project_id: i64, slug: &str, name: &str, sprint_id: i64) {
    let conn = &db.conn;
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, ?, ?)",
        params![project_id, slug, name],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (?, ?, 'S1', '2026-01-01', '2026-01-15')",
        params![sprint_id, project_id],
    )
    .unwrap();
    let student_id = format!("s{project_id}");
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id)
         VALUES (?, ?, ?, ?, ?)",
        params![student_id, student_id, student_id, "Student", project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (?, ?, 'Task', 'TASK', 'DONE', 5, ?, ?)",
        params![project_id * 100, format!("T-{project_id}"), student_id, sprint_id],
    )
    .unwrap();
}

#[test]
fn list_graded_project_ids_tracks_persisted_projects() {
    let db = make_db();
    seed_minimal_project(&db, 1, "team-01", "Team 01", 10);
    seed_minimal_project(&db, 2, "team-02", "Team 02", 20);

    assert!(list_graded_project_ids(&db.conn).unwrap().is_empty());

    let cfg = GradingConfig::default();
    for (pid, name) in [(1, "Team 01"), (2, "Team 02")] {
        let result = grade_project(&db.conn, pid, name, &[pid * 10], &cfg).unwrap();
        persist_project_grades(&db.conn, &result).unwrap();
    }

    assert_eq!(list_graded_project_ids(&db.conn).unwrap(), vec![1, 2]);
}

#[test]
fn subset_run_merges_workbook_projects_and_refreshes_existing() {
    let db = make_db();
    seed_minimal_project(&db, 1, "team-01", "Team 01", 10);
    seed_minimal_project(&db, 2, "team-02", "Team 02", 20);

    let cfg_dir = tempfile::tempdir().expect("cfgdir");
    GradingConfig::default()
        .write_to_dir(cfg_dir.path())
        .expect("write grading.toml");

    let out = cfg_dir.path().join("grading_sheet.xlsx");
    let today = "2026-03-01".to_string();

    run(
        &db,
        cfg_dir.path(),
        &RunOpts {
            project_filter: Some(vec!["team-01".to_string()]),
            out: Some(out.clone()),
            today: today.clone(),
            ..Default::default()
        },
    )
    .expect("first run");

    assert_eq!(list_graded_project_ids(&db.conn).unwrap(), vec![1]);

    // Mutate team-01 evidence; second run targets only team-02 but must refresh team-01 too.
    db.conn
        .execute("UPDATE tasks SET estimation_points = 20 WHERE id = 100", [])
        .unwrap();

    run(
        &db,
        cfg_dir.path(),
        &RunOpts {
            project_filter: Some(vec!["team-02".to_string()]),
            out: Some(out.clone()),
            today,
            ..Default::default()
        },
    )
    .expect("second run");

    let graded = list_graded_project_ids(&db.conn).unwrap();
    assert_eq!(graded, vec![1, 2]);

    let team1_raw: f64 = db
        .conn
        .query_row(
            "SELECT raw_points FROM student_final_grade
         WHERE project_id = 1 AND student_id = 's1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        (team1_raw - 20.0).abs() < 1e-9,
        "team-01 should be refreshed on merged export, got {team1_raw}"
    );

    assert!(out.is_file(), "merged workbook should exist");
}

#[test]
fn workbook_exports_all_llm_flags_not_only_workbook_projects() {
    let db = make_db();
    seed_minimal_project(&db, 1, "team-01", "Team 01", 10);
    seed_minimal_project(&db, 2, "team-02", "Team 02", 20);

    let mk = |pid: i64, summary: &str| LlmQualityFlagRow {
        project_id: pid,
        student_id: None,
        sprint_id: None,
        scope: "file".into(),
        target_ref: Some(format!("t:{pid}")),
        category: "other".into(),
        severity: "INFO".into(),
        summary: summary.into(),
        detail: None,
        backend: "claude-cli".into(),
        model_id: "m".into(),
        prompt_version: "1".into(),
        generated_at: "2026-01-01T00:00:00Z".into(),
    };
    persist_project_flags(&db.conn, 1, &[mk(1, "flag-01")]).unwrap();
    persist_project_flags(&db.conn, 2, &[mk(2, "flag-02")]).unwrap();

    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[1], "2026-03-01", &cfg).unwrap();
    assert_eq!(data.llm_flag_rows.len(), 2);
    let summaries: Vec<_> = data
        .llm_flag_rows
        .iter()
        .map(|r| r.summary.as_str())
        .collect();
    assert!(summaries.contains(&"flag-01"));
    assert!(summaries.contains(&"flag-02"));
}

#[test]
fn no_workbook_persists_without_export() {
    let db = make_db();
    seed_minimal_project(&db, 1, "team-01", "Team 01", 10);

    let cfg_dir = tempfile::tempdir().expect("cfgdir");
    GradingConfig::default()
        .write_to_dir(cfg_dir.path())
        .expect("write grading.toml");

    let out = cfg_dir.path().join("grading_sheet.xlsx");
    run(
        &db,
        cfg_dir.path(),
        &RunOpts {
            project_filter: Some(vec!["team-01".to_string()]),
            out: Some(out.clone()),
            no_workbook: true,
            today: "2026-03-01".to_string(),
            ..Default::default()
        },
    )
    .expect("no-workbook run");

    assert_eq!(list_graded_project_ids(&db.conn).unwrap(), vec![1]);
    assert!(
        !out.exists(),
        "workbook should not be written with --no-workbook"
    );
}
