//! Structural shaping: task resolution and team aggregation.

use crate::modulation::keep;
use crate::spec::StructuralSpec;
use crate::types::{
    AggregateKnobs, AiMaps, CritFinding, FindingKind, ProjectScopes, RawProject, RawTask,
    StudentScope, TaskScope,
};

/// Resolve tasks, apply keep modulation from the spec weights, and aggregate.
pub fn structural_scopes(raw: &RawProject, spec: &StructuralSpec) -> ProjectScopes {
    let maps = spec.ai_maps();
    let resolved = resolve_tasks(raw, &maps);
    let strength = spec.weights.get("ai_strength").copied().unwrap_or(1.0);
    let floor = spec.weights.get("floor_keep").copied().unwrap_or(0.2);
    let paired: Vec<(TaskScope, f64)> = resolved
        .into_iter()
        .map(|t| {
            let k = keep(t.model_m, t.level_l, strength, floor);
            (t, k)
        })
        .collect();
    aggregate(raw, &paired, &spec.aggregate_knobs())
}

/// Resolve per-task `model_m` / `level_l` from AI maps and the both-present declared gate.
///
/// A task is treated as declared only when `declared == true` AND both `ai_model` and
/// `ai_level` are present; otherwise undeclared fallbacks apply.
pub fn resolve_tasks(raw: &RawProject, maps: &AiMaps) -> Vec<TaskScope> {
    raw.tasks
        .iter()
        .map(|t| resolve_one_task(t, maps))
        .collect()
}

fn resolve_one_task(task: &RawTask, maps: &AiMaps) -> TaskScope {
    let both_present = task.declared && task.ai_model.is_some() && task.ai_level.is_some();

    let (model_m, level_l) = if both_present {
        let m = task
            .ai_model
            .as_deref()
            .map(|model| maps.models.get(model).copied().unwrap_or(1.0))
            .unwrap_or(maps.undeclared_model_m);
        let l = task
            .ai_level
            .as_deref()
            .map(|level| maps.levels.get(level).copied().unwrap_or(1.0))
            .unwrap_or(maps.undeclared_level_l);
        (m, l)
    } else {
        (maps.undeclared_model_m, maps.undeclared_level_l)
    };

    TaskScope {
        assignee_id: task.assignee_id.clone(),
        raw_points: task.raw_points,
        model_m,
        level_l,
        declared: both_present,
    }
}

/// Sum effective points, count CRITICAL findings/flags, and derive structural scalars.
pub fn aggregate(
    raw: &RawProject,
    tasks_with_keep: &[(TaskScope, f64)],
    knobs: &AggregateKnobs,
) -> ProjectScopes {
    let penalty_on = if knobs.penalty_mode == "subtractive" {
        1.0
    } else {
        0.0
    };

    let (crit_sa_count, crit_security_count, crit_cx_count) =
        count_crit_findings(&raw.crit_findings);

    let mut per_student: Vec<StudentScope> = raw
        .students
        .iter()
        .map(|s| {
            let crit_count = raw
                .student_flags
                .iter()
                .filter(|f| f.student_id == s.student_id && f.severity == "CRITICAL")
                .count() as f64;
            StudentScope {
                student_id: s.student_id.clone(),
                student_eff: 0.0,
                ai_keep: None,
                contribution: None,
                student_critical_count: crit_count,
            }
        })
        .collect();

    for (task, keep) in tasks_with_keep {
        if let Some(sp) = per_student
            .iter_mut()
            .find(|s| s.student_id == task.assignee_id)
        {
            sp.student_eff += task.raw_points * keep;
        }
    }

    let sum_raw: f64 = tasks_with_keep.iter().map(|(t, _)| t.raw_points).sum();
    let sum_eff: f64 = tasks_with_keep.iter().map(|(t, k)| t.raw_points * k).sum();

    let team_size = raw.team_size.max(1) as f64;
    let mean_raw = if sum_raw > 0.0 {
        sum_raw / team_size
    } else {
        0.0
    };
    let ai_factor = if sum_raw > 0.0 {
        sum_eff / sum_raw
    } else {
        1.0
    };

    for sp in &mut per_student {
        let student_raw: f64 = tasks_with_keep
            .iter()
            .filter(|(t, _)| t.assignee_id == sp.student_id)
            .map(|(t, _)| t.raw_points)
            .sum();
        sp.ai_keep = if student_raw > 0.0 {
            Some(sp.student_eff / student_raw)
        } else {
            None
        };
        sp.contribution = if sum_eff > 0.0 {
            Some(sp.student_eff / sum_eff)
        } else {
            None
        };
    }

    ProjectScopes {
        sum_raw,
        sum_eff,
        mean_raw,
        ai_factor,
        crit_sa_count,
        crit_security_count,
        crit_cx_count,
        penalty_on,
        students: per_student,
    }
}

