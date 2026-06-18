//! Hybrid cohort normalization (Grading v2 Phase 2).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::anchor::MetricAnchor;
use crate::axes::collect_cohort_samples;
use crate::spec::{GradeOutput, GradeSpec};
use crate::types::RawProject;

/// Full cohort grading result: shared bounds + per-project grades.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CohortGradeOutput {
    pub bounds: CohortBounds,
    pub projects: Vec<CohortProjectGrade>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CohortProjectGrade {
    pub project_id: i64,
    pub output: GradeOutput,
    /// Hybrid-normalized 0–10 preview per raw metric (explainability).
    pub normalized: BTreeMap<String, f64>,
}

/// Per-metric cohort statistics used for hybrid 0–10 mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricBounds {
    pub floor: f64,
    pub ceiling: f64,
    pub p10: f64,
    pub p90: f64,
    pub sample_count: usize,
    /// When true, normalize on a fixed ruler (linear floor→ceiling = 0→10),
    /// ignoring the cohort p10/p90 band — an ABSOLUTE, cohort-independent score.
    /// Enabled by the spec weight `absolute_axes`.
    #[serde(default)]
    pub absolute: bool,
}

/// Cohort-wide bounds for every tracked raw metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CohortBounds {
    pub metrics: BTreeMap<String, MetricBounds>,
}

#[derive(Debug, Clone, Copy)]
struct RawSample {
    key: &'static str,
    value: f64,
    present: bool,
}

/// Collect present raw metric samples from a project (axis inputs only).
pub fn collect_raw_samples(raw: &RawProject) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for s in raw_samples(raw) {
        if s.present {
            out.insert(s.key.to_string(), s.value);
        }
    }
    out
}

fn raw_samples(raw: &RawProject) -> Vec<RawSample> {
    let a = &raw.axis;
    let arch_weighted = a.arch_crit_count * 2.0 + a.arch_warn_count * 0.5;
    vec![
        RawSample {
            key: "documentation_raw",
            value: a.documentation_raw,
            present: a.doc_present,
        },
        RawSample {
            key: "code_quality_raw",
            value: a.code_quality_raw,
            present: a.cq_present,
        },
        RawSample {
            key: "cc_pct",
            value: a.cc_pct,
            present: a.cq_present,
        },
        RawSample {
            key: "mutation_score",
            value: a.mutation_score,
            present: a.cq_present && a.mutation_score > 0.0,
        },
        RawSample {
            key: "survival_raw",
            value: a.survival_raw,
            present: a.surv_present,
        },
        RawSample {
            key: "arch_weighted",
            value: arch_weighted,
            present: a.arch_present,
        },
    ]
}

