//! Wave P2 scenarios (T-T3.2 → T-T3.7). Architectural additions —
//! architecture conformance, ownership treemap, mutation testing,
//! curriculum versioning, threshold jitter, and the per-detector
//! regression-fixture meta-check.

use std::path::PathBuf;

use rusqlite::params;
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_blackbox::fixture::ids;
use sprint_grader_blackbox::Fixture;
use sprint_grader_core::Config;

fn count_flags(conn: &rusqlite::Connection, sprint_id: i64, ftype: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM flags WHERE sprint_id = ? AND flag_type = ?",
        params![sprint_id, ftype],
        |r| r.get(0),
    )
    .unwrap()
}

// ─── T-T3.2 — Architecture scan, artifact shape (P2.2 / P3.4) ─────────────
//
// T-P3.4 retired ARCHITECTURE_DRIFT — per-sprint trajectory has no meaning
// when the scan grades the code-on-main as a single artifact. The surface
// contract is now: scanning a repo writes one set of sprint-free
// `architecture_violations` rows; running the project-keyed artifact-flag
// dispatcher emits ARCHITECTURE_HOTSPOT entries in `student_artifact_flags`.

const ARCHITECTURE_TOML: &str = r#"
severity = "WARNING"
[[layers]]
name = "domain"
packages = ["**/domain/**"]
may_depend_on = []

[[layers]]
name = "application"
packages = ["**/application/**"]
may_depend_on = ["domain"]
"#;

fn write_java(path: PathBuf, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn t_t3_2_architecture_scan_writes_violations_and_flag_fires() {
    let tmp = tempfile::tempdir().unwrap();
    let (conn, paths) = Fixture::new().build(tmp.path()).unwrap();

    // Seed two Java files in the android repo dir: a domain class
    // illegally importing application code.
    let android_root = paths.project_dir.join("android-team-01");
    write_java(
        android_root.join("src/main/java/com/x/domain/User.java"),
        "package com.x.domain;\nimport com.x.application.UserService;\nclass User {}\n",
    );
    write_java(
        android_root.join("src/main/java/com/x/application/UserService.java"),
        "package com.x.application;\nclass UserService {}\n",
    );
    let rules =
        sprint_grader_architecture::ArchitectureRules::from_toml_str(ARCHITECTURE_TOML).unwrap();
    sprint_grader_architecture::scan_repo_to_db(&conn, &android_root, ids::ANDROID_REPO, &rules)
        .unwrap();

    let nrows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM architecture_violations WHERE repo_full_name = ?",
            [ids::ANDROID_REPO],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        nrows >= 1,
        "expected ≥1 architecture_violations row, got {nrows}"
    );

    // Run the artifact-flag dispatcher. Without git/blame data, no
    // attribution rows exist, so no ARCHITECTURE_HOTSPOT can fire — that's
    // the silent half of the contract. ARCHITECTURE_DRIFT is gone.
    sprint_grader_analyze::detect_artifact_flags_for_project_id(
        &conn,
        ids::PROJECT_ID,
        &Config::test_default(),
    )
    .unwrap();
    let hotspot: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM student_artifact_flags
             WHERE project_id = ? AND flag_type = 'ARCHITECTURE_HOTSPOT'",
            [ids::PROJECT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        hotspot, 0,
        "no blame data → no hotspot fires; rows live in student_artifact_flags now"
    );
}

// T-T3.2b — AST rules + per-student blame attribution (T-P3.1).
//
// Exercises the AST rule path: a Java class annotated `@RestController`
// holding a `*Repository` field must produce one violation row with
// non-NULL `start_line`/`end_line` and a `rule_kind` starting with `ast_`.
// Attribution silently writes 0 rows here (the temp project dir isn't a
// git repo, and the helper warns + skips on empty blame); the
// ARCHITECTURE_HOTSPOT detector therefore stays quiet, which is the
// negative half of the surface contract — the violation is recorded but
// can't fire the per-student flag without blame data.
const ARCHITECTURE_TOML_AST: &str = r#"
severity = "WARNING"
[[ast_rule]]
name = "controller-no-repository-field"
class_match.annotation = "RestController"
kind = "forbidden_field_type"
type_regex = ".*Repository$"
severity = "WARNING"
"#;

