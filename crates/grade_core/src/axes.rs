//! Grading v3 project axis scores: work (size + complexity) modulated by quality.

use std::collections::BTreeMap;

use crate::cohort::{hybrid_normalize, CohortBounds};
use crate::policy::{count_crit_findings, has_gradable_artifact};
use crate::spec::{ExtraTechComponent, GradeSpec};
use crate::types::{RawProject, RepoMetrics};

/// Per-project 0–10 axis scores injected into formula scope during `grade_cohort`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectAxisScores {
    pub quality: f64,
    pub complexity: f64,
    pub size: f64,
    /// Present-renormalized blend of size and complexity (`w_size` / `w_complexity`).
    pub work_base: f64,
    /// Quality score used in the multiplier (10 when quality absent — neutral).
    pub quality_eff: f64,
    /// `quality_floor + quality_blend × (quality_eff / 10)`.
    pub quality_multiplier: f64,
    pub quality_present: bool,
    pub complexity_present: bool,
    pub size_present: bool,
    pub work_base_present: bool,
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

/// Breadth slice of project size (structural counts on main).
const W_SIZE_BREADTH: f64 = 0.70;
/// Statement-volume slice of project size (repo-wide production methods).
const W_SIZE_VOLUME: f64 = 0.30;

pub fn repo_kind(repo_full_name: &str) -> &'static str {
    let lower = repo_full_name.to_lowercase();
    if lower.starts_with("android") || lower.contains("-android") || lower.contains("/android") {
        "android"
    } else {
        "spring"
    }
}

/// EXTRA_TECH aggregate: a raw weighted count of "extra technologies vs.
/// baseline" across the project's repos. Breadth (`extra_dependency_count`) is
/// counted per-unit; each curated depth feature is saturated to `[0,1]` (via
/// `extra_tech_cap`, except the already-bounded `fcm_android_room_store/2`) then
/// weighted. All weights/caps come from `spec.weights` with defaults, so the
/// value is deterministic (no cohort coupling) and the professor tunes it in the
/// desktop. Returns `(extra_tech, components)`; components list only signals with
/// `raw > 0`.
pub fn compute_extra_tech(raw: &RawProject, spec: &GradeSpec) -> (f64, Vec<ExtraTechComponent>) {
    let sum = |key: &str| -> f64 {
        raw.inventory
            .iter()
            .map(|r| r.metrics.get(key).copied().unwrap_or(0.0))
            .sum()
    };
    let w = |name: &str, default: f64| spec.weights.get(name).copied().unwrap_or(default);
    let cap = w("extra_tech_cap", 3.0).max(1.0);
    let sat = |x: f64| x.min(cap) / cap;

    let dep = sum("extra_dependency_count");
    let fcm_send = sum("fcm_send_call_count");
    let fcm_room = sum("fcm_android_room_store");
    let spec_defs = sum("specification_def_count");
    let email = sum("email_send_site_count");
    let gfx = sum("graphics_custom_draw_count");
    let av = sum("av_usage_count");

    // (key, raw, weight, normalized contribution-per-weight)
    let entries: [(&str, f64, f64, f64); 7] = [
        ("extra_dependency_count", dep, w("w_extra_dep", 1.0), dep),
        (
            "fcm_send_call_count",
            fcm_send,
            w("w_fcm_spring", 2.0),
            sat(fcm_send),
        ),
        (
            "fcm_android_room_store",
            fcm_room,
            w("w_fcm_android", 3.0),
            (fcm_room / 2.0).min(1.0),
        ),
        (
            "specification_def_count",
            spec_defs,
            w("w_spec", 3.0),
            sat(spec_defs),
        ),
        (
            "email_send_site_count",
            email,
            w("w_email", 2.0),
            sat(email),
        ),
        (
            "graphics_custom_draw_count",
            gfx,
            w("w_graphics", 2.0),
            sat(gfx),
        ),
        ("av_usage_count", av, w("w_av", 2.0), sat(av)),
    ];

    let mut total = 0.0;
    let mut components = Vec::new();
    for (key, raw_val, weight, normalized) in entries {
        if raw_val <= 0.0 {
            continue;
        }
        let contribution = normalized * weight;
        total += contribution;
        components.push(ExtraTechComponent {
            key: key.to_string(),
            raw: raw_val,
            weight,
            contribution,
        });
    }
    (total, components)
}

