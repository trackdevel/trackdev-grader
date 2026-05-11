//! Stage 5 (quality) — method-level AST metrics + SATD + sprint-over-sprint deltas.

pub mod complexity;
pub mod halstead;
pub mod i18n;
pub mod quality_delta;
pub mod satd;
pub mod testability;

pub use complexity::{analyze_file, analyze_method, MethodMetrics};
pub use halstead::{compute_halstead, maintainability_index, HalsteadMetrics};
pub use quality_delta::compute_all_quality;
pub use satd::{compute_satd_for_repo, satd_delta, scan_comments};

use rusqlite::{params, Connection};
use sprint_grader_core::finding::{LineSpan, RuleFinding, RuleKind, Severity};

/// W2.T2: read every `method_complexity_findings` row for `repo_full_name`
/// and convert it into a shared `RuleFinding`. The renderer unification
/// in W2.T5 will consume this in place of the per-crate
/// `ComplexityFinding` SELECT inlined in
/// `crates/report/src/markdown.rs`.
///
/// Path safety: the complexity stage stores repo-relative POSIX paths
/// in `file_path` (see `testability::discover_main_java_files`), so the
/// value passes through `RuleFinding::file_repo_relative` unchanged.
pub fn load_rule_findings_for_repo(
    conn: &Connection,
    repo_full_name: &str,
) -> rusqlite::Result<Vec<RuleFinding>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, start_line, end_line, rule_key, severity,
                measured_value, threshold, COALESCE(detail, '')
         FROM method_complexity_findings
         WHERE repo_full_name = ?
         ORDER BY file_path, start_line, rule_key",
    )?;
    let rows = stmt.query_map(params![repo_full_name], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<f64>>(5)?,
            r.get::<_, Option<f64>>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (file, s_line, e_line, rule_key, severity_s, measured, threshold, detail) = row?;
        let span = if e_line > s_line && s_line >= 1 {
            LineSpan::range(s_line as u32, e_line as u32)
        } else if s_line >= 1 {
            LineSpan::single(s_line as u32)
        } else {
            LineSpan::single(0)
        };
        let severity = match severity_s.to_ascii_uppercase().as_str() {
            "CRITICAL" | "ERROR" => Severity::Critical,
            "INFO" | "INFORMATIONAL" | "NOTICE" => Severity::Info,
            _ => Severity::Warning,
        };
        let extra = match (measured, threshold) {
            (Some(m), Some(t)) => Some(format!("{m} > {t}")),
            _ => None,
        };
        out.push(RuleFinding {
            rule_id: rule_key,
            kind: RuleKind::Complexity,
            severity,
            repo_full_name: repo_full_name.to_string(),
            file_repo_relative: file,
            span,
            evidence: detail,
            extra,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod lib_tests {
    use super::*;
    use sprint_grader_core::db::apply_schema;

    #[test]
    fn load_complexity_rule_findings_round_trips_through_db() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO method_complexity_findings
                (project_id, repo_full_name, file_path, class_name, method_name,
                 start_line, end_line, rule_key, severity,
                 measured_value, threshold, detail)
             VALUES
                (1, 'udg/spring-x', 'src/main/java/Login.java', 'LoginController',
                 'authenticate', 42, 99, 'wide-signature', 'WARNING',
                 12.0, 10.0, 'Method takes more parameters than the ceiling allows.'),
                (1, 'udg/spring-other', 'src/main/java/Z.java', 'Z',
                 'm', 1, 1, 'cyclomatic', 'CRITICAL', NULL, NULL, '');",
        )
        .unwrap();
        let findings = load_rule_findings_for_repo(&conn, "udg/spring-x").unwrap();
        assert_eq!(findings.len(), 1, "must scope by repo_full_name");
        let f = &findings[0];
        assert_eq!(f.kind, RuleKind::Complexity);
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.rule_id, "wide-signature");
        assert_eq!(f.file_repo_relative, "src/main/java/Login.java");
        assert_eq!(f.span, LineSpan::range(42, 99));
        assert_eq!(
            f.evidence,
            "Method takes more parameters than the ceiling allows."
        );
        assert_eq!(f.extra.as_deref(), Some("12 > 10"));
    }
}
