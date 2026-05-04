//! Java static-analysis stage (PMD / Checkstyle / SpotBugs) — T1 skeleton.
//!
//! Mirrors the shape of `sprint-grader-architecture` (T-P2.2/T-P3.1):
//! per-repo + per-sprint scan, blame-based per-student attribution, and a
//! `STATIC_ANALYSIS_HOTSPOT` flag in `analyze`. T1 ships only the type
//! surface and the SQL tables — no analyzer impls, no pipeline wiring.
//! Subsequent tasks (T2..T6) fill in PMD, Checkstyle, attribution,
//! pipeline+CLI, report rendering, and SpotBugs.

pub mod adapter;
pub mod config;
pub mod sarif;

pub use adapter::{
    Analyzer, AnalyzerConfig, AnalyzerInput, AnalyzerOutput, AnalyzerStatus, Category, Finding,
    Severity,
};
pub use config::Rules;

use std::path::Path;

use rusqlite::Connection;

/// Scan one cloned repo for one sprint, run enabled analyzers, persist
/// findings + per-student attribution. T1: stub returning `Ok(0)`; the
/// real implementation lands incrementally over T2..T6.
#[allow(unused_variables)]
pub fn scan_repo_to_db(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    rules: &Rules,
) -> rusqlite::Result<usize> {
    Ok(0)
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
