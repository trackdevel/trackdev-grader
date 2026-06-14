//! CLI-facing anchor calibration against a live grading.db.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use grade_core::{calibrate_spec, CalibrateReport, GradeSpec};
use serde_json::Value;
use sprint_grader_core::Database;
use tracing::warn;

use crate::grading_projection::load_cohort_raw_projects;

/// Load cohort projects from the DB and build a calibration report.
/// `target_top` freezes `work_scale` so the gradable cohort-top `work_base`
/// reaches that grade (v4 T3.2; default 10.0 at the CLI).
pub fn run_calibrate_anchors(
    db: &Database,
    today: &str,
    spec_path: &Path,
    project_filter: Option<&[String]>,
    target_top: f64,
) -> Result<CalibrateReport> {
    let spec = load_grade_spec(spec_path)?;
    let projects = load_cohort_raw_projects(db, today, project_filter)?;
    if projects.is_empty() {
        anyhow::bail!("no projects loaded from grading.db");
    }
    let report = calibrate_spec(&projects, &spec, target_top);
    if !report.metrics.iter().any(|m| m.key == "code_quality_raw") {
        warn!(
            "no code_quality_raw samples in cohort — maintainability is absent from the \
             quality multiplier; run `sprint-grader quality` (or `run-all`) so \
             student_sprint_quality is populated (mi_floor/mi_ceiling are fixed v4 anchors)"
        );
    }
    Ok(report)
}

/// Patch `grading.standard.json` in place, preserving top-level keys.
pub fn apply_calibration_to_spec_file(spec_path: &Path, report: &CalibrateReport) -> Result<()> {
    let text =
        fs::read_to_string(spec_path).with_context(|| format!("read {}", spec_path.display()))?;
    let mut root: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", spec_path.display()))?;

    root["anchors"] = serde_json::to_value(&report.anchors)?;

    // v4 (Q11): MI is a fixed absolute guard — the anchors-block
    // `code_quality_raw` must mirror the (preserved) mi_floor/mi_ceiling
    // weights, NOT the cohort-derived value, so the committed spec never
    // advertises an MI range the grade doesn't use.
    let mi_floor = root["weights"].get("mi_floor").and_then(Value::as_f64);
    let mi_ceiling = root["weights"].get("mi_ceiling").and_then(Value::as_f64);
    if let (Some(f), Some(c)) = (mi_floor, mi_ceiling) {
        root["anchors"]["code_quality_raw"] = serde_json::json!({ "floor": f, "ceiling": c });
    }

    let weights = root
        .get_mut("weights")
        .and_then(Value::as_object_mut)
        .context("spec missing weights object")?;

    // v4 (Q11): mi_floor / mi_ceiling are pure-absolute MI guard anchors and
    // are intentionally NOT recalibrated from the cohort (see sync_legacy_weights).
    if let Some(a) = report.anchors.get("arch_weighted") {
        weights.insert("arch_norm".into(), Value::from(a.ceiling / 2.0));
    }
    if let Some(a) = report.anchors.get("violation_density") {
        weights.insert("density_ceiling".into(), Value::from(a.ceiling));
    }
    if let Some(a) = report.anchors.get("production_loc") {
        weights.insert("prod_loc_ceiling".into(), Value::from(a.ceiling));
    }
    // v4 T3.2: freeze the work scale and the layer-architecture penalty slope.
    weights.insert("work_scale".into(), Value::from(report.work_scale));
    weights.insert("arch_k".into(), Value::from(report.arch_k));

    let note = format!(
        "Anchors auto-calibrated ({} projects): work_scale={:.4}, arch_k={:.4}; \
         project_final {:.2}–{:.2} → {:.2}–{:.2}. MI/arch/inventory floor & \
         ceiling from cohort min/max; work_scale freezes the cohort-top work_base \
         to the calibration target.",
        report.project_count,
        report.work_scale,
        report.arch_k,
        report.grade_range_before.0,
        report.grade_range_before.1,
        report.grade_range_after.0,
        report.grade_range_after.1,
    );
    if let Some(notes) = root.get_mut("notes").and_then(Value::as_array_mut) {
        notes.retain(|n| {
            n.as_str()
                .map(|s| !s.starts_with("Anchors auto-calibrated"))
                .unwrap_or(true)
        });
        notes.push(Value::String(note));
    }

    let pretty = serde_json::to_string_pretty(&root)?;
    fs::write(spec_path, format!("{pretty}\n"))
        .with_context(|| format!("write {}", spec_path.display()))?;
    Ok(())
}

pub fn load_grade_spec(path: &Path) -> Result<GradeSpec> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse grade spec {}", path.display()))
}

pub fn format_report_summary(report: &CalibrateReport) -> String {
    let mut lines = vec![
        format!("projects: {}", report.project_count),
        format!(
            "work_scale: {:.4}  (freezes cohort-top work_base to the target)",
            report.work_scale
        ),
        format!(
            "arch_k: {:.4}  (layer-architecture penalty slope)",
            report.arch_k
        ),
        format!(
            "project_final range: {:.2}–{:.2} → {:.2}–{:.2}",
            report.grade_range_before.0,
            report.grade_range_before.1,
            report.grade_range_after.0,
            report.grade_range_after.1,
        ),
        String::new(),
        "metric samples (current → suggested floor/ceiling):".into(),
    ];
    for m in &report.metrics {
        lines.push(format!(
            "  {} (n={}): min={:.2} p10={:.2} p90={:.2} max={:.2} | floor {:.2}/{:.2} → {:.2}/{:.2}",
            m.key,
            m.sample_count,
            m.min,
            m.p10,
            m.p90,
            m.max,
            m.current_floor,
            m.current_ceiling,
            m.suggested_floor,
            m.suggested_ceiling,
        ));
    }
    lines.join("\n")
}
