//! Cohort anchor calibration (Grading v2 Wave 4).
//!
//! Suggests `anchors.floor` / `anchors.ceiling` from a real cohort so hybrid
//! normalization spreads teams into the 2–8 middle band instead of clustering
//! in the absolute floor band when legacy defaults (e.g. `mi_floor = 50`) sit
//! above the cohort minimum.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::anchor::MetricAnchor;
use crate::axes::collect_cohort_samples;
use crate::cohort::percentile_linear;
use crate::grade::grade_cohort;
use crate::spec::GradeSpec;
use crate::types::RawProject;

/// Per-metric cohort histogram + suggested anchors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricCalibration {
    pub key: String,
    pub sample_count: usize,
    pub min: f64,
    pub p10: f64,
    pub p90: f64,
    pub max: f64,
    pub current_floor: f64,
    pub current_ceiling: f64,
    pub suggested_floor: f64,
    pub suggested_ceiling: f64,
}

/// Full calibration report for one cohort + spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrateReport {
    pub project_count: usize,
    pub metrics: Vec<MetricCalibration>,
    pub anchors: BTreeMap<String, MetricAnchor>,
    /// Frozen multiplicative scale so the gradable cohort-top `work_base`
    /// reaches `target_top` (v4 T3.2): `target_top / max(unscaled work_base)`.
    #[serde(default = "default_work_scale")]
    pub work_scale: f64,
    /// Subtractive layer-architecture penalty slope, recomputed from the
    /// (layer-only, per T2.1) breach distribution as `arch_cap / arch_norm`
    /// when the cohort shows a real breach; otherwise the spec's `arch_k`.
    #[serde(default = "default_arch_k")]
    pub arch_k: f64,
    pub grade_range_before: (f64, f64),
    pub grade_range_after: (f64, f64),
}

fn default_work_scale() -> f64 {
    1.0
}
fn default_arch_k() -> f64 {
    1.0
}

