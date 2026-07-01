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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/grading.db")
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

fn student_formula_value(
    out: &grade_core::CohortGradeOutput,
    projects: &[grade_core::RawProject],
    name_needle: &str,
    formula_name: &str,
) -> Option<f64> {
    for pg in &out.projects {
        let raw = projects.iter().find(|r| r.project_id == pg.project_id)?;
        for (stu, tree) in pg
            .output
            .grades
            .students
            .iter()
            .zip(pg.output.trees.students.iter())
        {
            let full = raw
                .students
                .iter()
                .find(|rs| rs.student_id == stu.student_id)
                .map(|rs| rs.full_name.as_str())
                .unwrap_or("");
            if !full.contains(name_needle) {
                continue;
            }
            return tree
                .formulas
                .iter()
                .find(|f| f.name == formula_name)
                .map(|f| f.node.value);
        }
    }
    None
}

#[test]
#[ignore = "calibrate anchor + project_gamma; run with --ignored --nocapture"]
fn calibrate_cabanes_anchor_and_sergi_floor() {
    let path = db_path();
    if !path.exists() {
        return;
    }
    let db = Database::open(&path).expect("open grading.db");
    let projects = load_cohort_raw_projects(&db, TODAY, None).expect("load projects");
    let mut spec = load_spec();
    let floor = 5.0f64;
    let tol = 0.5 * 10f64.powi(-(spec.meta.decimals as i32));

    // Anchor at Cabanes student_net so curved peaks at 10 for him.
    let probe = grade_cohort(&projects, &spec).expect("grade");
    let cabanes_net =
        student_formula_value(&probe, &projects, "Cabanes", "student_net").expect("cabanes net");
    spec.weights.insert("student_grade_anchor".into(), cabanes_net);
    println!("cabanes student_net anchor={cabanes_net:.4}");

    let mut best_gamma = spec
        .weights
        .get("project_grade_gamma")
        .copied()
        .unwrap_or(1.9785);
    let mut lo = 1.0f64;
    let mut hi = 3.0f64;
    for i in 0..40 {
        let mid = (lo + hi) / 2.0;
        spec.weights.insert("project_grade_gamma".into(), mid);
        let out = grade_cohort(&projects, &spec).expect("grade");
        let sergi = student_final_by_name(&out, &projects, "Fosas").unwrap_or(0.0);
        let cabanes = student_final_by_name(&out, &projects, "Cabanes").unwrap_or(0.0);
        let tens = students_at_ten(&out, &projects);
        println!(
            "iter={i:02} proj_g={mid:.4} sergi={sergi:.3} cabanes={cabanes:.3} n10={} {:?}",
            tens.len(),
            tens.iter().map(|s| s.split(',').next().unwrap_or(s)).collect::<Vec<_>>()
        );
        if sergi + tol < floor {
            hi = mid;
            continue;
        }
        best_gamma = mid;
        lo = mid;
    }
    spec.weights.insert("project_grade_gamma".into(), best_gamma);
    let final_out = grade_cohort(&projects, &spec).expect("grade");
    let sergi_f = student_final_by_name(&final_out, &projects, "Fosas").unwrap();
    let cabanes_f = student_final_by_name(&final_out, &projects, "Cabanes").unwrap();
    let tens_f = students_at_ten(&final_out, &projects);
    println!(
        "\nRECOMMENDED student_grade_anchor={cabanes_net:.4} project_grade_gamma={best_gamma:.4}"
    );
    println!("sergi={sergi_f:.3} cabanes={cabanes_f:.3} student_10s={:?}", tens_f);
    assert!(sergi_f + tol >= floor);
    assert_eq!(tens_f.len(), 1);
    assert!(tens_f[0].contains("Cabanes"));
    assert!(cabanes_f >= 9.995);
}

fn student_final_by_name(
    out: &grade_core::CohortGradeOutput,
    projects: &[grade_core::RawProject],
    name_needle: &str,
) -> Option<f64> {
    for pg in &out.projects {
        let raw = projects.iter().find(|r| r.project_id == pg.project_id)?;
        for stu in &pg.output.grades.students {
            let full = raw
                .students
                .iter()
                .find(|rs| rs.student_id == stu.student_id)
                .map(|rs| rs.full_name.as_str())
                .unwrap_or("");
            if full.contains(name_needle) {
                return Some(stu.student_final);
            }
        }
    }
    None
}

