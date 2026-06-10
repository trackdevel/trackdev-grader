//! Grading v2 project axis scores: cohort-normalized quality, size, complexity.

use std::collections::BTreeMap;

use crate::cohort::{hybrid_normalize, CohortBounds};
use crate::spec::GradeSpec;
use crate::types::{CritFinding, FindingKind, RawProject, RepoMetrics};

/// Per-project 0–10 axis scores injected into formula scope during `grade_cohort`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectAxisScores {
    pub quality: f64,
    pub complexity: f64,
    pub size: f64,
    pub quality_present: bool,
    pub complexity_present: bool,
    pub size_present: bool,
}

const SIZE_SPRING: &[&str] = &[
    "endpoint_count",
    "controller_count",
    "entity_count",
    "repository_count",
];
const SIZE_ANDROID: &[&str] = &[
    "fragment_count",
    "activity_count",
    "viewmodel_count",
    "room_database_count",
];
const COMPLEXITY_SPRING: &[&str] = &[
    "custom_query_count",
    "scheduled_task_count",
    "avg_cc_per_controller",
    "avg_statements_per_endpoint",
];
const COMPLEXITY_ANDROID: &[&str] = &[
    "reactive_wiring_density",
    "nav_dispatch_density",
    "avg_cc_per_fragment",
];

pub fn repo_kind(repo_full_name: &str) -> &'static str {
    if repo_full_name.starts_with("android-") {
        "android"
    } else {
        "spring"
    }
}

/// Collect all scalar samples for cohort bounds (project-level + per-repo inventory).
pub fn collect_cohort_samples(projects: &[RawProject]) -> BTreeMap<String, Vec<f64>> {
    let mut by_key: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for raw in projects {
        push_sample(&mut by_key, "code_quality_raw", raw.axis.cq_present, raw.axis.code_quality_raw);
        push_sample(
            &mut by_key,
            "mutation_score",
            raw.axis.cq_present && raw.axis.mutation_score > 0.0,
            raw.axis.mutation_score,
        );
        let arch_weighted =
            raw.axis.arch_crit_count * 2.0 + raw.axis.arch_warn_count * 0.5;
        push_sample(&mut by_key, "arch_weighted", raw.axis.arch_present, arch_weighted);
        if let Some(d) = violation_density_raw(raw) {
            by_key.entry("violation_density".into()).or_default().push(d);
        }
        for repo in &raw.inventory {
            for (k, v) in &repo.metrics {
                by_key.entry(k.clone()).or_default().push(*v);
            }
        }
    }
    by_key
}

fn push_sample(by_key: &mut BTreeMap<String, Vec<f64>>, key: &str, present: bool, value: f64) {
    if present {
        by_key.entry(key.to_string()).or_default().push(value);
    }
}

pub fn violation_density_raw(raw: &RawProject) -> Option<f64> {
    let (sa, security, cx) = count_crit_findings(&raw.crit_findings);
    let prod_loc_k = production_loc_k(raw);
    if prod_loc_k <= 0.0 && sa + security + cx == 0.0 {
        return None;
    }
    Some((sa + cx + security) / prod_loc_k.max(1.0))
}

fn production_loc_k(raw: &RawProject) -> f64 {
    raw.inventory
        .iter()
        .map(|r| r.metrics.get("production_loc").copied().unwrap_or(0.0))
        .sum::<f64>()
        / 1000.0
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

/// Hybrid-normalize every tracked raw metric for one project.
pub fn normalize_project_all(
    raw: &RawProject,
    bounds: &CohortBounds,
) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    if raw.axis.cq_present {
        norm_insert(&mut out, bounds, "code_quality_raw", raw.axis.code_quality_raw);
        if raw.axis.mutation_score > 0.0 {
            norm_insert(&mut out, bounds, "mutation_score", raw.axis.mutation_score);
        }
    }
    if raw.axis.arch_present {
        let arch_weighted =
            raw.axis.arch_crit_count * 2.0 + raw.axis.arch_warn_count * 0.5;
        norm_insert(&mut out, bounds, "arch_weighted", arch_weighted);
    }
    if let Some(d) = violation_density_raw(raw) {
        norm_insert(&mut out, bounds, "violation_density", d);
    }
    for repo in &raw.inventory {
        for (k, v) in &repo.metrics {
            norm_insert(&mut out, bounds, k, *v);
        }
    }
    out
}

fn norm_insert(out: &mut BTreeMap<String, f64>, bounds: &CohortBounds, key: &str, value: f64) {
    if let Some(b) = bounds.metrics.get(key) {
        out.insert(key.to_string(), hybrid_normalize(value, b));
    }
}

/// Compute blended quality / size / complexity axis scores for one project.
pub fn compute_project_axes(
    raw: &RawProject,
    normalized: &BTreeMap<String, f64>,
    spec: &GradeSpec,
) -> ProjectAxisScores {
    let w = &spec.weights;
    let w_android = w.get("w_android").copied().unwrap_or(0.6);
    let w_spring = w.get("w_spring").copied().unwrap_or(0.4);

    let quality = quality_axis(raw, normalized, w);
    let size = blend_repo_axis(
        raw,
        normalized,
        SIZE_ANDROID,
        SIZE_SPRING,
        w_android,
        w_spring,
    );
    let complexity = blend_repo_axis(
        raw,
        normalized,
        COMPLEXITY_ANDROID,
        COMPLEXITY_SPRING,
        w_android,
        w_spring,
    );

    ProjectAxisScores {
        quality: quality.score,
        complexity: complexity.score,
        size: size.score,
        quality_present: quality.present,
        complexity_present: complexity.present,
        size_present: size.present,
    }
}

