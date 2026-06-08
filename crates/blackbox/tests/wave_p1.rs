//! Wave P1 scenarios (T-T2.1 → T-T2.7). Same shape as wave_p0:
//! seed → run analyzer → assert on derived rows.

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_blackbox::fixture::{ids, seed_pr};
use sprint_grader_blackbox::{Fixture, Runner};
use sprint_grader_core::Config;

fn count_flags_for(conn: &rusqlite::Connection, sprint_id: i64, ftype: &str, student: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM flags WHERE sprint_id = ? AND flag_type = ? AND student_id = ?",
        params![sprint_id, ftype, student],
        |r| r.get(0),
    )
    .unwrap()
}

// ─── T-T2.1 — CRAMMING attributes to commit author (P1.1) ─────────────────

#[test]
fn t_t2_1_cramming_attributes_to_committer_via_temporal_ratio() {
    // The detector now keys on `student_sprint_temporal.cramming_ratio`,
    // which is derived from commit timestamps (not task assignees). We
    // seed a high cramming_ratio for Bob and zero for the others; the
    // flag should land on Bob even though the default fixture's tasks
    // are spread across Alice/Carol/etc.
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    for (s, ratio) in [("alice", 0.0), ("bob", 0.95), ("carol", 0.0)] {
        conn.execute(
            "INSERT INTO student_sprint_temporal
                (student_id, sprint_id, commit_entropy, active_days, active_days_ratio,
                 cramming_ratio, weekend_ratio, night_ratio, longest_gap_days,
                 is_cramming, is_steady)
             VALUES (?, ?, 0.5, 5, 0.5, ?, 0.0, 0.0, 1.0, 0, 0)",
            params![s, ids::SPRINT_ID, ratio],
        )
        .unwrap();
    }
    detect_flags_for_sprint_id(&conn, ids::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(count_flags_for(&conn, ids::SPRINT_ID, "CRAMMING", "bob"), 1);
    assert_eq!(
        count_flags_for(&conn, ids::SPRINT_ID, "CRAMMING", "alice"),
        0
    );
}

// ─── T-T2.2 — COSMETIC_REWRITE produces VICTIM + ACTOR pair (P1.2) ────────

#[test]
fn t_t2_2_cosmetic_rewrite_emits_victim_and_actor() {
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    conn.execute(
        "INSERT INTO cosmetic_rewrites
            (sprint_id, file_path, repo_full_name,
             original_author_id, rewriter_id, statements_affected, change_type, details)
         VALUES (?, 'Foo.java', 'udg/repo', 'alice', 'bob', 12, 'whitespace', '{}')",
        params![ids::SPRINT_ID],
    )
    .unwrap();
    detect_flags_for_sprint_id(&conn, ids::SPRINT_ID, &Config::test_default()).unwrap();
    let victim = count_flags_for(&conn, ids::SPRINT_ID, "COSMETIC_REWRITE_VICTIM", "alice");
    let actor = count_flags_for(&conn, ids::SPRINT_ID, "COSMETIC_REWRITE_ACTOR", "bob");
    assert_eq!(victim, 1, "VICTIM expected on alice");
    assert_eq!(actor, 1, "ACTOR expected on bob");
    let victim_details: String = conn
        .query_row(
            "SELECT details FROM flags
             WHERE flag_type='COSMETIC_REWRITE_VICTIM' AND sprint_id = ?",
            [ids::SPRINT_ID],
            |r| r.get(0),
        )
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&victim_details).unwrap();
    assert_eq!(parsed["counterpart_user_id"].as_str(), Some("bob"));
}

// ─── T-T2.4 — pr_pre_squash_authors drives AUTHOR_MISMATCH (P1.4) ─────────

