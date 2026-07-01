//! Integration tests for cohort grading (Grading v2).

use std::fs;
use std::path::PathBuf;

use grade_core::{
    grade_cohort, AxisInputs, GradeSpec, RawProject, RawStudent, RawTask, RepoMetrics,
};

fn load_spec() -> GradeSpec {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/grading.standard.json");
    let text = fs::read_to_string(path).expect("grading.standard.json");
    serde_json::from_str(&text).expect("parse spec")
}

#[test]
fn cohort_bounds_shared_across_batch() {
    let spec = load_spec();
    let mut projects = Vec::new();
    for (id, doc) in [(1, 1.0), (2, 3.0), (3, 5.0)] {
        projects.push(RawProject {
            project_id: id,
            name: format!("p{id}"),
            team_size: 2,
            axis: AxisInputs {
                documentation_raw: doc,
                doc_present: true,
                code_quality_raw: 70.0,
                cc_pct: 0.0,
                mutation_score: 0.0,
                cq_present: true,
                survival_raw: 0.0,
                surv_present: false,
                arch_crit_count: 0.0,
                arch_warn_count: 0.0,
                arch_present: false,
            },
            inventory: vec![RepoMetrics {
                repo_full_name: format!("repo-{id}"),
                metrics: [("production_loc", 1000.0)]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            }],
            tasks: vec![],
            students: vec![],
            crit_findings: vec![],
            student_flags: vec![],
        });
    }
    let out = grade_cohort(&projects, &spec).expect("grade_cohort");
    let mi = out
        .bounds
        .metrics
        .get("code_quality_raw")
        .expect("mi bounds");
    assert_eq!(mi.sample_count, 3);
}

#[test]
fn equal_contributors_receive_project_final() {
    let spec = load_spec();
    let inv = |name: &str, metrics: &[(&str, f64)]| RepoMetrics {
        repo_full_name: name.into(),
        metrics: metrics.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
    };
    let raw = RawProject {
        project_id: 99,
        name: "eq".into(),
        team_size: 3,
        axis: AxisInputs {
            documentation_raw: 0.0,
            doc_present: false,
            code_quality_raw: 72.0,
            cc_pct: 0.0,
            mutation_score: 0.5,
            cq_present: true,
            survival_raw: 0.0,
            surv_present: false,
            arch_crit_count: 0.0,
            arch_warn_count: 0.0,
            arch_present: true,
        },
        inventory: vec![
            inv(
                "spring-api",
                &[
                    ("endpoint_count", 6.0),
                    ("controller_count", 3.0),
                    ("entity_count", 2.0),
                    ("repository_count", 2.0),
                    ("production_loc", 3000.0),
                    ("custom_query_count", 1.0),
                    ("avg_cc_per_controller", 2.0),
                ],
            ),
            inv(
                "android-app",
                &[
                    ("fragment_count", 4.0),
                    ("activity_count", 1.0),
                    ("viewmodel_count", 3.0),
                    ("production_loc", 2000.0),
                    ("reactive_wiring_density", 0.5),
                    ("nav_dispatch_density", 0.3),
                    ("avg_cc_per_fragment", 2.5),
                ],
            ),
        ],
        tasks: vec![
            RawTask {
                assignee_id: "a".into(),
                raw_points: 10.0,
                ai_model: Some("Cap".into()),
                ai_level: Some("A".into()),
                declared: true,
                ai_exempt: false,
            },
            RawTask {
                assignee_id: "b".into(),
                raw_points: 10.0,
                ai_model: Some("Cap".into()),
                ai_level: Some("A".into()),
                declared: true,
                ai_exempt: false,
            },
            RawTask {
                assignee_id: "c".into(),
                raw_points: 10.0,
                ai_model: Some("Cap".into()),
                ai_level: Some("A".into()),
                declared: true,
                ai_exempt: false,
            },
        ],
        students: vec![
            RawStudent {
                student_id: "a".into(),
                full_name: "A".into(),
            },
            RawStudent {
                student_id: "b".into(),
                full_name: "B".into(),
            },
            RawStudent {
                student_id: "c".into(),
                full_name: "C".into(),
            },
        ],
        crit_findings: vec![],
        student_flags: vec![],
    };
    let out = grade_cohort(&[raw], &spec).expect("grade");
    let pf = out.projects[0].output.grades.project_final;
    // The ×team_size normalizer makes each equal contributor's net grade equal
    // the project grade; Grading v5's transparent curve sets student_curved =
    // student_net, then student_lift_* transforms it identically for all three.
    let k = spec.weights.get("student_lift_k").copied().unwrap_or(0.0);
    let pivot = spec
        .weights
        .get("student_lift_pivot")
        .copied()
        .unwrap_or(7.0);
    let curved = pf;
    let expected = (curved + k * curved * (pivot - curved).max(0.0) / pivot).clamp(0.0, 10.0);
    let students = &out.projects[0].output.grades.students;
    for stu in students {
        // Symmetry: every equal contributor receives the same final.
        assert!(
            (stu.student_final - students[0].student_final).abs() < 1e-9,
            "equal contributors must tie: {} got {}",
            stu.student_id,
            stu.student_final
        );
        // And that common value is the leniency-curved project grade.
        assert!(
            (stu.student_final - expected).abs() <= 0.02,
            "student {} got {} expected ~{} (curve of pf={})",
            stu.student_id,
            stu.student_final,
            expected,
            pf
        );
    }
}
