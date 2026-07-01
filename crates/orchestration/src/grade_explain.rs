//! Human-readable project grade breakdown from a live grading.db.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use grade_core::{compute_project_axes, grade_cohort, structural_scopes, GradeSpec};
use sprint_grader_core::Database;
use tracing::{info, warn};

use crate::grading_projection::load_cohort_raw_projects;
use crate::report_sync::android_repo_root;

/// Write one student-facing grade report (`GRADES.md`) per gradable project,
/// returning the paths written.
///
/// By default each `GRADES.md` lands beside the project's `REPORT.md` (the
/// Android repo clone under `entregues_dir/<project>/`); pass `out_dir` to
/// instead write every file flat into a single directory (one `GRADES.md` per
/// project would collide there, so the flat form is `notes_<project>.md`).
///
/// Ungradable / empty-shell projects are filtered out by `grade_cohort` and
/// produce no file; any name in `project_filter` that yields no report is logged
/// as a warning. Grades come from the same `load_cohort_raw_projects` +
/// `grade_cohort` path as [`explain_grades`], so the cohort normalization
/// matches a full run.
pub fn export_grade_markdown(
    db: &Database,
    today: &str,
    spec: &GradeSpec,
    project_filter: Option<&[String]>,
    entregues_dir: &Path,
    out_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let projects = load_cohort_raw_projects(db, today, project_filter)?;
    let cohort = grade_cohort(projects.as_slice(), spec)?;
    let ranks = grade_md::cohort_ranks_by_project_final(
        &cohort
            .projects
            .iter()
            .map(|p| (p.project_id, p.output.grades.project_final))
            .collect::<Vec<_>>(),
    );
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }

    let mut written = Vec::with_capacity(cohort.projects.len());
    let mut graded_names = Vec::with_capacity(cohort.projects.len());
    for pg in &cohort.projects {
        let raw = projects
            .iter()
            .find(|r| r.project_id == pg.project_id)
            .expect("raw project for graded cohort entry");
        let names: BTreeMap<String, String> = raw
            .students
            .iter()
            .map(|s| (s.student_id.clone(), s.full_name.clone()))
            .collect();

        let path = match out_dir {
            Some(dir) => dir.join(format!("notes_{}.md", slug(&raw.name))),
            None => match android_repo_root(entregues_dir, &raw.name) {
                Some(repo_root) => repo_root.join(grade_md::GRADES_FILENAME),
                None => {
                    warn!(
                        project = %raw.name,
                        "android repo clone not found; skipping GRADES.md (REPORT.md folder absent)"
                    );
                    continue;
                }
            },
        };
        let (work_base, work_base_present) =
            grade_md::ProjectGradeContext::work_base_from_grades(&pg.output.grades);
        let (cohort_rank, cohort_size) = ranks
            .get(&pg.project_id)
            .copied()
            .unwrap_or((1, cohort.projects.len().max(1)));
        let context = grade_md::ProjectGradeContext {
            work_base,
            work_base_present,
            cohort_rank,
            cohort_size,
        };
        grade_md::write_grades_markdown(
            &path,
            &raw.name,
            &names,
            &pg.output.grades,
            &raw.student_flags,
            &context,
            spec.meta.decimals,
        )?;
        info!(project = %raw.name, path = %path.display(), "wrote GRADES.md");
        graded_names.push(raw.name.clone());
        written.push(path);
    }

    if let Some(filter) = project_filter {
        for requested in filter {
            if !graded_names.iter().any(|n| n == requested) {
                warn!(
                    project = %requested,
                    "no GRADES.md written — project is ungradable or not found"
                );
            }
        }
    }
    Ok(written)
}

/// Filesystem-safe, lowercase slug of a project name (used only for the flat
/// `--out` form; the default per-project form uses the fixed `GRADES.md`).
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "project".to_string()
    } else {
        s
    }
}

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
        let g = &pg.output.grades;
        let (work_base, work_base_present) =
            grade_md::ProjectGradeContext::work_base_from_grades(g);
        let _ = writeln!(out, "\n=== {} (id={}) ===", raw.name, raw.project_id);
        let _ = writeln!(
            out,
            "project_final={:.2}  work_base={:.2} (present={})  [= structural {:.2} + extra_tech {:.2}]  ×  multiplier={:.3}  ai_factor={:.3}",
            g.project_final,
            work_base,
            work_base_present,
            g.work_base_structural,
            g.extra_tech,
            g.axes
                .iter()
                .find(|a| a.key == "quality_multiplier")
                .and_then(|a| a.score)
                .unwrap_or(0.0),
            g.ai_factor,
        );
        let sc = structural_scopes(raw, spec);
        let _ = writeln!(
            out,
            "  team_size={}  sum_raw={:.1}  sum_eff={:.2}  mean_raw={:.2}  team_quality_penalty={:.2}  (project_final = work_base×mult×ai_factor − team_quality_penalty)",
            pg.output.grades.team_size,
            sc.sum_raw,
            sc.sum_eff,
            sc.mean_raw,
            pg.output.grades.team_quality_penalty,
        );
        let _ = writeln!(
            out,
            "  (project_final = 10·(project_raw/10)^gamma, project_raw = work_base×mult×ai − team_quality_penalty)",
        );
        let axes = compute_project_axes(raw, &pg.normalized, &cohort.bounds, spec);
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
                "  {} raw_pts={:.1} eff_pts={:.2} contrib={:.3} ai_keep={:.2} | base={:.2} (=pf×contrib×size) beh_pen={:.2} cq_pen={:.2} final={:.2}",
                stu.student_id,
                stu.raw_points,
                stu.effective_points,
                stu.contribution.unwrap_or(0.0),
                stu.ai_keep.unwrap_or(1.0),
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