/// Resolve absolute floor/ceiling for a metric from spec anchors or legacy weights.
pub fn resolve_anchor(spec: &GradeSpec, key: &str) -> MetricAnchor {
    if let Some(a) = spec.anchors.get(key) {
        return a.clone();
    }
    let w = &spec.weights;
    match key {
        "documentation_raw" => MetricAnchor {
            floor: 0.0,
            ceiling: w.get("doc_max").copied().unwrap_or(6.0),
        },
        "code_quality_raw" => MetricAnchor {
            floor: w.get("mi_floor").copied().unwrap_or(50.0),
            ceiling: w.get("mi_ceiling").copied().unwrap_or(85.0),
        },
        "cc_pct" => MetricAnchor {
            floor: 0.0,
            ceiling: 100.0,
        },
        "mutation_score" => MetricAnchor {
            floor: 0.0,
            ceiling: 1.0,
        },
        "survival_raw" => MetricAnchor {
            floor: w.get("surv_floor").copied().unwrap_or(0.5),
            ceiling: w.get("surv_ceiling").copied().unwrap_or(0.95),
        },
        "arch_weighted" => MetricAnchor {
            floor: 0.0,
            ceiling: w.get("arch_norm").copied().unwrap_or(4.0) * 2.0,
        },
        "violation_density" => MetricAnchor {
            floor: 0.0,
            ceiling: w.get("density_ceiling").copied().unwrap_or(5.0),
        },
        "production_loc" => MetricAnchor {
            floor: 0.0,
            ceiling: w.get("prod_loc_ceiling").copied().unwrap_or(50_000.0),
        },
        "production_statement_count" => MetricAnchor {
            floor: 0.0,
            ceiling: w.get("prod_stmt_ceiling").copied().unwrap_or(15_000.0),
        },
        "endpoint_count"
        | "controller_count"
        | "entity_count"
        | "repository_count"
        | "fragment_count"
        | "activity_count"
        | "viewmodel_count"
        | "room_database_count" => MetricAnchor {
            floor: 0.0,
            ceiling: w.get("inventory_count_ceiling").copied().unwrap_or(50.0),
        },
        "custom_query_count"
        | "scheduled_task_count"
        | "observe_call_count"
        | "nav_dispatch_count"
        | "reactive_state_field_count" => MetricAnchor {
            floor: 0.0,
            ceiling: w.get("inventory_depth_ceiling").copied().unwrap_or(30.0),
        },
        "reactive_wiring_density"
        | "nav_dispatch_density"
        | "avg_cc_per_controller"
        | "avg_cc_per_fragment"
        | "avg_statements_per_endpoint" => MetricAnchor {
            floor: 0.0,
            ceiling: w.get("inventory_density_ceiling").copied().unwrap_or(10.0),
        },
        _ => MetricAnchor {
            floor: 0.0,
            ceiling: 1.0,
        },
    }
}

/// Build cohort bounds from all projects in the grading batch.
pub fn compute_cohort_bounds(projects: &[RawProject], spec: &GradeSpec) -> CohortBounds {
    let by_key = collect_cohort_samples(projects);
    // `absolute_axes` switches every metric to a fixed floor→ceiling ruler.
    let absolute = spec.weights.get("absolute_axes").copied().unwrap_or(0.0) != 0.0;

    let mut metrics = BTreeMap::new();
    for key in by_key.keys() {
        let Some(samples) = by_key.get(key) else {
            continue;
        };
        if samples.is_empty() {
            continue;
        }
        let anchor = resolve_anchor(spec, key);
        let mut sorted = samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        metrics.insert(
            key.clone(),
            MetricBounds {
                floor: anchor.floor,
                ceiling: anchor.ceiling,
                p10: percentile_linear(&sorted, 0.10),
                p90: percentile_linear(&sorted, 0.90),
                sample_count: sorted.len(),
                absolute,
            },
        );
    }
    CohortBounds { metrics }
}

/// Map one raw value to 0–10 using hybrid cohort rules.
pub fn hybrid_normalize(value: f64, bounds: &MetricBounds) -> f64 {
    // Absolute mode: fixed ruler, linear floor→ceiling = 0→10, cohort-independent.
    if bounds.absolute {
        if bounds.ceiling <= bounds.floor {
            return 0.0;
        }
        return (((value - bounds.floor) / (bounds.ceiling - bounds.floor)) * 10.0)
            .clamp(0.0, 10.0);
    }
    if value <= bounds.floor {
        if bounds.floor <= 0.0 {
            return 0.0;
        }
        return linear_map(value, 0.0, bounds.floor, 0.0, 2.0).clamp(0.0, 2.0);
    }
    if value >= bounds.ceiling {
        let top = bounds.ceiling + (bounds.ceiling - bounds.floor).max(1.0);
        return linear_map(value, bounds.ceiling, top, 8.0, 10.0).clamp(8.0, 10.0);
    }
    if (bounds.p90 - bounds.p10).abs() < 1e-12 {
        return 5.0;
    }
    linear_map(value, bounds.p10, bounds.p90, 2.0, 8.0).clamp(2.0, 8.0)
}

/// Hybrid-normalize all present raw metrics for one project.
pub fn normalize_project_metrics(raw: &RawProject, bounds: &CohortBounds) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for (key, value) in collect_raw_samples(raw) {
        if let Some(b) = bounds.metrics.get(&key) {
            out.insert(key, hybrid_normalize(value, b));
        }
    }
    out
}

