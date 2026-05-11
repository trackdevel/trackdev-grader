//! Shared finding types used by every rule-based scanner (architecture,
//! complexity, static analysis).
//!
//! W1.T1: these types are introduced **but not yet consumed** by any other
//! crate. Wave 2 migrates each scanner to emit `RuleFinding`s and the report
//! crate to render `AttributedFinding`s through a single template.

use std::cmp::Ordering;
use std::fmt;

/// Which scanner produced the finding. Used by the renderer to pick the
/// right label and (eventually) by aggregation logic that wants to count
/// findings per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleKind {
    Architecture,
    Complexity,
    StaticAnalysis,
}

impl fmt::Display for RuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            RuleKind::Architecture => "Architecture",
            RuleKind::Complexity => "Complexity",
            RuleKind::StaticAnalysis => "StaticAnalysis",
        };
        f.write_str(s)
    }
}

/// Severity buckets. Matches the strings already stored in
/// `architecture_violations.severity` and the static-analysis schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Severity::Critical => "CRITICAL",
            Severity::Warning => "WARNING",
            Severity::Info => "INFO",
        };
        f.write_str(s)
    }
}

/// A line range inside a file. `end` is inclusive when present; absent for
/// single-line findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineSpan {
    pub start: u32,
    pub end: Option<u32>,
}

impl LineSpan {
    pub fn single(line: u32) -> Self {
        Self {
            start: line,
            end: None,
        }
    }

    pub fn range(start: u32, end: u32) -> Self {
        Self {
            start,
            end: Some(end),
        }
    }

    /// Number of lines covered (always ≥ 1).
    pub fn line_count(&self) -> u32 {
        match self.end {
            Some(end) if end >= self.start => end - self.start + 1,
            _ => 1,
        }
    }
}

/// Ordered by `start`, then by `end` (treating `None` as `start` — i.e.
/// a single-line finding sorts before a multi-line one beginning at the
/// same start).
impl Ord for LineSpan {
    fn cmp(&self, other: &Self) -> Ordering {
        self.start.cmp(&other.start).then_with(|| {
            self.end
                .unwrap_or(self.start)
                .cmp(&other.end.unwrap_or(other.start))
        })
    }
}

impl PartialOrd for LineSpan {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A rule violation produced by one of the three scanners.
///
/// `file_repo_relative` MUST be a repo-relative POSIX path (use
/// `paths::repo_relative` at the scanner boundary). This is enforced by
/// the renderer in `crates/report` via debug-asserts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuleFinding {
    pub rule_id: String,
    pub kind: RuleKind,
    pub severity: Severity,
    pub repo_full_name: String,
    pub file_repo_relative: String,
    pub span: LineSpan,
    pub evidence: String,
    pub extra: Option<String>,
}

/// One student's contribution to a finding, expressed as a share in
/// `[0.0, 1.0]`.
///
/// The field is named `student_id` (not `author_login`) because the
/// blame attribution tables in the schema all carry `students.id`
/// values — the resolver in `collect::identity_resolver` resolves any
/// upstream `(login, email)` to a `student_id` before the per-crate
/// attribution stages run, so the value the renderer sees here is
/// already a stable TrackDev identity.
///
/// `blame_share` is `f64` to match the SQL `weight REAL` column. A
/// narrower `f32` lost precision around the percent-rounding boundary
/// (e.g. 0.325 round-trips to 0.324999… and renders as "32%" instead
/// of "33%"), breaking byte-identical re-rendering after the W2.T5
/// renderer migration.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorAttribution {
    pub student_id: String,
    pub blame_share: f64,
}

impl AuthorAttribution {
    /// Constructs a new attribution; debug-asserts the share is in `[0.0, 1.0]`.
    pub fn new(student_id: impl Into<String>, blame_share: f64) -> Self {
        debug_assert!(
            (0.0..=1.0).contains(&blame_share),
            "blame_share must be in [0.0, 1.0]; got {blame_share}"
        );
        Self {
            student_id: student_id.into(),
            blame_share,
        }
    }
}

/// A finding plus the authors responsible, sorted descending by share.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributedFinding {
    pub finding: RuleFinding,
    pub attributions: Vec<AuthorAttribution>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_span_ordering_uses_start_then_end() {
        let a = LineSpan::single(10);
        let b = LineSpan::range(10, 20);
        let c = LineSpan::single(11);
        assert!(a < b, "single sorts before range starting at the same line");
        assert!(b < c, "10..=20 sorts before single(11)");
        let mut spans = vec![c, b, a];
        spans.sort();
        assert_eq!(spans, vec![a, b, c]);
    }

    #[test]
    fn line_span_line_count_is_inclusive() {
        assert_eq!(LineSpan::single(42).line_count(), 1);
        assert_eq!(LineSpan::range(42, 42).line_count(), 1);
        assert_eq!(LineSpan::range(42, 99).line_count(), 58);
    }

    #[test]
    fn author_attribution_accepts_boundary_shares() {
        let _ = AuthorAttribution::new("alice", 0.0);
        let _ = AuthorAttribution::new("alice", 1.0);
        let _ = AuthorAttribution::new("alice", 0.5);
    }

    #[test]
    #[should_panic(expected = "blame_share must be in [0.0, 1.0]")]
    fn author_attribution_rejects_negative_share() {
        let _ = AuthorAttribution::new("alice", -0.1);
    }

    #[test]
    #[should_panic(expected = "blame_share must be in [0.0, 1.0]")]
    fn author_attribution_rejects_share_above_one() {
        let _ = AuthorAttribution::new("alice", 1.01);
    }

    #[test]
    fn severity_display_uses_uppercase_constants() {
        assert_eq!(Severity::Critical.to_string(), "CRITICAL");
        assert_eq!(Severity::Warning.to_string(), "WARNING");
        assert_eq!(Severity::Info.to_string(), "INFO");
    }

    #[test]
    fn rule_kind_display_uses_pascal_case() {
        assert_eq!(RuleKind::Architecture.to_string(), "Architecture");
        assert_eq!(RuleKind::Complexity.to_string(), "Complexity");
        assert_eq!(RuleKind::StaticAnalysis.to_string(), "StaticAnalysis");
    }
}
