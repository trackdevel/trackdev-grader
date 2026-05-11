//! Common analyzer surface — the trait every external tool (PMD, Checkstyle,
//! SpotBugs) implements, plus the normalised `Finding` model that all three
//! adapters produce after parsing the tool's SARIF output.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha1::{Digest, Sha1};
use sprint_grader_core::finding::{LineSpan, RuleFinding, RuleKind, Severity as CoreSeverity};

/// Inputs handed to every analyzer per (repo, sprint) invocation. Lifetime
/// `'a` borrows the cloned-repo paths and identifiers from the orchestration
/// layer — analyzers neither own nor mutate them.
pub struct AnalyzerInput<'a> {
    pub repo_path: &'a Path,
    /// `<org>/<repo>` form, e.g. `udg-pds/spring-pds26_4c`.
    pub repo_full_name: &'a str,
    /// HEAD SHA at scan time, written to `static_analysis_runs.head_sha` for
    /// reproducibility. `None` when the repo isn't a git working tree (rare).
    pub head_sha: Option<String>,
    /// Source roots to feed the analyzer (e.g. `["src/main/java"]`).
    pub source_roots: Vec<PathBuf>,
    /// Class output roots (populated from `compile_stage` worktrees). Empty
    /// when the latest PR for this sprint did not compile successfully — the
    /// SpotBugs adapter (T6) treats that as `SkippedNoClasses`.
    pub class_roots: Vec<PathBuf>,
    /// Major JDK version available to the analyzer process (17 / 21).
    pub jdk_major: u32,
    /// Scratch directory the analyzer may use for SARIF output and
    /// intermediate files. Created and cleaned up by the caller.
    pub work_dir: PathBuf,
    pub timeout: Duration,
    pub max_heap_mb: u32,
    pub locale: String,
}

/// Per-invocation analyzer configuration: which ruleset to use, the
/// minimum severity to keep, and a hard cap on findings to avoid pathological
/// noise (e.g. a malformed pom emitting thousands of structurally identical
/// warnings).
pub struct AnalyzerConfig {
    /// `"beginner" | "standard" | "strict"` or an absolute path to a custom
    /// ruleset XML. Resolution into a real on-disk file is the analyzer's
    /// responsibility (see `presets.rs` in T2).
    pub ruleset_ref: String,
    pub severity_floor: Severity,
    pub max_findings: usize,
}

/// Outcome of a single analyzer run, persisted to `static_analysis_runs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzerStatus {
    Ok,
    /// SpotBugs only — class output dir empty/missing because compile_stage
    /// reported failure for the relevant PR. Distinct from `Crashed` so the
    /// report can render "skipped — compile failed" honestly.
    SkippedNoClasses,
    Crashed,
    TimedOut,
}

impl AnalyzerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::SkippedNoClasses => "SKIPPED_NO_CLASSES",
            Self::Crashed => "CRASHED",
            Self::TimedOut => "TIMED_OUT",
        }
    }
}

pub struct AnalyzerOutput {
    pub status: AnalyzerStatus,
    pub findings: Vec<Finding>,
    /// Path to the raw SARIF artefact (kept for debugging on `Crashed`).
    pub raw_report: Option<PathBuf>,
    pub duration_ms: u64,
    pub diagnostics: String,
}

pub trait Analyzer {
    /// Stable identifier — `"pmd" | "checkstyle" | "spotbugs"`. Written into
    /// `static_analysis_findings.analyzer` and used as the report grouping
    /// key.
    fn id(&self) -> &'static str;
    fn version(&self) -> &str;
    /// SpotBugs returns true; PMD and Checkstyle return false.
    fn requires_classes(&self) -> bool;
    fn run(&self, input: &AnalyzerInput, cfg: &AnalyzerConfig) -> AnalyzerOutput;
}

// ---- Normalised finding model ---------------------------------------------

/// One row in `static_analysis_findings`. All three analyzers funnel into
/// this shape via `sarif::parse` (T2).
#[derive(Debug, Clone)]
pub struct Finding {
    pub analyzer: String,
    pub rule_id: String,
    pub category: Category,
    pub severity: Severity,
    /// Repo-relative POSIX path. Enforced by `sarif::parse(..., repo_root)`
    /// (W2.T3): findings whose URI lies outside `repo_root` are dropped
    /// before reaching this struct. Pre-W2.T3 rows may still hold absolute
    /// paths until the repo is re-scanned.
    pub file_path: String,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub message: String,
    pub help_uri: Option<String>,
    pub fingerprint: String,
}

impl Finding {
    /// W2.T3: convert one PMD/Checkstyle/SpotBugs finding into the shared
    /// `RuleFinding` shape consumed by the unified attribution +
    /// renderer pipeline. The `rule_id` is namespaced
    /// (`pmd:UnusedPrivateMethod`, `checkstyle:MissingJavadocMethod`,
    /// `spotbugs:DM_DEFAULT_ENCODING`) so the unified renderer can
    /// surface the source tool inline.
    pub fn into_rule_finding(self, repo_full_name: &str) -> RuleFinding {
        let span = match (self.start_line, self.end_line) {
            (Some(s), Some(e)) if e > s => LineSpan::range(s, e),
            (Some(s), _) => LineSpan::single(s),
            _ => LineSpan::single(0),
        };
        let severity = match self.severity {
            Severity::Critical => CoreSeverity::Critical,
            Severity::Warning => CoreSeverity::Warning,
            Severity::Info => CoreSeverity::Info,
        };
        RuleFinding {
            rule_id: format!("{}:{}", self.analyzer, self.rule_id),
            kind: RuleKind::StaticAnalysis,
            severity,
            repo_full_name: repo_full_name.to_string(),
            file_repo_relative: self.file_path,
            span,
            evidence: self.message,
            extra: None,
        }
    }

