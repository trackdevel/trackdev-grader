//! Three-level staged grade driver: task → project → student.

use std::collections::BTreeMap;

use crate::axes::{
    absent_project_axes, compute_project_axes, normalize_project_all, ProjectAxisScores,
};
use crate::cohort::{compute_cohort_bounds, CohortGradeOutput, CohortProjectGrade};
use crate::formula::{eval, EvalError, Scope};
use crate::policy::has_gradable_artifact;
use crate::shape::{aggregate, resolve_tasks};
use crate::spec::{
    AxisGrade, CodeQualityComponent, GradeOutput, GradeSpec, GradeTrees, NamedNode, ProjectGrades,
    StudentGrades, StudentTree, TaskTree,
};
use crate::types::{ProjectScopes, RawProject, TaskScope};

struct CohortCtx {
    axes: ProjectAxisScores,
    /// Per-(project, student) breakdown of the code-quality penalty; the capped
    /// sum of each list's `points` is the student's `codequality_penalty`.
    codequality_components: BTreeMap<(i64, String), Vec<CodeQualityComponent>>,
}

/// Grade an entire cohort: compute hybrid bounds from gradable projects, then grade
/// each gradable project with the current formula spec.
pub fn grade_cohort(
    projects: &[RawProject],
    spec: &GradeSpec,
) -> Result<CohortGradeOutput, EvalError> {
    let gradable: Vec<&RawProject> = projects
        .iter()
        .filter(|p| has_gradable_artifact(p))
        .collect();
    let gradable_owned: Vec<RawProject> = gradable.iter().map(|p| (*p).clone()).collect();
    let bounds = compute_cohort_bounds(&gradable_owned, spec);

    let mut scoped = Vec::with_capacity(gradable.len());
    for raw in &gradable {
        let scopes = project_scopes(raw, spec)?;
        scoped.push((raw.project_id, scopes));
    }
    let cq_components = codequality_penalty_components(&scoped, &spec.weights);

    let mut graded = Vec::with_capacity(gradable.len());
    for raw in &gradable {
        let normalized = normalize_project_all(raw, &bounds);
        let axes = compute_project_axes(raw, &normalized, &bounds, spec);
        let scopes = scoped
            .iter()
            .find(|(pid, _)| *pid == raw.project_id)
            .map(|(_, s)| s)
            .expect("scopes for gradable project");
        let output = grade_project_with_scopes(
            raw,
            spec,
            scopes,
            Some(&CohortCtx {
                axes,
                codequality_components: cq_components.clone(),
            }),
        )?;
        graded.push(CohortProjectGrade {
            project_id: raw.project_id,
            output,
            normalized,
        });
    }
    Ok(CohortGradeOutput {
        bounds,
        projects: graded,
    })
}

pub fn grade(raw: &RawProject, spec: &GradeSpec) -> Result<GradeOutput, EvalError> {
    if !has_gradable_artifact(raw) {
        let scopes = project_scopes(raw, spec)?;
        return grade_project_with_scopes(
            raw,
            spec,
            &scopes,
            Some(&CohortCtx {
                axes: absent_project_axes(),
                codequality_components: BTreeMap::new(),
            }),
        );
    }
    let out = grade_cohort(std::slice::from_ref(raw), spec)?;
    out.projects
        .into_iter()
        .next()
        .map(|p| p.output)
        .ok_or_else(|| EvalError::Domain {
            message: "grade_cohort returned no projects".into(),
        })
}

fn project_scopes(raw: &RawProject, spec: &GradeSpec) -> Result<ProjectScopes, EvalError> {
    let maps = spec.ai_maps();
    let resolved = resolve_tasks(raw, &maps);
    let weights = weights_with_constants(spec);
    let mut task_keeps = Vec::with_capacity(resolved.len());
    for task_scope in &resolved {
        let mut scope = task_scope_to_formula_scope(task_scope, &weights);
        for fd in &spec.formulas.task {
            let node = eval(&fd.expr, &scope, &fd.name, &fd.infix)?;
            scope.insert(fd.name.clone(), node.value);
        }
        let keep = scope.get("keep").copied().unwrap_or(1.0);
        task_keeps.push((task_scope.clone(), keep));
    }
    Ok(aggregate(raw, &task_keeps, &spec.aggregate_knobs()))
}

