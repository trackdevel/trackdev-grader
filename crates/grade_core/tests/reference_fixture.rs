//! Build and verify `apps/desktop/tests/fixtures/reference.*` golden files.
//!
//! Regenerate committed fixtures:
//!   UPDATE_REFERENCE_FIXTURES=1 cargo test -p grade_core reference_fixtures -- --ignored --nocapture

#[path = "support/raw_projection.rs"]
mod raw_projection;
#[path = "support/seeds.rs"]
mod seeds;

use std::fs;
use std::path::PathBuf;

use grade_core::{grade, structural_scopes, GradeOutput, GradeSpec};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sprint_grader_core::Database;

use raw_projection::load_raw_project;
use seeds::seed_all_fixtures;

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/desktop/tests/fixtures"
);
const TODAY: &str = "2026-03-01";

#[derive(Serialize, Deserialize, Clone)]
struct ReferenceGradeProject {
    project: ReferenceProjectGrade,
    students: Vec<ReferenceStudentGrade>,
}

#[derive(Serialize, Deserialize, Clone)]
struct ReferenceProjectGrade {
    project_id: i64,
    quality_grade: f64,
    quality_penalized: f64,
    project_penalty: f64,
    ai_factor: f64,
    final_grade: f64,
    review_gate: Option<String>,
    team_size: i64,
    axes: Vec<ReferenceAxis>,
}

#[derive(Serialize, Deserialize, Clone)]
struct ReferenceAxis {
    key: String,
    raw: Option<f64>,
    score: Option<f64>,
    present: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct ReferenceStudentGrade {
    student_id: String,
    raw_points: f64,
    effective_points: f64,
    ai_keep: Option<f64>,
    contribution: Option<f64>,
    base_grade: f64,
    student_penalty: f64,
    final_grade: f64,
    review_gate: Option<String>,
}

fn fixture_paths() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let dir = PathBuf::from(FIXTURE_DIR);
    (
        dir.join("reference.db"),
        dir.join("reference.grades.json"),
        dir.join("reference.raw_projects.json"),
        dir.join("reference.scopes.json"),
    )
}

fn load_spec() -> GradeSpec {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/grading.standard.json");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("parse grading.standard.json")
}

fn sprint_ids_on_conn(conn: &Connection, project_id: i64, today: &str) -> Vec<i64> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM sprints
             WHERE project_id = ? AND start_date IS NOT NULL
               AND start_date != '' AND start_date <= ?
             ORDER BY start_date ASC",
        )
        .expect("prepare sprint query");
    let rows = stmt
        .query_map(params![project_id, today], |r| r.get(0))
        .expect("query sprints");
    rows.filter_map(|r| r.ok()).collect()
}

fn make_db() -> Database {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("grading.db");
    let db = Database::open(&path).expect("open db");
    db.create_tables().expect("schema");
    std::mem::forget(dir);
    db
}

fn copy_db_fixture(db: &Database, dest: &std::path::Path) {
    db.conn
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("wal checkpoint before fixture copy");
    fs::copy(&db.db_path, dest).expect("copy reference.db");
}

fn output_to_reference(out: &GradeOutput, prior: Option<&ReferenceGradeProject>) -> ReferenceGradeProject {
    ReferenceGradeProject {
        project: ReferenceProjectGrade {
            project_id: out.grades.project_id,
            quality_grade: out.grades.quality_grade,
            quality_penalized: out.grades.quality_penalized,
            project_penalty: out.grades.project_penalty,
            ai_factor: out.grades.ai_factor,
            final_grade: out.grades.project_final,
            review_gate: prior.map(|p| p.project.review_gate.clone()).unwrap_or(None),
            team_size: out.grades.team_size,
            axes: out
                .grades
                .axes
                .iter()
                .map(|a| ReferenceAxis {
                    key: a.key.clone(),
                    raw: a.raw,
                    score: a.score,
                    present: a.present,
                })
                .collect(),
        },
        students: out
            .grades
            .students
            .iter()
            .map(|s| {
                let gate = prior.and_then(|p| {
                    p.students
                        .iter()
                        .find(|x| x.student_id == s.student_id)
                        .map(|x| x.review_gate.clone())
                        .unwrap_or(None)
                });
                ReferenceStudentGrade {
                    student_id: s.student_id.clone(),
                    raw_points: s.raw_points,
                    effective_points: s.effective_points,
                    ai_keep: s.ai_keep,
                    contribution: s.contribution,
                    base_grade: s.base_grade,
                    student_penalty: s.student_penalty,
                    final_grade: s.student_final,
                    review_gate: gate,
                }
            })
            .collect(),
    }
}

