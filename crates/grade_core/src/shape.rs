//! Structural shaping: task resolution and team aggregation.

use crate::modulation::keep;
use crate::policy::{
    behavioural_flag_graded, is_codequality_hotspot, ARCHITECTURE_HOTSPOT, COMPLEXITY_HOTSPOT,
    STATIC_ANALYSIS_HOTSPOT,
};
use crate::spec::StructuralSpec;
use crate::types::{
    AggregateKnobs, AiMaps, ProjectScopes, RawProject, RawTask, StudentScope, TaskScope,
};

/// Resolve tasks, apply keep modulation from the spec weights, and aggregate.
pub fn structural_scopes(raw: &RawProject, spec: &StructuralSpec) -> ProjectScopes {
    let maps = spec.ai_maps();
    let resolved = resolve_tasks(raw, &maps);
    let strength = spec.weights.get("ai_strength").copied().unwrap_or(1.0);
    let floor = spec.weights.get("floor_keep").copied().unwrap_or(0.2);
    // Genuinely-undeclared tasks keep a flat `undeclared_keep` of their points
    // (default 1.0 for older specs without the weight). Declared tasks — and
    // exempt tasks, which `resolve_one_task` maps to declared no-AI scalars —
    // go through keep modulation. Mirror of the spec task `keep` formula.
    let undeclared_keep = spec.weights.get("undeclared_keep").copied().unwrap_or(1.0);
    let paired: Vec<(TaskScope, f64)> = resolved
        .into_iter()
        .map(|t| {
            let k = if t.declared {
                keep(t.model_m, t.level_l, strength, floor)
            } else {
                undeclared_keep
            };
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
    // AI-exempt (e.g. AI-forbidden early sprint): treat as a fully-declared
    // no-AI task (model_m = level_l = 0) so the keep formula yields 1.0 and the
    // task keeps 100% of its points, regardless of any (void) declaration.
    if task.ai_exempt {
        return TaskScope {
            assignee_id: task.assignee_id.clone(),
            raw_points: task.raw_points,
            model_m: 0.0,
            level_l: 0.0,
            declared: true,
        };
    }

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

    let (crit_sa_count, crit_security_count, crit_cx_count) = (0.0, 0.0, 0.0);

    let mut per_student: Vec<StudentScope> = raw
        .students
        .iter()
        .map(|s| {
            let mut crit_count = 0.0;
            let mut arch_blame = 0.0;
            let mut cx_blame = 0.0;
            let mut sa_blame = 0.0;
            for f in raw
                .student_flags
                .iter()
                .filter(|f| f.student_id == s.student_id)
            {
                // Code-quality hotspots feed per-signal blame regardless of
                // severity — both CRITICAL and WARNING violations count toward
                // the quality penalty (the 80/20 author/team model). They are
                // partitioned out of the behavioural CRITICAL count, which
                // stays CRITICAL-only.
                match f.flag_type.as_str() {
                    ARCHITECTURE_HOTSPOT => arch_blame += f.weighted.unwrap_or(0.0),
                    COMPLEXITY_HOTSPOT => cx_blame += f.weighted.unwrap_or(0.0),
                    STATIC_ANALYSIS_HOTSPOT => sa_blame += f.weighted.unwrap_or(0.0),
                    _ if is_codequality_hotspot(&f.flag_type) => {}
                    // Behavioural CRITICAL flags count toward the student
                    // penalty, except policy-excluded ones (e.g. ZERO_TASKS).
                    _ if f.severity == "CRITICAL" && behavioural_flag_graded(&f.flag_type) => {
                        crit_count += 1.0
                    }
                    _ => {}
                }
            }
            StudentScope {
                student_id: s.student_id.clone(),
                student_eff: 0.0,
                ai_keep: None,
                contribution: None,
                student_critical_count: crit_count,
                arch_blame,
                cx_blame,
                sa_blame,
                codequality_penalty: 0.0,
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::types::{AxisInputs, CritFinding, FindingKind, RawStudent};

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
            inventory: vec![],
            tasks: vec![
                RawTask {
                    assignee_id: "alice".to_string(),
                    raw_points: 10.0,
                    ai_model: Some("Cap".to_string()),
                    ai_level: Some("A".to_string()),
                    declared: true,
                    ai_exempt: false,
                },
                RawTask {
                    assignee_id: "bob".to_string(),
                    raw_points: 10.0,
                    ai_model: Some("GPT-5.5".to_string()),
                    ai_level: Some("E".to_string()),
                    declared: true,
                    ai_exempt: false,
                },
                RawTask {
                    assignee_id: "alice".to_string(),
                    raw_points: 5.0,
                    ai_model: Some("Cursor".to_string()),
                    ai_level: None,
                    declared: true,
                    ai_exempt: false,
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
                flag_type: "SOME_FLAG".to_string(),
                weighted: None,
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
    fn resolve_exempt_task_is_full_keep_no_ai() {
        let maps = default_maps();
        let exempt = RawTask {
            assignee_id: "alice".to_string(),
            raw_points: 8.0,
            // Even a "worst case" declaration is ignored once exempt.
            ai_model: Some("GPT-5.5".to_string()),
            ai_level: Some("E".to_string()),
            declared: true,
            ai_exempt: true,
        };
        let scope = resolve_one_task(&exempt, &maps);
        assert!((scope.model_m - 0.0).abs() < 1e-9);
        assert!((scope.level_l - 0.0).abs() < 1e-9);
        assert!(scope.declared, "exempt resolves as declared no-AI");
        // Through the keep modulation this is full retention.
        assert!((keep(scope.model_m, scope.level_l, 1.0, 0.2) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn structural_scopes_splits_declared_undeclared_and_exempt() {
        use crate::spec::GradeSpec;

        let mut weights = BTreeMap::new();
        weights.insert("ai_strength".to_string(), 1.0);
        weights.insert("floor_keep".to_string(), 0.2);
        weights.insert("undeclared_keep".to_string(), 0.5);
        let spec = GradeSpec {
            meta: Default::default(),
            weights,
            anchors: BTreeMap::new(),
            models: BTreeMap::from([("GPT-5.5".to_string(), 1.0)]),
            levels: BTreeMap::from([("E".to_string(), 1.0)]),
            formulas: Default::default(),
            manual_fields: Default::default(),
            constants: Vec::new(),
        };

        let raw = RawProject {
            project_id: 7,
            name: "Mixed".to_string(),
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
                arch_present: false,
            },
            inventory: vec![],
            tasks: vec![
                // Declared worst-case → keep 0.2 → eff 2.0.
                RawTask {
                    assignee_id: "alice".to_string(),
                    raw_points: 10.0,
                    ai_model: Some("GPT-5.5".to_string()),
                    ai_level: Some("E".to_string()),
                    declared: true,
                    ai_exempt: false,
                },
                // Neither task nor parent declared → undeclared_keep 0.5 → eff 5.0.
                RawTask {
                    assignee_id: "bob".to_string(),
                    raw_points: 10.0,
                    ai_model: None,
                    ai_level: None,
                    declared: false,
                    ai_exempt: false,
                },
                // Exempt (AI-forbidden sprint) → keep 1.0 → eff 10.0.
                RawTask {
                    assignee_id: "carol".to_string(),
                    raw_points: 10.0,
                    ai_model: None,
                    ai_level: None,
                    declared: false,
                    ai_exempt: true,
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
                RawStudent {
                    student_id: "carol".to_string(),
                    full_name: "Carol".to_string(),
                },
            ],
            crit_findings: vec![],
            student_flags: vec![],
        };

        let scopes = structural_scopes(&raw, &spec);
        let eff = |id: &str| {
            scopes
                .students
                .iter()
                .find(|s| s.student_id == id)
                .unwrap()
                .student_eff
        };
        assert!((eff("alice") - 2.0).abs() < 1e-9);
        assert!((eff("bob") - 5.0).abs() < 1e-9);
        assert!((eff("carol") - 10.0).abs() < 1e-9);
        assert!((scopes.sum_eff - 17.0).abs() < 1e-9);
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
            inventory: vec![],
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
    fn hotspot_flags_partitioned_from_behavioural_critical_count() {
        let raw = RawProject {
            project_id: 5,
            name: "Hot".to_string(),
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
            inventory: vec![],
            tasks: vec![],
            students: vec![RawStudent {
                student_id: "u".to_string(),
                full_name: "U".to_string(),
            }],
            crit_findings: vec![],
            student_flags: vec![
                crate::types::StudentFlag {
                    student_id: "u".to_string(),
                    severity: "CRITICAL".to_string(),
                    source: "artifact".to_string(),
                    flag_type: ARCHITECTURE_HOTSPOT.to_string(),
                    weighted: Some(4.0),
                },
                crate::types::StudentFlag {
                    student_id: "u".to_string(),
                    severity: "CRITICAL".to_string(),
                    source: "sprint".to_string(),
                    flag_type: "LOW_SURVIVAL_RATE".to_string(),
                    weighted: None,
                },
            ],
        };
        let scopes = aggregate(&raw, &[], &default_knobs());
        let u = &scopes.students[0];
        assert!((u.arch_blame - 4.0).abs() < 1e-9);
        assert!((u.student_critical_count - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ungraded_behavioural_flags_excluded_from_critical_count() {
        let flag = |ft: &str| crate::types::StudentFlag {
            student_id: "u".to_string(),
            severity: "CRITICAL".to_string(),
            source: "sprint".to_string(),
            flag_type: ft.to_string(),
            weighted: None,
        };
        let raw = RawProject {
            project_id: 6,
            name: "B".to_string(),
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
            inventory: vec![],
            tasks: vec![],
            students: vec![RawStudent {
                student_id: "u".to_string(),
                full_name: "U".to_string(),
            }],
            crit_findings: vec![],
            // ZERO_TASKS + LOW_COMPOSITE_SCORE are policy-excluded; LOW_REVIEWS
            // and APPROVED_BROKEN_PR are graded → only 2 count.
            student_flags: vec![
                flag("ZERO_TASKS"),
                flag("LOW_COMPOSITE_SCORE"),
                flag("LOW_REVIEWS"),
                flag("APPROVED_BROKEN_PR"),
            ],
        };
        let scopes = aggregate(&raw, &[], &default_knobs());
        assert!((scopes.students[0].student_critical_count - 2.0).abs() < 1e-9);
    }

    #[test]
    fn count_security_findings_retired() {
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
            inventory: vec![],
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
        assert!((scopes.crit_sa_count - 0.0).abs() < 1e-9);
        assert!((scopes.crit_security_count - 0.0).abs() < 1e-9);
        assert!((scopes.crit_cx_count - 0.0).abs() < 1e-9);
    }
}
