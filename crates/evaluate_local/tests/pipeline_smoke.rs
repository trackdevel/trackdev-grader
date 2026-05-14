//! End-to-end pipeline tests against a mock backend. The mock returns
//! deterministic embeddings keyed off `DefaultHasher(input)` — no
//! cryptographic strength needed; we only care that the same input gives
//! the same embedding across calls within a process.
//!
//! All tests share the `seed_minimal_db` fixture (one project, one sprint,
//! one student, one task, two PRs) so each assertion can focus on the
//! pipeline branch under test.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection};
use sprint_grader_core::config::LocalEvaluateConfig;
use sprint_grader_core::Config;
use sprint_grader_evaluate_local::pipeline::{
    build_pr_embedding_input, embed_for_prs, run_local_hybrid_batch_with_backend, PrInputRow,
};
use sprint_grader_evaluate_local::{LocalLlmBackend, OllamaError, PrRidgeBundle};

// --- Mock backend ----------------------------------------------------------

/// Scripted chat response. Tests enqueue these in order; one pop per
/// `chat_json` call. Exhausting the queue surfaces as a `bail!` so test
/// expectations stay precise.
enum ChatResponse {
    /// Successful HTTP 200 with this body.
    Ok(String),
    /// HTTP 400 whose body contains the literal "format" — the canary
    /// for ollama versions that don't support schema-constrained sampling
    /// (`OllamaError::is_unsupported_format_400`).
    Status400Format,
}

struct MockOllamaBackend {
    embedding_dim: usize,
    embed_call_count: std::sync::atomic::AtomicUsize,
    chat_call_count: std::sync::atomic::AtomicUsize,
    chat_queue: Mutex<VecDeque<ChatResponse>>,
    last_chat_user: Mutex<Option<String>>,
    last_chat_schema_present: Mutex<Option<bool>>,
}

impl MockOllamaBackend {
    fn new(embedding_dim: usize) -> Self {
        Self {
            embedding_dim,
            embed_call_count: std::sync::atomic::AtomicUsize::new(0),
            chat_call_count: std::sync::atomic::AtomicUsize::new(0),
            chat_queue: Mutex::new(VecDeque::new()),
            last_chat_user: Mutex::new(None),
            last_chat_schema_present: Mutex::new(None),
        }
    }

    fn calls(&self) -> usize {
        self.embed_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn chat_calls(&self) -> usize {
        self.chat_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn enqueue_chat(&self, response: ChatResponse) {
        self.chat_queue.lock().unwrap().push_back(response);
    }

    fn last_user(&self) -> Option<String> {
        self.last_chat_user.lock().unwrap().clone()
    }

    fn last_schema_present(&self) -> Option<bool> {
        *self.last_chat_schema_present.lock().unwrap()
    }

    fn deterministic_vec(&self, input: &str) -> Vec<f32> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        input.hash(&mut hasher);
        let mut state = hasher.finish();
        (0..self.embedding_dim)
            .map(|_| {
                // Stateless LCG (Numerical Recipes constants) folded into
                // f32 ∈ [-1, 1]; deterministic per (input, dim, index).
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let signed = (state >> 33) as i64 - (1 << 30);
                (signed as f32) / ((1u64 << 30) as f32)
            })
            .collect()
    }
}

impl LocalLlmBackend for MockOllamaBackend {
    fn is_available(&self) -> bool {
        true
    }

    fn embed(&self, inputs: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(inputs.iter().map(|s| self.deterministic_vec(s)).collect())
    }

    fn chat_json(
        &self,
        _system: &str,
        user: &str,
        schema: Option<&serde_json::Value>,
    ) -> anyhow::Result<String> {
        self.chat_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.last_chat_user.lock().unwrap() = Some(user.to_string());
        *self.last_chat_schema_present.lock().unwrap() = Some(schema.is_some());
        match self.chat_queue.lock().unwrap().pop_front() {
            Some(ChatResponse::Ok(body)) => Ok(body),
            Some(ChatResponse::Status400Format) => {
                Err(OllamaError::Status(400, "format is not supported".to_string()).into())
            }
            None => anyhow::bail!("MockOllamaBackend chat queue exhausted"),
        }
    }
}

// --- Fixture helpers ------------------------------------------------------

const SPRINT_ID: i64 = 10;
const PROJECT_ID: i64 = 1;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/regressor")
}