fn count_crit_findings(findings: &[CritFinding]) -> (f64, f64, f64) {
    let mut sa = 0.0;
    let mut security = 0.0;
    let mut cx = 0.0;
    for f in findings {
        match f.kind {
            FindingKind::StaticAnalysis => {
                sa += 1.0;
                if f.category.as_deref() == Some("security") {
                    security += 1.0;
                }
            }
            FindingKind::Complexity => cx += 1.0,
        }
    }
    (sa, security, cx)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::types::{AxisInputs, RawStudent};

    fn default_maps() -> AiMaps {
        AiMaps {
            models: BTreeMap::from([
                ("Cap".to_string(), 0.0),
                ("GPT-5.5".to_string(), 1.0),
                ("Cursor".to_string(), 0.9),
            ]),
            levels: BTreeMap::from([("A".to_string(), 0.0), ("E".to_string(), 1.0)]),
            undeclared_model_m: 1.0,
            undeclared_level_l: 0.5,
        }
    }

    fn default_knobs() -> AggregateKnobs {
        AggregateKnobs {
            penalty_mode: "subtractive".to_string(),
        }
    }

    /// Mirrors `engine_parity.rs::seed_rich_example` task AI declarations.
    fn rich_example_project() -> RawProject {
        RawProject {
            project_id: 1,
            name: "Team 01".to_string(),
            team_size: 2,
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
            tasks: vec![
                RawTask {
                    assignee_id: "alice".to_string(),
                    raw_points: 10.0,
                    ai_model: Some("Cap".to_string()),
                    ai_level: Some("A".to_string()),
                    declared: true,
                },
                RawTask {
                    assignee_id: "bob".to_string(),
                    raw_points: 10.0,
                    ai_model: Some("GPT-5.5".to_string()),
                    ai_level: Some("E".to_string()),
                    declared: true,
                },
                RawTask {
                    assignee_id: "alice".to_string(),
                    raw_points: 5.0,
                    ai_model: Some("Cursor".to_string()),
                    ai_level: None,
                    declared: true,
                },
            ],
            students: vec![
                RawStudent {
                    student_id: "alice".to_string(),
                    full_name: "Alice".to_string(),
                },
                RawStudent {
                    student_id: "bob".to_string(),
                    full_name: "Bob".to_string(),
                },
            ],
            crit_findings: vec![],
            student_flags: vec![crate::types::StudentFlag {
                student_id: "bob".to_string(),
                severity: "CRITICAL".to_string(),
                source: "sprint".to_string(),
            }],
        }
    }

    #[test]
    fn resolve_tasks_both_present_gate() {
        let maps = default_maps();
        let tasks = resolve_tasks(&rich_example_project(), &maps);

        let cap_a = tasks.first().expect("alice cap/a");
        assert!((cap_a.model_m - 0.0).abs() < 1e-9);
        assert!((cap_a.level_l - 0.0).abs() < 1e-9);
        assert!(cap_a.declared);

        let gpt_e = tasks.get(1).expect("bob gpt/e");
        assert!((gpt_e.model_m - 1.0).abs() < 1e-9);
        assert!((gpt_e.level_l - 1.0).abs() < 1e-9);

        let cursor_undeclared = tasks.get(2).expect("alice cursor/no level");
        assert!(!cursor_undeclared.declared);
        assert!((cursor_undeclared.model_m - 1.0).abs() < 1e-9);
        assert!((cursor_undeclared.level_l - 0.5).abs() < 1e-9);
    }

    #[test]
    fn aggregate_rich_example_with_known_keeps() {
        let raw = rich_example_project();
        let maps = default_maps();
        let resolved = resolve_tasks(&raw, &maps);
        // Cap/A → 1.0, GPT-5.5/E → 0.2, Cursor/— → undeclared keep 0.6
        let keeps = [1.0, 0.2, 0.6];
        let paired: Vec<(TaskScope, f64)> =
            resolved.into_iter().zip(keeps.iter().copied()).collect();

        let scopes = aggregate(&raw, &paired, &default_knobs());

        assert!((scopes.sum_raw - 25.0).abs() < 1e-9);
        assert!((scopes.sum_eff - 15.0).abs() < 1e-9);
        assert!((scopes.mean_raw - 12.5).abs() < 1e-9);
        assert!((scopes.ai_factor - 15.0 / 25.0).abs() < 1e-9);

        let alice = scopes
            .students
            .iter()
            .find(|s| s.student_id == "alice")
            .unwrap();
        assert!((alice.student_eff - 13.0).abs() < 1e-9);
        let bob = scopes
            .students
            .iter()
            .find(|s| s.student_id == "bob")
            .unwrap();
        assert!((bob.student_eff - 2.0).abs() < 1e-9);
        assert!((bob.student_critical_count - 1.0).abs() < 1e-9);
    }

    #[test]
    fn aggregate_zero_delivery_ai_factor_one() {
        let raw = RawProject {
            project_id: 2,
            name: "Empty".to_string(),
            team_size: 2,
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
            tasks: vec![],
            students: vec![
                RawStudent {
                    student_id: "a".to_string(),
                    full_name: "A".to_string(),
                },
                RawStudent {
                    student_id: "b".to_string(),
                    full_name: "B".to_string(),
                },
            ],
            crit_findings: vec![],
            student_flags: vec![],
        };
        let scopes = aggregate(&raw, &[], &default_knobs());
        assert!((scopes.sum_raw - 0.0).abs() < 1e-9);
        assert!((scopes.mean_raw - 0.0).abs() < 1e-9);
        assert!((scopes.ai_factor - 1.0).abs() < 1e-9);
    }

    #[test]
    fn aggregate_penalty_off() {
        let raw = rich_example_project();
        let scopes = aggregate(
            &raw,
            &[],
            &AggregateKnobs {
                penalty_mode: "off".to_string(),
            },
        );
        assert!((scopes.penalty_on - 0.0).abs() < 1e-9);
    }

    #[test]
    fn count_security_findings() {
        let raw = RawProject {
            project_id: 3,
            name: "Sec".to_string(),
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
            tasks: vec![],
            students: vec![RawStudent {
                student_id: "a".to_string(),
                full_name: "A".to_string(),
            }],
            crit_findings: vec![
                CritFinding {
                    kind: FindingKind::StaticAnalysis,
                    category: Some("security".to_string()),
                },
                CritFinding {
                    kind: FindingKind::StaticAnalysis,
                    category: None,
                },
                CritFinding {
                    kind: FindingKind::Complexity,
                    category: None,
                },
            ],
            student_flags: vec![],
        };
        let scopes = aggregate(&raw, &[], &default_knobs());
        assert!((scopes.crit_sa_count - 2.0).abs() < 1e-9);
        assert!((scopes.crit_security_count - 1.0).abs() < 1e-9);
        assert!((scopes.crit_cx_count - 1.0).abs() < 1e-9);
    }
}
