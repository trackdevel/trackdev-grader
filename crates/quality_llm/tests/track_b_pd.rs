//! Track B PD: cursor-cli and ollama backend wiring.

use sprint_grader_core::{Config, Database, QualityLlmConfig};
use sprint_grader_quality_llm::{load_rubric, run_file_pass, FileCandidate};
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
"#;

fn write_java(entregues: &std::path::Path, project: &str) {
    let java_path = entregues
        .join(project)
        .join("spring-foo")
        .join("src/Foo.java");
    std::fs::create_dir_all(java_path.parent().unwrap()).unwrap();
    std::fs::write(&java_path, "public class Foo {}\n").unwrap();
}

fn setup_db(dir: &tempfile::TempDir) -> Database {
    let db = Database::open(&dir.path().join("g.db")).unwrap();
    db.create_tables().unwrap();
    db.conn
        .execute(
            "INSERT INTO projects (id, slug, name) VALUES (1,'t','T')",
            [],
        )
        .unwrap();
    db
}

#[test]
fn validate_rejects_anthropic_api_backend() {
    let ql = QualityLlmConfig {
        model_id: Some("claude-haiku-4-5-20251001".into()),
        backend: "anthropic-api".into(),
        ..Default::default()
    };
    let err = ql.validate_for_run().unwrap_err().to_string();
    assert!(err.contains("anthropic-api"));
}

#[test]
fn validate_rejects_unknown_backend() {
    let ql = QualityLlmConfig {
        model_id: Some("m".into()),
        backend: "openai-api".into(),
        ..Default::default()
    };
    let err = ql.validate_for_run().unwrap_err().to_string();
    assert!(err.contains("unknown backend"));
}

#[test]
fn cursor_backend_errors_when_cli_missing() {
    let dir = tempdir().unwrap();
    let mut toml = MINIMAL.to_string();
    toml.push_str(
        r#"
[quality_llm]
backend = "cursor-cli"
model_id = "composer-2.5"
cursor_cli_path = "/nonexistent/cursor-agent-quality-flags"
"#,
    );
    std::fs::write(dir.path().join("course.toml"), toml).unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# r\n").unwrap();
    let course = Config::load(dir.path()).unwrap();
    course.quality_llm.validate_for_run().unwrap();
    let rubric = load_rubric(dir.path(), &course.quality_llm).unwrap();
    let entregues = dir.path().join("entregues");
    write_java(&entregues, "T");
    let db = setup_db(&dir);
    let cand = FileCandidate {
        repo_full_name: "org/spring-foo".into(),
        file_path: "src/Foo.java".into(),
        statement_count: 1,
    };
    let err = run_file_pass(
        &db.conn,
        1,
        "T",
        &entregues,
        &course.quality_llm,
        &rubric,
        &[cand],
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("cursor agent CLI not found"));
}

#[test]
fn ollama_backend_errors_when_daemon_unreachable() {
    let dir = tempdir().unwrap();
    let mut toml = MINIMAL.to_string();
    toml.push_str(
        r#"
[quality_llm]
backend = "ollama"
model_id = "salamandra"
ollama_url = "http://127.0.0.1:1"
"#,
    );
    std::fs::write(dir.path().join("course.toml"), toml).unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# r\n").unwrap();
    let course = Config::load(dir.path()).unwrap();
    course.quality_llm.validate_for_run().unwrap();
    let rubric = load_rubric(dir.path(), &course.quality_llm).unwrap();
    let entregues = dir.path().join("entregues");
    write_java(&entregues, "T");
    let db = setup_db(&dir);
    let cand = FileCandidate {
        repo_full_name: "org/spring-foo".into(),
        file_path: "src/Foo.java".into(),
        statement_count: 1,
    };
    let err = run_file_pass(
        &db.conn,
        1,
        "T",
        &entregues,
        &course.quality_llm,
        &rubric,
        &[cand],
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("ollama not reachable"));
}

#[test]
fn ollama_backend_loads_from_course_toml() {
    let dir = tempdir().unwrap();
    let mut toml = MINIMAL.to_string();
    toml.push_str(
        r#"
[quality_llm]
backend = "ollama"
model_id = "salamandra"
ollama_url = "http://127.0.0.1:11434"
"#,
    );
    std::fs::write(dir.path().join("course.toml"), toml).unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# r\n").unwrap();
    let course = Config::load(dir.path()).unwrap();
    assert_eq!(course.quality_llm.backend, "ollama");
    course.quality_llm.validate_for_run().unwrap();
}
