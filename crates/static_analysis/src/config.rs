//! `config/static_analysis.toml` loader. Mirrors
//! `architecture::rules::ArchitectureRules::load`'s contract: present file →
//! parse and return `Rules`; absent file → caller skips the stage. Any IO or
//! parse error is surfaced verbatim — the orchestration block logs and moves
//! on so a malformed file never aborts the wider pipeline.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::adapter::Severity;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    /// Drop findings strictly below this severity before INSERT. Default
    /// `INFO` keeps everything (the per-tool severity caps still apply).
    #[serde(default = "default_severity_floor")]
    pub severity_floor: Severity,

    /// Per `(repo, sprint, analyzer)` cap on findings, applied after the
    /// severity floor. Guards against a pathological config emitting many
    /// thousands of structurally identical warnings.
    #[serde(default = "default_max_findings")]
    pub max_findings_per_analyzer: usize,

    /// Wall-clock budget for a single analyzer invocation (per repo).
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,

    /// `"es" | "ca" | "en"`. Surfaces in `JAVA_TOOL_OPTIONS` for analyzers
    /// that ship localised messages; otherwise informational.
    #[serde(default = "default_locale")]
    pub locale: String,

    #[serde(default)]
    pub pmd: PmdRules,
    #[serde(default)]
    pub checkstyle: CheckstyleRules,
    #[serde(default)]
    pub spotbugs: SpotBugsRules,
    #[serde(default)]
    pub reporting: ReportingRules,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PmdRules {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `"beginner" | "standard" | "strict"`. T1 stores the string verbatim;
    /// T2 resolves to the embedded XML.
    #[serde(default = "default_pmd_preset")]
    pub preset: String,
    /// Optional override; takes precedence over `preset` when set.
    #[serde(default)]
    pub ruleset_path: Option<String>,
    /// Within-repo copy-paste detection. Cross-team CPD is out of scope (it
    /// already lives in `survival::cross_team`).
    #[serde(default = "default_true")]
    pub include_cpd: bool,
    #[serde(default = "default_pmd_heap")]
    pub heap_mb: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckstyleRules {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_checkstyle_preset")]
    pub preset: String,
    #[serde(default)]
    pub ruleset_path: Option<String>,
    #[serde(default = "default_checkstyle_heap")]
    pub heap_mb: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpotBugsRules {
    /// Even when `enabled = true`, the analyzer auto-skips when the latest
    /// PR for the sprint did not compile (see T6).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `"min" | "less" | "default" | "more" | "max"`.
    #[serde(default = "default_spotbugs_effort")]
    pub effort: String,
    /// SpotBugs ranks 1 (worst) to 20 (mildest). Findings strictly above
    /// this rank are dropped at the analyzer level.
    #[serde(default = "default_spotbugs_min_rank")]
    pub min_rank: u32,
    #[serde(default = "default_true")]
    pub include_findsecbugs: bool,
    #[serde(default = "default_spotbugs_heap")]
    pub heap_mb: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportingRules {
    #[serde(default = "default_true")]
    pub group_by_file: bool,
    /// Maximum findings listed per student in `REPORT.md`. Surplus findings
    /// roll up into a single `… N more` line.
    #[serde(default = "default_top_n_per_student")]
    pub top_n_per_student: usize,
    #[serde(default = "default_true")]
    pub include_help_uri: bool,
}

impl Rules {
    /// Read and parse `static_analysis.toml`. Mirrors
    /// `ArchitectureRules::load`: any IO or parse error bubbles up with
    /// context; the orchestration layer decides whether to abort or warn.
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_toml_str(&text)
    }

    pub fn from_toml_str(text: &str) -> Result<Self> {
        toml::from_str::<Rules>(text).context("parsing static_analysis.toml")
    }
}

// --- Phase-1-confirmed defaults (do not re-prompt the user) ----------------

impl Default for Rules {
    fn default() -> Self {
        Self {
            severity_floor: default_severity_floor(),
            max_findings_per_analyzer: default_max_findings(),
            timeout_seconds: default_timeout_seconds(),
            locale: default_locale(),
            pmd: PmdRules::default(),
            checkstyle: CheckstyleRules::default(),
            spotbugs: SpotBugsRules::default(),
            reporting: ReportingRules::default(),
        }
    }
}

impl Default for PmdRules {
    fn default() -> Self {
        Self {
            enabled: true,
            preset: default_pmd_preset(),
            ruleset_path: None,
            include_cpd: true,
            heap_mb: default_pmd_heap(),
        }
    }
}

impl Default for CheckstyleRules {
    fn default() -> Self {
        Self {
            enabled: true,
            preset: default_checkstyle_preset(),
            ruleset_path: None,
            heap_mb: default_checkstyle_heap(),
        }
    }
}

impl Default for SpotBugsRules {
    fn default() -> Self {
        Self {
            enabled: true,
            effort: default_spotbugs_effort(),
            min_rank: default_spotbugs_min_rank(),
            include_findsecbugs: true,
            heap_mb: default_spotbugs_heap(),
        }
    }
}

impl Default for ReportingRules {
    fn default() -> Self {
        Self {
            group_by_file: true,
            top_n_per_student: default_top_n_per_student(),
            include_help_uri: true,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_severity_floor() -> Severity {
    Severity::Info
}
fn default_max_findings() -> usize {
    200
}
fn default_timeout_seconds() -> u64 {
    120
}
fn default_locale() -> String {
    // Phase-1: Spanish framing in the report; English rule messages.
    "es".to_string()
}
// Phase-1 strong recommendation: ship "beginner" presets by default since
// REPORT.md is committed back to team repos.
fn default_pmd_preset() -> String {
    "beginner".to_string()
}
fn default_checkstyle_preset() -> String {
    "beginner".to_string()
}
fn default_pmd_heap() -> u32 {
    512
}
fn default_checkstyle_heap() -> u32 {
    256
}
fn default_spotbugs_heap() -> u32 {
    1024
}
fn default_spotbugs_effort() -> String {
    "default".to_string()
}
fn default_spotbugs_min_rank() -> u32 {
    14
}
fn default_top_n_per_student() -> usize {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_phase1_recommendations() {
        let r = Rules::default();
        assert_eq!(r.locale, "es");
        assert_eq!(r.pmd.preset, "beginner");
        assert_eq!(r.checkstyle.preset, "beginner");
        assert!(r.pmd.enabled);
        assert!(r.checkstyle.enabled);
        assert!(r.spotbugs.enabled);
        assert_eq!(r.reporting.top_n_per_student, 5);
    }

    #[test]
    fn empty_toml_yields_default_blocks() {
        let r = Rules::from_toml_str("").unwrap();
        assert_eq!(r.timeout_seconds, 120);
        assert!(r.pmd.include_cpd);
    }

    #[test]
    fn full_toml_round_trips() {
        let body = r#"
severity_floor = "WARNING"
max_findings_per_analyzer = 50
timeout_seconds = 60
locale = "ca"

[pmd]
enabled = false
preset = "strict"
include_cpd = false
heap_mb = 1024

[checkstyle]
enabled = true
preset = "standard"
heap_mb = 384

[spotbugs]
enabled = true
effort = "max"
min_rank = 8
include_findsecbugs = false
heap_mb = 2048

[reporting]
group_by_file = false
top_n_per_student = 10
include_help_uri = false
"#;
        let r = Rules::from_toml_str(body).unwrap();
        assert_eq!(r.severity_floor, Severity::Warning);
        assert!(!r.pmd.enabled);
        assert_eq!(r.pmd.preset, "strict");
        assert_eq!(r.spotbugs.min_rank, 8);
        assert!(!r.reporting.group_by_file);
        assert_eq!(r.reporting.top_n_per_student, 10);
    }

    #[test]
    fn unknown_field_is_rejected() {
        // `deny_unknown_fields` catches typos in the TOML schema.
        let err = Rules::from_toml_str("ttimeout_seconds = 30\n");
        assert!(err.is_err());
    }
}
