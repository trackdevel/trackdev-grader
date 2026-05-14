//! Ridge-head dot-product semantics. The fixtures live next to this file
//! (`tests/fixtures/regressor/`) and ship with all-zero coefficients +
//! intercepts {1.5, 2.0, 3.5} so the prediction equals the intercept and
//! the test stays independent of any concrete embedding model.

use std::path::PathBuf;

use sprint_grader_evaluate_local::ridge::{PrRidgeBundle, RidgeHead};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/regressor")
}

#[test]
fn ridge_head_load_round_trips_through_serde() {
    let head = RidgeHead::load(&fixtures_dir().join("pr_title.json")).unwrap();
    assert_eq!(head.embedding_model, "bge-m3");
    assert_eq!(head.embedding_dim, 1024);
    assert_eq!(head.intercept, 1.5);
    assert_eq!(head.coefficients.len(), 1024);
    assert_eq!(head.n_train, 938);
}

#[test]
fn zero_coefficients_collapse_predict_to_intercept() {
    let title = RidgeHead::load(&fixtures_dir().join("pr_title.json")).unwrap();
    let zero = vec![0.0f32; 1024];
    let one = vec![1.0f32; 1024];
    assert!((title.predict(&zero) - 1.5).abs() < 1e-9);
    // With zero coefficients, the prediction equals the intercept
    // regardless of the input.
    assert!((title.predict(&one) - 1.5).abs() < 1e-9);
}

#[test]
fn dimension_mismatch_returns_nan_not_panic() {
    let title = RidgeHead::load(&fixtures_dir().join("pr_title.json")).unwrap();
    let too_short = vec![0.5f32; 512];
    assert!(title.predict(&too_short).is_nan());
    let too_long = vec![0.5f32; 2048];
    assert!(title.predict(&too_long).is_nan());
}

#[test]
fn bundle_load_optional_returns_some_when_all_three_present() {
    let bundle = PrRidgeBundle::load_optional(&fixtures_dir())
        .unwrap()
        .unwrap();
    assert!((bundle.title.intercept - 1.5).abs() < 1e-9);
    assert!((bundle.description.intercept - 2.0).abs() < 1e-9);
    assert!((bundle.total.intercept - 3.5).abs() < 1e-9);
}

#[test]
fn bundle_load_optional_returns_none_when_dir_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let bundle = PrRidgeBundle::load_optional(&missing).unwrap();
    assert!(bundle.is_none());
}

#[test]
fn bundle_load_optional_returns_none_when_one_head_missing() {
    let tmp = tempfile::tempdir().unwrap();
    // Only two of three files exist.
    let src = fixtures_dir();
    std::fs::copy(src.join("pr_title.json"), tmp.path().join("pr_title.json")).unwrap();
    std::fs::copy(
        src.join("pr_description.json"),
        tmp.path().join("pr_description.json"),
    )
    .unwrap();
    let bundle = PrRidgeBundle::load_optional(tmp.path()).unwrap();
    assert!(bundle.is_none());
}

#[test]
fn bundle_load_optional_propagates_parse_errors_for_corrupt_files() {
    let tmp = tempfile::tempdir().unwrap();
    let src = fixtures_dir();
    std::fs::copy(src.join("pr_title.json"), tmp.path().join("pr_title.json")).unwrap();
    std::fs::copy(
        src.join("pr_description.json"),
        tmp.path().join("pr_description.json"),
    )
    .unwrap();
    std::fs::write(tmp.path().join("pr_total.json"), b"{not json").unwrap();
    let result = PrRidgeBundle::load_optional(tmp.path());
    assert!(result.is_err(), "corrupt JSON must propagate as Err");
}
