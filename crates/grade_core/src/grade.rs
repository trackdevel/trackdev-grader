//! Three-level staged grade driver: task → project → student.

use std::collections::BTreeMap;

use crate::formula::{eval, EvalError, Scope};
use crate::shape::{aggregate, resolve_tasks};
use crate::spec::{
    AxisGrade, GradeOutput, GradeSpec, GradeTrees, NamedNode, ProjectGrades, StudentGrades,
    StudentTree, TaskTree,
};
use crate::types::{RawProject, TaskScope};

pub fn grade(raw: &RawProject, spec: &GradeSpec) -> Result<GradeOutput, EvalError> {
    let maps = spec.ai_maps();
    let resolved = resolve_tasks(raw, &maps);

    let mut task_keeps: Vec<(TaskScope, f64)> = Vec::with_capacity(resolved.len());
    let mut task_trees = Vec::with_capacity(resolved.len());

    for task_scope in &resolved {
        let mut scope = task_scope_to_formula_scope(task_scope, &spec.weights);
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

    let scopes = aggregate(raw, &task_keeps, &spec.aggregate_knobs());
    let manual = spec.manual_field_values(raw.project_id);
    let mut project_scope = build_project_scope(raw, &scopes, &spec.weights, &manual);
    let mut project_tree = Vec::new();
    for fd in &spec.formulas.project {
        let node = eval(&fd.expr, &project_scope, &fd.name, &fd.infix)?;
        project_scope.insert(fd.name.clone(), node.value);
        project_tree.push(NamedNode {
            name: fd.name.clone(),
            node,
        });
    }

    let quality_composite = project_scope
        .get("quality_composite")
        .copied()
        .unwrap_or(0.0);
    let quality_penalized = project_scope
        .get("quality_penalized")
        .copied()
        .unwrap_or(0.0);
    let project_penalty = project_scope.get("project_penalty").copied().unwrap_or(0.0);
    let project_final_raw = project_scope.get("project_final").copied().unwrap_or(0.0);

    let axes = build_axis_grades(raw, &project_scope);

    let mut student_grades = Vec::new();
    let mut student_trees = Vec::new();

    for stu in &scopes.students {
        let raw_points: f64 = task_keeps
            .iter()
            .filter(|(t, _)| t.assignee_id == stu.student_id)
            .map(|(t, _)| t.raw_points)
            .sum();

        let mut stu_scope = project_scope.clone();
        stu_scope.insert("student_eff".into(), stu.student_eff);
        stu_scope.insert("student_critical_count".into(), stu.student_critical_count);
        if let Some(k) = stu.ai_keep {
            stu_scope.insert("ai_keep".into(), k);
        }
        if let Some(c) = stu.contribution {
            stu_scope.insert("contribution".into(), c);
        }

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
            student_final,
        });
        student_trees.push(StudentTree {
            student_id: stu.student_id.clone(),
            formulas,
        });
    }

    let grades = ProjectGrades {
        project_id: raw.project_id,
        quality_grade: round_grade(
            quality_composite,
            spec.meta.decimals,
            spec.meta.quantize_final,
        ),
        quality_penalized: round_grade(
            quality_penalized,
            spec.meta.decimals,
            spec.meta.quantize_final,
        ),
        project_penalty,
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
    scopes: &crate::types::ProjectScopes,
    weights: &BTreeMap<String, f64>,
    manual: &BTreeMap<String, f64>,
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
    // Manual per-project fields, injected last. `or_insert` is defensive: a
    // name collision (which the spec validator rejects) can never clobber a
    // weight/raw/structural variable — the manual field is dropped instead.
    for (k, v) in manual {
        scope.entry(k.clone()).or_insert(*v);
    }
    scope
}

fn build_axis_grades(raw: &RawProject, project_scope: &Scope) -> Vec<AxisGrade> {
    let a = &raw.axis;
    vec![
        AxisGrade {
            key: "documentation".into(),
            raw: if a.doc_present {
                Some(a.documentation_raw)
            } else {
                None
            },
            score: if a.doc_present {
                project_scope.get("doc_axis").copied()
            } else {
                None
            },
            present: a.doc_present,
        },
        AxisGrade {
            key: "code_quality".into(),
            raw: if a.cq_present {
                Some(a.code_quality_raw)
            } else {
                None
            },
            score: if a.cq_present {
                project_scope.get("cq_axis").copied()
            } else {
                None
            },
            present: a.cq_present,
        },
        AxisGrade {
            key: "survival".into(),
            raw: if a.surv_present {
                Some(a.survival_raw)
            } else {
                None
            },
            score: if a.surv_present {
                project_scope.get("surv_axis").copied()
            } else {
                None
            },
            present: a.surv_present,
        },
        AxisGrade {
            key: "architecture".into(),
            raw: if a.arch_present {
                let k_crit = project_scope.get("k_crit").copied().unwrap_or(2.0);
                let k_warn = project_scope.get("k_warn").copied().unwrap_or(0.5);
                let arch_norm = project_scope.get("arch_norm").copied().unwrap_or(4.0);
                Some((k_crit * a.arch_crit_count + k_warn * a.arch_warn_count) / arch_norm)
            } else {
                None
            },
            score: if a.arch_present {
                project_scope.get("arch_axis").copied()
            } else {
                None
            },
            present: a.arch_present,
        },
    ]
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
    use crate::types::{AxisInputs, RawStudent};

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

        for (raw, exp) in raws.iter().zip(expected.iter()) {
            let out = grade(raw, &spec).expect("grade");
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
                out.grades.project_penalty,
                proj["project_penalty"].as_f64().unwrap(),
                tol,
                pid,
                "project_penalty",
            );
            assert_close(
                out.grades.ai_factor,
                proj["ai_factor"].as_f64().unwrap(),
                tol,
                pid,
                "ai_factor",
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
}
