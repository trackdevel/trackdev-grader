//! Human-readable project grade breakdown from a live grading.db.

use std::fmt::Write as _;

use anyhow::Result;
use grade_core::{compute_project_axes, grade_cohort, GradeSpec};
use sprint_grader_core::Database;

use crate::grading_projection::load_cohort_raw_projects;

/// Build a text report for one or more projects (all when `project_filter` is None).
pub fn explain_grades(
    db: &Database,
    today: &str,
    spec: &GradeSpec,
    project_filter: Option<&[String]>,
) -> Result<String> {
    let projects = load_cohort_raw_projects(db, today, project_filter)?;
    let cohort = grade_cohort(&projects, spec)?;
    let mut out = String::new();

    for pg in &cohort.projects {
        let raw = projects
            .iter()
            .find(|r| r.project_id == pg.project_id)
            .expect("raw project");
        let charged: usize = pg
            .output
            .grades
            .students
            .iter()
            .filter(|s| s.codequality_penalty > 0.0)
            .count();
        let axes = compute_project_axes(raw, &pg.normalized, &cohort.bounds, spec);
        let _ = writeln!(out, "\n=== {} (id={}) ===", raw.name, raw.project_id);
        let _ = writeln!(
            out,
            "project_final={:.2}  work_base={:.2} (present={})  ×  multiplier={:.3}",
            pg.output.grades.project_final,
            axes.work_base,
            axes.work_base_present,
            axes.quality_multiplier,
        );
        let _ = writeln!(
            out,
            "  size={:.2} (present={})  complexity={:.2} (present={})  quality={:.2} (present={})  quality_eff={:.2}",
            axes.size,
            axes.size_present,
            axes.complexity,
            axes.complexity_present,
            axes.quality,
            axes.quality_present,
            axes.quality_eff,
        );
        let arch_w = raw.axis.arch_crit_count * 2.0 + raw.axis.arch_warn_count * 0.5;
        let arch_norm = spec.weights.get("arch_norm").copied().unwrap_or(143.0);
        let _ = writeln!(
            out,
            "axis inputs: cq_present={} mi={:.1} arch_present={} crit={:.0} warn={:.0} arch_weighted={:.1} arch_weighted/arch_norm={:.2}",
            raw.axis.cq_present,
            raw.axis.code_quality_raw,
            raw.axis.arch_present,
            raw.axis.arch_crit_count,
            raw.axis.arch_warn_count,
            arch_w,
            arch_w / arch_norm,
        );
        let prod_loc: f64 = raw
            .inventory
            .iter()
            .map(|r| r.metrics.get("production_loc").copied().unwrap_or(0.0))
            .sum();
        let prod_stmt: f64 = raw
            .inventory
            .iter()
            .map(|r| {
                r.metrics
                    .get("production_statement_count")
                    .copied()
                    .unwrap_or(0.0)
            })
            .sum();
        let _ = writeln!(
            out,
            "inventory: {} repo(s)  production_loc={:.0}  production_statement_count={:.0}",
            raw.inventory.len(),
            prod_loc,
            prod_stmt,
        );
        for repo in &raw.inventory {
            let _ = writeln!(out, "  {}", repo.repo_full_name);
            for key in [
                "endpoint_count",
                "fragment_count",
                "entity_count",
                "production_statement_count",
            ] {
                if let Some(v) = repo.metrics.get(key) {
                    let norm = pg.normalized.get(key).copied().unwrap_or(f64::NAN);
                    let _ = writeln!(out, "    {key}: raw={v:.1} norm={norm:.2}");
                }
            }
        }
        if raw.inventory.is_empty() {
            let _ = writeln!(
                out,
                "  (no metrics loaded — check repo_full_name alignment vs project_inventory_runs)"
            );
        }
        for stu in &pg.output.grades.students {
            let _ = writeln!(
                out,
                "  {} base={:.2} behavioural_pen={:.2} codequality_pen={:.2} final={:.2}",
                stu.student_id,
                stu.base_grade,
                stu.student_penalty,
                stu.codequality_penalty,
                stu.student_final,
            );
        }
        let _ = writeln!(
            out,
            "  codequality charged: {charged}/{} students",
            pg.output.grades.students.len(),
        );
    }
    let total_students: usize = cohort
        .projects
        .iter()
        .map(|p| p.output.grades.students.len())
        .sum();
    let total_charged: usize = cohort
        .projects
        .iter()
        .flat_map(|p| p.output.grades.students.iter())
        .filter(|s| s.codequality_penalty > 0.0)
        .count();
    if total_students > 0 {
        let _ = writeln!(
            out,
            "\ncohort codequality charged: {total_charged}/{total_students} ({:.0}%)",
            100.0 * total_charged as f64 / total_students as f64,
        );
    }
    Ok(out)
}
