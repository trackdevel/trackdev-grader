//! End-to-end Checkstyle smoke test. Same shape as `pmd_smoke.rs`:
//! `#[ignore]` so `cargo test --workspace` stays green when no jar is
//! reachable. Run with `--ignored` after either setting `CHECKSTYLE_JAR`
//! or running `scripts/install-analyzers.sh`.

use std::path::PathBuf;
use std::time::Duration;

use sprint_grader_static_analysis::{
    checkstyle::Checkstyle, Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerStatus, Severity,
};

fn fixture_project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("foo_unused_field")
}

#[test]
#[ignore = "requires CHECKSTYLE_JAR or vendored jar; run with --ignored"]
fn checkstyle_runs_against_fixture_and_emits_findings() {
    let cs = match Checkstyle::discover() {
        Some(c) => c,
        None => {
            eprintln!(
                "Checkstyle jar not found; set CHECKSTYLE_JAR or run \
                 scripts/install-analyzers.sh. Skipping the smoke test."
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
        max_heap_mb: 256,
        locale: "es".into(),
    };
    let cfg = AnalyzerConfig {
        ruleset_ref: "standard".into(),
        severity_floor: Severity::Info,
        max_findings: 200,
    };

    let out = cs.run(&input, &cfg);
    assert_eq!(
        out.status,
        AnalyzerStatus::Ok,
        "Checkstyle must produce a parseable SARIF; diagnostics:\n{}",
        out.diagnostics
    );
    // The fixture has a public method without Javadoc; standard preset
    // includes `MissingJavadocMethod` so we expect at least one finding.
    assert!(
        !out.findings.is_empty(),
        "expected at least one Checkstyle finding for missing Javadoc"
    );
    for f in &out.findings {
        assert_eq!(f.analyzer, "checkstyle");
        assert!(
            f.file_path.ends_with("Foo.java"),
            "file_path: {}",
            f.file_path
        );
    }

    // Cross-check vs PMD T2: the two analyzers should *not* fire on the
    // same set of rule ids. Sanity that they aren't trivially redundant.
    let rules: std::collections::HashSet<&str> =
        out.findings.iter().map(|f| f.rule_id.as_str()).collect();
    assert!(
        !rules.contains("UnusedPrivateField"),
        "Checkstyle shouldn't surface PMD's UnusedPrivateField rule"
    );
}
