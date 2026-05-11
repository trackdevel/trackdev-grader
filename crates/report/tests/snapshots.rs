//! Golden-file snapshot test for the multi-sprint markdown report.
//!
//! Pins the rendered output for a tiny fixture (2 students, 1 sprint, 1 PR,
//! one finding of each rule kind: architecture, complexity, static analysis)
//! so any rendering regression — including the static-analysis URL bug that
//! produced `…/blob/HEAD//home/imartin/…` URLs in production — is caught in
//! CI. The explicit `!body.contains("/home/")` assertion makes that bug
//! class fail loudly without relying on a human eyeballing the snapshot diff.
//!
//! Update the snapshot intentionally after a rendering change with:
//!
//! ```sh
//! INSTA_UPDATE=always cargo test -p sprint-grader-report --test snapshots
//! ```

use rusqlite::{params, Connection};
use sprint_grader_core::db::apply_schema;
use sprint_grader_report::{generate_markdown_report_multi_to_path_with_opts, MultiReportOptions};
use std::path::Path;
use tempfile::TempDir;

/// Tiny synthetic project that exercises the three rule-finding renderers
/// without any real filesystem state. All file paths are repo-relative by
/// construction.
fn seed_mini_project(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO projects (id, slug, name) VALUES (1, 'pds-mini', 'PDS Mini');
         INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES
            (10, 1, 'Sprint 1', '2026-04-01T00:00Z', '2026-04-30T23:59Z');
         INSERT INTO students (id, full_name, github_login, team_project_id) VALUES
            ('alice', 'Alice Adams', 'alice-gh', 1),
            ('bob',   'Bob Brown',   'bob-gh',   1);
         INSERT INTO student_github_identity
            (student_id, identity_kind, identity_value, weight, confidence) VALUES
            ('alice', 'login', 'alice-gh', 1.0, 1.0),
            ('bob',   'login', 'bob-gh',   1.0, 1.0);",
    )
    .unwrap();
}

fn seed_task_and_pr(conn: &Connection) {
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                            assignee_id, sprint_id)
         VALUES (1, 'T-1', 'Login endpoint', 'TASK', 'DONE', 5, 'alice', 10)",
        params![],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pull_requests
            (id, pr_number, repo_full_name, title, url, author_id, merged, merged_at,
             additions, deletions, changed_files)
         VALUES ('pr-1', 1, 'udg-pds/spring-mini', 'Add login endpoint',
                 'https://github.com/udg-pds/spring-mini/pull/1', 'alice',
                 1, '2026-04-15T10:00Z', 100, 20, 5)",
        params![],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (1, 'pr-1')",
        params![],
    )
    .unwrap();
}

/// One architecture finding on Login.java, attributed entirely to Alice,
/// with the ARCHITECTURE_HOTSPOT gating flag so the per-student block renders.
fn seed_architecture_finding(conn: &Connection) {
    conn.execute(
        "INSERT INTO architecture_violations
            (repo_full_name, file_path, rule_name, violation_kind, offending_import,
             severity, start_line, end_line, rule_kind, explanation)
         VALUES ('udg-pds/spring-mini', 'src/main/java/Login.java', 'VALIDATION_IN_UI',
                 'ast_forbidden_field_type', 'com.x.repo.UserRepo', 'WARNING',
                 42, 99, 'ast_forbidden_field_type',
                 'Validation belongs in the service layer, not the controller.')",
        params![],
    )
    .unwrap();
    let v_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO architecture_violation_attribution
            (violation_rowid, student_id, lines_authored, total_lines, weight)
         VALUES (?, 'alice', 58, 58, 1.0)",
        params![v_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO student_artifact_flags
            (student_id, project_id, flag_type, severity, details)
         VALUES ('alice', 1, 'ARCHITECTURE_HOTSPOT', 'WARNING',
                 '{\"weighted\":1.0,\"min_weighted\":0.5}')",
        params![],
    )
    .unwrap();
}

