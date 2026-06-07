//! Track B PA: config, rubric, prefilter, persist scaffolding.

use sprint_grader_core::{Config, Database, QualityLlmConfig};
use sprint_grader_quality_llm::{
    delete_project_flags, file_flag_exists, list_all_flags, list_file_candidates,
    list_flagged_project_ids, list_flags_for_projects, load_rubric, persist_project_flags,
    run,
    LlmQualityFlagRow, QualityFlagsOpts,
};
use tempfile::tempdir;

fn minimal_course_toml(with_quality_llm: bool) -> String {
    let mut body = r#"
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
"#
    .to_string();
    if with_quality_llm {
        body.push_str(
            r#"
[quality_llm]
model_id = "claude-haiku-4-5-20251001"
"#,
        );
    }
    body
}

#[test]
fn quality_llm_defaults_when_block_absent() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("course.toml"), minimal_course_toml(false)).unwrap();
    std::fs::write(
        dir.path().join("quality-llm-rubric.md"),
        "# rubric\n",
    )
    .unwrap();
    let cfg = Config::load(dir.path()).unwrap();
    assert_eq!(cfg.quality_llm.backend, "claude-cli");
    assert_eq!(cfg.quality_llm.prompt_version, "1");
    assert_eq!(cfg.quality_llm.max_holistic, 1);
    assert!(cfg.quality_llm.model_id.is_none());
}

#[test]
fn validate_for_run_requires_model_id() {
    let ql = QualityLlmConfig::default();
    assert!(ql.validate_for_run().is_err());
    let mut ql = ql;
    ql.model_id = Some("claude-haiku-4-5-20251001".into());
    ql.validate_for_run().unwrap();
}

#[test]
fn rubric_loads_from_config_dir() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("course.toml"), minimal_course_toml(true)).unwrap();
    std::fs::write(
        dir.path().join("quality-llm-rubric.md"),
        "# Quality rubric\n",
    )
    .unwrap();
    let cfg = Config::load(dir.path()).unwrap();
    let rubric = load_rubric(dir.path(), &cfg.quality_llm).unwrap();
    assert!(rubric.body.contains("Quality rubric"));
}

#[test]
fn prefilter_lists_fingerprinted_java_files() {
    let db_path = tempdir().unwrap().path().join("g.db");
    let db = Database::open(&db_path).unwrap();
    db.create_tables().unwrap();
    db.conn
        .execute(
            "INSERT INTO projects (id, slug, name) VALUES (1, 't1', 'T1')",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO students (id, username, github_login, full_name, team_project_id)
             VALUES ('s1', 's1', 's1', 'S', 1)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO sprints (id, project_id, name, start_date, end_date)
             VALUES (10, 1, 'S1', '2026-01-01', '2026-01-15')",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
             VALUES (100, 'T-1', 'Task', 'TASK', 'DONE', 1, 's1', 10)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO pull_requests (id, pr_number, repo_full_name, url, title, state, merged)
             VALUES ('pr1', 1, 'org/repo', 'http://x', 't', 'MERGED', 1)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (100, 'pr1')",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO fingerprints
             (file_path, repo_full_name, statement_index, raw_fingerprint, normalized_fingerprint)
             VALUES ('src/Foo.java', 'org/repo', 0, 'a', 'b')",
            [],
        )
        .unwrap();

    let ql = QualityLlmConfig::default();
    let files = list_file_candidates(&db.conn, 1, &ql).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_path, "src/Foo.java");
}

#[test]
fn persist_and_list_round_trip() {
    let db_path = tempdir().unwrap().path().join("g.db");
    let db = Database::open(&db_path).unwrap();
    db.create_tables().unwrap();
    let row = LlmQualityFlagRow {
        project_id: 1,
        student_id: None,
        sprint_id: None,
        scope: "file".into(),
        target_ref: Some("org/repo:src/Foo.java".into()),
        category: "readability".into(),
        severity: "INFO".into(),
        summary: "Long method".into(),
        detail: None,
        backend: "claude-cli".into(),
        model_id: "claude-haiku-4-5-20251001".into(),
        prompt_version: "1".into(),
        generated_at: "2026-01-01T00:00:00Z".into(),
    };
    persist_project_flags(&db.conn, 1, std::slice::from_ref(&row)).unwrap();
    assert!(file_flag_exists(
        &db.conn,
        1,
        "org/repo:src/Foo.java",
        "claude-cli",
        "claude-haiku-4-5-20251001",
        "1"
    )
    .unwrap());
    let listed = list_flags_for_projects(&db.conn, &[1]).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].summary, "Long method");
}

#[test]
fn incremental_subset_delete_preserves_other_projects() {
    let db_path = tempdir().unwrap().path().join("g.db");
    let db = Database::open(&db_path).unwrap();
    db.create_tables().unwrap();
    let mk = |pid: i64, summary: &str| LlmQualityFlagRow {
        project_id: pid,
        student_id: None,
        sprint_id: None,
        scope: "file".into(),
        target_ref: Some(format!("t:{pid}")),
        category: "other".into(),
        severity: "INFO".into(),
        summary: summary.into(),
        detail: None,
        backend: "claude-cli".into(),
        model_id: "claude-haiku-4-5-20251001".into(),
        prompt_version: "1".into(),
        generated_at: "2026-01-01T00:00:00Z".into(),
    };
    persist_project_flags(&db.conn, 1, &[mk(1, "team-01")]).unwrap();
    persist_project_flags(&db.conn, 2, &[mk(2, "team-02")]).unwrap();
    delete_project_flags(&db.conn, 2).unwrap();
    assert_eq!(list_flagged_project_ids(&db.conn).unwrap(), vec![1]);
    let all = list_all_flags(&db.conn).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].summary, "team-01");
}

#[test]
fn run_fails_without_model_id_in_config() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("course.toml"), minimal_course_toml(false)).unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# r\n").unwrap();
    let db_path = dir.path().join("g.db");
    let db = Database::open(&db_path).unwrap();
    db.create_tables().unwrap();
    db.conn
        .execute("INSERT INTO projects (id, slug, name) VALUES (1,'t','T')", [])
        .unwrap();
    let err = run(
        &db,
        dir.path(),
        &QualityFlagsOpts {
            today: "2026-03-01".into(),
            entregues_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("model_id"));
}

#[test]
fn file_pass_errors_when_cli_missing_and_file_pending() {
    use sprint_grader_quality_llm::{run_file_pass, FileCandidate};

    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("quality-llm-rubric.md"), "# r\n").unwrap();
    let mut ql = QualityLlmConfig::default();
    ql.model_id = Some("claude-haiku-4-5-20251001".into());
    ql.claude_cli_path = "/nonexistent/claude-quality-flags".into();
    let rubric = load_rubric(dir.path(), &ql).unwrap();

    let entregues = dir.path().join("entregues");
    let java_path = entregues.join("T").join("spring-foo").join("src/Foo.java");
    std::fs::create_dir_all(java_path.parent().unwrap()).unwrap();
    std::fs::write(&java_path, "public class Foo {}\n").unwrap();

    let db = Database::open(&dir.path().join("g.db")).unwrap();
    db.create_tables().unwrap();
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
        &ql,
        &rubric,
        &[cand],
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("claude CLI not found"));
}
