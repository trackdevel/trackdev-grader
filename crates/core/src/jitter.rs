//! Per-run threshold jitter (T-P2.6 — anti-gaming).
//!
//! When `[grading] hidden_thresholds = true`, every fractional detector
//! knob is uniformly jittered by `± jitter_pct` once at pipeline start.
//! The jitter is seeded by `(today, course_id)` so the same `--today`
//! reproduces, but a student reading `course.toml` cannot engineer
//! activity to sit just below a known threshold.
//!
//! The realised values are persisted to `pipeline_run` for audit; reports
//! show only the published threshold and the jitter band, never the
//! realised value (Manheim & Garrabrant 2018 on adversarial Goodhart).
//!
//! Discrete-integer thresholds (hours, line floors) are intentionally NOT
//! jittered: a "48-hour cramming window" rounded to 47 vs 49 doesn't give
//! the anti-gaming benefit, but it does make logs harder to reason about.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use rusqlite::{params, Connection};
use serde_json::json;
use tracing::info;

use crate::Config;

/// Tiny SplitMix64 — deterministic, no allocation, no external dep.
/// Adequate for sampling 18 floats per pipeline run.
#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// Uniform double in [0, 1) via the upper 53 bits.
    fn next_unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) / ((1u64 << 53) as f64)
    }

    /// Uniform double in `[low, high)`.
    fn uniform(&mut self, low: f64, high: f64) -> f64 {
        low + self.next_unit() * (high - low)
    }
}

/// Combine `today` and `course_id` into a deterministic seed.
pub fn seed_for(today: &str, course_id: u32) -> u64 {
    let mut h = DefaultHasher::new();
    today.hash(&mut h);
    course_id.hash(&mut h);
    h.finish()
}

/// Apply jitter to a single positive fractional threshold.
fn jitter_one(rng: &mut SplitMix64, value: f64, jitter_pct: f64) -> f64 {
    let lo = value * (1.0 - jitter_pct);
    let hi = value * (1.0 + jitter_pct);
    rng.uniform(lo, hi)
}

/// Apply jitter to a value that can be negative (e.g. `regularity_declining_delta`
/// is `-0.30`). The "magnitude" is what gets perturbed, sign preserved.
fn jitter_signed(rng: &mut SplitMix64, value: f64, jitter_pct: f64) -> f64 {
    let mag = value.abs();
    let jittered = jitter_one(rng, mag, jitter_pct);
    if value < 0.0 {
        -jittered
    } else {
        jittered
    }
}

/// Audit record persisted to `pipeline_run`.
#[derive(Debug, Clone)]
pub struct JitterRecord {
    pub run_id: String,
    pub today: String,
    pub course_id: u32,
    pub jitter_pct: f64,
    pub seed: u64,
    pub thresholds: BTreeMap<String, f64>,
}