fn students_at_ten(
    out: &grade_core::CohortGradeOutput,
    projects: &[grade_core::RawProject],
) -> Vec<String> {
    let mut names = Vec::new();
    for pg in &out.projects {
        let raw = projects
            .iter()
            .find(|r| r.project_id == pg.project_id)
            .expect("raw");
        for stu in &pg.output.grades.students {
            if stu.student_final >= 9.995 {
                let full = raw
                    .students
                    .iter()
                    .find(|rs| rs.student_id == stu.student_id)
                    .map(|rs| rs.full_name.clone())
                    .unwrap_or_else(|| stu.student_id.clone());
                names.push(full);
            }
        }
    }
    names.sort();
    names
}

fn sergi_final(out: &grade_core::CohortGradeOutput, projects: &[grade_core::RawProject]) -> Option<f64> {
    for pg in &out.projects {
        let raw = projects.iter().find(|r| r.project_id == pg.project_id)?;
        if raw.name != "pds26-3b" {
            continue;
        }
        let stu = pg.output.grades.students.iter().find(|s| {
            raw.students
                .iter()
                .find(|rs| rs.student_id == s.student_id)
                .map(|rs| rs.full_name.contains("Fosas"))
                .unwrap_or(false)
        })?;
        return Some(stu.student_final);
    }
    None
}

fn count_tens(out: &grade_core::CohortGradeOutput) -> (usize, usize) {
    let mut project_tens = 0usize;
    let mut student_tens = 0usize;
    for pg in &out.projects {
        if pg.output.grades.project_final >= 9.995 {
            project_tens += 1;
        }
        for stu in &pg.output.grades.students {
            if stu.student_final >= 9.995 {
                student_tens += 1;
            }
        }
    }
    (project_tens, student_tens)
}

#[test]
#[ignore = "probe project_grade_gamma; run with --ignored --nocapture"]
fn gamma_anchor_sergi_fosas() {
    let path = db_path();
    if !path.exists() {
        eprintln!("skip: {} missing", path.display());
        return;
    }
    let db = Database::open(&path).expect("open grading.db");
    let mut spec = load_spec();
    let projects = load_cohort_raw_projects(&db, TODAY, None).expect("load projects");

    let baseline_gamma = spec.weights.get("project_grade_gamma").copied().unwrap_or(1.0);
    let baseline = grade_cohort(&projects, &spec).expect("grade cohort");
    let sergi0 = sergi_final(&baseline, &projects).expect("sergi in pds26-3b");
    let (p10_0, s10_0) = count_tens(&baseline);
    println!(
        "gamma={baseline_gamma:.4}  sergi={sergi0:.3}  project_10s={p10_0}  student_10s={s10_0}"
    );

    let mut best_gamma = baseline_gamma;
    let mut lo = 1.0f64;
    let mut hi = 3.0f64;
    let floor = 5.0f64;
    let tol = 0.5 * 10f64.powi(-(spec.meta.decimals as i32));
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        spec.weights.insert("project_grade_gamma".into(), mid);
        let out = grade_cohort(&projects, &spec).expect("grade cohort");
        let sergi = sergi_final(&out, &projects).expect("sergi");
        let (p10, s10) = count_tens(&out);
        println!("gamma={mid:.4} sergi={sergi:.3} project_10s={p10} student_10s={s10}");
        if sergi + tol >= floor {
            best_gamma = mid;
            lo = mid;
        } else {
            hi = mid;
        }
    }
    spec.weights.insert("project_grade_gamma".into(), best_gamma);
    let final_out = grade_cohort(&projects, &spec).expect("grade cohort");
    let sergi_f = sergi_final(&final_out, &projects).expect("sergi");
    let (p10_f, s10_f) = count_tens(&final_out);
    println!(
        "\nRECOMMENDED project_grade_gamma={best_gamma:.4}  sergi={sergi_f:.3}  project_10s={p10_f}  student_10s={s10_f}"
    );
    assert!(sergi_f + tol >= floor, "sergi below floor: {sergi_f}");
}