/// Collect all scalar samples for cohort bounds (project-level + per-repo inventory).
pub fn collect_cohort_samples(projects: &[RawProject]) -> BTreeMap<String, Vec<f64>> {
    let mut by_key: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for raw in projects {
        push_sample(
            &mut by_key,
            "code_quality_raw",
            raw.axis.cq_present,
            raw.axis.code_quality_raw,
        );
        push_sample(
            &mut by_key,
            "mutation_score",
            raw.axis.cq_present && raw.axis.mutation_score > 0.0,
            raw.axis.mutation_score,
        );
        let arch_weighted = raw.axis.arch_crit_count * 2.0 + raw.axis.arch_warn_count * 0.5;
        push_sample(
            &mut by_key,
            "arch_weighted",
            raw.axis.arch_present,
            arch_weighted,
        );
        if let Some(d) = violation_density_raw(raw) {
            by_key
                .entry("violation_density".into())
                .or_default()
                .push(d);
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

/// Hybrid-normalize every tracked raw metric for one project.
pub fn normalize_project_all(raw: &RawProject, bounds: &CohortBounds) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    if raw.axis.cq_present {
        norm_insert(
            &mut out,
            bounds,
            "code_quality_raw",
            raw.axis.code_quality_raw,
        );
        if raw.axis.mutation_score > 0.0 {
            norm_insert(&mut out, bounds, "mutation_score", raw.axis.mutation_score);
        }
    }
    if raw.axis.arch_present {
        let arch_weighted = raw.axis.arch_crit_count * 2.0 + raw.axis.arch_warn_count * 0.5;
        norm_insert(&mut out, bounds, "arch_weighted", arch_weighted);
    }
    if let Some(d) = violation_density_raw(raw) {
        norm_insert(&mut out, bounds, "violation_density", d);
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
    _normalized: &BTreeMap<String, f64>,
    bounds: &CohortBounds,
    spec: &GradeSpec,
) -> ProjectAxisScores {
    if !has_gradable_artifact(raw) {
        return absent_project_axes();
    }

    let w = &spec.weights;
    let w_android = w.get("w_android").copied().unwrap_or(0.6);
    let w_spring = w.get("w_spring").copied().unwrap_or(0.4);
    let w_size = w.get("w_size").copied().unwrap_or(0.2);
    let w_complexity = w.get("w_complexity").copied().unwrap_or(0.3);
    let quality_floor = w.get("quality_floor").copied().unwrap_or(0.25);
    let quality_blend = w.get("quality_blend").copied().unwrap_or(0.75);
    // v4: frozen multiplicative scale so the cohort-top work_base reaches 10
    // (uncapped — strong future cohorts may exceed 10).
    let work_scale = w.get("work_scale").copied().unwrap_or(1.0);

    let quality = quality_axis(raw, w);
    let breadth = blend_repo_axis(raw, bounds, SIZE_ANDROID, SIZE_SPRING, w_android, w_spring);
    let volume = statement_volume_axis(raw, bounds);
    let size = blend_size_axis(breadth, volume);
    let complexity = blend_repo_axis(
        raw,
        bounds,
        COMPLEXITY_ANDROID,
        COMPLEXITY_SPRING,
        w_android,
        w_spring,
    );

    let work = work_base_axis(size, complexity, w_size, w_complexity);
    let quality_eff = if quality.present { quality.score } else { 10.0 };
    let quality_multiplier = quality_floor + quality_blend * (quality_eff / 10.0);

    ProjectAxisScores {
        quality: quality.score,
        complexity: complexity.score,
        size: size.score,
        work_base: if work.present {
            work.score * work_scale
        } else {
            0.0
        },
        quality_eff,
        quality_multiplier,
        quality_present: quality.present,
        complexity_present: complexity.present,
        size_present: size.present,
        work_base_present: work.present,
    }
}

pub fn absent_project_axes() -> ProjectAxisScores {
    ProjectAxisScores {
        quality: 0.0,
        complexity: 0.0,
        size: 0.0,
        work_base: 0.0,
        quality_eff: 0.0,
        quality_multiplier: 0.0,
        quality_present: false,
        complexity_present: false,
        size_present: false,
        work_base_present: false,
    }
}

fn work_base_axis(
    size: AxisResult,
    complexity: AxisResult,
    w_size: f64,
    w_complexity: f64,
) -> AxisResult {
    match (size.present, complexity.present) {
        (false, false) => AxisResult {
            score: 0.0,
            present: false,
        },
        (true, false) => size,
        (false, true) => complexity,
        (true, true) => {
            let denom = w_size + w_complexity;
            AxisResult {
                score: ((w_size * size.score + w_complexity * complexity.score) / denom)
                    .clamp(0.0, 10.0),
                present: true,
            }
        }
    }
}

#[derive(Clone, Copy)]
struct AxisResult {
    score: f64,
    present: bool,
}

/// Map a raw value onto 0–10 by pure-absolute linear interpolation between
/// `floor` and `ceiling`, clamped. Unlike `hybrid_normalize`, this never
/// stretches the cohort's p10/p90 across the band — so a saturated metric
/// (the maintainability index clusters ~97) is not amplified into a full
/// spread (Grading v4 / Q11).
fn absolute_map(value: f64, floor: f64, ceiling: f64) -> f64 {
    if ceiling <= floor {
        return 0.0;
    }
    ((value - floor) / (ceiling - floor) * 10.0).clamp(0.0, 10.0)
}

/// Grading v4 quality axis: maintainability is the driver (absolute-mapped,
/// not cohort-percentile), mutation folds in when present, and high-level
/// (layer) architecture is a SUBTRACTIVE guard that only dents teams which
/// actually break package layering. Specific architecture / complexity /
/// static-analysis violations are charged to students, not here.
fn quality_axis(raw: &RawProject, w: &BTreeMap<String, f64>) -> AxisResult {
    let w_mi = w.get("w_mi").copied().unwrap_or(0.35);
    let w_mutation = w.get("w_mutation").copied().unwrap_or(0.10);
    let mi_floor = w.get("mi_floor").copied().unwrap_or(85.0);
    let mi_ceiling = w.get("mi_ceiling").copied().unwrap_or(98.0);

    let mut terms: Vec<(f64, f64)> = Vec::new();
    if raw.axis.cq_present {
        terms.push((
            w_mi,
            absolute_map(raw.axis.code_quality_raw, mi_floor, mi_ceiling),
        ));
        if raw.axis.mutation_score > 0.0 {
            terms.push((
                w_mutation,
                (raw.axis.mutation_score * 10.0).clamp(0.0, 10.0),
            ));
        }
    }
    // Quality axis is purely maintainability + mutation (how clean/tested the
    // code is). Architecture / complexity / static-analysis breaches are NOT
    // subtracted here any more — they are charged once through the 80/20
    // author/team quality-penalty model (`grade.rs`), which removes the former
    // double-count of `layer_dependency` (quality axis + per-student hotspot).
    // See `plans/quality_penalty_8020/PLAN.md`.
    present_renorm_mean(terms)
}

fn blend_repo_axis(
    raw: &RawProject,
    bounds: &CohortBounds,
    android_keys: &[&str],
    spring_keys: &[&str],
    w_android: f64,
    w_spring: f64,
) -> AxisResult {
    let android = repo_subscore(raw, bounds, "android", android_keys);
    let spring = repo_subscore(raw, bounds, "spring", spring_keys);

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

fn repo_subscore(raw: &RawProject, bounds: &CohortBounds, kind: &str, keys: &[&str]) -> AxisResult {
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
            if let Some(&v) = repo.metrics.get(*key) {
                if let Some(b) = bounds.metrics.get(*key) {
                    vals.push(hybrid_normalize(v, b));
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

fn statement_volume_axis(raw: &RawProject, bounds: &CohortBounds) -> AxisResult {
    let total: f64 = raw
        .inventory
        .iter()
        .map(|r| {
            r.metrics
                .get("production_statement_count")
                .copied()
                .unwrap_or(0.0)
        })
        .sum();
    if total <= 0.0 {
        return AxisResult {
            score: 0.0,
            present: false,
        };
    }
    let Some(b) = bounds.metrics.get("production_statement_count") else {
        return AxisResult {
            score: 0.0,
            present: false,
        };
    };
    AxisResult {
        score: hybrid_normalize(total, b).clamp(0.0, 10.0),
        present: true,
    }
}

fn blend_size_axis(breadth: AxisResult, volume: AxisResult) -> AxisResult {
    match (breadth.present, volume.present) {
        (false, false) => AxisResult {
            score: 0.0,
            present: false,
        },
        (true, false) => breadth,
        (false, true) => volume,
        (true, true) => AxisResult {
            score: (W_SIZE_BREADTH * breadth.score + W_SIZE_VOLUME * volume.score).clamp(0.0, 10.0),
            present: true,
        },
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
    use crate::types::{AxisInputs, CritFinding, FindingKind, RawStudent};

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
        raw.inventory[0]
            .metrics
            .insert("production_loc".into(), 2000.0);
        raw.crit_findings.push(CritFinding {
            kind: FindingKind::StaticAnalysis,
            category: None,
        });
        let d = violation_density_raw(&raw).expect("density");
        assert!((d - 0.5).abs() < 1e-9);
    }

    fn empty_spec() -> GradeSpec {
        GradeSpec {
            meta: crate::spec::Meta::default(),
            weights: BTreeMap::new(),
            anchors: BTreeMap::new(),
            models: BTreeMap::new(),
            levels: BTreeMap::new(),
            formulas: crate::spec::Formulas::default(),
            manual_fields: Default::default(),
            constants: vec![],
        }
    }

    #[test]
    fn compute_extra_tech_weights_breadth_and_depth() {
        let mut raw = project_with_inventory();
        // spring repo: 3 new deps + 5 Specification defs + 1 FCM send
        raw.inventory[0]
            .metrics
            .insert("extra_dependency_count".into(), 3.0);
        raw.inventory[0]
            .metrics
            .insert("specification_def_count".into(), 5.0);
        raw.inventory[0]
            .metrics
            .insert("fcm_send_call_count".into(), 1.0);
        // android repo: room-store 2 + av usage 6
        raw.inventory[1]
            .metrics
            .insert("fcm_android_room_store".into(), 2.0);
        raw.inventory[1]
            .metrics
            .insert("av_usage_count".into(), 6.0);

        let (total, comps) = compute_extra_tech(&raw, &empty_spec());
        // defaults cap=3: dep 3*1=3; spec sat(5)=1 *3=3; fcm_send sat(1)=1/3 *2=0.667;
        // room (2/2)=1 *3=3; av sat(6)=1 *2=2  →  total ≈ 11.667
        assert!((total - 11.6667).abs() < 0.01, "total={total}");
        assert_eq!(comps.len(), 5);
        let dep = comps
            .iter()
            .find(|c| c.key == "extra_dependency_count")
            .unwrap();
        assert_eq!(dep.contribution, 3.0);
        // Zero-valued signals (graphics, email) are omitted from the breakdown.
        assert!(comps.iter().all(|c| c.key != "graphics_custom_draw_count"));
    }

    #[test]
    fn work_base_blends_size_and_complexity_legacy_weights() {
        let size = AxisResult {
            score: 5.0,
            present: true,
        };
        let complexity = AxisResult {
            score: 7.5,
            present: true,
        };
        let work = work_base_axis(size, complexity, 0.2, 0.3);
        assert!((work.score - 6.5).abs() < 1e-9);
        assert!(work.present);
    }

    #[test]
    fn v3_multiplier_at_zero_quality_retains_quarter_of_work() {
        let quality_floor: f64 = 0.25;
        let quality_blend: f64 = 0.75;
        let quality_eff: f64 = 0.0;
        let m = quality_floor + quality_blend * (quality_eff / 10.0);
        assert!((m - 0.25).abs() < 1e-9);
        let work_base: f64 = 8.0;
        assert!((work_base * m - 2.0).abs() < 1e-9);
    }

    #[test]
    fn quality_axis_maps_mi_absolute_without_amplification() {
        let mut w = BTreeMap::new();
        w.insert("w_mi".into(), 0.35);
        w.insert("w_mutation".into(), 0.10);
        w.insert("mi_floor".into(), 85.0);
        w.insert("mi_ceiling".into(), 98.0);
        let mut a = project_with_inventory();
        a.axis.mutation_score = 0.0;
        a.axis.arch_present = false;
        let mut b = a.clone();
        a.axis.code_quality_raw = 97.0;
        b.axis.code_quality_raw = 98.0;
        // 97 vs 98 MI → a sub-point quality gap (no cohort-percentile blow-up).
        let (qa, qb) = (quality_axis(&a, &w), quality_axis(&b, &w));
        assert!(qb.score > qa.score && (qb.score - qa.score).abs() < 1.0);
        // A bloated team (MI 87) maps near the absolute floor.
        let mut c = a.clone();
        c.axis.code_quality_raw = 87.0;
        assert!(quality_axis(&c, &w).score < 2.5);
    }

    #[test]
    fn quality_axis_ignores_architecture_breaches() {
        // Architecture no longer dents the quality axis (it is charged through
        // the 80/20 author/team penalty instead). MI alone drives quality, so a
        // team with many layer breaches reads the same as a clean one.
        let mut w = BTreeMap::new();
        w.insert("w_mi".into(), 0.35);
        w.insert("mi_floor".into(), 85.0);
        w.insert("mi_ceiling".into(), 98.0);
        let mut clean = project_with_inventory();
        clean.axis.mutation_score = 0.0;
        clean.axis.code_quality_raw = 98.0; // MI_abs = 10.0
        clean.axis.arch_present = true;
        clean.axis.arch_crit_count = 0.0;
        clean.axis.arch_warn_count = 0.0;
        let mut breach = clean.clone();
        breach.axis.arch_crit_count = 5.0;
        breach.axis.arch_warn_count = 10.0;
        assert!((quality_axis(&clean, &w).score - 10.0).abs() < 1e-9);
        assert!((quality_axis(&breach, &w).score - 10.0).abs() < 1e-9);
    }

    #[test]
    fn size_axis_blends_breadth_and_statement_volume() {
        let breadth = AxisResult {
            score: 8.0,
            present: true,
        };
        let volume = AxisResult {
            score: 4.0,
            present: true,
        };
        let blended = blend_size_axis(breadth, volume);
        let expected = W_SIZE_BREADTH * 8.0 + W_SIZE_VOLUME * 4.0;
        assert!((blended.score - expected).abs() < 1e-9);
        assert!(blended.present);
    }

    #[test]
    fn repo_kind_detects_android_repos() {
        assert_eq!(repo_kind("android-foo"), "android");
        assert_eq!(repo_kind("udg-pds/android-pds26_1a"), "android");
        assert_eq!(repo_kind("org/spring"), "spring");
        assert_eq!(repo_kind("udg-pds/spring-pds26_1a"), "spring");
    }
}
