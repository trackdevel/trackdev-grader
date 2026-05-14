//! Triage rule-evaluation order: every branch covered, including the
//! NaN escape hatch from `RidgeHead::predict` on dimension mismatch.

use sprint_grader_core::config::LocalEvaluateConfig;
use sprint_grader_evaluate_local::flags::DetFlag;
use sprint_grader_evaluate_local::triage::{Decision, PrPrediction, TriagePolicy};

fn policy() -> TriagePolicy {
    TriagePolicy::from_config(&LocalEvaluateConfig::default())
}

#[test]
fn empty_body_short_circuits_regardless_of_other_flags() {
    let pred = PrPrediction {
        title: 1.5,
        description: 2.0,
        total: 3.5,
    };
    let d = policy().decide(&[DetFlag::EmptyBody, DetFlag::GenericTitle], Some(&pred));
    match d {
        Decision::ShortCircuit { kind, regressor } => {
            assert_eq!(kind, DetFlag::EmptyBody);
            assert!((regressor.total - 3.5).abs() < 1e-9);
        }
        other => panic!("expected EmptyBody short-circuit, got {other:?}"),
    }
}

#[test]
fn task_id_only_short_circuits_before_generic_title() {
    let pred = PrPrediction {
        title: 1.5,
        description: 2.0,
        total: 3.5,
    };
    let d = policy().decide(
        &[DetFlag::TaskIdOnlyBody, DetFlag::GenericTitle],
        Some(&pred),
    );
    match d {
        Decision::ShortCircuit { kind, .. } => assert_eq!(kind, DetFlag::TaskIdOnlyBody),
        other => panic!("expected TaskIdOnlyBody short-circuit, got {other:?}"),
    }
}

#[test]
fn generic_title_short_circuits_only_when_regressor_present() {
    let pred = PrPrediction {
        title: 0.0,
        description: 2.0,
        total: 2.0,
    };
    let d = policy().decide(&[DetFlag::GenericTitle], Some(&pred));
    match d {
        Decision::ShortCircuit { kind, .. } => assert_eq!(kind, DetFlag::GenericTitle),
        other => panic!("expected GenericTitle short-circuit, got {other:?}"),
    }
}

#[test]
fn generic_title_alone_without_regressor_routes_to_llm() {
    let d = policy().decide(&[DetFlag::GenericTitle], None);
    match d {
        Decision::NeedsLlm { regressor } => assert_eq!(regressor, PrPrediction::ZERO),
        other => panic!("expected NeedsLlm with zero fallback, got {other:?}"),
    }
}

#[test]
fn no_regressor_routes_to_llm_with_zero_fallback() {
    let d = policy().decide(&[], None);
    match d {
        Decision::NeedsLlm { regressor } => assert_eq!(regressor, PrPrediction::ZERO),
        other => panic!("expected NeedsLlm, got {other:?}"),
    }
}

#[test]
fn nan_total_routes_to_llm_with_zero_fallback() {
    let pred = PrPrediction {
        title: 1.0,
        description: 1.0,
        total: f64::NAN,
    };
    let d = policy().decide(&[], Some(&pred));
    match d {
        Decision::NeedsLlm { regressor } => assert_eq!(regressor, PrPrediction::ZERO),
        other => panic!("expected NeedsLlm with zero fallback for NaN, got {other:?}"),
    }
}

#[test]
fn total_in_borderline_band_routes_to_llm() {
    let pred = PrPrediction {
        title: 1.0,
        description: 2.0,
        total: 3.0,
    };
    let d = policy().decide(&[], Some(&pred));
    match d {
        Decision::NeedsLlm { regressor } => assert!((regressor.total - 3.0).abs() < 1e-9),
        other => panic!("expected NeedsLlm, got {other:?}"),
    }
}

#[test]
fn far_from_grid_routes_to_llm() {
    // total below band_low (1.0) but title+description snap-distance is
    // greater than grid_snap_max (0.20) → Rule 6 fires.
    let pred = PrPrediction {
        title: 0.6,
        description: 0.0,
        total: 0.6,
    };
    // Snap target: snap_title(0.6) + snap_description(0.0) = 0.5 + 0.0 = 0.5
    // Distance from 0.6 to 0.5 is 0.10, below the 0.20 default → would NOT
    // trigger Rule 6. Bump grid_snap_max via config.
    let cfg = LocalEvaluateConfig {
        grid_snap_max: 0.05,
        ..LocalEvaluateConfig::default()
    };
    let pol = TriagePolicy::from_config(&cfg);
    let d = pol.decide(&[], Some(&pred));
    match d {
        Decision::NeedsLlm { regressor } => assert!((regressor.total - 0.6).abs() < 1e-9),
        other => panic!("expected NeedsLlm via rule 6, got {other:?}"),
    }
}

#[test]
fn snap_accepts_regressor_when_total_outside_band_and_near_grid() {
    // total above band_high (5.0), close to a grid point.
    let pred = PrPrediction {
        title: 2.0,
        description: 3.5,
        total: 5.5,
    };
    let d = policy().decide(&[], Some(&pred));
    match d {
        Decision::Snap {
            title,
            description,
            total,
        } => {
            assert_eq!(title, 2.0);
            assert_eq!(description, 3.5);
            assert!((total - 5.5).abs() < 1e-9);
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}

#[test]
fn snap_handles_below_band_low_with_grid_match() {
    // total = 0.0, below band_low (1.0). title=0.0, desc=0.0 → snapped to
    // (0, 0). Diff = 0 ≤ grid_snap_max. Rule 7 fires.
    let pred = PrPrediction::ZERO;
    let d = policy().decide(&[], Some(&pred));
    match d {
        Decision::Snap {
            title,
            description,
            total,
        } => {
            assert_eq!(title, 0.0);
            assert_eq!(description, 0.0);
            assert_eq!(total, 0.0);
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}