#[test]
#[ignore = "print student_net for cohort 10s; run with --ignored --nocapture"]
fn diagnose_students_at_ten() {
    let path = db_path();
    if !path.exists() {
        return;
    }
    let db = Database::open(&path).expect("open grading.db");
    let spec = load_spec();
    let projects = load_cohort_raw_projects(&db, TODAY, None).expect("load projects");
    let out = grade_cohort(&projects, &spec).expect("grade cohort");

    #[derive(Debug)]
    struct Row {
        name: String,
        project: String,
        project_final: f64,
        base_grade: f64,
        student_final: f64,
    }
    let mut rows = Vec::new();
    for pg in &out.projects {
        let raw = projects.iter().find(|r| r.project_id == pg.project_id).unwrap();
        for stu in &pg.output.grades.students {
            if stu.student_final < 9.995 {
                continue;
            }
            let full = raw
                .students
                .iter()
                .find(|rs| rs.student_id == stu.student_id)
                .map(|rs| rs.full_name.clone())
                .unwrap_or_else(|| stu.student_id.clone());
            rows.push(Row {
                name: full,
                project: raw.name.clone(),
                project_final: pg.output.grades.project_final,
                base_grade: stu.base_grade,
                student_final: stu.student_final,
            });
        }
    }
    rows.sort_by(|a, b| b.student_final.partial_cmp(&a.student_final).unwrap());
    for r in &rows {
        println!(
            "{:30} project={:12} project_final={:.2} base={:.2} student_final={:.2}",
            r.name, r.project, r.project_final, r.base_grade, r.student_final
        );
    }
    let sergi = student_final_by_name(&out, &projects, "Fosas").unwrap();
    let cabanes = student_final_by_name(&out, &projects, "Cabanes").unwrap();
    println!("sergi={sergi:.3} cabanes={cabanes:.3} count={}", rows.len());

    // All students with student_final >= 9.5 for spread inspection.
    println!("\n--- all students >= 9.5 ---");
    let mut all = Vec::new();
    for pg in &out.projects {
        let raw = projects.iter().find(|r| r.project_id == pg.project_id).unwrap();
        for stu in &pg.output.grades.students {
            if stu.student_final < 9.5 {
                continue;
            }
            let full = raw
                .students
                .iter()
                .find(|rs| rs.student_id == stu.student_id)
                .map(|rs| rs.full_name.clone())
                .unwrap_or_else(|| stu.student_id.clone());
            all.push((stu.student_final, stu.base_grade, full));
        }
    }
    all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for (fin, base, name) in all {
        println!("  {name:30} base={base:.2} final={fin:.2}");
    }
}

#[test]
#[ignore = "scan work_scale + gammas; run with --ignored --nocapture"]
fn scan_work_scale_and_gammas() {
    let path = db_path();
    if !path.exists() {
        return;
    }
    let db = Database::open(&path).expect("open grading.db");
    let projects = load_cohort_raw_projects(&db, TODAY, None).expect("load projects");
    let floor = 5.0f64;
    let tol = 0.5 * 10f64.powi(-2);

    for ws in [1.4925, 1.35, 1.25, 1.15, 1.05, 1.0, 0.95, 0.90] {
        for pg in [1.9785, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0] {
            let mut spec = load_spec();
            spec.weights.insert("work_scale".into(), ws);
            spec.weights.insert("project_grade_gamma".into(), pg);
            let out = grade_cohort(&projects, &spec).expect("grade");
            let sergi = student_final_by_name(&out, &projects, "Fosas").unwrap_or(0.0);
            let cabanes = student_final_by_name(&out, &projects, "Cabanes").unwrap_or(0.0);
            let boudad = student_final_by_name(&out, &projects, "Boudad").unwrap_or(0.0);
            let tens = students_at_ten(&out, &projects);
            let cabanes_base = out
                .projects
                .iter()
                .flat_map(|p| p.output.grades.students.iter())
                .find(|s| {
                    projects
                        .iter()
                        .flat_map(|r| r.students.iter())
                        .any(|rs| rs.student_id == s.student_id && rs.full_name.contains("Cabanes"))
                })
                .map(|s| s.base_grade)
                .unwrap_or(0.0);
            let boudad_base = out
                .projects
                .iter()
                .flat_map(|p| p.output.grades.students.iter())
                .find(|s| {
                    projects
                        .iter()
                        .flat_map(|r| r.students.iter())
                        .any(|rs| rs.student_id == s.student_id && rs.full_name.contains("Boudad"))
                })
                .map(|s| s.base_grade)
                .unwrap_or(0.0);
            if tens.len() <= 3 || (boudad_base <= cabanes_base && tens.len() <= 5) {
                println!(
                    "ws={ws:.3} pg={pg:.2} sergi={sergi:.2} cab={cabanes:.2}({cabanes_base:.2}) boud={boudad:.2}({boudad_base:.2}) n10={}",
                    tens.len()
                );
            }
            if sergi + tol >= floor
                && tens.len() == 1
                && tens[0].contains("Cabanes")
                && cabanes >= 9.995
            {
                println!("  >>> MATCH ws={ws} pg={pg}");
            }
        }
    }
}

