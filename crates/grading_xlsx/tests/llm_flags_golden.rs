//! Track B PE: populated `LLM_Flags` sheet (file + holistic rows).

use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
use rusqlite::params;
use sprint_grader_core::Database;
use sprint_grader_grading_xlsx::{
    load_workbook_data, write_workbook_buffer, GradingConfig, LLM_FLAGS_SHEET_NAME,
};
use sprint_grader_quality_llm::{persist_project_flags, LlmQualityFlagRow};
use std::io::Cursor;
use tempfile::tempdir;

const PROJECT_ID: i64 = 1;

fn make_db() -> Database {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("grading.db");
    let db = Database::open(&path).expect("open db");
    db.create_tables().expect("schema");
    std::mem::forget(dir);
    db
}

fn seed_project(db: &Database) {
    db.conn
        .execute(
            "INSERT INTO projects (id, slug, name) VALUES (?, 'team-01', 'Team 01')",
            params![PROJECT_ID],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO sprints (id, project_id, name, start_date, end_date)
             VALUES (10, ?, 'S1', '2026-01-01', '2026-01-15')",
            params![PROJECT_ID],
        )
        .unwrap();
}

fn cell_string(range: &calamine::Range<Data>, row: u32, col: u32) -> Option<String> {
    match range.get_value((row, col)) {
        Some(Data::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Data::Float(v)) => Some(v.to_string()),
        Some(Data::Int(v)) => Some(v.to_string()),
        _ => None,
    }
}

#[test]
fn llm_flags_sheet_contains_file_and_holistic_rows() {
    let db = make_db();
    seed_project(&db);

    let rows = [
        LlmQualityFlagRow {
            project_id: PROJECT_ID,
            student_id: None,
            sprint_id: None,
            scope: "file".into(),
            target_ref: Some("org/spring:src/Foo.java".into()),
            category: "testing".into(),
            severity: "WARNING".into(),
            summary: "No unit tests in Foo".into(),
            detail: Some("JUnit imports absent.".into()),
            backend: "claude-cli".into(),
            model_id: "claude-haiku-4-5-20251001".into(),
            prompt_version: "1".into(),
            generated_at: "2026-01-01T00:00:00Z".into(),
        },
        LlmQualityFlagRow {
            project_id: PROJECT_ID,
            student_id: Some("alice".into()),
            sprint_id: None,
            scope: "project".into(),
            target_ref: Some(format!("project:{PROJECT_ID}")),
            category: "testing".into(),
            severity: "INFO".into(),
            summary: "Team-wide sparse test coverage".into(),
            detail: None,
            backend: "claude-cli".into(),
            model_id: "claude-haiku-4-5-20251001".into(),
            prompt_version: "1".into(),
            generated_at: "2026-01-01T00:00:01Z".into(),
        },
    ];
    persist_project_flags(&db.conn, PROJECT_ID, &rows).unwrap();

    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    assert_eq!(data.llm_flag_rows.len(), 2);
    assert_eq!(data.llm_flag_rows[0].scope, "file");
    assert_eq!(
        data.llm_flag_rows[1].target_ref.as_deref(),
        Some("project:1")
    );

    let buf = write_workbook_buffer(&data, &cfg).unwrap();
    let cursor = Cursor::new(buf);
    let mut workbook: Xlsx<_> = open_workbook_from_rs(cursor).expect("open xlsx");
    assert!(
        workbook
            .sheet_names()
            .iter()
            .any(|n| n == LLM_FLAGS_SHEET_NAME),
        "missing {LLM_FLAGS_SHEET_NAME} sheet"
    );
    let range = workbook
        .worksheet_range(LLM_FLAGS_SHEET_NAME)
        .expect("LLM_Flags range");

    assert_eq!(cell_string(&range, 0, 3), Some("scope".into()));
    assert_eq!(cell_string(&range, 0, 4), Some("target_ref".into()));
    assert_eq!(cell_string(&range, 1, 3), Some("file".into()));
    assert_eq!(
        cell_string(&range, 1, 4),
        Some("org/spring:src/Foo.java".into())
    );
    assert_eq!(cell_string(&range, 1, 7), Some("No unit tests in Foo".into()));
    assert_eq!(cell_string(&range, 2, 3), Some("project".into()));
    assert_eq!(cell_string(&range, 2, 4), Some("project:Team 01".into()));
    assert_eq!(
        cell_string(&range, 2, 7),
        Some("Team-wide sparse test coverage".into())
    );
    assert_eq!(cell_string(&range, 2, 1), Some("alice".into()));
    assert_eq!(cell_string(&range, 0, 0), Some("project".into()));
    assert_eq!(cell_string(&range, 0, 2), Some("sprint".into()));
}
