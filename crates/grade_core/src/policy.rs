//! Grading policy: per-rule exclusions, artifact gates, and finding weights.

use crate::types::{CritFinding, FindingKind, RawProject};

/// Architecture AST rule IDs excluded from project-grade `arch_*_count` inputs.
///
/// `SERVICE_PUBLIC_METHOD_USES_NON_DTO` is advisory-only for the quality axis;
/// it may still appear in reports and `ARCHITECTURE_HOTSPOT` at reduced weight.
pub const ARCH_RULES_IGNORED_IN_GRADING: &[&str] = &["SERVICE_PUBLIC_METHOD_USES_NON_DTO"];

/// Architecture rules counted at reduced weight (vs 1.0). Used for project
/// `arch_*_count` inputs and `ARCHITECTURE_HOTSPOT` blame aggregation.
pub const ARCH_RULES_REDUCED_WEIGHT_IN_GRADING: &[(&str, f64)] = &[
    ("FINDVIEWBYID_USAGE", 0.25),
    ("SERVICE_PUBLIC_METHOD_USES_NON_DTO", 0.25),
];

pub fn arch_rule_ignored_in_grading(rule_id: &str) -> bool {
    ARCH_RULES_IGNORED_IN_GRADING.contains(&rule_id)
}

fn arch_rule_reduced_weight(rule_id: &str) -> Option<f64> {
    ARCH_RULES_REDUCED_WEIGHT_IN_GRADING
        .iter()
        .find(|(id, _)| *id == rule_id)
        .map(|(_, w)| *w)
}

/// Effective multiplier for one architecture violation in project-grade
/// `arch_crit_count` / `arch_warn_count`. Ignored rules return `0.0`.
pub fn arch_rule_grading_weight(rule_id: &str) -> f64 {
    if arch_rule_ignored_in_grading(rule_id) {
        return 0.0;
    }
    arch_rule_reduced_weight(rule_id).unwrap_or(1.0)
}

/// Effective multiplier for `ARCHITECTURE_HOTSPOT` blame aggregation. Ignored
/// rules still contribute here when listed in [`ARCH_RULES_REDUCED_WEIGHT_IN_GRADING`].
pub fn arch_rule_hotspot_weight(rule_id: &str) -> f64 {
    arch_rule_reduced_weight(rule_id).unwrap_or(1.0)
}

/// Behavioural CRITICAL flag types excluded from the grade by policy: they are
/// still detected and reported, but do not contribute to `student_critical_count`
/// (and therefore not to `student_penalty`). 2026-06: ZERO_TASKS and
/// LOW_COMPOSITE_SCORE removed from grading — both re-charge contribution that is
/// already captured by effective points (code/tasks → `student_contribution`).
pub const BEHAVIOURAL_FLAGS_UNGRADED: &[&str] = &["ZERO_TASKS", "LOW_COMPOSITE_SCORE"];

/// True when a behavioural CRITICAL flag should count toward the student penalty.
pub fn behavioural_flag_graded(flag_type: &str) -> bool {
    !BEHAVIOURAL_FLAGS_UNGRADED.contains(&flag_type)
}

/// Severity multiplier for code-quality hotspot blame (CRITICAL ≫ WARNING), so a
/// forbidden import weighs more than a style warning. Used by the per-(rule,file)
/// dampened blame aggregation in `analyze::flags`. See
/// `plans/quality_penalty_8020/PLAN.md` (Phase 2).
pub fn quality_severity_weight(severity: &str) -> f64 {
    match severity.to_ascii_uppercase().as_str() {
        "CRITICAL" | "ERROR" => 1.0,
        "WARNING" => 0.25,
        "INFO" => 0.1,
        _ => 0.25,
    }
}

/// Effective weight of one CRITICAL method-complexity finding in violation density.
pub const COMPLEXITY_CRIT_WEIGHT: f64 = 1.0 / 3.0;

pub const ARCHITECTURE_HOTSPOT: &str = "ARCHITECTURE_HOTSPOT";
pub const COMPLEXITY_HOTSPOT: &str = "COMPLEXITY_HOTSPOT";
pub const STATIC_ANALYSIS_HOTSPOT: &str = "STATIC_ANALYSIS_HOTSPOT";