#[test]
#[ignore = "probe project_grade_gamma for Sergi floor + sole Cabanes 10; run with --ignored --nocapture"]
fn gamma_anchor_sergi_and_sole_cabanes_ten() {
    let path = db_path();
    if !path.exists() {
        eprintln!("skip: {} missing", path.display());
        return;
    }
    let db = Database::open(&path).expect("open grading.db");
    let mut spec = load_spec();
    let projects = load_cohort_raw_projects(&db, TODAY, None).expect("load projects");

    let floor = 5.0f64;
    let tol = 0.5 * 10f64.powi(-(spec.meta.decimals as i32));
    let start = spec.weights.get("project_grade_gamma").copied().unwrap_or(1.0);

    let mut best_gamma = start;
    let mut lo = start;
    let mut hi = 6.0f64;

    for i in 0..48 {
        let mid = (lo + hi) / 2.0;
        spec.weights.insert("project_grade_gamma".into(), mid);
        let out = grade_cohort(&projects, &spec).expect("grade cohort");
        let sergi = student_final_by_name(&out, &projects, "Fosas").expect("sergi");
        let cabanes = student_final_by_name(&out, &projects, "Cabanes");
        let tens = students_at_ten(&out, &projects);
        let sole_cabanes = tens.len() == 1 && tens[0].contains("Cabanes");
        println!(
            "iter={i:02} gamma={mid:.4} sergi={sergi:.3} cabanes={} tens={:?}",
            cabanes.map(|g| format!("{g:.3}")).unwrap_or_else(|| "?".into()),
            tens,
        );

        if sergi + tol < floor {
            hi = mid;
            continue;
        }
        if tens.is_empty() {
            // Cabanes fell below 10 — too aggressive.
            hi = mid;
            continue;
        }
        if !sole_cabanes {
            lo = mid;
            continue;
        }
        best_gamma = mid;
        lo = mid;
    }

    spec.weights.insert("project_grade_gamma".into(), best_gamma);
    let final_out = grade_cohort(&projects, &spec).expect("grade cohort");
    let sergi_f = student_final_by_name(&final_out, &projects, "Fosas").expect("sergi");
    let cabanes_f = student_final_by_name(&final_out, &projects, "Cabanes").expect("cabanes");
    let tens_f = students_at_ten(&final_out, &projects);
    println!(
        "\nRECOMMENDED project_grade_gamma={best_gamma:.4}  sergi={sergi_f:.3}  cabanes={cabanes_f:.3}  student_10s={:?}",
        tens_f
    );
    assert!(sergi_f + tol >= floor, "sergi below floor: {sergi_f}");
    assert_eq!(tens_f.len(), 1, "expected exactly one 10: {tens_f:?}");
    assert!(
        tens_f[0].contains("Cabanes"),
        "expected Cabanes at 10, got {:?}",
        tens_f[0]
    );
}
