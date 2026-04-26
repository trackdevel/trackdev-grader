//! Pitest mutation-report XML parser (T-P2.4).
//!
//! Pitest writes a `mutations.xml` like:
//!
//! ```xml
//! <mutations>
//!   <mutation detected='true' status='KILLED' numberOfTestsRun='1'>
//!     <sourceFile>Foo.java</sourceFile>
//!     <mutatedClass>com.x.Foo</mutatedClass>
//!     <lineNumber>42</lineNumber>
//!   </mutation>
//!   <mutation detected='false' status='SURVIVED' numberOfTestsRun='1'>
//!     ...
//!   </mutation>
//! </mutations>
//! ```
//!
//! We need the `KILLED / total` ratio. Statuses Pitest uses:
//! `KILLED`, `SURVIVED`, `NO_COVERAGE`, `TIMED_OUT`, `MEMORY_ERROR`,
//! `RUN_ERROR`, `NON_VIABLE`. Conventionally the score is
//! `(killed + timed_out) / (total - non_viable)` — non-viable mutants
//! never compile so they don't count for or against the team.
//!
//! Implementation: a tiny tag scanner over the file contents — no XML
//! crate needed. Pitest writes a flat per-mutation list with stable
//! attribute ordering, so a `regex`-style match against
//! `<mutation ...>` openers is robust.

use std::path::Path;

use regex::Regex;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PitestSummary {
    pub mutants_total: u64,
    pub mutants_killed: u64,
    /// Sum of NON_VIABLE mutants — excluded from the denominator
    /// because they couldn't be compiled and therefore tell us nothing
    /// about test quality.
    pub non_viable: u64,
}

impl PitestSummary {
    /// `(killed) / (total − non_viable)` clamped to `[0, 1]`. Returns
    /// `None` when there's no scoreable mutant (empty report or every
    /// mutant non-viable) — the caller decides how to surface that.
    pub fn score(&self) -> Option<f64> {
        let denom = self.mutants_total.saturating_sub(self.non_viable);
        if denom == 0 {
            return None;
        }
        Some(self.mutants_killed as f64 / denom as f64)
    }
}

pub fn parse_pitest_xml_str(xml: &str) -> PitestSummary {
    // Match the opening `<mutation ...>` tag including its attributes.
    // Attribute order is "detected" then "status"; we tolerate either
    // order and arbitrary intervening whitespace.
    let opener = Regex::new(r#"(?i)<mutation\b([^>]*)>"#).unwrap();
    let attr = Regex::new(r#"(?i)\b(\w+)\s*=\s*['"]([^'"]*)['"]"#).unwrap();

    let mut summary = PitestSummary::default();
    for caps in opener.captures_iter(xml) {
        let attrs = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        summary.mutants_total += 1;
        let mut status: Option<String> = None;
        for ac in attr.captures_iter(attrs) {
            let key = ac.get(1).map(|m| m.as_str().to_ascii_lowercase());
            let val = ac.get(2).map(|m| m.as_str().to_ascii_uppercase());
            if key.as_deref() == Some("status") {
                status = val;
            }
        }
        match status.as_deref() {
            Some("KILLED") => summary.mutants_killed += 1,
            Some("NON_VIABLE") => summary.non_viable += 1,
            _ => {}
        }
    }
    summary
}

pub fn parse_pitest_xml(path: &Path) -> std::io::Result<PitestSummary> {
    let text = std::fs::read_to_string(path)?;
    Ok(parse_pitest_xml_str(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden fixture: the same shape Pitest's `scmMutationCoverage`
    /// emits. Tests cover the four status branches we count, plus the
    /// non-viable exclusion in the denominator.
    const PITEST_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<mutations>
  <mutation detected='true' status='KILLED' numberOfTestsRun='3'>
    <sourceFile>Foo.java</sourceFile>
    <mutatedClass>com.x.Foo</mutatedClass>
    <lineNumber>10</lineNumber>
  </mutation>
  <mutation detected='true' status='KILLED' numberOfTestsRun='1'>
    <sourceFile>Foo.java</sourceFile>
    <mutatedClass>com.x.Foo</mutatedClass>
    <lineNumber>11</lineNumber>
  </mutation>
  <mutation detected='false' status='SURVIVED' numberOfTestsRun='1'>
    <sourceFile>Foo.java</sourceFile>
    <mutatedClass>com.x.Foo</mutatedClass>
    <lineNumber>12</lineNumber>
  </mutation>
  <mutation detected='false' status='NO_COVERAGE' numberOfTestsRun='0'>
    <sourceFile>Bar.java</sourceFile>
    <mutatedClass>com.x.Bar</mutatedClass>
    <lineNumber>20</lineNumber>
  </mutation>
  <mutation detected='false' status='NON_VIABLE' numberOfTestsRun='0'>
    <sourceFile>Bar.java</sourceFile>
    <mutatedClass>com.x.Bar</mutatedClass>
    <lineNumber>21</lineNumber>
  </mutation>
</mutations>
"#;

    #[test]
    fn parses_status_counts_from_pitest_fixture() {
        let s = parse_pitest_xml_str(PITEST_XML);
        assert_eq!(s.mutants_total, 5);
        assert_eq!(s.mutants_killed, 2);
        assert_eq!(s.non_viable, 1);
    }

    #[test]
    fn score_excludes_non_viable_from_denominator() {
        // killed=2, total=5, non_viable=1 → denom=4 → 2/4 = 0.50.
        let s = parse_pitest_xml_str(PITEST_XML);
        assert_eq!(s.score(), Some(0.5));
    }

    #[test]
    fn score_is_none_when_all_mutants_non_viable() {
        let xml = r#"<mutations>
  <mutation status='NON_VIABLE'></mutation>
  <mutation status='NON_VIABLE'></mutation>
</mutations>"#;
        let s = parse_pitest_xml_str(xml);
        assert_eq!(s.mutants_total, 2);
        assert_eq!(s.non_viable, 2);
        assert_eq!(s.score(), None);
    }

    #[test]
    fn score_is_none_for_empty_report() {
        let s = parse_pitest_xml_str("<mutations></mutations>");
        assert_eq!(s.mutants_total, 0);
        assert_eq!(s.score(), None);
    }

    #[test]
    fn tolerates_double_quoted_attributes() {
        let xml = r#"<mutations>
  <mutation detected="true" status="KILLED"></mutation>
  <mutation detected="false" status="SURVIVED"></mutation>
</mutations>"#;
        let s = parse_pitest_xml_str(xml);
        assert_eq!(s.mutants_total, 2);
        assert_eq!(s.mutants_killed, 1);
    }

    #[test]
    fn ignores_attribute_order() {
        let xml = r#"<mutations>
  <mutation status='KILLED' detected='true' numberOfTestsRun='1'></mutation>
</mutations>"#;
        let s = parse_pitest_xml_str(xml);
        assert_eq!(s.mutants_killed, 1);
    }
}