fn linear_map(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    if (x1 - x0).abs() < 1e-12 {
        return y0;
    }
    y0 + (x - x0) / (x1 - x0) * (y1 - y0)
}

/// Linear-interpolation percentile on a sorted slice (`p` in [0, 1]).
pub fn percentile_linear(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let w = pos - lo as f64;
    sorted[lo] * (1.0 - w) + sorted[hi] * w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AxisInputs;

    fn axis() -> AxisInputs {
        AxisInputs {
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
        }
    }

    fn project(id: i64, mi: f64) -> RawProject {
        RawProject {
            project_id: id,
            name: format!("p{id}"),
            team_size: 2,
            axis: AxisInputs {
                code_quality_raw: mi,
                cq_present: true,
                ..axis()
            },
            inventory: vec![],
            tasks: vec![],
            students: vec![],
            crit_findings: vec![],
            student_flags: vec![],
        }
    }

    #[test]
    fn percentile_linear_interpolates() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((percentile_linear(&v, 0.0) - 1.0).abs() < 1e-9);
        assert!((percentile_linear(&v, 1.0) - 5.0).abs() < 1e-9);
        assert!((percentile_linear(&v, 0.5) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn hybrid_middle_band_maps_p10_to_2_and_p90_to_8() {
        let b = MetricBounds {
            floor: 0.0,
            ceiling: 100.0,
            p10: 2.0,
            p90: 8.0,
            sample_count: 5,
            absolute: false,
        };
        assert!((hybrid_normalize(2.0, &b) - 2.0).abs() < 1e-9);
        assert!((hybrid_normalize(8.0, &b) - 8.0).abs() < 1e-9);
        assert!((hybrid_normalize(5.0, &b) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn absolute_mode_is_linear_floor_to_ceiling() {
        let b = MetricBounds {
            floor: 0.0,
            ceiling: 100.0,
            p10: 2.0,
            p90: 8.0,
            sample_count: 5,
            absolute: true,
        };
        // Fixed ruler: value/ceiling × 10, independent of p10/p90.
        assert!((hybrid_normalize(50.0, &b) - 5.0).abs() < 1e-9);
        assert!((hybrid_normalize(100.0, &b) - 10.0).abs() < 1e-9);
        assert!((hybrid_normalize(150.0, &b) - 10.0).abs() < 1e-9); // clamped
        assert!((hybrid_normalize(0.0, &b) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn hybrid_floor_and_ceiling_bands() {
        let b = MetricBounds {
            floor: 2.0,
            ceiling: 8.0,
            p10: 3.0,
            p90: 7.0,
            sample_count: 5,
            absolute: false,
        };
        assert!(hybrid_normalize(0.0, &b) >= 0.0 && hybrid_normalize(0.0, &b) <= 2.0);
        assert!((hybrid_normalize(2.0, &b) - 2.0).abs() < 1e-9);
        assert!((hybrid_normalize(8.0, &b) - 8.0).abs() < 1e-9);
        assert!(hybrid_normalize(20.0, &b) >= 8.0 && hybrid_normalize(20.0, &b) <= 10.0);
    }

    #[test]
    fn cohort_bounds_use_full_batch() {
        let projects = vec![project(1, 1.0), project(2, 3.0), project(3, 5.0)];
        let spec = GradeSpec {
            meta: Default::default(),
            weights: BTreeMap::new(),
            anchors: BTreeMap::new(),
            models: BTreeMap::new(),
            levels: BTreeMap::new(),
            formulas: Default::default(),
            manual_fields: Default::default(),
            constants: Vec::new(),
        };
        let bounds = compute_cohort_bounds(&projects, &spec);
        let mi = bounds.metrics.get("code_quality_raw").expect("mi");
        assert_eq!(mi.sample_count, 3);
        assert!(mi.p10 <= mi.p90);
    }
}