#[test]
fn t_t3_2b_ast_rules_record_line_ranges_and_rule_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let (conn, paths) = Fixture::new().build(tmp.path()).unwrap();
    let android_root = paths.project_dir.join("android-team-01");
    write_java(
        android_root.join("src/main/java/com/x/web/UserController.java"),
        "package com.x.web;\n\
         @RestController\n\
         public class UserController {\n\
             private UserRepository userRepository;\n\
         }\n",
    );
    let rules = sprint_grader_architecture::ArchitectureRules::from_toml_str(ARCHITECTURE_TOML_AST)
        .unwrap();
    sprint_grader_architecture::scan_repo_to_db(&conn, &android_root, ids::ANDROID_REPO, &rules)
        .unwrap();

    let (rule_kind, start, end): (Option<String>, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT rule_kind, start_line, end_line FROM architecture_violations
             WHERE repo_full_name = ? AND rule_name = 'controller-no-repository-field'",
            [ids::ANDROID_REPO],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(rule_kind.as_deref(), Some("ast_forbidden_field_type"));
    assert!(
        start.is_some() && end.is_some(),
        "AST rules must carry a line range"
    );
    assert!(start.unwrap() >= 1 && end.unwrap() >= start.unwrap());

    // The fixture's project dir is not a git repo, so attribution skips
    // silently and the per-student artifact hotspot flag stays quiet.
    sprint_grader_analyze::detect_artifact_flags_for_project_id(
        &conn,
        ids::PROJECT_ID,
        &Config::test_default(),
    )
    .unwrap();
    let hotspot: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM student_artifact_flags
             WHERE project_id = ? AND flag_type = 'ARCHITECTURE_HOTSPOT'",
            [ids::PROJECT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        hotspot, 0,
        "no blame data → no hotspot fires; this is the silent half of the contract"
    );
}