/// Mutate `config` in-place to apply per-knob jitter. No-op when
/// `config.grading.hidden_thresholds` is false. Returns the audit record
/// (still useful with jitter=0 — provides a `pipeline_run` row for run id
/// tracking).
///
/// Order-pair invariants are enforced: for `(warn, crit)` pairs that
/// detector code reads as "warn ≤ crit", the jittered values are swapped
/// when needed so independent sampling doesn't flip the band semantics.
pub fn apply_threshold_jitter(config: &mut Config, today: &str, course_id: u32) -> JitterRecord {
    let seed = seed_for(today, course_id);
    let mut rng = SplitMix64::new(seed);
    let jitter_pct = if config.grading.hidden_thresholds {
        config.grading.jitter_pct.max(0.0)
    } else {
        0.0
    };
    let mut realised: BTreeMap<String, f64> = BTreeMap::new();

    if jitter_pct > 0.0 {
        // Independent samples for non-paired knobs. Sample first, store later
        // so a future field reorder doesn't change the seed sequence.
        let t = &mut config.thresholds;
        t.carrying_team_pct = jitter_one(&mut rng, t.carrying_team_pct, jitter_pct);
        t.cramming_commit_pct = jitter_one(&mut rng, t.cramming_commit_pct, jitter_pct);
        t.contribution_imbalance_stddev =
            jitter_one(&mut rng, t.contribution_imbalance_stddev, jitter_pct);
        t.low_survival_rate_stddev = jitter_one(&mut rng, t.low_survival_rate_stddev, jitter_pct);
        t.low_survival_absolute_floor =
            jitter_one(&mut rng, t.low_survival_absolute_floor, jitter_pct);
        t.raw_normalized_divergence_threshold =
            jitter_one(&mut rng, t.raw_normalized_divergence_threshold, jitter_pct);
        realised.insert("carrying_team_pct".into(), t.carrying_team_pct);
        realised.insert("cramming_commit_pct".into(), t.cramming_commit_pct);
        realised.insert(
            "contribution_imbalance_stddev".into(),
            t.contribution_imbalance_stddev,
        );
        realised.insert(
            "low_survival_rate_stddev".into(),
            t.low_survival_rate_stddev,
        );
        realised.insert(
            "low_survival_absolute_floor".into(),
            t.low_survival_absolute_floor,
        );
        realised.insert(
            "raw_normalized_divergence_threshold".into(),
            t.raw_normalized_divergence_threshold,
        );

        let dt = &mut config.detector_thresholds;
        // Ordered pair: gini_warn ≤ gini_crit — sample independently then
        // swap if jitter inverted them.
        let mut gw = jitter_one(&mut rng, dt.gini_warn, jitter_pct);
        let mut gc = jitter_one(&mut rng, dt.gini_crit, jitter_pct);
        if gw > gc {
            std::mem::swap(&mut gw, &mut gc);
        }
        dt.gini_warn = gw;
        dt.gini_crit = gc;
        // Composite is the inverse: composite_warn ≥ composite_crit (low score
        // is bad). Enforce the opposite invariant.
        let mut cw = jitter_one(&mut rng, dt.composite_warn, jitter_pct);
        let mut cc = jitter_one(&mut rng, dt.composite_crit, jitter_pct);
        if cw < cc {
            std::mem::swap(&mut cw, &mut cc);
        }
        dt.composite_warn = cw;
        dt.composite_crit = cc;
        dt.late_regularity = jitter_one(&mut rng, dt.late_regularity, jitter_pct);
        dt.team_inequality_outlier_deviation =
            jitter_one(&mut rng, dt.team_inequality_outlier_deviation, jitter_pct);
        // Trajectory CV bands: low ≤ high.
        let mut tcvl = jitter_one(&mut rng, dt.trajectory_cv_low, jitter_pct);
        let mut tcvh = jitter_one(&mut rng, dt.trajectory_cv_high, jitter_pct);
        if tcvl > tcvh {
            std::mem::swap(&mut tcvl, &mut tcvh);
        }
        dt.trajectory_cv_low = tcvl;
        dt.trajectory_cv_high = tcvh;
        dt.trajectory_slope_p_value = jitter_one(&mut rng, dt.trajectory_slope_p_value, jitter_pct);
        dt.regularity_declining_delta =
            jitter_signed(&mut rng, dt.regularity_declining_delta, jitter_pct);
        dt.cosmetic_rewrite_pct_of_lat =
            jitter_one(&mut rng, dt.cosmetic_rewrite_pct_of_lat, jitter_pct);
        dt.bulk_rename_adds_dels_ratio =
            jitter_one(&mut rng, dt.bulk_rename_adds_dels_ratio, jitter_pct);

        for (name, value) in [
            ("gini_warn", dt.gini_warn),
            ("gini_crit", dt.gini_crit),
            ("composite_warn", dt.composite_warn),
            ("composite_crit", dt.composite_crit),
            ("late_regularity", dt.late_regularity),
            (
                "team_inequality_outlier_deviation",
                dt.team_inequality_outlier_deviation,
            ),
            ("trajectory_cv_low", dt.trajectory_cv_low),
            ("trajectory_cv_high", dt.trajectory_cv_high),
            ("trajectory_slope_p_value", dt.trajectory_slope_p_value),
            ("regularity_declining_delta", dt.regularity_declining_delta),
            (
                "cosmetic_rewrite_pct_of_lat",
                dt.cosmetic_rewrite_pct_of_lat,
            ),
            (
                "bulk_rename_adds_dels_ratio",
                dt.bulk_rename_adds_dels_ratio,
            ),
        ] {
            realised.insert(name.into(), value);
        }
    }

    let run_id = format!("run-{:016x}", seed);
    info!(
        run_id = %run_id,
        hidden = config.grading.hidden_thresholds,
        jitter_pct,
        keys = realised.len(),
        "threshold jitter applied"
    );
    JitterRecord {
        run_id,
        today: today.into(),
        course_id,
        jitter_pct,
        seed,
        thresholds: realised,
    }
}

