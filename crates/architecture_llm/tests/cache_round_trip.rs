//! End-to-end test for the LLM architecture review stage using a
//! deterministic stub judge — no network required.
//!
//! Wave 4 deprecated `run_llm_review_for_repo`; this test still
//! exercises the cache + persistence path to protect the
//! emergency-rollback flag (`[architecture] llm_review = true`). The
//! file-level `#[allow(deprecated)]` keeps the build warning-clean.

#![allow(deprecated)]

use rusqlite::Connection;
use sprint_grader_architecture::Rubric;
use sprint_grader_architecture_llm::judge::{Judge, JudgeError, LlmResponse, LlmViolation};
use sprint_grader_architecture_llm::run_llm_review_for_repo;
use sprint_grader_core::db::apply_schema;
use std::fs;
use tempfile::TempDir;

/// Tiny stub: every call returns one fixed violation. Used in place of
/// the real `LlmJudge` so the cache + persistence path is exercisable
/// without an Anthropic key. `calls` is atomic because the production
/// signature now requires `&(dyn Judge + Send + Sync)` — the orchestrator
/// fans out per-file judge calls across a Rayon pool sized by
/// `architecture.judge_workers`.
struct OneShotJudge {
    model: String,
    calls: std::sync::atomic::AtomicU32,
}

impl Judge for OneShotJudge {
    fn model_id(&self) -> &str {
        &self.model
    }
    fn judge(
        &self,
        _file_path: &str,
        _rubric: &str,
        _bytes: &[u8],
    ) -> Result<LlmResponse, JudgeError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(LlmResponse {
            violations: vec![LlmViolation {
                rule_id: "FAT_METHOD".into(),
                severity: "WARNING".into(),
                start_line: 1,
                end_line: 2,
                explanation: "stubbed".into(),
            }],
        })
    }
}

fn fixture_rubric() -> Rubric {
    sprint_grader_architecture::rubric::parse(
        "---\nversion: 1\n---\n\
         # Spring Boot rubric\n\
         - thin controllers\n",
    )
    .unwrap()
}

fn write_java(dir: &std::path::Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

#[test]
fn llm_review_writes_violations_then_caches() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/Foo.java",
        "package com.x;\nclass Foo {}\n",
    );

    let rubric = fixture_rubric();
    let judge = OneShotJudge {
        model: "stub-model".into(),
        calls: std::sync::atomic::AtomicU32::new(0),
    };

    let n = run_llm_review_for_repo(&conn, tmp.path(), "udg/spring-x", &rubric, &judge, &[], 1)
        .unwrap();
    assert_eq!(n, 1, "one violation written");
    assert_eq!(
        judge.calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the file was sent to the judge once"
    );

    // Second pass: same file_sha + rubric + model → cache hit, no new call.
    let n2 = run_llm_review_for_repo(&conn, tmp.path(), "udg/spring-x", &rubric, &judge, &[], 1)
        .unwrap();
    assert_eq!(n2, 1);
    assert_eq!(
        judge.calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "second run was a cache hit"
    );

    // The LLM violation row carries rule_kind='llm' and the explanation column.
    let (rule_kind, explanation): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT rule_kind, explanation FROM architecture_violations
             WHERE rule_name = 'FAT_METHOD'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rule_kind.as_deref(), Some("llm"));
    assert_eq!(explanation.as_deref(), Some("stubbed"));
}

#[test]
fn rerun_idempotent_does_not_duplicate() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    let tmp = TempDir::new().unwrap();
    write_java(tmp.path(), "Foo.java", "class Foo {}\n");
    let rubric = fixture_rubric();
    let judge = OneShotJudge {
        model: "stub-model".into(),
        calls: std::sync::atomic::AtomicU32::new(0),
    };

    run_llm_review_for_repo(&conn, tmp.path(), "x", &rubric, &judge, &[], 1).unwrap();
    run_llm_review_for_repo(&conn, tmp.path(), "x", &rubric, &judge, &[], 1).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM architecture_violations WHERE rule_kind = 'llm'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "DELETE-then-INSERT keeps the row count stable");
}

#[test]
fn skip_globs_filter_files_before_llm_call() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    let tmp = TempDir::new().unwrap();
    write_java(tmp.path(), "build/generated/Foo.java", "class Foo {}\n");
    write_java(tmp.path(), "src/main/java/Bar.java", "class Bar {}\n");
    let rubric = fixture_rubric();
    let judge = OneShotJudge {
        model: "stub-model".into(),
        calls: std::sync::atomic::AtomicU32::new(0),
    };
    run_llm_review_for_repo(
        &conn,
        tmp.path(),
        "x",
        &rubric,
        &judge,
        &["**/build/**".to_string()],
        1,
    )
    .unwrap();
    assert_eq!(
        judge.calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "only the non-skipped file goes to the judge"
    );
}

#[test]
fn out_of_range_line_numbers_are_dropped() {
    struct LiarJudge {
        model: String,
    }
    impl Judge for LiarJudge {
        fn model_id(&self) -> &str {
            &self.model
        }
        fn judge(&self, _: &str, _: &str, _: &[u8]) -> Result<LlmResponse, JudgeError> {
            Ok(LlmResponse {
                violations: vec![
                    LlmViolation {
                        rule_id: "OK".into(),
                        severity: "INFO".into(),
                        start_line: 1,
                        end_line: 1,
                        explanation: "ok".into(),
                    },
                    LlmViolation {
                        rule_id: "BAD".into(),
                        severity: "WARNING".into(),
                        start_line: 999,
                        end_line: 1000,
                        explanation: "fabricated".into(),
                    },
                ],
            })
        }
    }
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    let tmp = TempDir::new().unwrap();
    write_java(tmp.path(), "Foo.java", "class Foo {}\n");
    let rubric = fixture_rubric();
    let judge = LiarJudge {
        model: "stub".into(),
    };
    let n = run_llm_review_for_repo(&conn, tmp.path(), "x", &rubric, &judge, &[], 1).unwrap();
    assert_eq!(n, 1, "OK row inserted, BAD row dropped");
    let kept_rule: String = conn
        .query_row(
            "SELECT rule_name FROM architecture_violations WHERE rule_kind = 'llm'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept_rule, "OK");
}
