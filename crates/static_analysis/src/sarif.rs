//! SARIF 2.1.0 → `Finding` normaliser. Single ingest path shared by all
//! three analyzers (PMD in T2, Checkstyle in T3, SpotBugs in T6).
//!
//! T1 stub: returns an empty vector. The real implementation lands in T2
//! together with the PMD adapter, since PMD's SARIF output is the most
//! permissive and exercises every field shape.

use std::path::Path;

use anyhow::Result;

use crate::adapter::Finding;

/// Parse a SARIF 2.1.0 report off disk into normalised `Finding`s. T1
/// stub — always `Ok(vec![])`.
#[allow(unused_variables)]
pub fn parse(path: &Path) -> Result<Vec<Finding>> {
    Ok(vec![])
}