#[test]
fn t_t3_2_architecture_scan_skipped_silently_when_no_rules_file() {
    // Surface contract: when the project lays out repos but no
    // architecture rules are loaded (analogue of "config/architecture.toml
    // absent"), the table stays empty.
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    let nrows: i64 = conn
        .query_row("SELECT COUNT(*) FROM architecture_violations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(nrows, 0);
}

// ─── T-T3.3 — Ownership table + truck_factor (P2.3) ───────────────────────

#[test]
fn t_t3_3_team_sprint_ownership_truck_factor() {
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    // Seed fingerprints so 95% coverage is reached by alice + bob.
    // 10 statements to alice, 10 to bob, 1 to carol → top-2 = 20/21.
    let mut fid = 10_000;
    for student in ["alice"].iter().cycle().take(10) {
        seed_fingerprint(&conn, fid, ids::SPRINT_ID, student);
        fid += 1;
    }
    for student in ["bob"].iter().cycle().take(10) {
        seed_fingerprint(&conn, fid, ids::SPRINT_ID, student);
        fid += 1;
    }
    seed_fingerprint(&conn, fid, ids::SPRINT_ID, "carol");

    let written =
        sprint_grader_repo_analysis::ownership::compute_team_ownership(&conn, ids::SPRINT_ID)
            .unwrap();
    assert!(written >= 1, "ownership write count: {written}");
    let (truck_factor, owners): (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT truck_factor, owners_csv FROM team_sprint_ownership WHERE sprint_id = ?",
            [ids::SPRINT_ID],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(truck_factor, Some(2), "expected truck_factor=2");
    let owners = owners.unwrap_or_default();
    assert!(
        owners.contains("alice") && owners.contains("bob"),
        "owners_csv should list alice + bob, got {owners}"
    );
}

fn seed_fingerprint(conn: &rusqlite::Connection, id: i64, sprint_id: i64, author_login: &str) {
    conn.execute(
        "INSERT INTO fingerprints
            (id, sprint_id, repo_full_name, file_path, statement_index,
             method_name, raw_fingerprint, normalized_fingerprint, blame_author_login)
         VALUES (?, ?, ?, ?, ?, 'm', 'r', 'n', ?)",
        params![
            id,
            sprint_id,
            ids::ANDROID_REPO,
            format!("F{id}.java"),
            id,
            author_login,
        ],
    )
    .unwrap();
}

// ─── T-T3.4 — Mutation testing end-to-end with a fake Pitest ──────────────

#[test]
fn t_t3_4_pitest_xml_drives_pr_mutation_and_low_score_flag() {
    // Black-box-shaped: write a fake mutations.xml that matches what
    // Pitest would produce, parse it via the public API, persist into
    // pr_mutation, then run the LOW_MUTATION_SCORE detector. This
    // exercises the same code path the orchestrator runs without
    // requiring the JVM/Gradle.
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    let xml_path = tmp.path().join("mutations.xml");
    std::fs::write(
        &xml_path,
        r#"<?xml version='1.0' encoding='UTF-8'?>
<mutations>
  <mutation detected='true' status='KILLED'></mutation>
  <mutation detected='false' status='SURVIVED'></mutation>
  <mutation detected='false' status='SURVIVED'></mutation>
  <mutation detected='false' status='SURVIVED'></mutation>
  <mutation detected='false' status='SURVIVED'></mutation>
</mutations>"#,
    )
    .unwrap();
    let summary = sprint_grader_compile::parse_pitest_xml(&xml_path).unwrap();
    assert_eq!(summary.mutants_total, 5);
    assert_eq!(summary.mutants_killed, 1);
    let score = summary.score().unwrap();
    assert!((score - 0.20).abs() < 1e-6);

    // Persist alongside the default first PR.
    conn.execute(
        "INSERT INTO pr_mutation
            (pr_id, repo_name, sprint_id, mutants_total, mutants_killed,
             mutation_score, duration_seconds)
         VALUES ('pr-default-0', 'android-team-01', ?, 5, 1, 0.20, 12.0)",
        [ids::SPRINT_ID],
    )
    .unwrap();
    detect_flags_for_sprint_id(&conn, ids::SPRINT_ID, &Config::test_default()).unwrap();
    let n = count_flags(&conn, ids::SPRINT_ID, "LOW_MUTATION_SCORE");
    assert_eq!(n, 1);
    let severity: String = conn
        .query_row(
            "SELECT severity FROM flags WHERE flag_type='LOW_MUTATION_SCORE' AND sprint_id = ?",
            [ids::SPRINT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(severity, "WARNING");
}

#[test]
fn t_t3_4_pitest_null_score_silences_the_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    conn.execute(
        "INSERT INTO pr_mutation
            (pr_id, repo_name, sprint_id, mutants_total, mutants_killed,
             mutation_score, duration_seconds)
         VALUES ('pr-default-0', 'android-team-01', ?, 3, 0, NULL, 9.0)",
        [ids::SPRINT_ID],
    )
    .unwrap();
    detect_flags_for_sprint_id(&conn, ids::SPRINT_ID, &Config::test_default()).unwrap();
    assert_eq!(count_flags(&conn, ids::SPRINT_ID, "LOW_MUTATION_SCORE"), 0);
}

// ─── T-T3.5 — Curriculum freeze idempotent + snapshot wins ────────────────

#[test]
fn t_t3_5_freeze_curriculum_is_idempotent_and_snapshot_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    // Seed a small live curriculum.
    for (cat, val) in [("api", "List.of"), ("api", "Optional"), ("idiom", "stream")] {
        conn.execute(
            "INSERT INTO curriculum_concepts (category, value, source_file, sprint_taught)
             VALUES (?, ?, 'lecture-1.tex', 1)",
            [cat, val],
        )
        .unwrap();
    }
    // First freeze writes the snapshot.
    let n1 =
        sprint_grader_curriculum::freeze_curriculum_for_sprint(&conn, ids::SPRINT_ID, 2).unwrap();
    assert_eq!(n1, 3);
    // Second freeze is a no-op.
    let n2 =
        sprint_grader_curriculum::freeze_curriculum_for_sprint(&conn, ids::SPRINT_ID, 2).unwrap();
    assert_eq!(n2, 0);
    let in_snapshot: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM curriculum_concepts_snapshot WHERE sprint_id = ?",
            [ids::SPRINT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(in_snapshot, 3);

    // Mutate the live table — snapshot should still win for the frozen sprint.
    conn.execute(
        "INSERT INTO curriculum_concepts (category, value, source_file, sprint_taught)
         VALUES ('api', 'Stream.of', 'lecture-2.tex', 1)",
        [],
    )
    .unwrap();
    let allowed =
        sprint_grader_curriculum::get_allowed_concepts_with_snapshot(&conn, ids::SPRINT_ID, 2)
            .unwrap();
    let api_set = allowed.get("api").expect("api category present");
    assert!(api_set.contains("List.of"));
    assert!(
        !api_set.contains("Stream.of"),
        "snapshot should not see live-table additions"
    );
}

// ─── T-T3.6 — Threshold jitter reproducible by `--today` ──────────────────

#[test]
fn t_t3_6_threshold_jitter_seed_is_today_plus_course_id() {
    let s1 = sprint_grader_core::jitter::seed_for("2026-04-26", 999);
    let s2 = sprint_grader_core::jitter::seed_for("2026-04-26", 999);
    assert_eq!(s1, s2, "same (today, course_id) → identical seed");
    let s3 = sprint_grader_core::jitter::seed_for("2026-04-27", 999);
    assert_ne!(s1, s3, "different `today` should produce a different seed");
}

#[test]
fn t_t3_6_apply_threshold_jitter_records_pipeline_run() {
    let tmp = tempfile::tempdir().unwrap();
    let (conn, _paths) = Fixture::new().build(tmp.path()).unwrap();
    let mut config = Config::test_default();
    config.grading.hidden_thresholds = true;
    config.grading.jitter_pct = 0.10;
    let record = sprint_grader_core::jitter::apply_threshold_jitter(&mut config, "2026-04-26", 999);
    sprint_grader_core::jitter::record_pipeline_run(&conn, &record).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM pipeline_run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    let (today, jitter_pct, seed): (String, f64, i64) = conn
        .query_row(
            "SELECT today, jitter_pct, seed FROM pipeline_run",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(today, "2026-04-26");
    assert!((jitter_pct - 0.10).abs() < 1e-9);
    assert_eq!(
        seed as u64,
        sprint_grader_core::jitter::seed_for("2026-04-26", 999)
    );
}

// ─── T-T3.7 — Per-detector regression fixtures still pass ─────────────────

#[test]
fn t_t3_7_per_detector_test_fixtures_count_holds() {
    // Meta-check: cannot rename / delete a fixture without bumping the
    // floor here. The repo has 30+ flag_*.rs files in
    // crates/analyze/tests/. A regression that drops one (e.g. a
    // refactor that loses LOW_MUTATION_SCORE coverage) trips this guard.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("analyze")
        .join("tests");
    let entries = std::fs::read_dir(&dir).unwrap();
    let mut count = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let n = name.to_string_lossy();
        if n.starts_with("flag_") && n.ends_with(".rs") {
            count += 1;
        }
    }
    assert!(
        count >= 30,
        "expected ≥30 flag_*.rs fixtures in {} (found {count})",
        dir.display()
    );
}
