//! READ-ONLY preview: current student curve vs the proposed "transparent" curve.
//!
//! Safety: this copies `data/grading.db` to a temp file and opens only the copy,
//! so the irreplaceable original is never opened or mutated (read via fs::copy).
//!
//!   cargo test -p sprint-grader-orchestration --test grade_preview \
//!       -- --ignored --nocapture
//!
//! Override the reference date with PREVIEW_TODAY=YYYY-MM-DD.

use std::fs;
use std::path::PathBuf;

use grade_core::{grade_cohort, Expr, GradeSpec, StudentGrades};
use sprint_grader_core::Database;
use sprint_grader_orchestration::grading_projection::load_cohort_raw_projects;

fn today() -> String {
    std::env::var("PREVIEW_TODAY").unwrap_or_else(|_| "2026-06-10".to_string())
}

fn load_spec() -> GradeSpec {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/grading.standard.json");
    let text = fs::read_to_string(path).expect("grading.standard.json");
    serde_json::from_str(&text).expect("parse spec")
}

/// The two proposed edits, applied to a clone of the current spec.
fn proposed(spec: &GradeSpec) -> GradeSpec {
    let mut s = spec.clone();
    // Edit 1: transparent curve — student_curved := student_net.
    for f in s.formulas.student.iter_mut() {
        if f.name == "student_curved" {
            f.expr = Expr::Var {
                name: "student_net".into(),
            };
            f.infix = "student_net".into();
        }
    }
    // Edit 2: lower the per-student code-quality cap 2.0 -> 1.5.
    s.weights.insert("qpen_author_cap".into(), 1.5);
    s
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

#[test]
#[ignore]
fn preview_transparent_curve() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/grading.db");
    let tmp = std::env::temp_dir().join("grading_preview_ro.db");
    fs::copy(&src, &tmp).expect("copy grading.db to temp (read-only on original)");
    let db = Database::open(&tmp).expect("open temp db");

    let today = today();
    let projects = load_cohort_raw_projects(&db, &today, None).expect("load cohort");

    let spec_old = load_spec();
    let spec_new = proposed(&spec_old);
    let old = grade_cohort(&projects, &spec_old).expect("grade old");
    let new = grade_cohort(&projects, &spec_new).expect("grade new");

    let pname = |pid: i64| {
        projects
            .iter()
            .find(|r| r.project_id == pid)
            .map(|r| r.name.clone())
            .unwrap_or_default()
    };
    let name_of = |pid: i64, sid: &str| {
        projects
            .iter()
            .find(|r| r.project_id == pid)
            .and_then(|r| r.students.iter().find(|s| s.student_id == sid))
            .map(|s| s.full_name.clone())
            .unwrap_or_else(|| sid.to_string())
    };
    let new_student = |pid: i64, sid: &str| -> Option<StudentGrades> {
        new.projects
            .iter()
            .find(|p| p.project_id == pid)
            .and_then(|p| {
                p.output
                    .grades
                    .students
                    .iter()
                    .find(|s| s.student_id == sid)
                    .cloned()
            })
    };

    println!("\nTODAY = {today}   gradable projects = {}", projects.len());

    println!("\n=== project_final (old -> new; must be identical) ===");
    let mut prows: Vec<_> = old
        .projects
        .iter()
        .map(|p| {
            let np = new
                .projects
                .iter()
                .find(|q| q.project_id == p.project_id)
                .unwrap();
            (
                pname(p.project_id),
                p.output.grades.project_final,
                np.output.grades.project_final,
            )
        })
        .collect();
    prows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    for (name, o, n) in &prows {
        let flag = if (o - n).abs() > 0.005 { "  <-- CHANGED" } else { "" };
        println!("{name:16} {o:.2} -> {n:.2}{flag}");
    }

    let mut deltas: Vec<f64> = Vec::new();
    let mut dropped = 0;
    for p in &old.projects {
        for s in &p.output.grades.students {
            if let Some(ns) = new_student(p.project_id, &s.student_id) {
                let d = ns.student_final - s.student_final;
                deltas.push(d);
                if d < -0.005 {
                    dropped += 1;
                }
            }
        }
    }
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = deltas.len().max(1);
    let mean = deltas.iter().sum::<f64>() / n as f64;
    println!("\n=== cohort student deltas ===");
    println!(
        "students={}  mean=+{mean:.2}  min={:.2}  max=+{:.2}  DROPPED={dropped}",
        deltas.len(),
        deltas.first().copied().unwrap_or(0.0),
        deltas.last().copied().unwrap_or(0.0),
    );

    for tag in ["4c", "1b"] {
        let Some(p) = old
            .projects
            .iter()
            .find(|p| pname(p.project_id).contains(tag))
        else {
            continue;
        };
        let pid = p.project_id;
        println!(
            "\n=== {} (team {}) — project_final {:.2} ===",
            pname(pid),
            p.output.grades.team_size,
            p.output.grades.project_final
        );
        println!(
            "{:<20} {:>6} {:>6} {:>11} {:>13}",
            "student", "contr", "aikeep", "cqpen o->n", "final o->n  Δ"
        );
        let mut srows: Vec<_> = p.output.grades.students.iter().collect();
        srows.sort_by(|a, b| b.student_final.partial_cmp(&a.student_final).unwrap());
        for s in srows {
            let ns = new_student(pid, &s.student_id);
            let (cqn, fin_n) = ns
                .map(|x| (x.codequality_penalty, x.student_final))
                .unwrap_or((s.codequality_penalty, s.student_final));
            println!(
                "{:<20} {:>6.3} {:>6} {:>4.2}->{:<4.2} {:>5.2}->{:<5.2} {:+.2}",
                trunc(&name_of(pid, &s.student_id), 20),
                s.contribution.unwrap_or(0.0),
                s.ai_keep
                    .map(|k| format!("{k:.2}"))
                    .unwrap_or_else(|| "-".into()),
                s.codequality_penalty,
                cqn,
                s.student_final,
                fin_n,
                fin_n - s.student_final,
            );
        }
    }
}