/// One complexity finding (parameters > ceiling) + gating flag.
fn seed_complexity_finding(conn: &Connection) {
    conn.execute(
        "INSERT INTO method_complexity_findings
            (project_id, repo_full_name, file_path, class_name, method_name,
             start_line, end_line, rule_key, severity,
             measured_value, threshold, detail)
         VALUES (1, 'udg-pds/spring-mini', 'src/main/java/Login.java',
                 'LoginController', 'authenticate', 42, 99,
                 'wide_signature', 'WARNING', 12.0, 10.0,
                 'Method takes more parameters than the ceiling allows.')",
        params![],
    )
    .unwrap();
    let f_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO method_complexity_attribution
            (finding_id, student_id, lines_attributed, weighted_lines, weight)
         VALUES (?, 'alice', 58, 58.0, 1.0)",
        params![f_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO student_artifact_flags
            (student_id, project_id, flag_type, severity, details)
         VALUES ('alice', 1, 'COMPLEXITY_HOTSPOT', 'WARNING',
                 '{\"weighted\":1.0}')",
        params![],
    )
    .unwrap();
}

/// One static-analysis finding (PMD UnusedPrivateMethod) + attribution row.
fn seed_static_analysis_finding(conn: &Connection) {
    conn.execute(
        "INSERT INTO static_analysis_findings
            (repo_full_name, analyzer, rule_id, severity, file_path,
             start_line, end_line, message, fingerprint)
         VALUES ('udg-pds/spring-mini', 'pmd', 'UnusedPrivateMethod', 'INFO',
                 'src/main/java/Login.java', 42, 99,
                 'Avoid unused private methods such as helper().',
                 'fp-1')",
        params![],
    )
    .unwrap();
    let f_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO static_analysis_finding_attribution
            (finding_id, student_id, lines_authored, total_lines, weight)
         VALUES (?, 'alice', 58, 58, 1.0)",
        params![f_id],
    )
    .unwrap();
}

fn render_report(conn: &Connection, out_dir: &Path) -> String {
    let path = out_dir.join("mini_report.md");
    let opts = MultiReportOptions::instructor();
    generate_markdown_report_multi_to_path_with_opts(conn, 1, "PDS Mini", &[10], &path, opts)
        .expect("markdown render");
    std::fs::read_to_string(&path).expect("read rendered report")
}

#[test]
fn mini_project_report_contains_no_absolute_paths() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    seed_mini_project(&conn);
    seed_task_and_pr(&conn);
    seed_architecture_finding(&conn);
    seed_complexity_finding(&conn);
    seed_static_analysis_finding(&conn);

    let tmp = TempDir::new().unwrap();
    let body = render_report(&conn, tmp.path());

    assert!(
        !body.contains("/home/"),
        "rendered report must never embed absolute filesystem paths. \
         The static-analysis URL bug (W1.T3) showed up as URLs like \
         `…/blob/HEAD//home/imartin/…`. Body follows:\n\n{}",
        body
    );

    // One occurrence of each RuleKind reaches the rendered output. The
    // exact bullet shape is pinned by the snapshot test below; this
    // assertion just guarantees all three code paths fired.
    assert!(
        body.contains("VALIDATION IN UI"),
        "architecture finding (RuleKind::Architecture) missing from report:\n{}",
        body
    );
    assert!(
        body.contains("LoginController.authenticate()"),
        "complexity finding (RuleKind::Complexity) missing from report:\n{}",
        body
    );
    assert!(
        body.contains("pmd:UnusedPrivateMethod"),
        "static-analysis finding (RuleKind::StaticAnalysis) missing from report:\n{}",
        body
    );

    // Every blob URL is repo-relative. Three findings × one repo = three URLs.
    let blob_url_count = body.matches("blob/HEAD/src/main/java/Login.java").count();
    assert_eq!(
        blob_url_count, 3,
        "expected three repo-relative blob URLs (one per finding); got {} in:\n{}",
        blob_url_count, body
    );
}

#[test]
fn mini_project_report_snapshot() {
    let conn = Connection::open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    seed_mini_project(&conn);
    seed_task_and_pr(&conn);
    seed_architecture_finding(&conn);
    seed_complexity_finding(&conn);
    seed_static_analysis_finding(&conn);

    let tmp = TempDir::new().unwrap();
    let body = render_report(&conn, tmp.path());

    insta::with_settings!({
        // Scrub the "generated at" line so the snapshot is reproducible
        // across runs.
        filters => vec![
            (r"Generated at \d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[^\n]*", "Generated at <SCRUBBED>"),
        ],
        snapshot_path => "snapshots",
    }, {
        insta::assert_snapshot!("mini_report", body);
    });
}
