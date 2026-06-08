//! Phase 4 acceptance: the emitted single-file HTML has every sentinel
//! substituted, wires all 25 knobs, carries the parity banner + VIEWS + an
//! embedded snapshot, and references NO external asset URLs.

use rusqlite::params;
use sprint_grader_core::Database;
use sprint_grader_grading_html::{build_snapshot_bytes, render_html};
use sprint_grader_grading_xlsx::{load_workbook_data, GradingConfig};
use tempfile::tempdir;

const PROJECT_ID: i64 = 1;
const SPRINT_ID: i64 = 10;

const SCALAR_KNOBS: [&str; 25] = [
    "w_doc",
    "w_cq",
    "w_surv",
    "w_arch",
    "ai_strength",
    "floor_keep",
    "undeclared_model_m",
    "undeclared_level_l",
    "max_penalty_points",
    "student_penalty_cap",
    "crit_sa_points",
    "crit_cx_points",
    "crit_flag_points",
    "security_extra",
    "doc_max",
    "mi_floor",
    "mi_ceiling",
    "cc_penalty",
    "test_bonus",
    "test_cap",
    "surv_floor",
    "surv_ceiling",
    "k_crit",
    "k_warn",
    "arch_norm",
];

fn rendered_page() -> String {
    let dir = tempdir().unwrap();
    let db = Database::open(&dir.path().join("grading.db")).unwrap();
    db.create_tables().unwrap();
    let conn = &db.conn;
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, 'team-01', 'Team 01')",
        params![PROJECT_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (?, ?, 'S1', '2026-01-01', '2026-01-15')",
        params![SPRINT_ID, PROJECT_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id)
         VALUES ('alice', 'alice', 'alice', 'Alice', ?)",
        params![PROJECT_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (1, 'T-1', 'A', 'TASK', 'DONE', 10, 'alice', ?)",
        params![SPRINT_ID],
    )
    .unwrap();

    let cfg = GradingConfig::default();
    let data = load_workbook_data(&db, &[PROJECT_ID], "2026-03-01", &cfg).unwrap();
    let snapshot = build_snapshot_bytes(&data, &cfg).unwrap();
    render_html(&snapshot, &cfg).unwrap()
}

#[test]
fn all_sentinels_substituted() {
    let html = rendered_page();
    for sentinel in [
        "/*__APP_CSS__*/",
        "/*__SQL_WASM_JS__*/",
        "/*__MATHJS__*/",
        "/*__ENGINE_JS__*/",
        "/*__APP_JS__*/",
        "/*__DEFAULT_KNOBS_JSON__*/",
        "/*__SQL_WASM_B64__*/",
        "/*__SNAPSHOT_B64__*/",
    ] {
        assert!(
            !html.contains(sentinel),
            "sentinel left unsubstituted: {sentinel}"
        );
    }
}

#[test]
fn page_wires_all_25_knobs_and_core_structure() {
    let html = rendered_page();
    // Each knob name appears as a JSON key in the injected default-knobs vector.
    for name in SCALAR_KNOBS {
        assert!(
            html.contains(&format!("\"{name}\"")),
            "missing knob: {name}"
        );
    }
    assert!(html.contains("knob-"), "missing knob input id scheme");
    assert!(
        html.contains("id=\"parity-banner\""),
        "missing parity banner"
    );
    assert!(html.contains("id=\"main-nav\""), "missing main navigation");
    assert!(html.contains("parseRoute"), "missing hash router");
    assert!(html.contains("explainStudent"), "missing grade explanation tree");
    assert!(html.contains("initSqlJs("), "missing sql.js init call");
    assert!(
        html.contains("wasmBinary"),
        "wasm must be passed as wasmBinary"
    );
}

#[test]
fn no_external_asset_references() {
    let html = rendered_page();
    // The page must be fully offline: no asset is loaded over the network.
    // (Incidental http(s) URLs inside vendored library comments are fine; what
    // matters is that nothing is *referenced* for loading.)
    for needle in [
        "src=\"http",
        "href=\"http",
        "//unpkg.com",
        "//cdn",
        "cdnjs",
        "registry.npmjs.org",
    ] {
        assert!(
            !html.contains(needle),
            "external asset reference found: {needle}"
        );
    }
}

#[test]
fn page_embeds_a_nontrivial_snapshot() {
    let html = rendered_page();
    // sql.js wasm (~870 KB b64) + snapshot + math.js → a large self-contained file.
    assert!(
        html.len() > 500_000,
        "page suspiciously small: {} bytes",
        html.len()
    );
}

/// Decode the base64 a `window.<var> = "..."` assignment embeds.
fn extract_b64(html: &str, var: &str) -> Vec<u8> {
    use base64::Engine as _;
    let needle = format!("window.{var} = \"");
    let start = html.find(&needle).expect("assignment present") + needle.len();
    let rest = &html[start..];
    let end = rest.find('"').expect("closing quote");
    base64::engine::general_purpose::STANDARD
        .decode(&rest[..end])
        .expect("valid base64")
}

#[test]
fn embedded_payloads_decode_and_open() {
    let html = rendered_page();
    // The embedded wasm must decode to a real WebAssembly module.
    let wasm = extract_b64(&html, "__SQL_WASM_B64__");
    assert_eq!(&wasm[0..4], b"\0asm", "embedded wasm is not a valid module");
    // The embedded snapshot must round-trip back to an openable SQLite DB.
    let snap = extract_b64(&html, "__SNAPSHOT_B64__");
    let tf = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tf.path(), &snap).unwrap();
    let conn = rusqlite::Connection::open(tf.path()).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM reference_project", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        n, 1,
        "embedded snapshot should round-trip the graded project"
    );
}