fn seed_minimal_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    sprint_grader_core::db::apply_schema(&conn).unwrap();
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, ?, ?)",
        params![PROJECT_ID, "team-test", "Test Team"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES (?, ?, ?, ?, ?)",
        params![SPRINT_ID, PROJECT_ID, "S1", "2026-01-01", "2026-01-14"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, email, team_project_id)
         VALUES (?, ?, ?, ?, ?, ?)",
        params![
            "alice",
            "alice",
            "alice",
            "Alice",
            "alice@example.com",
            PROJECT_ID
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, sprint_id, assignee_id)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            1i64,
            "T-1",
            "Login flow",
            "TASK",
            "DONE",
            SPRINT_ID,
            "alice"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO student_sprint_metrics (student_id, sprint_id) VALUES (?, ?)",
        params!["alice", SPRINT_ID],
    )
    .unwrap();
    conn
}

fn seed_pr(conn: &Connection, pr_id: &str, title: &str, body: &str) {
    conn.execute(
        "INSERT INTO pull_requests (id, pr_number, repo_full_name, title, body, author_id)
         VALUES (?, ?, ?, ?, ?, ?)",
        params![pr_id, 1i64, "org/repo", title, body, "alice"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (?, ?)",
        params![1i64, pr_id],
    )
    .unwrap();
}

fn count_pr_doc_rows(conn: &Connection, pr_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
        params![pr_id, SPRINT_ID],
        |r| r.get(0),
    )
    .unwrap()
}

