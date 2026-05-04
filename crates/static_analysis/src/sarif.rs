//! SARIF 2.1.0 → `Finding` normaliser. Single ingest path shared by all
//! three analyzers (PMD now, Checkstyle in T3, SpotBugs in T6). The tool
//! is identified via `runs[*].tool.driver.name` so callers can hand us a
//! SARIF blob without telling us who produced it.
//!
//! Severity mapping (per `proposal_v2.md §4.3`):
//! - **PMD**: `level: error` → CRITICAL, `warning` → WARNING, `note` → INFO.
//!   PMD 7 maps priorities 1-2 → error, 3 → warning, 4-5 → note before
//!   emitting SARIF, so reading `level` matches the priority-band rule.
//! - **Checkstyle**: `error` → WARNING (style/javadoc rules should not
//!   outrank a NullPointerException), everything else → INFO.
//! - **SpotBugs**: `properties.rank` is the source of truth — 1-4 →
//!   CRITICAL, 5-9 → WARNING, 10-20 → INFO. Falls back to `level` when
//!   rank is missing.
//!
//! Categories are derived per analyzer (helpUri parsing for PMD,
//! `properties.category` for Checkstyle, `properties.category` for
//! SpotBugs). Unknown categories collapse to a per-analyzer default.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::adapter::{Category, Finding, Severity};

// --- SARIF schema (just the slice we actually consume) ---------------------

#[derive(Debug, Deserialize)]
struct SarifReport {
    #[serde(default)]
    runs: Vec<SarifRun>,
}

#[derive(Debug, Deserialize)]
struct SarifRun {
    tool: SarifTool,
    #[serde(default)]
    results: Vec<SarifResult>,
}

#[derive(Debug, Deserialize)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(Debug, Deserialize)]
struct SarifDriver {
    #[serde(default)]
    name: String,
    /// Tool version, e.g. "7.7.0". Reserved for diagnostics in T5 — kept on
    /// the struct so future use lands without re-touching the schema.
    #[serde(default)]
    #[allow(dead_code)]
    version: Option<String>,
    #[serde(default)]
    rules: Vec<SarifRule>,
}

