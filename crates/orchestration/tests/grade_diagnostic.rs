//! Per-project grade diagnostic against grading.db (ignored by default).
//!
//!   cargo test -p sprint-grader-orchestration grade_diagnostic -- --ignored --nocapture

use std::fs;
use std::path::PathBuf;

use grade_core::{grade_cohort, GradeSpec};
use sprint_grader_core::Database;
use sprint_grader_orchestration::grading_projection::load_cohort_raw_projects;

const TODAY: &str = "2026-06-10";

fn load_spec() -> GradeSpec {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/grading.standard.json");
    let text = fs::read_to_string(path).expect("grading.standard.json");
    serde_json::from_str(&text).expect("parse spec")
}

fn db_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/entregues/grading.db")
}

#[test]
#[ignore]
fn grade_diagnostic_pds26_top_teams() {
    let path = db_path();
    let db = Database::open(&path).expect("open grading.db");
    let spec = load_spec();
    let projects = load_cohort_raw_projects(&db, TODAY, None).expect("load projects");
    let out = grade_cohort(&projects, &spec).expect("grade cohort");

    let mut rows: Vec<_> = out
        .projects
        .iter()
        .map(|p| {
            let raw = projects
                .iter()
                .find(|r| r.project_id == p.project_id)
                .expect("raw");
            (
                raw.name.clone(),
                p.output.grades.project_final,
                p.output.grades.axes.clone(),
                raw.axis.code_quality_raw,
                raw.axis.cq_present,
                raw.axis.arch_crit_count,
                raw.axis.arch_warn_count,
                raw.inventory.len(),
            )
        })
        .collect();
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    println!("\n=== project_final (sorted) ===");
    for (name, final_g, axes, mi, cq, crit, warn, inv) in &rows {
        println!(
            "{name:16} final={final_g:.2}  mi={mi:.1} cq={cq} arch_crit={crit} arch_warn={warn} inv_repos={inv}"
        );
        for ax in axes {
            if ax.present {
                println!(
                    "    axis {:12} raw={:?} score={:?}",
                    ax.key, ax.raw, ax.score
                );
            }
        }
    }

    for target in ["test", "pds26-1a", "pds26-1b"] {
        let Some(pg) = out.projects.iter().find(|p| {
            projects
                .iter()
                .find(|r| r.project_id == p.project_id)
                .map(|r| r.name.as_str())
                == Some(target)
        }) else {
            println!("\n{target}: NOT FOUND");
            continue;
        };
        let raw = projects
            .iter()
            .find(|r| r.project_id == pg.project_id)
            .expect("raw");
        let arch_w = raw.axis.arch_crit_count * 2.0 + raw.axis.arch_warn_count * 0.5;
        println!(
            "\n=== {target} detail ===\nproject_final={:.2} quality={:.2} complexity={:.2} size={:.2}",
            pg.output.grades.project_final,
            pg.output.grades.axes.iter().find(|a| a.key == "quality").and_then(|a| a.score).unwrap_or(0.0),
            pg.output.grades.axes.iter().find(|a| a.key == "complexity").and_then(|a| a.score).unwrap_or(0.0),
            pg.output.grades.axes.iter().find(|a| a.key == "size").and_then(|a| a.score).unwrap_or(0.0),
        );
        println!(
            "arch_weighted raw={arch_w:.1} (crit={} warn={}) arch_present={} repos_in_pr={} tasks={} students={}",
            raw.axis.arch_crit_count,
            raw.axis.arch_warn_count,
            raw.axis.arch_present,
            db.conn
                .query_row(
                    "SELECT COUNT(DISTINCT pr.repo_full_name) FROM pull_requests pr
                     JOIN pr_authors pa ON pa.pr_id = pr.id
                     JOIN students s ON s.id = pa.student_id
                     WHERE s.team_project_id = ?",
                    [pg.project_id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0),
            raw.tasks.len(),
            raw.students.len(),
        );
        for repo in &raw.inventory {
            println!("  repo {}", repo.repo_full_name);
            for (k, v) in &repo.metrics {
                let norm = pg.normalized.get(k).copied().unwrap_or(f64::NAN);
                println!("    {k}: raw={v:.2} norm={norm:.2}");
            }
        }
    }

    // Data completeness
    let cq_rows: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM student_sprint_quality WHERE avg_maintainability IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    println!("\nstudent_sprint_quality rows with MI: {cq_rows}");
}
