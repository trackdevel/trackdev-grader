//! `export_grade_markdown` writes one student-facing `GRADES.md` per gradable
//! project and skips empty-shell projects. The flat `--out` form is exercised
//! here (the default per-project form writes beside each REPORT.md, which needs
//! on-disk repo clones).

use std::fs;
use std::path::PathBuf;

use grade_core::GradeSpec;
use sprint_grader_core::Database;
use sprint_grader_orchestration::export_grade_markdown;
use tempfile::tempdir;

const TODAY: &str = "2026-06-19";

fn load_spec() -> GradeSpec {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/grading.standard.json");
    let text = fs::read_to_string(path).expect("grading.standard.json");
    serde_json::from_str(&text).expect("parse spec")
}

/// One gradable project (id 1, has inventory) and one empty-shell project
/// (id 2, no inventory → not gradable).
fn seed_db(db: &Database) {
    let c = &db.conn;
    c.execute(
        "INSERT INTO projects (id, slug, name) VALUES
           (1, 'team-01', 'Team 01'),
           (2, 'team-02', 'Team 02')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id) VALUES
           ('alice', 'alice', 'alice', 'Alice Liddell', 1),
           ('bob',   'bob',   'bob',   'Bob Stone',     2)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES
           (100, 1, 'S1', '2026-01-01', '2026-01-15'),
           (200, 2, 'S1', '2026-01-01', '2026-01-15')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id) VALUES
           (1, 'T-1', 'a', 'TASK', 'DONE', 5, 'alice', 100),
           (2, 'T-2', 'b', 'TASK', 'DONE', 5, 'bob',   200)",
        [],
    )
    .unwrap();
    // Only project 1 has a structural inventory → only it is gradable.
    c.execute(
        "INSERT INTO project_inventory_runs (project_id, repo_full_name, status, metric_count, scanned_at) VALUES
           (1, 'org/team-01', 'OK', 1, '2026-03-02')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO repo_structural_metrics (repo_full_name, metric_key, value) VALUES
           ('org/team-01', 'production_loc', 800.0)",
        [],
    )
    .unwrap();
}

#[test]
fn writes_one_markdown_per_gradable_project_and_skips_empty_shell() {
    let db_dir = tempdir().unwrap();
    let db = Database::open(&db_dir.path().join("g.db")).expect("open db");
    db.create_tables().expect("schema");
    seed_db(&db);

    let entregues = tempdir().unwrap();
    let out = tempdir().unwrap();
    let spec = load_spec();
    let written =
        export_grade_markdown(&db, TODAY, &spec, None, entregues.path(), Some(out.path()))
            .expect("export");

    assert_eq!(written.len(), 1, "only the gradable project yields a file");
    let path = out.path().join("notes_team-01.md");
    assert!(path.is_file(), "expected {}", path.display());
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("# Notes — Team 01"));
    assert!(body.contains("## Equip"));
    assert!(body.contains("Alice Liddell"));
    // The empty-shell project produced nothing.
    assert!(!out.path().join("notes_team-02.md").exists());
}
