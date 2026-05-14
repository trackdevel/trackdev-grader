//! Decide what to do for each PR given its detected flags and (optional)
//! ridge prediction. Rules are evaluated in order; the first one that
//! fires wins.
//!
//! Rule order is load-bearing: `ShortCircuit` short-circuits the LLM
//! call, `NeedsLlm` defers to the chat backend, `Snap` accepts the
//! regressor prediction as-is. See plan §"Triage".

use sprint_grader_core::config::LocalEvaluateConfig;

use crate::flags::DetFlag;
use crate::persist::{snap_description, snap_title};

/// The regressor's three predictions for a single PR. `total` is what
/// triage's band check reads; `title` / `description` flow through to the
/// `Snap` decision (after grid-snapping).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrPrediction {
    pub title: f64,
    pub description: f64,
    pub total: f64,
}

impl PrPrediction {
    pub const ZERO: Self = Self {
        title: 0.0,
        description: 0.0,
        total: 0.0,
    };
}

/// What to do with this PR. The shape carries the regressor prediction
/// through to persistence so the short-circuit and NeedsLlm branches can
/// reuse a regressor mean as the fallback score.
#[derive(Debug, Clone)]
pub enum Decision {
    /// Accept the regressor output. Use the snapped scores verbatim.
    Snap {
        title: f64,
        description: f64,
        total: f64,
    },
    /// Borderline / disabled regressor. Defer to the LLM. The carried
    /// regressor prediction is the fallback if the LLM call fails.
    NeedsLlm { regressor: PrPrediction },
    /// Short-circuit: a deterministic flag fired. The persist layer
    /// applies a flag-specific score mapping using the regressor's
    /// title / description prediction where applicable.
    ShortCircuit {
        kind: DetFlag,
        regressor: PrPrediction,
    },
}

/// Triage thresholds, derived from [`LocalEvaluateConfig`].
#[derive(Debug, Clone, Copy)]
pub struct TriagePolicy {
    pub band_low: f64,
    pub band_high: f64,
    pub grid_snap_max: f64,
}

impl TriagePolicy {
    pub fn from_config(cfg: &LocalEvaluateConfig) -> Self {
        Self {
            band_low: cfg.pr_total_band_low,
            band_high: cfg.pr_total_band_high,
            grid_snap_max: cfg.grid_snap_max,
        }
    }

    /// Apply the triage rules in evaluation order. See plan §"Triage" for
    /// the authoritative spec.
    pub fn decide(&self, flags: &[DetFlag], regressor: Option<&PrPrediction>) -> Decision {
        // Rule 1: EmptyBody dominates (always short-circuit at 0 description).
        if flags.contains(&DetFlag::EmptyBody) {
            return Decision::ShortCircuit {
                kind: DetFlag::EmptyBody,
                regressor: regressor.copied().unwrap_or(PrPrediction::ZERO),
            };
        }
        // Rule 2: TaskIdOnlyBody is also a content-free body.
        if flags.contains(&DetFlag::TaskIdOnlyBody) {
            return Decision::ShortCircuit {
                kind: DetFlag::TaskIdOnlyBody,
                regressor: regressor.copied().unwrap_or(PrPrediction::ZERO),
            };
        }
        // Rule 3: GenericTitle is a short-circuit only when we already have
        // a regressor prediction for the description; otherwise we want the
        // LLM to weigh in.
        if flags.contains(&DetFlag::GenericTitle) {
            if let Some(pred) = regressor {
                return Decision::ShortCircuit {
                    kind: DetFlag::GenericTitle,
                    regressor: *pred,
                };
            }
        }
        // Rule 4: No regressor or NaN prediction → route to LLM with the
        // zero fallback. `f64::NAN.is_nan()` covers dim-mismatch.
        let pred = match regressor {
            Some(p) if !p.total.is_nan() => *p,
            _ => {
                return Decision::NeedsLlm {
                    regressor: PrPrediction::ZERO,
                }
            }
        };
        // Rule 5: total lands in the borderline band → LLM.
        if pred.total >= self.band_low && pred.total <= self.band_high {
            return Decision::NeedsLlm { regressor: pred };
        }
        // Rule 6: total snaps further than grid_snap_max from a grid cell → LLM.
        let snap_t = snap_title(pred.title);
        let snap_d = snap_description(pred.description);
        let snapped_total = snap_t + snap_d;
        if (pred.total - snapped_total).abs() > self.grid_snap_max {
            return Decision::NeedsLlm { regressor: pred };
        }
        // Rule 7: accept the regressor as-is.
        Decision::Snap {
            title: snap_t,
            description: snap_d,
            total: snap_t + snap_d,
        }
    }
}
