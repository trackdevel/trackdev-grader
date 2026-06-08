//! Track B PB: file-pass helpers without invoking Claude.

use sprint_grader_core::Config;
use sprint_grader_core::Database;
use sprint_grader_quality_llm::{load_rubric, parse_quality_flags_json, run_file_pass};
use tempfile::tempdir;

const MINIMAL: &str = r#"
[course]
name = "test"
num_sprints = 1
pm_base_url = "https://example.test"
github_org = "org"
course_id = 1
[thresholds]
carrying_team_pct = 0.4
cramming_hours = 48
cramming_commit_pct = 0.7
single_commit_dump_lines = 200
micro_pr_max_lines = 10
low_doc_score = 2
contribution_imbalance_stddev = 1.5
contribution_imbalance_min_abs_deviation = 0.05
low_survival_rate_stddev = 1.5
low_survival_absolute_floor = 0.85
raw_normalized_divergence_threshold = 0.2
[[build.profiles]]
repo_pattern = "^spring-"
command = "./gradlew bootJar"
timeout_seconds = 60
working_dir = "."
[evaluate]
model_id = "claude-haiku-4-5-20251001"
[quality_llm]
model_id = "claude-haiku-4-5-20251001"
claude_cli_path = "/nonexistent/claude"
"#;

#[test]
fn file_pass_with_no_candidates_skips_cli_check() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("course.toml"), MINIMAL).unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# rubric\n").unwrap();
    let course = Config::load(dir.path()).unwrap();
    let rubric = load_rubric(dir.path(), &course.quality_llm).unwrap();
    let db = Database::open(&dir.path().join("g.db")).unwrap();
    db.create_tables().unwrap();
    db.conn
        .execute(
            "INSERT INTO projects (id, slug, name) VALUES (1,'t','T')",
            [],
        )
        .unwrap();

    let stats = run_file_pass(
        &db.conn,
        1,
        "T",
        dir.path(),
        &course.quality_llm,
        &rubric,
        &[],
        false,
    )
    .unwrap();
    assert_eq!(stats.judged, 0);
}

#[test]
fn parse_quality_flags_handles_multiple_entries() {
    let raw = r#"{
      "flags": [
        {"category":"testing","severity":"INFO","summary":"No unit tests"},
        {"category":"complexity","severity":"WARNING","summary":"Deep nesting","detail":"Four levels in handleSave."}
      ]
    }"#;
    let flags = parse_quality_flags_json(raw).unwrap();
    assert_eq!(flags.len(), 2);
    assert_eq!(flags[1].severity, "WARNING");
}