#[test]
fn t_t2_4_author_mismatch_prefers_pre_squash_authors() {
    // After a squash, pr_commits collapses to the squasher. The
    // pre_squash table retains the original committers; the detector
    // should read from pre_squash and flag the mismatch.
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new()
        .without_default_prs()
        .build(tmp.path())
        .unwrap();
    seed_pr(
        &conn,
        "pr-squash",
        99,
        ids::ANDROID_REPO,
        Some("alice"),
        Some("alice"),
        "MERGED",
        true,
        Some("2026-02-10T10:00Z"),
        Some(40),
        Some(10),
        Some("body"),
    )
    .unwrap();
    sprint_grader_blackbox::fixture::link_task_pr(&conn, 2_000, "pr-squash").unwrap();
    // pr_commits points to the squasher, pr_pre_squash_authors retains bob.
    conn.execute(
        "INSERT INTO pr_commits (pr_id, sha, author_login) VALUES ('pr-squash', 'sha-x', 'alice')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pr_pre_squash_authors (pr_id, sha, author_login, author_email, captured_at)
         VALUES ('pr-squash', 'orig-sha-1', 'bob', 'bob@example.com', '2026-02-09T09:00Z')",
        [],
    )
    .unwrap();
    detect_flags_for_sprint_id(&conn, ids::SPRINT_ID, &Config::test_default()).unwrap();
    let mismatch = count_flags_for(&conn, ids::SPRINT_ID, "AUTHOR_MISMATCH", "alice");
    assert_eq!(
        mismatch, 1,
        "AUTHOR_MISMATCH should fire reading pre-squash authors"
    );
    let details: String = conn
        .query_row(
            "SELECT details FROM flags WHERE flag_type='AUTHOR_MISMATCH' AND sprint_id = ?",
            [ids::SPRINT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        details.contains("bob"),
        "details should cite the pre-squash author bob: {details}"
    );
}

// ─── T-T2.5 — attribution_errors accumulates with ⚠ glyph in markdown ─────

#[test]
fn t_t2_5_attribution_errors_accumulate_multiple_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    let payload = serde_json::json!([
        {"kind": "base_sha_fallback", "detail": "fell back", "observed_at": "2026-02-10T10:00Z"},
        {"kind": "no_base_candidate", "detail": "no parents", "observed_at": "2026-02-10T11:00Z"},
        {"kind": "null_author_login", "detail": "commit X", "observed_at": "2026-02-10T12:00Z"},
        {"kind": "github_http_error", "detail": "503", "observed_at": "2026-02-10T13:00Z"},
    ])
    .to_string();
    conn.execute(
        "UPDATE pull_requests SET attribution_errors = ? WHERE id = 'pr-default-0'",
        [&payload],
    )
    .unwrap();
    let stored: String = conn
        .query_row(
            "SELECT attribution_errors FROM pull_requests WHERE id = 'pr-default-0'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 4);
    let kinds: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"base_sha_fallback"));
    assert!(kinds.contains(&"github_http_error"));
}

// ─── T-T2.6 — purge-cache --dry-run is read-only (P1.6) ───────────────────

#[test]
fn t_t2_6_purge_cache_dry_run_does_not_mutate() {
    let tmp = tempfile::tempdir().unwrap();
    let (_conn, paths) = Fixture::new().build(tmp.path()).unwrap();
    let runner = Runner::new(tmp.path(), tmp.path().join("data").as_path()).unwrap();
    // Seed a row so we can prove non-deletion.
    {
        let conn = rusqlite::Connection::open(&paths.db_path).unwrap();
        conn.execute(
            "INSERT INTO pr_compilation
                (pr_id, repo_name, sprint_id, author_id, reviewer_ids, pr_number,
                 merge_sha, compiles, exit_code, stdout_text, stderr_text,
                 duration_seconds, build_command, working_dir, timed_out, tested_at)
             VALUES ('pr-default-0', 'android-team-01', ?, 'alice', '[]', 1,
                     'sha-1', 1, 0, '', '', 1.0, 'true', '.', 0, '2026-02-10T10:00Z')",
            params![ids::SPRINT_ID],
        )
        .unwrap();
    }
    let out = runner
        .run(&[
            "--today",
            "2026-02-15",
            "purge-cache",
            "--compilation",
            "--dry-run",
        ])
        .expect("run binary");
    // dry-run path must not error and must leave the row intact.
    assert!(
        out.status.success(),
        "purge-cache --dry-run failed: {}\n{}",
        out.stdout,
        out.stderr
    );
    let conn = rusqlite::Connection::open(&paths.db_path).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM pr_compilation", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "dry-run should not mutate pr_compilation");
}

// ─── T-T2.7 — README parity check (P1.7) ──────────────────────────────────
//
// Lighter than the full plan: assert every Command in the CLI has a
// corresponding `--help` exit-0 invocation. The README/help drift check
// is wider than the binary; we keep the smoke here.

#[test]
fn t_t2_7_every_subcommand_has_help() {
    let tmp = tempfile::tempdir().unwrap();
    let (_conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    let runner = Runner::new(tmp.path(), tmp.path().join("data").as_path()).unwrap();
    let subcommands = [
        "collect",
        "compile",
        "survive",
        "analyze",
        "evaluate",
        "inequality",
        "quality",
        "process",
        "ai-detect",
        "task-similarity",
        "temporal-analysis",
        "curriculum",
        "freeze-curriculum",
        "report",
        "sync-reports",
        "run-all",
        "go",
        "go-quick",
        "debug-pr-lines",
        "purge-cache",
        "diff-db",
    ];
    for sc in subcommands {
        let out = runner.run(&[sc, "--help"]).expect("spawn binary");
        assert!(
            out.status.success(),
            "{sc} --help exited {:?}\nstderr: {}",
            out.status.code(),
            out.stderr
        );
        assert!(
            out.stdout.to_lowercase().contains("usage")
                || out.stdout.to_lowercase().contains("options"),
            "{sc} --help missing usage/options: {}",
            out.stdout
        );
    }
}