/// Suggest absolute anchors from cohort raw samples.
///
/// Floor is placed slightly below the cohort minimum (except natural [0,1]
/// metrics) so every team escapes the 0–2 absolute floor band. Ceiling is
/// placed above the cohort maximum so strong teams can reach the 8–10 tail.
pub fn suggest_anchors(
    projects: &[RawProject],
    spec: &GradeSpec,
) -> BTreeMap<String, MetricAnchor> {
    let by_key = collect_cohort_samples(projects);
    let mut anchors = BTreeMap::new();
    for (key, mut samples) in by_key {
        if samples.is_empty() {
            continue;
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let min = samples[0];
        let max = samples[samples.len() - 1];
        let floor = suggest_floor(&key, min);
        let ceiling = suggest_ceiling(&key, min, max, floor);
        anchors.insert(
            key,
            MetricAnchor {
                floor: round_anchor(floor),
                ceiling: round_anchor(ceiling),
            },
        );
    }
    // Preserve explicit anchors for metrics absent from this cohort batch.
    for (key, anchor) in &spec.anchors {
        anchors.entry(key.clone()).or_insert_with(|| anchor.clone());
    }
    anchors
}

fn suggest_floor(key: &str, min: f64) -> f64 {
    if natural_zero_floor(key) || min <= 0.0 {
        0.0
    } else {
        (min * 0.95).max(0.0)
    }
}

fn suggest_ceiling(key: &str, min: f64, max: f64, floor: f64) -> f64 {
    if key == "mutation_score" {
        return 1.0;
    }
    if max <= floor {
        return floor + 1.0;
    }
    let span = (max - min).max(1.0);
    (max + 0.1 * span).max(floor + 1.0)
}

fn natural_zero_floor(key: &str) -> bool {
    matches!(
        key,
        "mutation_score"
            | "arch_weighted"
            | "violation_density"
            | "documentation_raw"
            | "cc_pct"
            | "survival_raw"
            | "production_loc"
            | "production_statement_count"
    ) || key.ends_with("_count")
        || key.ends_with("_density")
}

fn round_anchor(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Finer rounding (4 dp) for scale factors where 2 dp would visibly miss the
/// target (e.g. work_scale 1.2723 vs 1.27 → cohort-top 9.98 instead of 10.00).
fn round_scale(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

/// Apply suggested anchors to an in-memory spec and sync legacy weight keys.
pub fn apply_anchors_to_spec(spec: &mut GradeSpec, anchors: &BTreeMap<String, MetricAnchor>) {
    spec.anchors = anchors.clone();
    sync_legacy_weights(spec, anchors);
}

fn sync_legacy_weights(spec: &mut GradeSpec, anchors: &BTreeMap<String, MetricAnchor>) {
    // v4 (Q11): `mi_floor` / `mi_ceiling` are PURE-ABSOLUTE guard anchors
    // (≈85→0 / 98→10) and must NOT be recomputed from the cohort — the MI
    // cluster (96.6–98.3) would otherwise be stretched into a noise-amplified
    // spread. Calibration leaves them at their spec values on purpose.
    if let Some(a) = anchors.get("arch_weighted") {
        spec.weights.insert("arch_norm".into(), a.ceiling / 2.0);
    }
    if let Some(a) = anchors.get("violation_density") {
        spec.weights.insert("density_ceiling".into(), a.ceiling);
    }
    if let Some(a) = anchors.get("production_loc") {
        spec.weights.insert("prod_loc_ceiling".into(), a.ceiling);
    }
    if let Some(a) = anchors.get("production_statement_count") {
        spec.weights.insert("prod_stmt_ceiling".into(), a.ceiling);
    }
    let count_ceiling = inventory_family_ceiling(
        anchors,
        &[
            "endpoint_count",
            "controller_count",
            "entity_count",
            "repository_count",
            "fragment_count",
            "activity_count",
            "viewmodel_count",
            "room_database_count",
        ],
    );
    if let Some(v) = count_ceiling {
        spec.weights.insert("inventory_count_ceiling".into(), v);
    }
    let depth_ceiling = inventory_family_ceiling(
        anchors,
        &[
            "custom_query_count",
            "scheduled_task_count",
            "observe_call_count",
            "nav_dispatch_count",
            "reactive_state_field_count",
        ],
    );
    if let Some(v) = depth_ceiling {
        spec.weights.insert("inventory_depth_ceiling".into(), v);
    }
    let density_ceiling = inventory_family_ceiling(
        anchors,
        &[
            "reactive_wiring_density",
            "nav_dispatch_density",
            "avg_cc_per_controller",
            "avg_cc_per_fragment",
            "avg_statements_per_endpoint",
        ],
    );
    if let Some(v) = density_ceiling {
        spec.weights.insert("inventory_density_ceiling".into(), v);
    }
}

fn inventory_family_ceiling(
    anchors: &BTreeMap<String, MetricAnchor>,
    keys: &[&str],
) -> Option<f64> {
    keys.iter()
        .filter_map(|k| anchors.get(*k).map(|a| a.ceiling))
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

/// Build a calibration report comparing grades before and after anchor
/// suggestions. `target_top` is the grade the gradable cohort-top `work_base`
/// is frozen to via `work_scale` (v4 T3.2; pass 10.0 for the standard scale).
pub fn calibrate_spec(
    projects: &[RawProject],
    spec: &GradeSpec,
    target_top: f64,
) -> CalibrateReport {
    let anchors = suggest_anchors(projects, spec);
    let metrics = build_metric_report(projects, spec, &anchors);
    let grade_range_before = project_final_range(projects, spec);

    let mut calibrated = spec.clone();
    apply_anchors_to_spec(&mut calibrated, &anchors);

    // Freeze work_scale so the gradable cohort-top work_base reaches target_top.
    // Uncapped — a stronger future cohort may exceed it. Computed on the
    // anchor-applied spec so the inventory normalization matches the final one.
    let max_wb = max_unscaled_work_base(projects, &calibrated);
    let work_scale = if max_wb > 0.0 {
        round_scale(target_top / max_wb)
    } else {
        spec.weights.get("work_scale").copied().unwrap_or(1.0)
    };
    calibrated.weights.insert("work_scale".into(), work_scale);

    // Derive the layer-architecture penalty slope from the (layer-only, per
    // T2.1) breach distribution. arch_norm = arch_weighted ceiling / 2 is the
    // weighted-breach level that earns the full arch_cap, so arch_k =
    // arch_cap / arch_norm. Guarded: a near-clean cohort (arch_norm < 1.0,
    // i.e. under one critical-equivalent breach) keeps the spec's arch_k so a
    // single trivial warning isn't inflated into a punitive slope.
    let arch_cap = calibrated.weights.get("arch_cap").copied().unwrap_or(3.0);
    let arch_norm = calibrated.weights.get("arch_norm").copied().unwrap_or(0.0);
    let arch_k = if arch_norm >= 1.0 {
        round_scale(arch_cap / arch_norm)
    } else {
        spec.weights.get("arch_k").copied().unwrap_or(1.0)
    };
    calibrated.weights.insert("arch_k".into(), arch_k);

    let grade_range_after = project_final_range(projects, &calibrated);
    CalibrateReport {
        project_count: projects.len(),
        metrics,
        anchors,
        work_scale,
        arch_k,
        grade_range_before,
        grade_range_after,
    }
}

/// Maximum unscaled `work_base` over the gradable cohort (work_scale forced to
/// 1.0). The basis for the freeze scale so the cohort-top reaches the target.
fn max_unscaled_work_base(projects: &[RawProject], spec: &GradeSpec) -> f64 {
    let mut probe = spec.clone();
    probe.weights.insert("work_scale".into(), 1.0);
    let Ok(out) = grade_cohort(projects, &probe) else {
        return 0.0;
    };
    out.projects
        .iter()
        .filter_map(|p| {
            p.output
                .grades
                .axes
                .iter()
                .find(|a| a.key == "work_base")
                .and_then(|a| a.score)
        })
        .fold(0.0_f64, f64::max)
}

fn build_metric_report(
    projects: &[RawProject],
    spec: &GradeSpec,
    suggested: &BTreeMap<String, MetricAnchor>,
) -> Vec<MetricCalibration> {
    let by_key = collect_cohort_samples(projects);
    let mut out = Vec::new();
    for (key, mut samples) in by_key {
        if samples.is_empty() {
            continue;
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let current = crate::cohort::resolve_anchor(spec, &key);
        let sugg = suggested.get(&key).cloned().unwrap_or(current.clone());
        out.push(MetricCalibration {
            key: key.clone(),
            sample_count: samples.len(),
            min: samples[0],
            p10: percentile_linear(&samples, 0.10),
            p90: percentile_linear(&samples, 0.90),
            max: samples[samples.len() - 1],
            current_floor: current.floor,
            current_ceiling: current.ceiling,
            suggested_floor: sugg.floor,
            suggested_ceiling: sugg.ceiling,
        });
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

fn project_final_range(projects: &[RawProject], spec: &GradeSpec) -> (f64, f64) {
    let Ok(out) = grade_cohort(projects, spec) else {
        return (0.0, 0.0);
    };
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for p in &out.projects {
        let g = p.output.grades.project_final;
        min = min.min(g);
        max = max.max(g);
    }
    if min.is_finite() && max.is_finite() {
        (min, max)
    } else {
        (0.0, 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AxisInputs, RepoMetrics};
    use std::collections::BTreeMap;

    fn axis(mi: f64) -> AxisInputs {
        AxisInputs {
            documentation_raw: 0.0,
            doc_present: false,
            code_quality_raw: mi,
            cc_pct: 0.0,
            mutation_score: 0.0,
            cq_present: true,
            survival_raw: 0.0,
            surv_present: false,
            arch_crit_count: 0.0,
            arch_warn_count: 0.0,
            arch_present: false,
        }
    }

    fn project(id: i64, mi: f64) -> RawProject {
        RawProject {
            project_id: id,
            name: format!("p{id}"),
            team_size: 5,
            axis: axis(mi),
            inventory: vec![RepoMetrics {
                repo_full_name: format!("org/spring-p{id}"),
                metrics: BTreeMap::from([
                    ("production_loc".into(), 5000.0 + id as f64 * 100.0),
                    ("endpoint_count".into(), 10.0 + id as f64),
                ]),
            }],
            tasks: vec![],
            students: vec![],
            crit_findings: vec![],
            student_flags: vec![],
        }
    }

    fn load_spec() -> GradeSpec {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/grading.standard.json");
        let text = std::fs::read_to_string(path).expect("grading.standard.json");
        serde_json::from_str(&text).expect("parse spec")
    }

    #[test]
    fn suggest_floor_sits_below_cohort_min() {
        let projects: Vec<_> = (0..8).map(|i| project(i, 36.0 + i as f64)).collect();
        let spec = load_spec();
        let anchors = suggest_anchors(&projects, &spec);
        let cq = anchors.get("code_quality_raw").expect("cq anchor");
        assert!(cq.floor < 36.0);
        assert!(cq.ceiling > 43.0);
    }

    #[test]
    fn calibration_reports_cohort_mi_and_spreads_grades() {
        let projects: Vec<_> = (0..10).map(|i| project(i, 35.0 + i as f64 * 1.5)).collect();
        let spec = load_spec();
        let report = calibrate_spec(&projects, &spec, 10.0);
        // The cohort MI metric is still reported (informational): v4 does NOT
        // recalibrate mi_floor from it (Q11), but the sample histogram remains.
        let cq = report
            .metrics
            .iter()
            .find(|m| m.key == "code_quality_raw")
            .expect("cq metric");
        assert!(
            cq.suggested_floor < 50.0,
            "cohort MI min should sit below legacy mi_floor=50: {:?}",
            cq
        );
        // v4 spreads grades via work_scale + inventory anchors (not mi_floor):
        // the calibrated cohort has a real project_final range, not a flat band.
        assert!(
            report.grade_range_after.1 > report.grade_range_after.0,
            "calibrated cohort should have a non-degenerate spread: {:?}",
            report.grade_range_after
        );
    }

    #[test]
    fn work_scale_freezes_cohort_top_work_base_to_target() {
        let projects: Vec<_> = (0..6).map(|i| project(i, 90.0 + i as f64)).collect();
        let spec = load_spec();
        let target = 10.0;
        let report = calibrate_spec(&projects, &spec, target);
        assert!(report.work_scale > 0.0, "work_scale must be positive");

        // Apply the calibrated anchors + work_scale, then confirm the
        // gradable cohort-top work_base lands on the target.
        let mut calibrated = spec.clone();
        apply_anchors_to_spec(&mut calibrated, &report.anchors);
        calibrated
            .weights
            .insert("work_scale".into(), report.work_scale);
        let out = grade_cohort(&projects, &calibrated).expect("grade");
        let max_wb = out
            .projects
            .iter()
            .filter_map(|p| {
                p.output
                    .grades
                    .axes
                    .iter()
                    .find(|a| a.key == "work_base")
                    .and_then(|a| a.score)
            })
            .fold(0.0_f64, f64::max);
        assert!(
            (max_wb - target).abs() < 0.05,
            "cohort-top work_base {max_wb} should ≈ {target} after work_scale {}",
            report.work_scale
        );
    }

    #[test]
    fn arch_k_recomputed_from_layer_breach_distribution() {
        let spec = load_spec();
        let arch_cap = spec.weights.get("arch_cap").copied().expect("arch_cap");
        // A cohort with a real spread of layer breaches (0..4 critical each).
        let projects: Vec<_> = (0..5)
            .map(|i| {
                let mut p = project(i, 95.0);
                p.axis.arch_present = true;
                p.axis.arch_crit_count = i as f64;
                p
            })
            .collect();
        let report = calibrate_spec(&projects, &spec, 10.0);
        let arch_norm = report
            .anchors
            .get("arch_weighted")
            .map(|a| a.ceiling / 2.0)
            .expect("arch_weighted anchor");
        assert!(
            arch_norm >= 1.0,
            "expected a real breach: arch_norm {arch_norm}"
        );
        assert!(
            (report.arch_k - arch_cap / arch_norm).abs() < 1e-3,
            "arch_k {} should be arch_cap/arch_norm = {}",
            report.arch_k,
            arch_cap / arch_norm
        );
    }

    #[test]
    fn arch_k_kept_when_cohort_has_no_real_breach() {
        let spec = load_spec();
        let spec_arch_k = spec.weights.get("arch_k").copied().unwrap_or(1.0);
        // All teams clean (arch_present, zero breaches) → guard keeps spec arch_k.
        let projects: Vec<_> = (0..4)
            .map(|i| {
                let mut p = project(i, 95.0);
                p.axis.arch_present = true;
                p
            })
            .collect();
        let report = calibrate_spec(&projects, &spec, 10.0);
        assert!((report.arch_k - spec_arch_k).abs() < 1e-9);
    }

    #[test]
    fn apply_preserves_fixed_mi_anchors() {
        // v4 Q11: MI anchors are pure-absolute guards; calibration must NOT
        // overwrite mi_floor/mi_ceiling from a cohort-derived code_quality_raw.
        let mut spec = load_spec();
        let mi_floor = spec.weights.get("mi_floor").copied();
        let mi_ceiling = spec.weights.get("mi_ceiling").copied();
        let mut anchors = BTreeMap::new();
        anchors.insert(
            "code_quality_raw".into(),
            MetricAnchor {
                floor: 33.0,
                ceiling: 60.0,
            },
        );
        apply_anchors_to_spec(&mut spec, &anchors);
        assert_eq!(spec.weights.get("mi_floor").copied(), mi_floor);
        assert_eq!(spec.weights.get("mi_ceiling").copied(), mi_ceiling);
    }
}
