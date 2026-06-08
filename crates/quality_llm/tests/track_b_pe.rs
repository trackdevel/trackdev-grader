//! Track B PE: end-to-end `run()` without LLM invocation (no file candidates).

use sprint_grader_core::Database;
use sprint_grader_quality_llm::{run, QualityFlagsOpts};
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
max_holistic = 0
claude_cli_path = "/nonexistent/claude"
"#;

#[test]
fn run_completes_without_llm_when_no_candidates_and_holistic_off() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("course.toml"), MINIMAL).unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# rubric\n").unwrap();
    let db = Database::open(&dir.path().join("g.db")).unwrap();
    db.create_tables().unwrap();
    db.conn
        .execute(
            "INSERT INTO projects (id, slug, name) VALUES (1,'t','Team 01')",
            [],
        )
        .unwrap();

    let course = sprint_grader_core::Config::load(dir.path()).unwrap();
    assert_eq!(course.quality_llm.max_holistic, 0);

    run(
        &db,
        dir.path(),
        &QualityFlagsOpts {
            today: "2026-03-01".into(),
            entregues_dir: dir.path().join("entregues"),
            max_holistic: Some(0),
            ..Default::default()
        },
    )
    .expect("run should skip LLM when prefilter yields no files");

    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM llm_quality_flag", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
