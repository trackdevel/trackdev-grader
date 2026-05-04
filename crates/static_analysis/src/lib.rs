//! Java static-analysis stage (PMD / Checkstyle / SpotBugs) — T1 skeleton.
//!
//! Mirrors the shape of `sprint-grader-architecture` (T-P2.2/T-P3.1):
//! per-repo + per-sprint scan, blame-based per-student attribution, and a
//! `STATIC_ANALYSIS_HOTSPOT` flag in `analyze`. T1 ships only the type
//! surface and the SQL tables — no analyzer impls, no pipeline wiring.
//! Subsequent tasks (T2..T6) fill in PMD, Checkstyle, attribution,
//! pipeline+CLI, report rendering, and SpotBugs.

pub mod adapter;
pub mod attribution;
pub mod checkstyle;
pub mod config;
pub mod pmd;
pub mod presets;
pub mod sarif;

pub use adapter::{
    Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerOutput, AnalyzerStatus, Category, Finding,
    Severity,
};
pub use attribution::attribute_findings_for_repo;
pub use checkstyle::{Checkstyle, CHECKSTYLE_VERSION};
pub use config::Rules;
pub use pmd::{Pmd, PMD_VERSION};

use std::path::Path;

use rusqlite::Connection;
use tracing::warn;

/// Scan one cloned repo for one sprint, run enabled analyzers, persist
/// findings + per-student attribution. T4: analyzer loop is still a stub
/// (T5 wires PMD/Checkstyle into the scan_repo path), but the post-scan
/// attribution call is wired so direct callers and tests can exercise
/// the full flow once they've inserted findings themselves.
#[allow(unused_variables)]
pub fn scan_repo_to_db(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    rules: &Rules,
) -> rusqlite::Result<usize> {
    // Attribution always runs after the analyzer block writes its rows.
    // Mirrors the architecture crate's pattern at lib.rs:100-111: we log
    // and continue rather than propagate, so a single team's broken git
    // repo can't abort the whole pipeline.
    match attribute_findings_for_repo(conn, repo_path, repo_full_name, sprint_id) {
        Ok(n) => Ok(n),
        Err(e) => {
            warn!(
                repo = repo_full_name,
                sprint_id = sprint_id,
                error = %e,
                "static-analysis attribution failed; continuing"
            );
            Ok(0)
        }
    }
}

/// Project-level convenience used by the orchestration block: walk the
/// known repo subdirectories under `project_root` and call
/// `scan_repo_to_db` for each. T1 stub.
#[allow(unused_variables)]
pub fn scan_project_to_db(
    conn: &Connection,
    project_root: &Path,
    sprint_id: i64,
    rules: &Rules,
) -> rusqlite::Result<usize> {
    Ok(0)
}