/// Cohort-wide per-student code-quality penalty from percentile bands per signal.
/// The three blame signals, paired with their `StudentScope` field index used
/// by the ranking loop (3 = arch, 4 = cx, 5 = sa) and the display name.
const CQ_DIMENSIONS: [(usize, &str); 3] = [
    (3, "architecture"),
    (4, "complexity"),
    (5, "static_analysis"),
];

/// Per-(project, student) breakdown of the code-quality penalty. For each blame
/// signal, students are ranked cohort-wide by blame-per-point; the top
/// `cq_crit_pct` land in the critical band and the next `cq_warn_pct` in the
/// warning band, each contributing `cq_crit_pts` / `cq_warn_pts`. The overall
/// `cq_cap` is applied later when the components are summed (see
/// `grade_project_with_scopes`), so it is intentionally not used here.
fn codequality_penalty_components(
    scoped: &[(i64, ProjectScopes)],
    weights: &BTreeMap<String, f64>,
) -> BTreeMap<(i64, String), Vec<CodeQualityComponent>> {
    let cq_min_points = weights.get("cq_min_points").copied().unwrap_or(1.0);
    let cq_abs_floor = weights.get("cq_abs_floor").copied().unwrap_or(0.0);
    let cq_crit_pct = weights.get("cq_crit_pct").copied().unwrap_or(0.10);
    let cq_warn_pct = weights.get("cq_warn_pct").copied().unwrap_or(0.30);
    let cq_crit_pts = weights.get("cq_crit_pts").copied().unwrap_or(1.0);
    let cq_warn_pts = weights.get("cq_warn_pts").copied().unwrap_or(0.5);

    let mut rows: Vec<(i64, String, f64, f64, f64, f64)> = Vec::new();
    for (project_id, scopes) in scoped {
        for stu in &scopes.students {
            rows.push((
                *project_id,
                stu.student_id.clone(),
                stu.student_eff,
                stu.arch_blame,
                stu.cx_blame,
                stu.sa_blame,
            ));
        }
    }

    let mut out: BTreeMap<(i64, String), Vec<CodeQualityComponent>> = BTreeMap::new();
    for (blame_idx, dimension) in CQ_DIMENSIONS {
        let mut ranked: Vec<(i64, String, f64, f64)> = rows
            .iter()
            .filter_map(|(pid, sid, eff, arch, cx, sa)| {
                let blame = match blame_idx {
                    3 => *arch,
                    4 => *cx,
                    _ => *sa,
                };
                if blame <= 0.0 {
                    return None;
                }
                let bpp = blame / eff.max(cq_min_points);
                if bpp < cq_abs_floor {
                    return None;
                }
                Some((*pid, sid.clone(), blame, bpp))
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.3.partial_cmp(&a.3)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        let n = ranked.len();
        if n == 0 {
            continue;
        }
        let crit_cutoff = (n as f64 * cq_crit_pct).ceil() as usize;
        let warn_cutoff = (n as f64 * cq_warn_pct).ceil() as usize;
        for (i, (pid, sid, blame, bpp)) in ranked.iter().enumerate() {
            let (tier, points) = if i < crit_cutoff {
                ("critical", cq_crit_pts)
            } else if i < warn_cutoff {
                ("warning", cq_warn_pts)
            } else {
                ("", 0.0)
            };
            if points > 0.0 {
                out.entry((*pid, sid.clone()))
                    .or_default()
                    .push(CodeQualityComponent {
                        dimension: dimension.to_string(),
                        blame: *blame,
                        blame_per_point: *bpp,
                        tier: tier.to_string(),
                        points,
                    });
            }
        }
    }
    out
}

fn grade_project_with_scopes(
    raw: &RawProject,
    spec: &GradeSpec,
    scopes: &ProjectScopes,
    cohort: Option<&CohortCtx>,
) -> Result<GradeOutput, EvalError> {
    let maps = spec.ai_maps();
    let resolved = resolve_tasks(raw, &maps);
    let weights = weights_with_constants(spec);

    let mut task_keeps: Vec<(TaskScope, f64)> = Vec::with_capacity(resolved.len());
    let mut task_trees = Vec::with_capacity(resolved.len());

    for task_scope in &resolved {
        let mut scope = task_scope_to_formula_scope(task_scope, &weights);
        let mut keep_node = None;
        for fd in &spec.formulas.task {
            let node = eval(&fd.expr, &scope, &fd.name, &fd.infix)?;
            scope.insert(fd.name.clone(), node.value);
            if fd.name == "keep" {
                keep_node = Some(node);
            }
        }
        let keep = scope.get("keep").copied().unwrap_or(1.0);
        let node = keep_node.ok_or_else(|| EvalError::Domain {
            message: "task formulas must define keep".into(),
        })?;
        task_trees.push(TaskTree {
            assignee_id: task_scope.assignee_id.clone(),
            raw_points: task_scope.raw_points,
            keep,
            node,
        });
        task_keeps.push((task_scope.clone(), keep));
    }

    let manual = spec.manual_field_values(raw.project_id);
    let axis_scores = cohort.map(|c| &c.axes);
    let mut project_scope = build_project_scope(raw, scopes, &weights, &manual, axis_scores);
    let mut project_tree = Vec::new();
    for fd in &spec.formulas.project {
        let node = eval(&fd.expr, &project_scope, &fd.name, &fd.infix)?;
        project_scope.insert(fd.name.clone(), node.value);
        project_tree.push(NamedNode {
            name: fd.name.clone(),
            node,
        });
    }

    let quality_grade = project_scope.get("quality").copied().unwrap_or_else(|| {
        project_scope
            .get("quality_composite")
            .copied()
            .unwrap_or(0.0)
    });
    let project_final_raw = project_scope.get("project_final").copied().unwrap_or(0.0);

    let axes = build_axis_grades(axis_scores, &project_scope);

    let mut student_grades = Vec::new();
    let mut student_trees = Vec::new();

    for stu in &scopes.students {
        let raw_points: f64 = task_keeps
            .iter()
            .filter(|(t, _)| t.assignee_id == stu.student_id)
            .map(|(t, _)| t.raw_points)
            .sum();

        let cq_components = cohort
            .and_then(|c| {
                c.codequality_components
                    .get(&(raw.project_id, stu.student_id.clone()))
                    .cloned()
            })
            .unwrap_or_default();
        let cq_cap = spec.weights.get("cq_cap").copied().unwrap_or(3.0);
        // Clamp to [0, cap]; `.max(0.0)` also normalises the empty-sum -0.0.
        let cq_penalty = cq_components
            .iter()
            .map(|c| c.points)
            .sum::<f64>()
            .min(cq_cap)
            .max(0.0);

        let mut stu_scope = project_scope.clone();
        stu_scope.insert("student_eff".into(), stu.student_eff);
        stu_scope.insert("student_critical_count".into(), stu.student_critical_count);
        stu_scope.insert("codequality_penalty".into(), cq_penalty);
        if let Some(k) = stu.ai_keep {
            stu_scope.insert("ai_keep".into(), k);
        }
        let contribution = stu.contribution.unwrap_or(0.0);
        stu_scope.insert("contribution".into(), contribution);
        stu_scope.insert("student_contribution".into(), contribution);

        let mut formulas = Vec::new();
        for fd in &spec.formulas.student {
            let node = eval(&fd.expr, &stu_scope, &fd.name, &fd.infix)?;
            stu_scope.insert(fd.name.clone(), node.value);
            formulas.push(NamedNode {
                name: fd.name.clone(),
                node,
            });
        }

        let base_grade = stu_scope.get("student_base").copied().unwrap_or(0.0);
        let student_penalty = stu_scope.get("student_penalty").copied().unwrap_or(0.0);
        let student_final_raw = stu_scope.get("student_final").copied().unwrap_or(0.0);

        let student_final = if stu.student_eff <= 0.0 {
            0.0
        } else {
            round_grade(
                student_final_raw,
                spec.meta.decimals,
                spec.meta.quantize_final,
            )
        };

        student_grades.push(StudentGrades {
            student_id: stu.student_id.clone(),
            raw_points,
            effective_points: stu.student_eff,
            ai_keep: stu.ai_keep,
            contribution: stu.contribution,
            base_grade: round_grade(base_grade, spec.meta.decimals, spec.meta.quantize_final),
            student_penalty,
            codequality_penalty: cq_penalty,
            codequality_components: cq_components,
            student_final,
        });
        student_trees.push(StudentTree {
            student_id: stu.student_id.clone(),
            formulas,
        });
    }

    let grades = ProjectGrades {
        project_id: raw.project_id,
        quality_grade: round_grade(quality_grade, spec.meta.decimals, spec.meta.quantize_final),
        quality_penalized: round_grade(quality_grade, spec.meta.decimals, spec.meta.quantize_final),
        project_penalty: 0.0,
        ai_factor: scopes.ai_factor,
        project_final: round_grade(
            project_final_raw,
            spec.meta.decimals,
            spec.meta.quantize_final,
        ),
        team_size: raw.team_size,
        axes,
        students: student_grades,
    };

    Ok(GradeOutput {
        grades,
        trees: GradeTrees {
            project: project_tree,
            students: student_trees,
            tasks: task_trees,
        },
    })
}

/// Weights plus user-defined constants, for formula-scope injection. Constants
/// never clobber a real weight (a name collision is a spec-validation error),
/// matching the defensive `or_insert` used for manual fields.
fn weights_with_constants(spec: &GradeSpec) -> BTreeMap<String, f64> {
    let mut w = spec.weights.clone();
    for (k, v) in spec.constant_values() {
        w.entry(k).or_insert(v);
    }
    w
}

fn task_scope_to_formula_scope(task: &TaskScope, weights: &BTreeMap<String, f64>) -> Scope {
    let mut scope = weights
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect::<Scope>();
    scope.insert("raw_points".into(), task.raw_points);
    scope.insert("model_m".into(), task.model_m);
    scope.insert("level_l".into(), task.level_l);
    scope.insert("declared".into(), if task.declared { 1.0 } else { 0.0 });
    scope
}

fn build_project_scope(
    raw: &RawProject,
    scopes: &ProjectScopes,
    weights: &BTreeMap<String, f64>,
    manual: &BTreeMap<String, f64>,
    axis_scores: Option<&ProjectAxisScores>,
) -> Scope {
    let mut scope = weights
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect::<Scope>();
    let a = &raw.axis;
    scope.insert("documentation_raw".into(), a.documentation_raw);
    scope.insert("code_quality_raw".into(), a.code_quality_raw);
    scope.insert("cc_pct".into(), a.cc_pct);
    scope.insert("mutation_score".into(), a.mutation_score);
    scope.insert("survival_raw".into(), a.survival_raw);
    scope.insert("arch_crit_count".into(), a.arch_crit_count);
    scope.insert("arch_warn_count".into(), a.arch_warn_count);
    scope.insert("doc_present".into(), bool01(a.doc_present));
    scope.insert("cq_present".into(), bool01(a.cq_present));
    scope.insert("surv_present".into(), bool01(a.surv_present));
    scope.insert("arch_present".into(), bool01(a.arch_present));
    scope.insert("team_size".into(), raw.team_size as f64);
    scope.insert("sum_raw".into(), scopes.sum_raw);
    scope.insert("sum_eff".into(), scopes.sum_eff);
    scope.insert("mean_raw".into(), scopes.mean_raw);
    scope.insert("ai_factor".into(), scopes.ai_factor);
    scope.insert("crit_sa_count".into(), scopes.crit_sa_count);
    scope.insert("crit_security_count".into(), scopes.crit_security_count);
    scope.insert("crit_cx_count".into(), scopes.crit_cx_count);
    scope.insert("penalty_on".into(), scopes.penalty_on);
    if let Some(ax) = axis_scores {
        scope.insert("quality".into(), ax.quality);
        scope.insert("complexity".into(), ax.complexity);
        scope.insert("size".into(), ax.size);
        scope.insert("work_base".into(), ax.work_base);
        scope.insert("quality_eff".into(), ax.quality_eff);
        scope.insert("quality_multiplier".into(), ax.quality_multiplier);
        scope.insert("quality_present".into(), bool01(ax.quality_present));
        scope.insert("complexity_present".into(), bool01(ax.complexity_present));
        scope.insert("size_present".into(), bool01(ax.size_present));
        scope.insert("work_base_present".into(), bool01(ax.work_base_present));
    }
    // Manual per-project fields, injected last. `or_insert` is defensive: a
    // name collision (which the spec validator rejects) can never clobber a
    // weight/raw/structural variable — the manual field is dropped instead.
    for (k, v) in manual {
        scope.entry(k.clone()).or_insert(*v);
    }
    scope
}

fn build_axis_grades(
    axis_scores: Option<&ProjectAxisScores>,
    project_scope: &Scope,
) -> Vec<AxisGrade> {
    if let Some(ax) = axis_scores {
        return vec![
            AxisGrade {
                key: "size".into(),
                raw: None,
                score: Some(ax.size),
                present: ax.size_present,
            },
            AxisGrade {
                key: "complexity".into(),
                raw: None,
                score: Some(ax.complexity),
                present: ax.complexity_present,
            },
            AxisGrade {
                key: "quality".into(),
                raw: None,
                score: Some(ax.quality),
                present: ax.quality_present,
            },
            AxisGrade {
                key: "work_base".into(),
                raw: None,
                score: Some(ax.work_base),
                present: ax.work_base_present,
            },
            AxisGrade {
                key: "quality_multiplier".into(),
                raw: None,
                score: Some(ax.quality_multiplier),
                present: ax.work_base_present,
            },
        ];
    }
    // Legacy v1 axis keys when cohort context is absent (should not occur in production).
    vec![AxisGrade {
        key: "quality".into(),
        raw: None,
        score: project_scope.get("quality_composite").copied(),
        present: project_scope.get("quality_composite").is_some(),
    }]
}

fn bool01(b: bool) -> f64 {
    if b {
        1.0
    } else {
        0.0
    }
}

pub fn round_grade(value: f64, decimals: u32, quantize_final: f64) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    let rounded = (value * factor).round() / factor;
    if quantize_final > 0.0 {
        (rounded / quantize_final).round() * quantize_final
    } else {
        rounded
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::types::{AxisInputs, RawStudent, RepoMetrics};

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/tests/fixtures")
    }

    fn load_spec() -> GradeSpec {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/grading.standard.json");
        let text = fs::read_to_string(path).expect("grading.standard.json");
        serde_json::from_str(&text).expect("parse spec")
    }

    #[test]
    fn parity_reference_grades_within_half_ulp() {
        let spec = load_spec();
        let raw_path = fixture_dir().join("reference.raw_projects.json");
        let grades_path = fixture_dir().join("reference.grades.json");
        if !raw_path.exists() || !grades_path.exists() {
            eprintln!("skip: reference fixtures missing");
            return;
        }
        let raws: Vec<RawProject> =
            serde_json::from_str(&fs::read_to_string(raw_path).unwrap()).unwrap();
        let expected: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(grades_path).unwrap()).unwrap();
        let tol = 0.5 * 10f64.powi(-(spec.meta.decimals as i32));

        let cohort = grade_cohort(&raws, &spec).expect("grade_cohort");
        for (raw, exp) in raws.iter().zip(expected.iter()) {
            let out = cohort
                .projects
                .iter()
                .find(|p| p.project_id == raw.project_id)
                .map(|p| &p.output)
                .unwrap_or_else(|| panic!("missing cohort grade for {}", raw.project_id));
            let pid = raw.project_id;
            let proj = &exp["project"];
            assert_close(
                out.grades.quality_grade,
                proj["quality_grade"].as_f64().unwrap(),
                tol,
                pid,
                "quality_grade",
            );
            assert_close(
                out.grades.quality_penalized,
                proj["quality_penalized"].as_f64().unwrap(),
                tol,
                pid,
                "quality_penalized",
            );
            assert_close(
                out.grades.project_final,
                proj["final_grade"].as_f64().unwrap(),
                tol,
                pid,
                "project_final",
            );

            for exp_stu in exp["students"].as_array().unwrap() {
                let sid = exp_stu["student_id"].as_str().unwrap();
                let stu = out
                    .grades
                    .students
                    .iter()
                    .find(|s| s.student_id == sid)
                    .unwrap_or_else(|| panic!("missing student {sid}"));
                assert_close(
                    stu.raw_points,
                    exp_stu["raw_points"].as_f64().unwrap(),
                    tol,
                    pid,
                    &format!("{sid}.raw_points"),
                );
                assert_close(
                    stu.effective_points,
                    exp_stu["effective_points"].as_f64().unwrap(),
                    tol,
                    pid,
                    &format!("{sid}.effective_points"),
                );
                assert_close(
                    stu.base_grade,
                    exp_stu["base_grade"].as_f64().unwrap(),
                    tol,
                    pid,
                    &format!("{sid}.base_grade"),
                );
                assert_close(
                    stu.student_penalty,
                    exp_stu["student_penalty"].as_f64().unwrap(),
                    tol,
                    pid,
                    &format!("{sid}.student_penalty"),
                );
                if let Some(cq) = exp_stu.get("codequality_penalty").and_then(|v| v.as_f64()) {
                    assert_close(
                        stu.codequality_penalty,
                        cq,
                        tol,
                        pid,
                        &format!("{sid}.codequality_penalty"),
                    );
                }
                assert_close(
                    stu.student_final,
                    exp_stu["final_grade"].as_f64().unwrap(),
                    tol,
                    pid,
                    &format!("{sid}.final_grade"),
                );
            }
        }
    }

    fn assert_close(actual: f64, expected: f64, tol: f64, pid: i64, field: &str) {
        assert!(
            (actual - expected).abs() <= tol,
            "project {pid} {field}: got {actual}, expected {expected}, tol {tol}"
        );
    }

    #[test]
    fn empty_shell_without_inventory_scores_zero_project_final() {
        let spec = load_spec();
        let raw = RawProject {
            project_id: 1,
            name: "test".into(),
            team_size: 3,
            axis: AxisInputs {
                documentation_raw: 0.0,
                doc_present: false,
                code_quality_raw: 0.0,
                cc_pct: 0.0,
                mutation_score: 0.0,
                cq_present: false,
                survival_raw: 0.0,
                surv_present: false,
                arch_crit_count: 0.0,
                arch_warn_count: 0.0,
                arch_present: true,
            },
            inventory: vec![],
            tasks: vec![],
            students: vec![],
            crit_findings: vec![],
            student_flags: vec![],
        };
        let out = grade(&raw, &spec).unwrap();
        assert!((out.grades.project_final - 0.0).abs() < 1e-9);
        assert!(!out.grades.axes.iter().any(|a| a.present));
    }

    #[test]
    fn student_eff_zero_yields_zero_final() {
        let spec = load_spec();
        let raw = RawProject {
            project_id: 99,
            name: "t".into(),
            team_size: 1,
            axis: AxisInputs {
                documentation_raw: 4.0,
                doc_present: true,
                code_quality_raw: 0.0,
                cc_pct: 0.0,
                mutation_score: 0.0,
                cq_present: false,
                survival_raw: 0.0,
                surv_present: false,
                arch_crit_count: 0.0,
                arch_warn_count: 0.0,
                arch_present: true,
            },
            inventory: vec![],
            tasks: vec![],
            students: vec![RawStudent {
                student_id: "solo".into(),
                full_name: "Solo".into(),
            }],
            crit_findings: vec![],
            student_flags: vec![],
        };
        let out = grade(&raw, &spec).unwrap();
        assert!((out.grades.students[0].student_final - 0.0).abs() < 1e-9);
    }

    fn mk_raw(project_id: i64) -> RawProject {
        RawProject {
            project_id,
            name: "t".into(),
            team_size: 1,
            axis: AxisInputs {
                documentation_raw: 4.0,
                doc_present: true,
                code_quality_raw: 0.0,
                cc_pct: 0.0,
                mutation_score: 0.0,
                cq_present: false,
                survival_raw: 0.0,
                surv_present: false,
                arch_crit_count: 0.0,
                arch_warn_count: 0.0,
                arch_present: true,
            },
            inventory: vec![],
            tasks: vec![],
            students: vec![RawStudent {
                student_id: "solo".into(),
                full_name: "Solo".into(),
            }],
            crit_findings: vec![],
            student_flags: vec![],
        }
    }

    #[test]
    fn manual_field_values_apply_override_then_default() {
        use crate::spec::{ManualFieldDef, ManualFields};
        let mut spec = load_spec();
        let mut row = BTreeMap::new();
        row.insert("team_bonus".to_string(), 2.0);
        let mut values = BTreeMap::new();
        values.insert("99".to_string(), row);
        spec.manual_fields = ManualFields {
            defs: vec![
                ManualFieldDef {
                    name: "team_bonus".into(),
                    value: 1.0,
                    description: String::new(),
                },
                ManualFieldDef {
                    name: "oral".into(),
                    value: 0.5,
                    description: String::new(),
                },
            ],
            values,
        };
        // Project 99 overrides team_bonus, inherits oral's default.
        let m99 = spec.manual_field_values(99);
        assert_eq!(m99.get("team_bonus"), Some(&2.0));
        assert_eq!(m99.get("oral"), Some(&0.5));
        // Project 100 has no overrides → both defaults.
        let m100 = spec.manual_field_values(100);
        assert_eq!(m100.get("team_bonus"), Some(&1.0));
        assert_eq!(m100.get("oral"), Some(&0.5));
    }

    #[test]
    fn manual_field_override_reaches_project_final() {
        use crate::spec::{FormulaDef, ManualFieldDef, ManualFields};
        let mut spec = load_spec();
        let mut row = BTreeMap::new();
        row.insert("team_bonus".to_string(), 2.0);
        let mut values = BTreeMap::new();
        values.insert("99".to_string(), row);
        spec.manual_fields = ManualFields {
            defs: vec![ManualFieldDef {
                name: "team_bonus".into(),
                value: 1.0,
                description: String::new(),
            }],
            values,
        };
        // Wire the manual field straight into project_final.
        let pf: FormulaDef = serde_json::from_str(
            r#"{"name":"project_final","infix":"team_bonus","expr":{"op":"var","name":"team_bonus"}}"#,
        )
        .unwrap();
        let idx = spec
            .formulas
            .project
            .iter()
            .position(|f| f.name == "project_final")
            .unwrap();
        spec.formulas.project[idx] = pf;

        // Project 99 uses the override (2.0); project 100 falls back to the default (1.0).
        let out99 = grade(&mk_raw(99), &spec).unwrap();
        assert!((out99.grades.project_final - 2.0).abs() < 1e-9);
        let out100 = grade(&mk_raw(100), &spec).unwrap();
        assert!((out100.grades.project_final - 1.0).abs() < 1e-9);
    }

    #[test]
    fn empty_manual_fields_inject_nothing() {
        let spec = load_spec();
        assert!(spec.manual_field_values(99).is_empty());
    }

    #[test]
    fn constant_reaches_project_final() {
        use crate::spec::{ConstantDef, FormulaDef};
        let mut spec = load_spec();
        spec.constants.push(ConstantDef {
            name: "bonus_k".into(),
            value: 3.0,
            description: String::new(),
        });
        // Wire the constant straight into project_final.
        let pf: FormulaDef = serde_json::from_str(
            r#"{"name":"project_final","infix":"bonus_k","expr":{"op":"var","name":"bonus_k"}}"#,
        )
        .unwrap();
        let idx = spec
            .formulas
            .project
            .iter()
            .position(|f| f.name == "project_final")
            .unwrap();
        spec.formulas.project[idx] = pf;

        let out = grade(&mk_raw(99), &spec).unwrap();
        assert!((out.grades.project_final - 3.0).abs() < 1e-9);
    }

    #[test]
    fn constant_does_not_clobber_a_weight() {
        use crate::spec::ConstantDef;
        let mut spec = load_spec();
        let floor = spec.weights.get("floor_keep").copied().expect("floor_keep");
        spec.constants.push(ConstantDef {
            name: "floor_keep".into(),
            value: 999.0,
            description: String::new(),
        });
        let merged = weights_with_constants(&spec);
        assert_eq!(merged.get("floor_keep").copied(), Some(floor));
    }

    #[test]
    fn codequality_penalty_ranks_per_point_and_caps() {
        use crate::policy::ARCHITECTURE_HOTSPOT;
        use crate::types::{RepoMetrics, StudentFlag};
        use std::collections::BTreeMap;

        let mut spec = load_spec();
        spec.weights.insert("cq_min_points".into(), 1.0);
        spec.weights.insert("cq_abs_floor".into(), 0.0);
        spec.weights.insert("cq_crit_pct".into(), 0.10);
        spec.weights.insert("cq_warn_pct".into(), 0.30);
        spec.weights.insert("cq_crit_pts".into(), 1.0);
        spec.weights.insert("cq_warn_pts".into(), 0.5);
        spec.weights.insert("cq_cap".into(), 3.0);

        let inv = || {
            vec![RepoMetrics {
                repo_full_name: "r".into(),
                metrics: BTreeMap::from([("production_loc".into(), 1000.0)]),
            }]
        };

        // Ten gradable students with nonzero arch blame; top bpp → crit, next two → warn.
        let mut projects = Vec::new();
        for (pid, bpp_target) in [
            (1, 5.0),
            (2, 4.0),
            (3, 3.0),
            (4, 2.0),
            (5, 1.0),
            (6, 0.9),
            (7, 0.8),
            (8, 0.7),
            (9, 0.6),
            (10, 0.5),
        ]
        .iter()
        {
            let (pid, bpp) = (*pid, *bpp_target);
            let eff = 10.0;
            let blame = bpp * eff;
            let sid = format!("s{pid}");
            projects.push(RawProject {
                project_id: pid,
                name: format!("p{pid}"),
                team_size: 1,
                axis: AxisInputs {
                    documentation_raw: 0.0,
                    doc_present: false,
                    code_quality_raw: 0.0,
                    cc_pct: 0.0,
                    mutation_score: 0.0,
                    cq_present: false,
                    survival_raw: 0.0,
                    surv_present: false,
                    arch_crit_count: 0.0,
                    arch_warn_count: 0.0,
                    arch_present: false,
                },
                inventory: inv(),
                tasks: vec![crate::types::RawTask {
                    assignee_id: sid.clone(),
                    raw_points: eff,
                    ai_model: Some("Cap".into()),
                    ai_level: Some("A".into()),
                    declared: true,
                }],
                students: vec![RawStudent {
                    student_id: sid.clone(),
                    full_name: sid.clone(),
                }],
                crit_findings: vec![],
                student_flags: vec![StudentFlag {
                    student_id: sid,
                    severity: "CRITICAL".into(),
                    source: "artifact".into(),
                    flag_type: ARCHITECTURE_HOTSPOT.into(),
                    weighted: Some(blame),
                }],
            });
        }

        let cohort = grade_cohort(&projects, &spec).unwrap();
        let pen = |sid: &str| {
            cohort
                .projects
                .iter()
                .flat_map(|p| p.output.grades.students.iter())
                .find(|s| s.student_id == sid)
                .unwrap()
                .codequality_penalty
        };
        assert!((pen("s1") - 1.0).abs() < 1e-9);
        assert!((pen("s2") - 0.5).abs() < 1e-9);
        assert!((pen("s3") - 0.5).abs() < 1e-9);
        assert!((pen("s4") - 0.0).abs() < 1e-9);
        assert!((pen("s10") - 0.0).abs() < 1e-9);

        // The per-signal breakdown explains each penalty: the capped sum of a
        // student's component points equals codequality_penalty.
        let comps = |sid: &str| {
            cohort
                .projects
                .iter()
                .flat_map(|p| p.output.grades.students.iter())
                .find(|s| s.student_id == sid)
                .unwrap()
                .codequality_components
                .clone()
        };
        let c1 = comps("s1");
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].dimension, "architecture");
        assert_eq!(c1[0].tier, "critical");
        assert!((c1[0].points - 1.0).abs() < 1e-9);
        assert!((c1[0].blame - 50.0).abs() < 1e-9); // bpp 5.0 * eff 10.0
        let c2 = comps("s2");
        assert_eq!(c2[0].tier, "warning");
        assert!((c2[0].points - 0.5).abs() < 1e-9);
        assert!(comps("s4").is_empty());
    }

    #[test]
    fn grade_cohort_skips_empty_shell_projects() {
        let spec = load_spec();
        let gradable = RawProject {
            project_id: 1,
            name: "ok".into(),
            team_size: 1,
            axis: AxisInputs {
                documentation_raw: 0.0,
                doc_present: false,
                code_quality_raw: 0.0,
                cc_pct: 0.0,
                mutation_score: 0.0,
                cq_present: false,
                survival_raw: 0.0,
                surv_present: false,
                arch_crit_count: 0.0,
                arch_warn_count: 0.0,
                arch_present: false,
            },
            inventory: vec![RepoMetrics {
                repo_full_name: "r".into(),
                metrics: std::collections::BTreeMap::from([("production_loc".into(), 500.0)]),
            }],
            tasks: vec![],
            students: vec![],
            crit_findings: vec![],
            student_flags: vec![],
        };
        let empty = RawProject {
            project_id: 99,
            name: "shell".into(),
            team_size: 1,
            axis: gradable.axis.clone(),
            inventory: vec![],
            tasks: vec![],
            students: vec![],
            crit_findings: vec![],
            student_flags: vec![],
        };
        let cohort = grade_cohort(&[gradable, empty], &spec).unwrap();
        assert_eq!(cohort.projects.len(), 1);
        assert_eq!(cohort.projects[0].project_id, 1);
    }
}
