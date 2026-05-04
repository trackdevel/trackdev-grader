//! End-to-end SpotBugs smoke test. `#[ignore]`d so it stays green on
//! hosts without SpotBugs installed. Run with `--ignored` after either
//! setting `SPOTBUGS_HOME` or running `scripts/install-analyzers.sh`.
//!
//! Unlike the PMD/Checkstyle smoke tests, SpotBugs needs compiled class
//! files. We compile the fixture `Foo.java` into a scratch directory
//! using `javac`, then point SpotBugs at it.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use sprint_grader_static_analysis::{
    spotbugs::SpotBugs, Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerStatus, Severity,
};

fn fixture_source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("foo_unused_field")
}

#[test]
#[ignore = "requires SpotBugs (SPOTBUGS_HOME, /opt/spotbugs, or vendored); run with --ignored"]
fn spotbugs_runs_against_compiled_fixture() {
    let spotbugs = match SpotBugs::discover(false) {
        Some(s) => s,
        None => {
            eprintln!(
                "SpotBugs launcher not found; set SPOTBUGS_HOME or run \
                 scripts/install-analyzers.sh. Skipping smoke test."
            );
            return;
        }
    };

    let src = fixture_source_root();
    let work = tempfile::tempdir().expect("scratch dir");
    let classes_dir = work.path().join("classes");
    std::fs::create_dir_all(&classes_dir).unwrap();

    // Compile the fixture so SpotBugs has something to analyze.
    let foo_java = src.join("Foo.java");
    let javac_status = Command::new("javac")
        .arg("-d")
        .arg(&classes_dir)
        .arg(&foo_java)
        .status()
        .expect("invoke javac");
    if !javac_status.success() {
        eprintln!("javac failed; skipping SpotBugs smoke");
        return;
    }

    let input = AnalyzerInput {
        repo_path: &src,
        repo_full_name: "fixture/foo",
        head_sha: None,
        source_roots: vec![src.clone()],
        class_roots: vec![classes_dir.clone()],
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

    let out = spotbugs.run(&input, &cfg);
    assert_eq!(
        out.status,
        AnalyzerStatus::Ok,
        "SpotBugs must complete cleanly; diagnostics:\n{}",
        out.diagnostics
    );
    // Assertion is loose by design — SpotBugs may or may not flag a
    // trivial unused-field fixture depending on the rank threshold and
    // the bundled detector set. We require only that the run succeeded
    // and that any findings are tagged correctly.
    for f in &out.findings {
        assert_eq!(f.analyzer, "spotbugs");
    }
}
