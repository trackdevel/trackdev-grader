//! Track B PC: holistic tier targeting and resume.

use sprint_grader_core::{Database, QualityLlmConfig};
use sprint_grader_quality_llm::{
    holistic_flag_exists, insert_flag, load_file_flag_summaries, load_rubric, run_holistic_pass,
    LlmQualityFlagRow,
};
use tempfile::tempdir;

fn seed_project(db: &Database) {
    db.conn
        .execute(
            "INSERT INTO projects (id, slug, name) VALUES (1, 't', 'Team 01')",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO students (id, username, github_login, full_name, team_project_id)
             VALUES ('s1', 's1', 's1', 'S', 1)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO sprints (id, project_id, name, start_date, end_date)
             VALUES (10, 1, 'S1', '2026-01-01', '2026-01-15')",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
             VALUES (100, 'T-1', 'Task', 'TASK', 'DONE', 1, 's1', 10)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO pull_requests (id, pr_number, repo_full_name, url, title, state, merged)
             VALUES ('pr1', 1, 'org/android', 'http://x', 't', 'MERGED', 1),
                    ('pr2', 2, 'org/spring', 'http://y', 't2', 'MERGED', 1)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (100, 'pr1'), (100, 'pr2')",
            [],
        )
        .unwrap();
}

#[test]
fn holistic_skipped_when_cap_zero() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# r\n").unwrap();
    let ql = QualityLlmConfig {
        model_id: Some("m".into()),
        ..Default::default()
    };
    let rubric = load_rubric(dir.path(), &ql).unwrap();
    let db = Database::open(&dir.path().join("g.db")).unwrap();
    db.create_tables().unwrap();
    seed_project(&db);

    let stats = run_holistic_pass(&db.conn, 1, "Team 01", &ql, &rubric, 0, false).unwrap();
    assert_eq!(stats.judged, 0);
    assert_eq!(stats.skipped_cap, 1);
}

#[test]
fn holistic_resume_skips_existing_project_row() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# r\n").unwrap();
    let ql = QualityLlmConfig {
        model_id: Some("claude-haiku-4-5-20251001".into()),
        ..Default::default()
    };
    let rubric = load_rubric(dir.path(), &ql).unwrap();
    let db = Database::open(&dir.path().join("g.db")).unwrap();
    db.create_tables().unwrap();
    seed_project(&db);

    insert_flag(
        &db.conn,
        &LlmQualityFlagRow {
            project_id: 1,
            student_id: None,
            sprint_id: None,
            scope: "project".into(),
            target_ref: Some("project:1".into()),
            category: "other".into(),
            severity: "INFO".into(),
            summary: "existing".into(),
            detail: None,
            backend: ql.backend.clone(),
            model_id: ql.resolved_model_id().to_string(),
            prompt_version: ql.prompt_version.clone(),
            generated_at: "2026-01-01T00:00:00Z".into(),
        },
    )
    .unwrap();
    assert!(holistic_flag_exists(
        &db.conn,
        1,
        "project:1",
        &ql.backend,
        ql.resolved_model_id(),
        &ql.prompt_version,
    )
    .unwrap());

    let stats = run_holistic_pass(&db.conn, 1, "Team 01", &ql, &rubric, 1, true).unwrap();
    assert_eq!(stats.skipped_resume, 1);
    assert_eq!(stats.judged, 0);
}

#[test]
fn load_file_flags_filters_by_repo_prefix() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    sprint_grader_core::db::apply_schema(&conn).unwrap();
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (1, 't', 'T')",
        [],
    )
    .unwrap();
    for (target, summary) in [
        ("org/android:src/A.java", "android issue"),
        ("org/spring:src/B.java", "spring issue"),
    ] {
        insert_flag(
            &conn,
            &LlmQualityFlagRow {
                project_id: 1,
                student_id: None,
                sprint_id: None,
                scope: "file".into(),
                target_ref: Some(target.into()),
                category: "other".into(),
                severity: "INFO".into(),
                summary: summary.into(),
                detail: None,
                backend: "claude-cli".into(),
                model_id: "m".into(),
                prompt_version: "1".into(),
                generated_at: "2026-01-01T00:00:00Z".into(),
            },
        )
        .unwrap();
    }
    let android = load_file_flag_summaries(&conn, 1, Some("org/android")).unwrap();
    assert_eq!(android.len(), 1);
    assert_eq!(android[0].summary, "android issue");
}