#[derive(Debug, Deserialize)]
struct SarifRule {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "helpUri")]
    help_uri: Option<String>,
    #[serde(default)]
    properties: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SarifResult {
    #[serde(default, rename = "ruleId")]
    rule_id: Option<String>,
    #[serde(default, rename = "ruleIndex")]
    rule_index: Option<i64>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    message: Option<SarifMessage>,
    #[serde(default)]
    locations: Vec<SarifLocation>,
    #[serde(default)]
    properties: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SarifMessage {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SarifLocation {
    #[serde(default, rename = "physicalLocation")]
    physical: Option<SarifPhysical>,
}

#[derive(Debug, Deserialize)]
struct SarifPhysical {
    #[serde(default, rename = "artifactLocation")]
    artifact: Option<SarifArtifact>,
    #[serde(default)]
    region: Option<SarifRegion>,
}

#[derive(Debug, Deserialize)]
struct SarifArtifact {
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SarifRegion {
    #[serde(default, rename = "startLine")]
    start_line: Option<u32>,
    #[serde(default, rename = "endLine")]
    end_line: Option<u32>,
}

// --- Public API -------------------------------------------------------------

/// Parse a SARIF 2.1.0 report off disk into normalised `Finding`s. The
/// analyzer is auto-detected from `runs[*].tool.driver.name`. Empty or
/// missing `runs` arrays return an empty vec; structurally-malformed JSON
/// surfaces as `Err`.
pub fn parse(path: &Path) -> Result<Vec<Finding>> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading SARIF from {}", path.display()))?;
    parse_str(&body)
}

pub fn parse_str(body: &str) -> Result<Vec<Finding>> {
    let report: SarifReport =
        serde_json::from_str(body).context("parsing SARIF — not valid JSON or wrong schema")?;
    let mut out = Vec::new();
    for run in &report.runs {
        let driver_name = run.tool.driver.name.to_ascii_lowercase();
        let analyzer_id = analyzer_id_from_driver(&driver_name);
        let rule_lookup = build_rule_lookup(&run.tool.driver.rules);
        for result in &run.results {
            if let Some(finding) = result_to_finding(analyzer_id, &rule_lookup, result) {
                out.push(finding);
            }
        }
    }
    Ok(out)
}

fn analyzer_id_from_driver(driver: &str) -> &'static str {
    if driver.contains("pmd") {
        "pmd"
    } else if driver.contains("checkstyle") {
        "checkstyle"
    } else if driver.contains("spotbugs") || driver.contains("findbugs") {
        "spotbugs"
    } else {
        // Unknown tool — keep the SARIF parseable but tag conservatively.
        "unknown"
    }
}

#[derive(Debug, Default, Clone)]
struct RuleMeta {
    id: Option<String>,
    help_uri: Option<String>,
    category: Option<String>,
}

fn build_rule_lookup(rules: &[SarifRule]) -> Vec<RuleMeta> {
    rules
        .iter()
        .map(|r| RuleMeta {
            id: r.id.clone(),
            help_uri: r.help_uri.clone(),
            category: r
                .properties
                .as_ref()
                .and_then(|v| v.get("category"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        })
        .collect()
}

fn result_to_finding(
    analyzer: &'static str,
    rules: &[RuleMeta],
    result: &SarifResult,
) -> Option<Finding> {
    // rule_id: prefer `result.ruleId`; fall back to indexed lookup, then
    // to a defensive sentinel.
    let rule_meta = result
        .rule_index
        .and_then(|idx| usize::try_from(idx).ok())
        .and_then(|idx| rules.get(idx))
        .cloned()
        .unwrap_or_default();

    let rule_id = result
        .rule_id
        .clone()
        .or_else(|| rule_meta.id.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let location = result.locations.first().and_then(|l| l.physical.as_ref());
    let file_path = location
        .and_then(|p| p.artifact.as_ref())
        .and_then(|a| a.uri.clone())
        .unwrap_or_default();
    if file_path.is_empty() {
        // No physical location → drop. We can't blame-attribute file-level
        // findings, so they don't earn their slot in the report.
        return None;
    }
    // SARIF URIs sometimes carry a `file://` prefix — normalise.
    let file_path = strip_file_scheme(&file_path);

    let region = location.and_then(|p| p.region.as_ref());
    let start_line = region.and_then(|r| r.start_line);
    let end_line = region.and_then(|r| r.end_line).or(start_line);

    let level = result.level.as_deref().unwrap_or("warning");
    let result_props = result.properties.as_ref();
    let severity = severity_from(analyzer, level, result_props);

    let category = category_for(analyzer, &rule_meta, result_props);

    let message = result
        .message
        .as_ref()
        .and_then(|m| m.text.clone())
        .unwrap_or_else(|| rule_id.clone());

    let help_uri = rule_meta.help_uri.filter(|s| !s.is_empty());

    let fingerprint =
        Finding::compute_fingerprint(analyzer, &rule_id, &file_path, start_line, &message);

    Some(Finding {
        analyzer: analyzer.to_string(),
        rule_id,
        category,
        severity,
        file_path,
        start_line,
        end_line,
        message,
        help_uri,
        fingerprint,
    })
}

fn strip_file_scheme(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

fn severity_from(
    analyzer: &'static str,
    level: &str,
    result_props: Option<&serde_json::Value>,
) -> Severity {
    let level = level.to_ascii_lowercase();
    match analyzer {
        "checkstyle" => match level.as_str() {
            // Checkstyle's "error" is the strictest level it emits. In a
            // teaching context, missing Javadoc shouldn't outrank a
            // NullPointerException — clamp at WARNING.
            "error" => Severity::Warning,
            _ => Severity::Info,
        },
        "spotbugs" => {
            // SpotBugs ships the rank in `properties.rank` (1=worst,
            // 20=mildest). Fall through to level when missing.
            let rank = result_props
                .and_then(|v| v.get("rank"))
                .and_then(|v| v.as_u64());
            match rank {
                Some(r) if r <= 4 => Severity::Critical,
                Some(r) if r <= 9 => Severity::Warning,
                Some(_) => Severity::Info,
                None => severity_from_level(&level),
            }
        }
        // PMD: priority 1-2 → "error", 3 → "warning", 4-5 → "note".
        _ => severity_from_level(&level),
    }
}

fn severity_from_level(level: &str) -> Severity {
    match level {
        "error" => Severity::Critical,
        "warning" => Severity::Warning,
        "note" | "info" => Severity::Info,
        _ => Severity::Warning,
    }
}

fn category_for(
    analyzer: &'static str,
    rule_meta: &RuleMeta,
    result_props: Option<&serde_json::Value>,
) -> Category {
    if let Some(cat) = result_props
        .and_then(|v| v.get("category"))
        .and_then(|v| v.as_str())
        .and_then(Category::from_str)
    {
        return cat;
    }
    if let Some(ref tag) = rule_meta.category {
        if let Some(cat) = Category::from_str(tag) {
            return cat;
        }
    }
    if let Some(ref uri) = rule_meta.help_uri {
        if let Some(cat) = pmd_category_from_helpuri(uri) {
            return cat;
        }
    }
    // Per-analyzer default for unmappable rules.
    match analyzer {
        "spotbugs" => Category::Bug,
        "checkstyle" => Category::Style,
        _ => Category::Style,
    }
}

/// PMD 7 helpUri pattern: `…/pmd_rules_java_<category>.html#<rule_id>`.
/// Returns `None` for non-PMD URLs or when the category segment isn't one
/// of PMD's eight standard categories.
fn pmd_category_from_helpuri(uri: &str) -> Option<Category> {
    let after = uri.split("pmd_rules_java_").nth(1)?;
    let cat_segment = after.split('.').next()?;
    match cat_segment {
        "bestpractices" | "errorprone" => Some(Category::Bug),
        "codestyle" => Some(Category::Style),
        "design" | "performance" | "multithreading" => Some(Category::Complexity),
        "documentation" => Some(Category::Documentation),
        "security" => Some(Category::Security),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PMD_SARIF: &str = r#"{
        "version": "2.1.0",
        "runs": [
            {
                "tool": {
                    "driver": {
                        "name": "PMD",
                        "version": "7.7.0",
                        "rules": [
                            {
                                "id": "UnusedPrivateField",
                                "helpUri": "https://docs.pmd-code.org/pmd-doc-7.7.0/pmd_rules_java_bestpractices.html#unusedprivatefield"
                            },
                            {
                                "id": "EmptyCatchBlock",
                                "helpUri": "https://docs.pmd-code.org/pmd-doc-7.7.0/pmd_rules_java_errorprone.html#emptycatchblock"
                            }
                        ]
                    }
                },
                "results": [
                    {
                        "ruleId": "UnusedPrivateField",
                        "ruleIndex": 0,
                        "level": "warning",
                        "message": {"text": "Avoid unused private fields such as 'foo'."},
                        "locations": [
                            {
                                "physicalLocation": {
                                    "artifactLocation": {"uri": "src/main/java/Foo.java"},
                                    "region": {"startLine": 7, "endLine": 7}
                                }
                            }
                        ]
                    },
                    {
                        "ruleId": "EmptyCatchBlock",
                        "ruleIndex": 1,
                        "level": "error",
                        "message": {"text": "Avoid empty catch blocks."},
                        "locations": [
                            {
                                "physicalLocation": {
                                    "artifactLocation": {"uri": "src/main/java/Foo.java"},
                                    "region": {"startLine": 12}
                                }
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;

    #[test]
    fn parses_pmd_sarif_into_findings() {
        let findings = parse_str(PMD_SARIF).unwrap();
        assert_eq!(findings.len(), 2);

        let f0 = &findings[0];
        assert_eq!(f0.analyzer, "pmd");
        assert_eq!(f0.rule_id, "UnusedPrivateField");
        assert_eq!(f0.severity, Severity::Warning);
        assert_eq!(f0.category, Category::Bug); // bestpractices → Bug
        assert_eq!(f0.file_path, "src/main/java/Foo.java");
        assert_eq!(f0.start_line, Some(7));
        assert_eq!(f0.end_line, Some(7));
        assert!(f0.help_uri.as_deref().unwrap().contains("pmd-code.org"));

        let f1 = &findings[1];
        assert_eq!(f1.severity, Severity::Critical); // PMD level=error → CRITICAL
        assert_eq!(f1.category, Category::Bug); // errorprone → Bug
        assert_eq!(
            f1.start_line,
            Some(12),
            "endLine missing — start_line still parsed"
        );
        assert_eq!(
            f1.end_line,
            Some(12),
            "missing endLine should fall back to start_line"
        );
    }

    #[test]
    fn fingerprint_is_set_per_finding() {
        let findings = parse_str(PMD_SARIF).unwrap();
        assert_eq!(findings[0].fingerprint.len(), 40);
        assert_ne!(findings[0].fingerprint, findings[1].fingerprint);
    }

    #[test]
    fn empty_runs_yields_empty_findings() {
        let body = r#"{"version": "2.1.0", "runs": []}"#;
        assert_eq!(parse_str(body).unwrap().len(), 0);
    }

    #[test]
    fn malformed_json_returns_error() {
        let err = parse_str("not json").unwrap_err();
        assert!(err.to_string().contains("SARIF"));
    }

    #[test]
    fn results_without_location_are_dropped() {
        let body = r#"{
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {"name": "PMD", "rules": []}},
                "results": [
                    {"ruleId": "Foo", "level": "warning", "message": {"text": "x"}}
                ]
            }]
        }"#;
        let findings = parse_str(body).unwrap();
        assert_eq!(
            findings.len(),
            0,
            "file-level findings (no physicalLocation) are unattributable; drop them"
        );
    }

    #[test]
    fn checkstyle_error_clamps_to_warning() {
        let body = r#"{
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {"name": "Checkstyle", "rules": []}},
                "results": [{
                    "ruleId": "MissingJavadocMethod",
                    "level": "error",
                    "message": {"text": "Missing javadoc"},
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": {"uri": "Foo.java"},
                            "region": {"startLine": 3}
                        }
                    }]
                }]
            }]
        }"#;
        let findings = parse_str(body).unwrap();
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].analyzer, "checkstyle");
    }

    #[test]
    fn spotbugs_rank_drives_severity() {
        let body = r#"{
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {"name": "SpotBugs", "rules": []}},
                "results": [
                    {
                        "ruleId": "NP_NULL_ON_SOME_PATH",
                        "level": "warning",
                        "message": {"text": "..."},
                        "properties": {"rank": 3},
                        "locations": [{
                            "physicalLocation": {
                                "artifactLocation": {"uri": "X.java"},
                                "region": {"startLine": 1}
                            }
                        }]
                    },
                    {
                        "ruleId": "DM_DEFAULT_ENCODING",
                        "level": "warning",
                        "message": {"text": "..."},
                        "properties": {"rank": 15},
                        "locations": [{
                            "physicalLocation": {
                                "artifactLocation": {"uri": "X.java"},
                                "region": {"startLine": 5}
                            }
                        }]
                    }
                ]
            }]
        }"#;
        let findings = parse_str(body).unwrap();
        assert_eq!(findings[0].severity, Severity::Critical); // rank 3
        assert_eq!(findings[1].severity, Severity::Info); // rank 15
    }

    #[test]
    fn pmd_helpuri_category_is_used_when_properties_missing() {
        let body = r#"{
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {
                    "name": "PMD",
                    "rules": [{
                        "id": "X",
                        "helpUri": "https://docs.pmd-code.org/pmd-doc-7.7.0/pmd_rules_java_security.html#x"
                    }]
                }},
                "results": [{
                    "ruleId": "X",
                    "ruleIndex": 0,
                    "level": "warning",
                    "message": {"text": "..."},
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": {"uri": "Foo.java"},
                            "region": {"startLine": 1}
                        }
                    }]
                }]
            }]
        }"#;
        let findings = parse_str(body).unwrap();
        assert_eq!(findings[0].category, Category::Security);
    }
}