/// Persist the audit record. Idempotent: re-runs with the same seed
/// overwrite the row (`INSERT OR REPLACE`), which is the right semantic
/// since the realised values are a pure function of the seed.
pub fn record_pipeline_run(conn: &Connection, record: &JitterRecord) -> rusqlite::Result<()> {
    let thresholds_json = if record.thresholds.is_empty() {
        None
    } else {
        Some(json!(record.thresholds).to_string())
    };
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    conn.execute(
        "INSERT OR REPLACE INTO pipeline_run
            (run_id, today, course_id, jitter_pct, seed, thresholds_json, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            record.run_id,
            record.today,
            record.course_id as i64,
            record.jitter_pct,
            record.seed as i64,
            thresholds_json,
            now,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(hidden: bool, jitter: f64) -> Config {
        let mut c = Config::test_default();
        c.grading.hidden_thresholds = hidden;
        c.grading.jitter_pct = jitter;
        c
    }

    #[test]
    fn no_op_when_hidden_thresholds_disabled() {
        let mut c = cfg(false, 0.10);
        let baseline = c.detector_thresholds.gini_warn;
        let rec = apply_threshold_jitter(&mut c, "2026-04-26", 1);
        assert_eq!(c.detector_thresholds.gini_warn, baseline);
        assert!(rec.thresholds.is_empty(), "no jitter → no realised entries");
        assert_eq!(rec.jitter_pct, 0.0);
    }

    #[test]
    fn same_today_same_course_same_thresholds() {
        let mut a = cfg(true, 0.10);
        let mut b = cfg(true, 0.10);
        let ra = apply_threshold_jitter(&mut a, "2026-04-26", 7);
        let rb = apply_threshold_jitter(&mut b, "2026-04-26", 7);
        assert_eq!(ra.seed, rb.seed);
        assert_eq!(
            a.detector_thresholds.gini_warn,
            b.detector_thresholds.gini_warn
        );
        assert_eq!(
            a.thresholds.carrying_team_pct,
            b.thresholds.carrying_team_pct
        );
        assert_eq!(ra.thresholds, rb.thresholds);
    }

    #[test]
    fn different_today_can_differ_within_band() {
        let mut a = cfg(true, 0.10);
        let mut b = cfg(true, 0.10);
        apply_threshold_jitter(&mut a, "2026-04-26", 7);
        apply_threshold_jitter(&mut b, "2026-04-27", 7);
        assert_ne!(
            a.detector_thresholds.gini_warn, b.detector_thresholds.gini_warn,
            "different `today` should produce different jitter"
        );
        // Both must stay within the published ±jitter_pct band.
        let baseline = Config::test_default().detector_thresholds.gini_warn;
        for v in [
            a.detector_thresholds.gini_warn,
            b.detector_thresholds.gini_warn,
        ] {
            assert!(
                v >= baseline * 0.9 && v < baseline * 1.1,
                "out of band: {v}"
            );
        }
    }

    #[test]
    fn warn_crit_pairs_preserve_order() {
        // Hammer with extreme jitter so the un-protected sampling path would
        // routinely flip the order. Invariant: warn ≤ crit for gini, and
        // composite_warn ≥ composite_crit (composite is "low is bad").
        for day in ["2026-04-26", "2026-04-27", "2026-04-28", "2026-04-29"] {
            let mut c = cfg(true, 0.50);
            apply_threshold_jitter(&mut c, day, 1);
            assert!(
                c.detector_thresholds.gini_warn <= c.detector_thresholds.gini_crit,
                "gini ordering violated on {day}: {} > {}",
                c.detector_thresholds.gini_warn,
                c.detector_thresholds.gini_crit,
            );
            assert!(
                c.detector_thresholds.composite_warn >= c.detector_thresholds.composite_crit,
                "composite ordering violated on {day}: {} < {}",
                c.detector_thresholds.composite_warn,
                c.detector_thresholds.composite_crit,
            );
            assert!(
                c.detector_thresholds.trajectory_cv_low <= c.detector_thresholds.trajectory_cv_high,
                "trajectory_cv ordering violated on {day}",
            );
        }
    }

    #[test]
    fn signed_threshold_keeps_its_sign() {
        // regularity_declining_delta defaults to -0.30. Whatever jitter does,
        // the value must stay negative — a positive value would invert the
        // detector's "delta < threshold" condition.
        for day in ["2026-04-26", "2026-04-27", "2026-04-28"] {
            let mut c = cfg(true, 0.50);
            apply_threshold_jitter(&mut c, day, 1);
            assert!(
                c.detector_thresholds.regularity_declining_delta < 0.0,
                "regularity_declining_delta flipped sign on {day}: {}",
                c.detector_thresholds.regularity_declining_delta,
            );
        }
    }

    #[test]
    fn record_pipeline_run_writes_one_row_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::apply_schema(&conn).unwrap();
        let mut c = cfg(true, 0.05);
        let rec = apply_threshold_jitter(&mut c, "2026-04-26", 1);
        record_pipeline_run(&conn, &rec).unwrap();
        record_pipeline_run(&conn, &rec).unwrap(); // re-run same seed
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM pipeline_run", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "same seed must collapse to one row");
        let stored: String = conn
            .query_row("SELECT run_id FROM pipeline_run", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, rec.run_id);
    }
}
