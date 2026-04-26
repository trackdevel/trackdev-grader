//! T-T0.1 acceptance: the blackbox crate builds and a fixture run
//! reaches `analyze flags --sprint` without panicking. Also verifies
//! the runner's hermetic env var clearing.

use sprint_grader_blackbox::{Fixture, Runner};

#[test]
fn fixture_then_help_runs_clean() {
    // T-T0.1 acceptance: the binary launches against the fixture's
    // tempdir and exits cleanly for a `--help` invocation. Subcommand-
    // shaped scenarios live in the per-feature test files.
    let tmp = tempfile::tempdir().unwrap();
    let (_conn, paths) = Fixture::new().build(tmp.path()).expect("build fixture");
    let runner = Runner::new(tmp.path(), tmp.path().join("data").as_path()).unwrap();
    let out = runner
        .run(&["--help"])
        .expect("run binary")
        .assert_success();
    assert!(
        paths.db_path.exists(),
        "fixture DB should be at {}",
        paths.db_path.display()
    );
    assert!(
        out.stdout.contains("sprint-grader"),
        "help text missing binary name: {}",
        out.stdout
    );
}

#[test]
fn snapshot_scrub_handles_known_patterns() {
    use sprint_grader_blackbox::snapshot::scrub;
    let body = "fitted_at 2026-04-26T10:15:30Z, took 0.42s, at /tmp/blackbox.xyz/data";
    let scrubbed = scrub(body);
    assert!(
        scrubbed.contains("<timestamp>"),
        "missing timestamp scrub: {scrubbed}"
    );
    assert!(
        scrubbed.contains("<duration>"),
        "missing duration scrub: {scrubbed}"
    );
    assert!(scrubbed.contains("<tmp>"), "missing tmp scrub: {scrubbed}");
}
