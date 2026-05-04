//! End-to-end PMD smoke test.
//!
//! Marked `#[ignore]` so `cargo test --workspace` stays green on hosts
//! without PMD installed. Run explicitly with:
//!
//!     PMD_HOME=/path/to/pmd-bin-7.7.0 \
//!         cargo test -p sprint-grader-static-analysis -- --ignored pmd_smoke
//!
//! Or, with the analyzer JAR fetched via `scripts/install-analyzers.sh`,
//! the discovery picks up the vendored launcher with no env var.

use std::path::PathBuf;
use std::time::Duration;

use sprint_grader_static_analysis::{
    pmd::Pmd, Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerStatus, Severity,
};

/// Returns the path to the bundled fixture project.
fn fixture_project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("foo_unused_field")
}

#[test]
#[ignore = "requires PMD on PATH or PMD_HOME env; run with --ignored"]
fn pmd_runs_against_fixture_and_finds_unused_private_field() {
    let pmd = match Pmd::discover() {
        Some(p) => p,
        None => {
            eprintln!(
                "PMD launcher not found; run scripts/install-analyzers.sh or set PMD_HOME. \
                 Skipping the smoke test."
            );
            return;
        }
    };

    let repo = fixture_project_root();
    assert!(repo.exists(), "fixture must exist at {}", repo.display());

    let work = tempfile::tempdir().expect("scratch dir");

    let input = AnalyzerInput {
        repo_path: &repo,
        repo_full_name: "fixture/foo",
        head_sha: None,
        source_roots: vec![repo.clone()],
        class_roots: vec![],
        jdk_major: 21,
        work_dir: work.path().to_path_buf(),
        timeout: Duration::from_secs(60),
        max_heap_mb: 512,
        locale: "en".into(),
    };
    let cfg = AnalyzerConfig {
        ruleset_ref: "standard".into(),
        severity_floor: Severity::Info,
        max_findings: 200,
    };

    let out = pmd.run(&input, &cfg);
    assert_eq!(
        out.status,
        AnalyzerStatus::Ok,
        "PMD must complete cleanly; diagnostics:\n{}",
        out.diagnostics
    );
    assert!(
        !out.findings.is_empty(),
        "PMD should produce at least one finding for the fixture"
    );
    let rules: std::collections::HashSet<&str> =
        out.findings.iter().map(|f| f.rule_id.as_str()).collect();
    assert!(
        rules.contains("UnusedPrivateField"),
        "expected UnusedPrivateField in {:?}",
        rules
    );
    for f in &out.findings {
        assert_eq!(f.analyzer, "pmd");
        assert!(
            f.file_path.ends_with("Foo.java"),
            "file_path: {}",
            f.file_path
        );
    }
}