    /// Stable identifier used for the `UNIQUE (repo, sprint, fingerprint)`
    /// constraint on `static_analysis_findings`. SHA-1 of the canonical
    /// composite `analyzer|rule_id|file_path|start_line|message[..120]` —
    /// `start_line` is rendered as the empty string when `None` so file-level
    /// findings still hash deterministically.
    pub fn compute_fingerprint(
        analyzer: &str,
        rule_id: &str,
        file_path: &str,
        start_line: Option<u32>,
        message: &str,
    ) -> String {
        let msg_120: String = message.chars().take(120).collect();
        let composite = format!(
            "{}|{}|{}|{}|{}",
            analyzer,
            rule_id,
            file_path,
            start_line.map(|n| n.to_string()).unwrap_or_default(),
            msg_120,
        );
        let digest = Sha1::digest(composite.as_bytes());
        format!("{:x}", digest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::Warning => "WARNING",
            Self::Info => "INFO",
        }
    }

    /// Comparison helper for the `severity_floor` cut-off. `Critical` is the
    /// strongest; `Info` the weakest.
    pub fn rank(&self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Critical => 2,
        }
    }

    /// Returns `true` when `self >= other` (i.e. `self` is at least as severe
    /// as `other`). Used to drop sub-floor findings before INSERT.
    pub fn at_least(&self, other: Severity) -> bool {
        self.rank() >= other.rank()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Style,
    Bug,
    Security,
    Complexity,
    Duplication,
    Documentation,
}

impl Category {
    /// Lenient mapping from the string forms the three tools and their
    /// SARIF outputs use. Returns `None` on unknown labels — the caller
    /// substitutes a reasonable default (typically `Bug` for SpotBugs and
    /// `Style` for the source-only tools).
    ///
    /// Named `from_str` to match the plan; the standard `FromStr` trait
    /// would force `Result<_, Err>`, but we want `Option` here.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "style" | "codestyle" => Some(Self::Style),
            "bug" | "correctness" | "errorprone" | "error-prone" => Some(Self::Bug),
            "security" | "malicious_code" | "malicious-code" => Some(Self::Security),
            "complexity" | "design" | "performance" | "multithreading" => Some(Self::Complexity),
            "duplication" | "cpd" => Some(Self::Duplication),
            "documentation" | "javadoc" | "doc" => Some(Self::Documentation),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Style => "style",
            Self::Bug => "bug",
            Self::Security => "security",
            Self::Complexity => "complexity",
            Self::Duplication => "duplication",
            Self::Documentation => "documentation",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_rank_orders_correctly() {
        assert!(Severity::Critical.at_least(Severity::Warning));
        assert!(Severity::Critical.at_least(Severity::Info));
        assert!(Severity::Warning.at_least(Severity::Info));
        assert!(!Severity::Info.at_least(Severity::Warning));
        assert!(Severity::Warning.at_least(Severity::Warning));
    }

    #[test]
    fn severity_round_trips_via_str() {
        for s in [Severity::Critical, Severity::Warning, Severity::Info] {
            assert!(["CRITICAL", "WARNING", "INFO"].contains(&s.as_str()));
        }
    }

    #[test]
    fn category_from_str_is_lenient() {
        assert_eq!(Category::from_str("Style"), Some(Category::Style));
        assert_eq!(
            Category::from_str("CORRECTNESS"),
            Some(Category::Bug),
            "SpotBugs uses 'CORRECTNESS' for what we file under Bug"
        );
        assert_eq!(
            Category::from_str("multithreading"),
            Some(Category::Complexity)
        );
        assert_eq!(Category::from_str("garbage"), None);
    }

    #[test]
    fn fingerprint_is_stable_across_calls() {
        let a = Finding::compute_fingerprint("pmd", "UnusedPrivateField", "Foo.java", Some(7), "x");
        let b = Finding::compute_fingerprint("pmd", "UnusedPrivateField", "Foo.java", Some(7), "x");
        assert_eq!(a, b);
        // Differs on any input change.
        let c = Finding::compute_fingerprint("pmd", "UnusedPrivateField", "Foo.java", Some(8), "x");
        assert_ne!(a, c);
    }

    #[test]
    fn fingerprint_handles_missing_start_line() {
        let a = Finding::compute_fingerprint("pmd", "R", "F.java", None, "msg");
        // Sanity: 40 hex chars (sha1 length).
        assert_eq!(a.len(), 40);
    }

    #[test]
    fn fingerprint_truncates_message_at_120_chars() {
        let short = "a".repeat(120);
        let long = "a".repeat(500);
        let f1 = Finding::compute_fingerprint("pmd", "R", "F.java", Some(1), &short);
        let f2 = Finding::compute_fingerprint("pmd", "R", "F.java", Some(1), &long);
        assert_eq!(
            f1, f2,
            "messages identical in their first 120 chars must hash the same"
        );
    }
}