/// Per-student code-quality hotspot types (partitioned out of behavioural CRITICAL count).
pub fn is_codequality_hotspot(flag_type: &str) -> bool {
    matches!(
        flag_type,
        ARCHITECTURE_HOTSPOT | COMPLEXITY_HOTSPOT | STATIC_ANALYSIS_HOTSPOT
    )
}

/// Parse blame magnitude from a hotspot flag's `details` JSON.
///
/// Architecture and static-analysis hotspots store `weighted`; complexity stores `score`.
pub fn hotspot_blame_magnitude(flag_type: &str, details: Option<&str>) -> f64 {
    let Some(text) = details else {
        return 0.0;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return 0.0;
    };
    if flag_type == COMPLEXITY_HOTSPOT {
        return v.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0);
    }
    if is_codequality_hotspot(flag_type) {
        return v.get("weighted").and_then(|x| x.as_f64()).unwrap_or(0.0);
    }
    0.0
}

/// Sum of `production_loc` across structural inventory repos (Java LOC proxy).
pub fn structural_production_loc(raw: &RawProject) -> f64 {
    raw.inventory
        .iter()
        .map(|r| r.metrics.get("production_loc").copied().unwrap_or(0.0))
        .sum()
}

/// Sum of `production_statement_count` across structural inventory repos.
pub fn structural_production_statement_count(raw: &RawProject) -> f64 {
    raw.inventory
        .iter()
        .map(|r| {
            r.metrics
                .get("production_statement_count")
                .copied()
                .unwrap_or(0.0)
        })
        .sum()
}

/// Project-grade axes require scanned structural inventory with non-zero code mass.
///
/// Story points and PR repo names alone do not satisfy this gate (Invariant I1).
pub fn has_gradable_artifact(raw: &RawProject) -> bool {
    structural_production_loc(raw) > 0.0 || structural_production_statement_count(raw) > 0.0
}

/// Count CRITICAL static-analysis and complexity findings with policy weights.
pub fn count_crit_findings(findings: &[CritFinding]) -> (f64, f64, f64) {
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
            FindingKind::Complexity => cx += COMPLEXITY_CRIT_WEIGHT,
        }
    }
    (sa, security, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CritFinding;

    #[test]
    fn gradable_artifact_requires_production_mass() {
        use crate::types::{AxisInputs, RawProject, RepoMetrics};
        use std::collections::BTreeMap;

        let empty = RawProject {
            project_id: 1,
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
                arch_present: true,
            },
            inventory: vec![],
            tasks: vec![],
            students: vec![],
            crit_findings: vec![],
            student_flags: vec![],
        };
        assert!(!has_gradable_artifact(&empty));

        let mut with_loc = empty.clone();
        with_loc.inventory.push(RepoMetrics {
            repo_full_name: "org/spring".into(),
            metrics: BTreeMap::from([("production_loc".into(), 1200.0)]),
        });
        assert!(has_gradable_artifact(&with_loc));

        let mut with_stmts = empty.clone();
        with_stmts.inventory.push(RepoMetrics {
            repo_full_name: "org/spring".into(),
            metrics: BTreeMap::from([("production_statement_count".into(), 42.0)]),
        });
        assert!(has_gradable_artifact(&with_stmts));
    }

    #[test]
    fn reduced_arch_rules_count_at_quarter_weight() {
        assert!((arch_rule_grading_weight("FINDVIEWBYID_USAGE") - 0.25).abs() < 1e-9);
        assert!(
            (arch_rule_grading_weight("SERVICE_PUBLIC_METHOD_USES_NON_DTO") - 0.0).abs() < 1e-9
        );
        assert!(
            (arch_rule_hotspot_weight("SERVICE_PUBLIC_METHOD_USES_NON_DTO") - 0.25).abs() < 1e-9
        );
        assert!((arch_rule_grading_weight("CONTROLLER_USES_REPOSITORY") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn complexity_critical_counts_at_one_third() {
        let findings = vec![CritFinding {
            kind: FindingKind::Complexity,
            category: None,
        }];
        let (_, _, cx) = count_crit_findings(&findings);
        assert!((cx - COMPLEXITY_CRIT_WEIGHT).abs() < 1e-9);
    }
}