fn read_pr_row(conn: &Connection, pr_id: &str) -> (f64, f64, f64, String) {
    conn.query_row(
        "SELECT title_score, description_score, total_doc_score, justification
         FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
        params![pr_id, SPRINT_ID],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

fn config_for_regressor_dir(dir: impl Into<PathBuf>) -> Config {
    let mut cfg = Config::test_default();
    // Push the regressor.total intercept out of the default
    // NeedsLlm band so the zero-coefficient prediction snaps cleanly.
    // (Band [4.0, 5.0] keeps total=3.5 outside, which then falls under
    // rule 7 → Snap.)
    cfg.evaluate.local = LocalEvaluateConfig {
        regressor_dir: dir.into(),
        pr_total_band_low: 4.0,
        pr_total_band_high: 5.0,
        ..LocalEvaluateConfig::default()
    };
    cfg
}

/// Like [`config_for_regressor_dir`] but keeps the regressor's total
/// (=intercept 3.5 with zero coefficients) INSIDE the NeedsLlm band so
/// the pipeline invokes `llm_score_borderline`. Used by every P3 chat
/// test.
fn config_with_llm_band(dir: impl Into<PathBuf>) -> Config {
    let mut cfg = Config::test_default();
    cfg.evaluate.local = LocalEvaluateConfig {
        regressor_dir: dir.into(),
        pr_total_band_low: 3.0,
        pr_total_band_high: 4.0,
        ..LocalEvaluateConfig::default()
    };
    cfg
}

fn load_bundle(dir: &Path) -> Option<PrRidgeBundle> {
    PrRidgeBundle::load_optional(dir).unwrap()
}

// --- Tests ------------------------------------------------------------------

#[test]
fn embed_input_matches_trainer_shape() {
    // Byte-identical to `tools/train_regressor/train.py::build_inputs`.
    // If either side moves, the regressor weights become out of sync with
    // production embeddings and predictions silently drift.
    let row = PrInputRow {
        pr_id: "pr-1".into(),
        sprint_id: SPRINT_ID,
        title: Some("Implement login controller".into()),
        body: Some("Adds the login controller behind the auth service.".into()),
        task_name: Some("Login flow".into()),
        parent_story: Some("User authentication".into()),
    };
    let got = build_pr_embedding_input(&row);
    let want = "Task: Login flow\nUser Story: User authentication\nTitle: Implement login controller\nDescription:\nAdds the login controller behind the auth service.";
    assert_eq!(got, want);
}

#[test]
fn embed_input_handles_missing_fields_with_defaults() {
    let row = PrInputRow {
        pr_id: "pr-1".into(),
        sprint_id: SPRINT_ID,
        title: None,
        body: None,
        task_name: None,
        parent_story: None,
    };
    let got = build_pr_embedding_input(&row);
    let want = "Task: \nUser Story: N/A\nTitle: \nDescription:\n(empty)";
    assert_eq!(got, want);
}

#[test]
fn embed_for_prs_batches_in_chunks_of_32() {
    let mock = MockOllamaBackend::new(8);
    let strings: Vec<String> = (0..70).map(|i| format!("input-{i}")).collect();
    let refs: Vec<&str> = strings.iter().map(|s| s.as_str()).collect();
    let vectors = embed_for_prs(&mock, &refs).unwrap();
    assert_eq!(vectors.len(), 70);
    // 70 inputs / 32 batch size = 3 HTTP calls (32, 32, 6).
    assert_eq!(mock.calls(), 3);
}

#[test]
fn pipeline_with_zero_weight_regressor_writes_intercept() {
    let conn = seed_minimal_db();
    seed_pr(
        &conn,
        "pr-good",
        "Implement login controller with JWT-based auth",
        "Adds the login controller and wires it to the existing auth service. \
         Linked to task PDS-42; verify by running the auth test suite.",
    );

    let cfg = config_for_regressor_dir(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    assert!(bundle.is_some(), "fixtures must load cleanly");
    let mock = MockOllamaBackend::new(1024);

    let stats =
        run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref())
            .unwrap();

    assert_eq!(stats.items_total, 1);
    assert_eq!(stats.regressor_only, 1);
    assert_eq!(stats.short_circuited, 0);
    assert_eq!(stats.llm_used, 0);
    assert_eq!(stats.failures, 0);

    let (title, desc, total, just) = read_pr_row(&conn, "pr-good");
    // Intercepts (1.5, 2.0, 3.5) snap to themselves on the 0.25 grid.
    assert!((title - 1.5).abs() < 1e-9);
    assert!((desc - 2.0).abs() < 1e-9);
    assert!((total - 3.5).abs() < 1e-9);
    assert_eq!(just, "local: regressor");
}

#[test]
fn pipeline_skips_already_scored_via_resume_guard() {
    let conn = seed_minimal_db();
    seed_pr(
        &conn,
        "pr-scored",
        "Implement login controller with JWT-based auth",
        "Adds the login controller and wires it to the existing auth service.",
    );
    // Seed an existing row with a non-local justification so we can detect
    // accidental overwrites (the pipeline must never touch this row).
    conn.execute(
        "INSERT INTO pr_doc_evaluation
         (pr_id, sprint_id, title_score, description_score, total_doc_score, justification)
         VALUES (?, ?, ?, ?, ?, ?)",
        params!["pr-scored", SPRINT_ID, 1.0, 1.0, 2.0, "manual override"],
    )
    .unwrap();

    let cfg = config_for_regressor_dir(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    let mock = MockOllamaBackend::new(1024);

    let stats =
        run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref())
            .unwrap();

    // The PR was already scored, so it shows up in items_already_scored.
    // The select pre-filters via NOT IN, so items_total stays at 0.
    assert_eq!(stats.items_total, 0);
    assert_eq!(stats.items_already_scored, 0);
    assert_eq!(stats.regressor_only, 0);
    // No embedding calls were issued because the resume guard cut the
    // PR before the embed step.
    assert_eq!(mock.calls(), 0);

    // Existing row preserved as-is.
    let (title, desc, total, just) = read_pr_row(&conn, "pr-scored");
    assert!((title - 1.0).abs() < 1e-9);
    assert!((desc - 1.0).abs() < 1e-9);
    assert!((total - 2.0).abs() < 1e-9);
    assert_eq!(just, "manual override");
    assert_eq!(count_pr_doc_rows(&conn, "pr-scored"), 1);
}

#[test]
fn pipeline_with_missing_regressor_dir_marks_rows_regressor_disabled() {
    let conn = seed_minimal_db();
    seed_pr(
        &conn,
        "pr-needs-llm",
        "Implement login controller with JWT-based auth",
        "Adds the login controller and wires it to the existing auth service. \
         Linked to task PDS-42; verify by running the auth test suite.",
    );

    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let cfg = config_for_regressor_dir(&missing);
    assert!(
        PrRidgeBundle::load_optional(&missing).unwrap().is_none(),
        "regressor must be absent"
    );
    let mock = MockOllamaBackend::new(1024);

    let stats =
        run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, None).unwrap();

    assert_eq!(stats.items_total, 1);
    assert_eq!(stats.llm_used, 1);
    assert_eq!(stats.regressor_only, 0);
    // No embedding was attempted because bundle was None.
    assert_eq!(mock.calls(), 0);

    let (title, desc, total, just) = read_pr_row(&conn, "pr-needs-llm");
    assert_eq!(title, 0.0);
    assert_eq!(desc, 0.0);
    assert_eq!(total, 0.0);
    assert_eq!(just, "local: regressor-disabled");
}

#[test]
fn pipeline_with_wrong_embedding_dim_routes_to_needs_llm_without_panic() {
    let conn = seed_minimal_db();
    seed_pr(
        &conn,
        "pr-dim",
        "Implement login controller with JWT-based auth",
        "Adds the login controller and wires it to the existing auth service.",
    );

    let cfg = config_for_regressor_dir(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    assert_eq!(bundle.as_ref().unwrap().title.embedding_dim, 1024);
    // Mock returns 512-dim embeddings → RidgeHead::predict returns NaN
    // → triage routes to NeedsLlm with regressor=ZERO → write_decision
    // detects NaN via RegressorContext::DimMismatch.
    let mock = MockOllamaBackend::new(512);

    let stats =
        run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref())
            .unwrap();

    assert_eq!(stats.items_total, 1);
    assert_eq!(stats.llm_used, 1);
    assert_eq!(stats.regressor_only, 0);

    let (title, desc, total, just) = read_pr_row(&conn, "pr-dim");
    assert_eq!(title, 0.0);
    assert_eq!(desc, 0.0);
    assert_eq!(total, 0.0);
    assert_eq!(just, "local: dim-mismatch");
}

#[test]
fn pipeline_short_circuits_empty_body_with_regressor_present() {
    let conn = seed_minimal_db();
    // Body shorter than 20 chars → EmptyBody short-circuit.
    seed_pr(&conn, "pr-empty", "Implement login controller", "TODO");

    let cfg = config_for_regressor_dir(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    let mock = MockOllamaBackend::new(1024);

    let stats =
        run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref())
            .unwrap();

    assert_eq!(stats.short_circuited, 1);
    assert_eq!(stats.regressor_only, 0);

    let (title, desc, _total, just) = read_pr_row(&conn, "pr-empty");
    // EmptyBody: title gets snap_title(regressor.title) = 1.5, desc = 0.
    assert!((title - 1.5).abs() < 1e-9);
    assert_eq!(desc, 0.0);
    assert_eq!(just, "local: empty body");
}

// --- P3 LLM-fallback tests --------------------------------------------------

fn seed_borderline_pr(conn: &Connection, pr_id: &str) {
    seed_pr(
        conn,
        pr_id,
        "Implement login controller with JWT-based auth",
        "Adds the login controller and wires it to the existing auth service. \
         Linked to task PDS-42; verify by running the auth test suite.",
    );
}

#[test]
fn pipeline_borderline_with_schema_valid_llm_response_writes_regressor_plus_llm() {
    let conn = seed_minimal_db();
    seed_borderline_pr(&conn, "pr-llm");

    let cfg = config_with_llm_band(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    let mock = MockOllamaBackend::new(1024);
    // Schema-valid first attempt. Values picked to land on the grid.
    mock.enqueue_chat(ChatResponse::Ok(
        r#"{"title_score": 1.75, "description_score": 3.0, "total_doc_score": 4.75, "justification": "good"}"#
            .to_string(),
    ));

    let stats =
        run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref())
            .unwrap();
    assert_eq!(stats.llm_used, 1);
    assert_eq!(mock.chat_calls(), 1, "only one chat call needed");
    assert_eq!(mock.last_schema_present(), Some(true));

    let (title, desc, total, just) = read_pr_row(&conn, "pr-llm");
    assert!((title - 1.75).abs() < 1e-9);
    assert!((desc - 3.0).abs() < 1e-9);
    assert!((total - 4.75).abs() < 1e-9);
    assert_eq!(just, "local: regressor+llm");
}

#[test]
fn pipeline_borderline_with_prefixed_retry_succeeds_writes_regressor_plus_llm() {
    let conn = seed_minimal_db();
    seed_borderline_pr(&conn, "pr-retry");

    let cfg = config_with_llm_band(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    let mock = MockOllamaBackend::new(1024);
    // First attempt: garbage prose — extract_json_object returns None.
    mock.enqueue_chat(ChatResponse::Ok(
        "I'm sorry but I'm not sure what to say".to_string(),
    ));
    // Second attempt (after prefix reminder): clean JSON.
    mock.enqueue_chat(ChatResponse::Ok(
        r#"{"title_score": 1.25, "description_score": 2.5}"#.to_string(),
    ));

    run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref()).unwrap();
    assert_eq!(mock.chat_calls(), 2, "parse retry should have fired once");
    let prefixed = mock.last_user().unwrap();
    assert!(
        prefixed.starts_with("Reply ONLY with a JSON object matching the schema."),
        "second-call user message must carry the schema-reminder prefix; got: {prefixed:?}"
    );

    let (title, desc, total, just) = read_pr_row(&conn, "pr-retry");
    assert!((title - 1.25).abs() < 1e-9);
    assert!((desc - 2.5).abs() < 1e-9);
    assert!((total - 3.75).abs() < 1e-9);
    assert_eq!(just, "local: regressor+llm");
}

#[test]
fn pipeline_borderline_with_double_parse_failure_falls_back_to_regressor_mean() {
    let conn = seed_minimal_db();
    seed_borderline_pr(&conn, "pr-fail");

    let cfg = config_with_llm_band(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    let mock = MockOllamaBackend::new(1024);
    // Both attempts return garbage; persist falls back to snapped
    // regressor mean (snap(1.5)+snap(2.0)=3.5).
    mock.enqueue_chat(ChatResponse::Ok("garbage one".to_string()));
    mock.enqueue_chat(ChatResponse::Ok("garbage two".to_string()));

    run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref()).unwrap();
    assert_eq!(mock.chat_calls(), 2);

    let (title, desc, total, just) = read_pr_row(&conn, "pr-fail");
    assert!((title - 1.5).abs() < 1e-9);
    assert!((desc - 2.0).abs() < 1e-9);
    assert!((total - 3.5).abs() < 1e-9);
    assert_eq!(just, "local: llm-fallback-failed");
}

#[test]
fn pipeline_borderline_with_400_format_retries_without_schema_and_succeeds() {
    let conn = seed_minimal_db();
    seed_borderline_pr(&conn, "pr-no-format");

    let cfg = config_with_llm_band(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    let mock = MockOllamaBackend::new(1024);
    // First attempt: 400 with "format" in body → triggers retry-without-format.
    mock.enqueue_chat(ChatResponse::Status400Format);
    // Retry-without-format: schema=None, response parses cleanly.
    mock.enqueue_chat(ChatResponse::Ok(
        r#"{"title_score": 2.0, "description_score": 4.0}"#.to_string(),
    ));

    run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref()).unwrap();
    assert_eq!(
        mock.chat_calls(),
        2,
        "expected 2 chat calls: schema-constrained + retry-without-format"
    );
    // The last chat call (the retry) must have had schema=None.
    assert_eq!(
        mock.last_schema_present(),
        Some(false),
        "retry-without-format must call chat_json with None schema"
    );
    // Retry user message carries the inline sketch reminder.
    let retry_user = mock.last_user().unwrap();
    assert!(
        retry_user.starts_with("Reply ONLY with a JSON object matching:"),
        "retry must inline the schema sketch; got: {retry_user:?}"
    );

    let (title, desc, _total, just) = read_pr_row(&conn, "pr-no-format");
    assert!((title - 2.0).abs() < 1e-9);
    assert!((desc - 4.0).abs() < 1e-9);
    assert_eq!(just, "local: regressor+llm");
}

#[test]
fn pipeline_borderline_with_400_format_retry_failure_marks_format_unsupported() {
    let conn = seed_minimal_db();
    seed_borderline_pr(&conn, "pr-fmt-bad");

    let cfg = config_with_llm_band(fixtures_dir());
    let bundle = load_bundle(&fixtures_dir());
    let mock = MockOllamaBackend::new(1024);
    // First attempt: 400+format → triggers retry-without-format.
    mock.enqueue_chat(ChatResponse::Status400Format);
    // Retry response is unparseable.
    mock.enqueue_chat(ChatResponse::Ok("still no json here".to_string()));

    run_local_hybrid_batch_with_backend(&conn, &[SPRINT_ID], &cfg, &mock, bundle.as_ref()).unwrap();
    assert_eq!(mock.chat_calls(), 2);

    let (title, desc, total, just) = read_pr_row(&conn, "pr-fmt-bad");
    // Falls back to snapped regressor mean = (1.5, 2.0, 3.5).
    assert!((title - 1.5).abs() < 1e-9);
    assert!((desc - 2.0).abs() < 1e-9);
    assert!((total - 3.5).abs() < 1e-9);
    assert_eq!(just, "local: llm-format-unsupported");
}
