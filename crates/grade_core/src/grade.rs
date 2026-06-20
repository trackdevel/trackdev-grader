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

    let mut graded = Vec::with_capacity(gradable.len());
    for raw in &gradable {
        let normalized = normalize_project_all(raw, &bounds);
        let axes = compute_project_axes(raw, &normalized, &bounds, spec);
        let scopes = scoped
            .iter()
            .find(|(pid, _)| *pid == raw.project_id)
            .map(|(_, s)| s)
            .expect("scopes for gradable project");
        let output = grade_project_with_scopes(raw, spec, scopes, Some(&CohortCtx { axes }))?;
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

/// The three blame signals and the per-signal `scale_s` weight key.
const QUALITY_SIGNALS: [(&str, &str); 3] = [
    ("architecture", "qpen_arch_scale"),
    ("complexity", "qpen_cx_scale"),
    ("static_analysis", "qpen_sa_scale"),
];

/// Knobs for the absolute 80/20 quality penalty (see
/// `plans/quality_penalty_8020/PLAN.md`).
struct QualityPenaltyKnobs {
    arch_scale: f64,
    cx_scale: f64,
    sa_scale: f64,
    sig_cap: f64,
    author_share: f64,
    team_share: f64,
    author_cap: f64,
    team_cap: f64,
}

impl QualityPenaltyKnobs {
    fn from_weights(w: &BTreeMap<String, f64>) -> Self {
        let g = |k: &str, d: f64| w.get(k).copied().unwrap_or(d);
        Self {
            arch_scale: g("qpen_arch_scale", 0.1),
            cx_scale: g("qpen_cx_scale", 0.02),
            sa_scale: g("qpen_sa_scale", 0.1),
            sig_cap: g("qpen_sig_cap", 1.0),
            author_share: g("qpen_author_share", 0.8),
            team_share: g("qpen_team_share", 0.2),
            author_cap: g("qpen_author_cap", 2.0),
            team_cap: g("qpen_team_cap", 1.5),
        }
    }
    fn scale(&self, dimension: &str) -> f64 {
        match dimension {
            "architecture" => self.arch_scale,
            "complexity" => self.cx_scale,
            _ => self.sa_scale,
        }
    }
}

/// Per-project, absolute code-quality penalty split 80% author / 20% team.
///
/// For each student `i` and signal `s`, `pts_s = min(sig_cap, scale_s ·
/// blame[i,s])`; the student's quality points `P_i = Σ_s pts_s`. The author
/// keeps `author_share · P_i` (capped at `author_cap`) on `codequality_penalty`;
/// the team pool `team_share · Σ_i P_i` (capped at `team_cap`) is subtracted
/// from `project_final`. Returns `(per-student (author_penalty, components),
/// team_penalty)`.
fn quality_penalty_for_project(
    scopes: &ProjectScopes,
    weights: &BTreeMap<String, f64>,
) -> (BTreeMap<String, (f64, Vec<CodeQualityComponent>)>, f64) {
    let k = QualityPenaltyKnobs::from_weights(weights);
    let mut per_student: BTreeMap<String, (f64, Vec<CodeQualityComponent>)> = BTreeMap::new();
    let mut team_pool = 0.0;
    for stu in &scopes.students {
        let mut components = Vec::new();
        let mut p_i = 0.0;
        for (dimension, _) in QUALITY_SIGNALS {
            let blame = match dimension {
                "architecture" => stu.arch_blame,
                "complexity" => stu.cx_blame,
                _ => stu.sa_blame,
            };
            if blame <= 0.0 {
                continue;
            }
            let pts = (k.scale(dimension) * blame).min(k.sig_cap);
            if pts <= 0.0 {
                continue;
            }
            p_i += pts;
            components.push(CodeQualityComponent {
                dimension: dimension.to_string(),
                blame,
                blame_per_point: k.scale(dimension),
                tier: String::new(),
                points: k.author_share * pts,
            });
        }
        let author_penalty = (k.author_share * p_i).min(k.author_cap);
        team_pool += k.team_share * p_i;
        per_student.insert(stu.student_id.clone(), (author_penalty, components));
    }
    let team_penalty = team_pool.min(k.team_cap).max(0.0);
    (per_student, team_penalty)
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

    // Absolute 80/20 code-quality penalty: per-student author share +
    // project-level team share. Computed before project formulas so
    // `project_final` can subtract `team_quality_penalty`.
    let (quality_penalties, team_quality_penalty) = quality_penalty_for_project(scopes, &weights);

    let manual = spec.manual_field_values(raw.project_id);
    let axis_scores = cohort.map(|c| &c.axes);
    let mut project_scope = build_project_scope(raw, scopes, &weights, &manual, axis_scores);
    project_scope.insert("team_quality_penalty".into(), team_quality_penalty);
    // EXTRA_TECH: inject the aggregate before project formulas run so a formula
    // can reference `extra_tech`. Default weight 0 → grade-inert until wired.
    let (extra_tech, extra_tech_components) = crate::axes::compute_extra_tech(raw, spec);
    project_scope.insert("extra_tech".into(), extra_tech);
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

        // Tasks whose "Ús de IA" attribute is set on neither the task nor its
        // parent USER_STORY (the parent fallback is already folded into
        // `declared` by the projection), excluding AI-exempt early sprints.
        // Counted from raw tasks: an exempt task resolves to `declared == true`
        // in `TaskScope`, so the raw `!declared && !ai_exempt` predicate is the
        // unambiguous source.
        let ai_undeclared_count = raw
            .tasks
            .iter()
            .filter(|t| t.assignee_id == stu.student_id && !t.declared && !t.ai_exempt)
            .count() as i64;

        // Author share (80%, capped) of this student's quality findings, with
        // the per-signal breakdown for the report. The 20% team share already
        // went to `team_quality_penalty` on `project_final`.
        let (cq_penalty, cq_components) = quality_penalties
            .get(&stu.student_id)
            .cloned()
            .unwrap_or((0.0, Vec::new()));

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
            ai_undeclared_count,
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
        team_quality_penalty: round_grade(
            team_quality_penalty,
            spec.meta.decimals,
            spec.meta.quantize_final,
        ),
        team_size: raw.team_size,
        axes,
        extra_tech,
        extra_tech_components,
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
    fn quality_penalty_splits_80_author_20_team_absolute() {
        use crate::policy::{ARCHITECTURE_HOTSPOT, COMPLEXITY_HOTSPOT, STATIC_ANALYSIS_HOTSPOT};
        use crate::types::{RepoMetrics, StudentFlag};
        use std::collections::BTreeMap;

        let mut spec = load_spec();
        for (k, v) in [
            ("qpen_arch_scale", 0.1),
            ("qpen_cx_scale", 0.02),
            ("qpen_sa_scale", 0.1),
            ("qpen_sig_cap", 1.0),
            ("qpen_author_share", 0.8),
            ("qpen_team_share", 0.2),
            ("qpen_author_cap", 2.0),
            ("qpen_team_cap", 1.5),
        ] {
            spec.weights.insert(k.into(), v);
        }

        let flag = |sid: &str, ft: &str, w: f64| StudentFlag {
            student_id: sid.into(),
            severity: "CRITICAL".into(),
            source: "artifact".into(),
            flag_type: ft.into(),
            weighted: Some(w),
        };
        let student = |sid: &str| RawStudent {
            student_id: sid.into(),
            full_name: sid.into(),
        };
        let task = |sid: &str| crate::types::RawTask {
            assignee_id: sid.into(),
            raw_points: 10.0,
            ai_model: Some("Cap".into()),
            ai_level: Some("A".into()),
            declared: true,
            ai_exempt: false,
        };

        let raw = RawProject {
            project_id: 1,
            name: "p".into(),
            team_size: 4,
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
                metrics: BTreeMap::from([("production_loc".into(), 1000.0)]),
            }],
            tasks: vec![task("a"), task("b"), task("c"), task("d")],
            students: vec![student("a"), student("b"), student("c"), student("d")],
            crit_findings: vec![],
            student_flags: vec![
                flag("a", ARCHITECTURE_HOTSPOT, 5.0),  // 0.1*5 = 0.5 pts
                flag("b", ARCHITECTURE_HOTSPOT, 20.0), // 0.1*20 = 2.0 → sig cap 1.0
                // c: clean
                flag("d", ARCHITECTURE_HOTSPOT, 20.0),    // 1.0
                flag("d", COMPLEXITY_HOTSPOT, 200.0),     // 0.02*200 = 4 → cap 1.0
                flag("d", STATIC_ANALYSIS_HOTSPOT, 20.0), // 1.0 → P=3.0
            ],
        };

        let out = grade(&raw, &spec).unwrap();
        let g = &out.grades;
        let pen = |sid: &str| {
            g.students
                .iter()
                .find(|s| s.student_id == sid)
                .unwrap()
                .codequality_penalty
        };
        // Author share = 0.8 × min(author_cap, Σ per-signal pts).
        assert!((pen("a") - 0.4).abs() < 1e-9); // 0.8 × 0.5
        assert!((pen("b") - 0.8).abs() < 1e-9); // 0.8 × 1.0
        assert!((pen("c") - 0.0).abs() < 1e-9);
        assert!((pen("d") - 2.0).abs() < 1e-9); // 0.8 × 3.0 = 2.4 → author cap 2.0
                                                // Team pool = 0.2 × (0.5 + 1.0 + 0 + 3.0) = 0.9, under team cap 1.5.
        assert!((g.team_quality_penalty - 0.9).abs() < 1e-9);

        // The breakdown for the all-signals student lists three capped signals.
        let d = g.students.iter().find(|s| s.student_id == "d").unwrap();
        assert_eq!(d.codequality_components.len(), 3);
        assert!(d
            .codequality_components
            .iter()
            .all(|c| (c.points - 0.8).abs() < 1e-9)); // 0.8 × sig pts (1.0)
    }

    #[test]
    fn ai_undeclared_count_excludes_declared_parent_and_exempt() {
        use crate::types::RawTask;
        let spec = load_spec();
        let mk_task = |declared: bool, ai_exempt: bool| RawTask {
            assignee_id: "alice".into(),
            raw_points: 5.0,
            ai_model: if declared { Some("Cap".into()) } else { None },
            ai_level: if declared { Some("A".into()) } else { None },
            declared,
            ai_exempt,
        };
        let raw = RawProject {
            project_id: 7,
            name: "t".into(),
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
                metrics: BTreeMap::from([("production_loc".into(), 500.0)]),
            }],
            tasks: vec![
                mk_task(true, false),  // own/parent declared → not counted
                mk_task(true, false),  // declared → not counted
                mk_task(false, false), // genuinely undeclared (sprint 3–4) → counted
                mk_task(false, true),  // AI-exempt early sprint → not counted
            ],
            students: vec![RawStudent {
                student_id: "alice".into(),
                full_name: "Alice".into(),
            }],
            crit_findings: vec![],
            student_flags: vec![],
        };
        let out = grade(&raw, &spec).unwrap();
        let alice = out
            .grades
            .students
            .iter()
            .find(|s| s.student_id == "alice")
            .expect("alice");
        assert_eq!(alice.ai_undeclared_count, 1);
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
