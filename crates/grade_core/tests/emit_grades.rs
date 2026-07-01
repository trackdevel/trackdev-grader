//! READ-ONLY: print reference.grades.json for the current spec to stdout.
//! (Temporary helper; the sandbox blocks cargo from writing tracked fixtures.)
//!   cargo test -p grade_core --test emit_grades -- --ignored --nocapture

use std::fs;
use std::path::PathBuf;

use grade_core::{grade_cohort, GradeOutput, GradeSpec, RawProject};
use serde::{Deserialize, Serialize};

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
    codequality_penalty: f64,
    final_grade: f64,
    review_gate: Option<String>,
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/tests/fixtures")
}

fn output_to_reference(
    out: &GradeOutput,
    prior: Option<&ReferenceGradeProject>,
) -> ReferenceGradeProject {
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
                    codequality_penalty: s.codequality_penalty,
                    final_grade: s.student_final,
                    review_gate: gate,
                }
            })
            .collect(),
    }
}

#[test]
#[ignore]
fn emit_reference_grades() {
    let spec: GradeSpec = serde_json::from_str(
        &fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/grading.standard.json"),
        )
        .expect("read spec"),
    )
    .expect("parse spec");
    let raws: Vec<RawProject> = serde_json::from_str(
        &fs::read_to_string(fixture_dir().join("reference.raw_projects.json")).expect("read raw"),
    )
    .expect("parse raw");
    let prior: Vec<ReferenceGradeProject> = serde_json::from_str(
        &fs::read_to_string(fixture_dir().join("reference.grades.json")).expect("read grades"),
    )
    .expect("parse grades");

    let cohort = grade_cohort(&raws, &spec).expect("grade");
    let mut out = Vec::new();
    for (i, entry) in cohort.projects.iter().enumerate() {
        out.push(output_to_reference(&entry.output, prior.get(i)));
    }
    println!("<<<REFGRADES");
    println!("{}", serde_json::to_string_pretty(&out).expect("ser"));
    println!("REFGRADES>>>");
}