struct AxisResult {
    score: f64,
    present: bool,
}

fn quality_axis(
    raw: &RawProject,
    normalized: &BTreeMap<String, f64>,
    w: &BTreeMap<String, f64>,
) -> AxisResult {
    let w_mi = w.get("w_mi").copied().unwrap_or(0.35);
    let w_arch = w.get("w_arch").copied().unwrap_or(0.30);
    let w_density = w.get("w_density").copied().unwrap_or(0.25);
    let w_mutation = w.get("w_mutation").copied().unwrap_or(0.10);

    let mut terms: Vec<(f64, f64)> = Vec::new();
    if raw.axis.cq_present {
        if let Some(&n) = normalized.get("code_quality_raw") {
            terms.push((w_mi, n));
        }
        if raw.axis.mutation_score > 0.0 {
            if let Some(&n) = normalized.get("mutation_score") {
                terms.push((w_mutation, n));
            }
        }
    }
    if raw.axis.arch_present {
        if let Some(&n) = normalized.get("arch_weighted") {
            terms.push((w_arch, (10.0 - n).clamp(0.0, 10.0)));
        }
    }
    if let Some(&n) = normalized.get("violation_density") {
        terms.push((w_density, (10.0 - n).clamp(0.0, 10.0)));
    }

    present_renorm_mean(terms)
}

fn blend_repo_axis(
    raw: &RawProject,
    normalized: &BTreeMap<String, f64>,
    android_keys: &[&str],
    spring_keys: &[&str],
    w_android: f64,
    w_spring: f64,
) -> AxisResult {
    let android = repo_subscore(raw, normalized, "android", android_keys);
    let spring = repo_subscore(raw, normalized, "spring", spring_keys);

    match (android.present, spring.present) {
        (false, false) => AxisResult {
            score: 0.0,
            present: false,
        },
        (true, false) => android,
        (false, true) => spring,
        (true, true) => {
            let denom = w_android + w_spring;
            AxisResult {
                score: (w_android * android.score + w_spring * spring.score) / denom,
                present: true,
            }
        }
    }
}

fn repo_subscore(
    raw: &RawProject,
    normalized: &BTreeMap<String, f64>,
    kind: &str,
    keys: &[&str],
) -> AxisResult {
    let repos: Vec<&RepoMetrics> = raw
        .inventory
        .iter()
        .filter(|r| repo_kind(&r.repo_full_name) == kind)
        .collect();
    if repos.is_empty() {
        return AxisResult {
            score: 0.0,
            present: false,
        };
    }
    let mut scores = Vec::new();
    for repo in repos {
        let mut vals = Vec::new();
        for key in keys {
            if repo.metrics.contains_key(*key) {
                if let Some(&n) = normalized.get(*key) {
                    vals.push(n);
                }
            }
        }
        if !vals.is_empty() {
            scores.push(vals.iter().sum::<f64>() / vals.len() as f64);
        }
    }
    if scores.is_empty() {
        return AxisResult {
            score: 0.0,
            present: false,
        };
    }
    AxisResult {
        score: scores.iter().sum::<f64>() / scores.len() as f64,
        present: true,
    }
}

fn present_renorm_mean(terms: Vec<(f64, f64)>) -> AxisResult {
    if terms.is_empty() {
        return AxisResult {
            score: 0.0,
            present: false,
        };
    }
    let w_sum: f64 = terms.iter().map(|(w, _)| w).sum();
    let score = terms.iter().map(|(w, s)| w * s).sum::<f64>() / w_sum;
    AxisResult {
        score: score.clamp(0.0, 10.0),
        present: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AxisInputs, RawStudent};

    fn project_with_inventory() -> RawProject {
        RawProject {
            project_id: 1,
            name: "t".into(),
            team_size: 2,
            axis: AxisInputs {
                documentation_raw: 0.0,
                doc_present: false,
                code_quality_raw: 70.0,
                cc_pct: 0.0,
                mutation_score: 0.8,
                cq_present: true,
                survival_raw: 0.0,
                surv_present: false,
                arch_crit_count: 1.0,
                arch_warn_count: 2.0,
                arch_present: true,
            },
            tasks: vec![],
            students: vec![RawStudent {
                student_id: "a".into(),
                full_name: "A".into(),
            }],
            crit_findings: vec![],
            inventory: vec![
                RepoMetrics {
                    repo_full_name: "spring-api".into(),
                    metrics: BTreeMap::from([
                        ("endpoint_count".into(), 5.0),
                        ("controller_count".into(), 3.0),
                    ]),
                },
                RepoMetrics {
                    repo_full_name: "android-app".into(),
                    metrics: BTreeMap::from([("fragment_count".into(), 4.0)]),
                },
            ],
            student_flags: vec![],
        }
    }

    #[test]
    fn violation_density_uses_prod_loc_k() {
        let mut raw = project_with_inventory();
        raw.inventory[0].metrics.insert("production_loc".into(), 2000.0);
        raw.crit_findings.push(CritFinding {
            kind: FindingKind::StaticAnalysis,
            category: None,
        });
        let d = violation_density_raw(&raw).expect("density");
        assert!((d - 0.5).abs() < 1e-9);
    }

    #[test]
    fn repo_kind_detects_android_prefix() {
        assert_eq!(repo_kind("android-foo"), "android");
        assert_eq!(repo_kind("org/spring"), "spring");
    }
}