#[test]
#[ignore = "run with UPDATE_REFERENCE_FIXTURES=1 to regenerate committed fixtures"]
fn reference_fixtures_generate() {
    if std::env::var("UPDATE_REFERENCE_FIXTURES").ok().as_deref() != Some("1") {
        eprintln!("skip: set UPDATE_REFERENCE_FIXTURES=1 to write fixtures");
        return;
    }
    let db = make_db();
    seed_all_fixtures(&db.conn);
    let (db_path, grades_path, raw_path, scopes_path) = fixture_paths();
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).expect("mkdir fixtures");
    }

    let prior_grades: Option<Vec<ReferenceGradeProject>> = grades_path
        .exists()
        .then(|| {
            fs::read_to_string(&grades_path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
        })
        .flatten();

    copy_db_fixture(&db, &db_path);

    let spec = load_spec();
    let project_ids = [1i64, 2, 3, 4];
    let mut grades = Vec::new();
    let mut raw_projects = Vec::new();
    let mut scopes = Vec::new();
    for (i, &pid) in project_ids.iter().enumerate() {
        let sprint_ids = db.sprint_ids_up_to_current(pid, TODAY).expect("sprints");
        let raw = load_raw_project(&db.conn, pid, &sprint_ids).expect("raw project");
        let out = grade(&raw, &spec).expect("grade");
        let prior = prior_grades.as_ref().and_then(|g| g.get(i));
        grades.push(output_to_reference(&out, prior));
        raw_projects.push(raw.clone());
        scopes.push(structural_scopes(&raw, &spec));
    }
    fs::write(
        &grades_path,
        serde_json::to_string_pretty(&grades).expect("serialize grades"),
    )
    .expect("write reference.grades.json");
    fs::write(
        &raw_path,
        serde_json::to_string_pretty(&raw_projects).expect("serialize raw"),
    )
    .expect("write reference.raw_projects.json");
    fs::write(
        &scopes_path,
        serde_json::to_string_pretty(&scopes).expect("serialize scopes"),
    )
    .expect("write reference.scopes.json");
}

#[test]
fn committed_reference_fixtures_exist() {
    let (_, grades_path, raw_path, scopes_path) = fixture_paths();
    assert!(
        grades_path.exists(),
        "missing {}; run UPDATE_REFERENCE_FIXTURES=1 cargo test -p grade_core reference_fixtures -- --ignored",
        grades_path.display()
    );
    assert!(raw_path.exists(), "missing raw fixture");
    assert!(scopes_path.exists(), "missing scopes fixture");
}

#[test]
fn reference_structural_scopes_on_committed_db() {
    let (db_path, _, raw_path, scopes_path) = fixture_paths();
    if !db_path.exists() {
        eprintln!("skip: committed reference.db missing");
        return;
    }
    let spec = load_spec();
    let expected_raw: Vec<grade_core::RawProject> =
        serde_json::from_str(&fs::read_to_string(raw_path).expect("read raw fixture"))
            .expect("parse raw fixture");
    let expected_scopes: Vec<grade_core::ProjectScopes> =
        serde_json::from_str(&fs::read_to_string(scopes_path).expect("read scopes fixture"))
            .expect("parse scopes fixture");

    let conn = Connection::open(&db_path).expect("open reference.db");
    for (i, pid) in [1i64, 2, 3, 4].iter().enumerate() {
        let sprint_ids = sprint_ids_on_conn(&conn, *pid, TODAY);
        let raw = load_raw_project(&conn, *pid, &sprint_ids).expect("load raw");
        let scopes = structural_scopes(&raw, &spec);
        assert_eq!(
            &raw, &expected_raw[i],
            "raw project {pid} drift — regenerate fixtures"
        );
        assert_scopes_close(&scopes, &expected_scopes[i], *pid);
    }
}

fn assert_scopes_close(
    actual: &grade_core::ProjectScopes,
    expected: &grade_core::ProjectScopes,
    pid: i64,
) {
    let eps = 1e-9;
    assert!((actual.sum_raw - expected.sum_raw).abs() < eps, "pid {pid} sum_raw");
    assert!((actual.sum_eff - expected.sum_eff).abs() < eps, "pid {pid} sum_eff");
    assert!((actual.mean_raw - expected.mean_raw).abs() < eps, "pid {pid} mean_raw");
    assert!(
        (actual.ai_factor - expected.ai_factor).abs() < eps,
        "pid {pid} ai_factor"
    );
    assert_eq!(actual.students.len(), expected.students.len(), "pid {pid} students");
    for exp in &expected.students {
        let act = actual
            .students
            .iter()
            .find(|s| s.student_id == exp.student_id)
            .unwrap_or_else(|| panic!("pid {pid} missing student {}", exp.student_id));
        assert!((act.student_eff - exp.student_eff).abs() < eps);
    }
}
