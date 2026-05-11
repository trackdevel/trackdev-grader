//! Unified attribution for `RuleFinding`s (W2.T4).
//!
//! All three scanners (architecture, complexity, static analysis)
//! produce findings of the same shape (`finding::RuleFinding`) and
//! persist per-student blame shares to a parallel `_attribution` table.
//! The unified renderer in W2.T5 consumes `AttributedFinding`s built
//! here, so the per-crate hand-rolled "finding + attribution" SELECTs in
//! `crates/report/src/markdown.rs` can collapse into one render path.
//!
//! `attribute()` is the pure packaging function — given a finding and
//! a pre-loaded vec of `AuthorAttribution`s, it sorts by descending
//! share, debug-asserts the shares sum to 1.0 (or the vec is empty),
//! and returns the packed `AttributedFinding`.
//!
//! `load_attributed_findings_for_repo()` is the bulk DB loader. It
//! issues one `JOIN` per `RuleKind` and returns every finding for a
//! repo with its full attribution list. Findings without a blame row
//! surface with an empty `attributions` vec; the renderer skips the
//! attribution row for those.

use rusqlite::{params, Connection};

use crate::finding::{
    AttributedFinding, AuthorAttribution, LineSpan, RuleFinding, RuleKind, Severity,
};

/// Package a `RuleFinding` + its `attributions` into an
/// `AttributedFinding`. Shares are sorted descending (so the renderer
/// emits the dominant author first); a debug-assertion catches genuine
/// double-counting (sum > 1 + ε) that would otherwise produce
/// nonsensical percentages. Empty attributions are returned unchanged —
/// the renderer treats them as "no blame information, skip the
/// attribution row".
///
/// **Note on the sum-to-1 invariant.** PLAN.md W2.T4 originally
/// specified "shares sum to 1.0 ± 0.001". Real `method_complexity_attribution`
/// rows violate that: when git blame can't resolve every line in the
/// offending range (deleted commits, binary merges, generated stubs),
/// the per-student shares sum to less than 1.0 and the renderer shows
/// the surviving partial shares verbatim ("50% of lines"). The
/// assertion is therefore one-sided: we only panic on `sum > 1 + ε`,
/// not on `sum < 1 - ε`.
pub fn attribute(
    finding: RuleFinding,
    mut attributions: Vec<AuthorAttribution>,
) -> AttributedFinding {
    attributions.sort_by(|a, b| {
        b.blame_share
            .partial_cmp(&a.blame_share)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Stable tiebreaker so two authors with identical shares
            // render in deterministic order across runs.
            .then_with(|| a.student_id.cmp(&b.student_id))
    });
    if !attributions.is_empty() {
        let total: f64 = attributions.iter().map(|a| a.blame_share).sum();
        debug_assert!(
            total <= 1.0 + 0.001,
            "attribution shares must not sum to more than 1.0 (got {total}) for finding {:?}; \
             a value above 1 indicates double-counted blame, an upstream bug",
            finding.rule_id
        );
    }
    AttributedFinding {
        finding,
        attributions,
    }
}

/// Read every `RuleFinding` of `kind` for `repo_full_name` together
/// with its per-student blame shares, returning one `AttributedFinding`
/// per finding (with `attributions == vec![]` when no blame row exists).
///
/// SQL: one `LEFT JOIN finding_table × attribution_table` per `kind`.
/// Rows are ordered by `(file_path, start_line, rule)` so re-runs
/// produce identical sort order across calls.
pub fn load_attributed_findings_for_repo(
    conn: &Connection,
    repo_full_name: &str,
    kind: RuleKind,
) -> rusqlite::Result<Vec<AttributedFinding>> {
    match kind {
        RuleKind::Architecture => load_architecture(conn, repo_full_name),
        RuleKind::Complexity => load_complexity(conn, repo_full_name),
        RuleKind::StaticAnalysis => load_static_analysis(conn, repo_full_name),
    }
}

fn load_architecture(
    conn: &Connection,
    repo_full_name: &str,
) -> rusqlite::Result<Vec<AttributedFinding>> {
    // architecture_violations has no `id` column — the renderer's
    // existing JOIN keys off implicit `rowid`. Mirror that here.
    let mut stmt = conn.prepare(
        "SELECT v.rowid, v.file_path, v.rule_name, v.offending_import, v.severity,
                v.start_line, v.end_line, COALESCE(v.explanation, ''),
                a.student_id, a.weight
         FROM architecture_violations v
         LEFT JOIN architecture_violation_attribution a ON a.violation_rowid = v.rowid
         WHERE v.repo_full_name = ?
         ORDER BY v.file_path, v.start_line, v.rule_name, v.offending_import,
                  v.rowid, a.weight DESC, a.student_id",
    )?;
    let rows = stmt.query_map(params![repo_full_name], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, Option<f64>>(9)?,
        ))
    })?;
    let mut acc: Vec<AttributedFinding> = Vec::new();
    let mut last_id: Option<i64> = None;
    for row in rows {
        let (id, file, rule_name, offending, severity_s, s_line, e_line, explanation, sid, w) =
            row?;
        if last_id != Some(id) {
            let span = build_span(s_line, e_line);
            let finding = RuleFinding {
                rule_id: rule_name,
                kind: RuleKind::Architecture,
                severity: parse_severity(&severity_s),
                repo_full_name: repo_full_name.to_string(),
                file_repo_relative: file,
                span,
                evidence: explanation,
                extra: Some(offending),
            };
            acc.push(AttributedFinding {
                finding,
                attributions: Vec::new(),
            });
            last_id = Some(id);
        }
        if let (Some(s), Some(weight)) = (sid, w) {
            // LEFT JOIN can produce a `NULL` attribution row when no
            // student claimed the violation; only push real rows.
            // Architecture's existing renderer drops weight <= 0 rows
            // upstream of the SELECT — keep parity here.
            if weight > 0.0 {
                let last = acc.last_mut().expect("just pushed");
                last.attributions.push(AuthorAttribution::new(s, weight));
            }
        }
    }
    Ok(acc
        .into_iter()
        .map(|af| attribute(af.finding, af.attributions))
        .collect())
}

fn load_complexity(
    conn: &Connection,
    repo_full_name: &str,
) -> rusqlite::Result<Vec<AttributedFinding>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.file_path, f.start_line, f.end_line, f.rule_key, f.severity,
                f.measured_value, f.threshold, COALESCE(f.detail, ''),
                a.student_id, a.weight
         FROM method_complexity_findings f
         LEFT JOIN method_complexity_attribution a ON a.finding_id = f.id
         WHERE f.repo_full_name = ?
         ORDER BY f.file_path, f.start_line, f.rule_key, f.id,
                  a.weight DESC, a.student_id",
    )?;
    let rows = stmt.query_map(params![repo_full_name], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<f64>>(6)?,
            r.get::<_, Option<f64>>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, Option<String>>(9)?,
            r.get::<_, Option<f64>>(10)?,
        ))
    })?;
    let mut acc: Vec<AttributedFinding> = Vec::new();
    let mut last_id: Option<i64> = None;
    for row in rows {
        let (id, file, s_line, e_line, rule_key, severity_s, measured, threshold, detail, sid, w) =
            row?;
        if last_id != Some(id) {
            let span = if e_line > s_line && s_line >= 1 {
                LineSpan::range(s_line as u32, e_line as u32)
            } else if s_line >= 1 {
                LineSpan::single(s_line as u32)
            } else {
                LineSpan::single(0)
            };
            let extra = match (measured, threshold) {
                (Some(m), Some(t)) => Some(format!("{m} > {t}")),
                _ => None,
            };
            let finding = RuleFinding {
                rule_id: rule_key,
                kind: RuleKind::Complexity,
                severity: parse_severity(&severity_s),
                repo_full_name: repo_full_name.to_string(),
                file_repo_relative: file,
                span,
                evidence: detail,
                extra,
            };
            acc.push(AttributedFinding {
                finding,
                attributions: Vec::new(),
            });
            last_id = Some(id);
        }
        if let (Some(s), Some(weight)) = (sid, w) {
            if weight > 0.0 {
                let last = acc.last_mut().expect("just pushed");
                last.attributions.push(AuthorAttribution::new(s, weight));
            }
        }
    }
    Ok(acc
        .into_iter()
        .map(|af| attribute(af.finding, af.attributions))
        .collect())
}

fn load_static_analysis(
    conn: &Connection,
    repo_full_name: &str,
) -> rusqlite::Result<Vec<AttributedFinding>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.file_path, f.analyzer, f.rule_id, f.severity,
                f.start_line, f.end_line, f.message,
                a.student_id, a.weight
         FROM static_analysis_findings f
         LEFT JOIN static_analysis_finding_attribution a ON a.finding_id = f.id
         WHERE f.repo_full_name = ?
         ORDER BY f.file_path, f.start_line, f.analyzer, f.rule_id, f.id,
                  a.weight DESC, a.student_id",
    )?;
    let rows = stmt.query_map(params![repo_full_name], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, Option<f64>>(9)?,
        ))
    })?;
    let mut acc: Vec<AttributedFinding> = Vec::new();
    let mut last_id: Option<i64> = None;
    for row in rows {
        let (id, file, analyzer, rule_id, severity_s, s_line, e_line, message, sid, w) = row?;
        if last_id != Some(id) {
            let span = match (s_line, e_line) {
                (Some(s), Some(e)) if e > s && s >= 1 && e >= 1 => {
                    LineSpan::range(s as u32, e as u32)
                }
                (Some(s), _) if s >= 1 => LineSpan::single(s as u32),
                _ => LineSpan::single(0),
            };
            let finding = RuleFinding {
                rule_id: format!("{analyzer}:{rule_id}"),
                kind: RuleKind::StaticAnalysis,
                severity: parse_severity(&severity_s),
                repo_full_name: repo_full_name.to_string(),
                file_repo_relative: file,
                span,
                evidence: message,
                extra: None,
            };
            acc.push(AttributedFinding {
                finding,
                attributions: Vec::new(),
            });
            last_id = Some(id);
        }
        if let (Some(s), Some(weight)) = (sid, w) {
            if weight > 0.0 {
                let last = acc.last_mut().expect("just pushed");
                last.attributions.push(AuthorAttribution::new(s, weight));
            }
        }
    }
    Ok(acc
        .into_iter()
        .map(|af| attribute(af.finding, af.attributions))
        .collect())
}

fn build_span(start: Option<i64>, end: Option<i64>) -> LineSpan {
    match (start, end) {
        (Some(s), Some(e)) if e > s && s >= 1 && e >= 1 => LineSpan::range(s as u32, e as u32),
        (Some(s), _) if s >= 1 => LineSpan::single(s as u32),
        _ => LineSpan::single(0),
    }
}

fn parse_severity(s: &str) -> Severity {
    match s.to_ascii_uppercase().as_str() {
        "CRITICAL" | "ERROR" => Severity::Critical,
        "INFO" | "INFORMATIONAL" | "NOTICE" => Severity::Info,
        _ => Severity::Warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::apply_schema;

    fn fixture_finding(rule_id: &str) -> RuleFinding {
        RuleFinding {
            rule_id: rule_id.to_string(),
            kind: RuleKind::Architecture,
            severity: Severity::Warning,
            repo_full_name: "o/r".to_string(),
            file_repo_relative: "Foo.java".to_string(),
            span: LineSpan::range(42, 99),
            evidence: String::new(),
            extra: None,
        }
    }

    #[test]
    fn sole_author_carries_full_share() {
        // Acceptance criteria (a): one author with 1.0 share.
        let af = attribute(
            fixture_finding("rule"),
            vec![AuthorAttribution::new("alice", 1.0)],
        );
        assert_eq!(af.attributions.len(), 1);
        assert_eq!(af.attributions[0].student_id, "alice");
        assert!((af.attributions[0].blame_share - 1.0).abs() < 1e-6);
    }

    #[test]
    fn two_author_split_sorts_dominant_first() {
        // Acceptance criteria (b): 50/50 split. Stable tie-break is by
        // student_id so the dominant author is alice (alphabetic) when
        // shares match; here we make bob explicitly larger so the
        // ordering is unambiguous.
        let af = attribute(
            fixture_finding("rule"),
            vec![
                AuthorAttribution::new("alice", 0.4),
                AuthorAttribution::new("bob", 0.6),
            ],
        );
        assert_eq!(af.attributions[0].student_id, "bob");
        assert_eq!(af.attributions[1].student_id, "alice");
    }

    #[test]
    fn tied_shares_use_student_id_as_stable_tiebreaker() {
        let af = attribute(
            fixture_finding("rule"),
            vec![
                AuthorAttribution::new("bob", 0.5),
                AuthorAttribution::new("alice", 0.5),
            ],
        );
        assert_eq!(af.attributions[0].student_id, "alice");
        assert_eq!(af.attributions[1].student_id, "bob");
    }

    #[test]
    fn range_with_non_uniform_line_counts_preserves_caller_shares() {
        // Acceptance criteria (c): a range spanning two authors with
        // 7 vs 51 lines yields shares 7/58 ≈ 0.12 and 51/58 ≈ 0.88,
        // pre-computed by the scanner's blame step. attribute() must
        // not re-normalise.
        let af = attribute(
            fixture_finding("rule"),
            vec![
                AuthorAttribution::new("alice", 7.0 / 58.0),
                AuthorAttribution::new("bob", 51.0 / 58.0),
            ],
        );
        assert_eq!(af.attributions[0].student_id, "bob");
        assert!((af.attributions[0].blame_share - 51.0 / 58.0).abs() < 1e-6);
    }

    #[test]
    fn empty_attributions_pass_through_unchanged() {
        let af = attribute(fixture_finding("rule"), Vec::new());
        assert!(af.attributions.is_empty());
    }

    #[test]
    fn partial_blame_summing_below_one_is_accepted() {
        // method_complexity_attribution legitimately produces partial
        // sums when git blame can't cover the entire offending range
        // (deleted commits, binary merges, generated stubs). The
        // renderer prints the surviving shares as-is.
        let af = attribute(
            fixture_finding("rule"),
            vec![
                AuthorAttribution::new("alice", 0.3),
                AuthorAttribution::new("bob", 0.2),
            ],
        );
        assert_eq!(af.attributions.len(), 2);
        let sum: f64 = af.attributions.iter().map(|a| a.blame_share).sum();
        assert!((sum - 0.5).abs() < 1e-6, "partial sum passes through");
    }

    #[test]
    #[should_panic(expected = "attribution shares must not sum to more than 1.0")]
    fn debug_asserts_when_shares_exceed_one() {
        // sum > 1 is the genuinely-broken case (double-counted blame),
        // and *that* is what the debug-assert catches.
        let _ = attribute(
            fixture_finding("rule"),
            vec![
                AuthorAttribution::new("alice", 0.7),
                AuthorAttribution::new("bob", 0.5),
            ],
        );
    }

    fn mk_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn bulk_loader_dispatches_by_kind_for_architecture() {
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, file_path, rule_name, violation_kind,
                 offending_import, severity, start_line, end_line, rule_kind, explanation)
             VALUES
                ('o/r', 'A.java', 'VALIDATION_IN_UI', 'ast_forbidden_method_call',
                 'A::validate', 'WARNING', 42, 99, 'ast_forbidden_method_call',
                 'Validation belongs in service.');
             INSERT INTO architecture_violation_attribution
                (violation_rowid, student_id, lines_authored, total_lines, weight)
             SELECT rowid, 'alice', 58, 58, 1.0 FROM architecture_violations
             WHERE rule_name = 'VALIDATION_IN_UI';",
        )
        .unwrap();
        let afs = load_attributed_findings_for_repo(&conn, "o/r", RuleKind::Architecture).unwrap();
        assert_eq!(afs.len(), 1);
        assert_eq!(afs[0].finding.kind, RuleKind::Architecture);
        assert_eq!(afs[0].finding.rule_id, "VALIDATION_IN_UI");
        assert_eq!(afs[0].attributions.len(), 1);
        assert_eq!(afs[0].attributions[0].student_id, "alice");
        assert!((afs[0].attributions[0].blame_share - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bulk_loader_dispatches_by_kind_for_complexity() {
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO method_complexity_findings
                (project_id, repo_full_name, file_path, class_name, method_name,
                 start_line, end_line, rule_key, severity,
                 measured_value, threshold, detail)
             VALUES
                (1, 'o/r', 'Login.java', 'LoginController', 'authenticate',
                 42, 99, 'wide-signature', 'WARNING',
                 12.0, 10.0, 'Method takes more parameters than the ceiling allows.');
             INSERT INTO method_complexity_attribution
                (finding_id, student_id, lines_attributed, weighted_lines, weight)
             VALUES (last_insert_rowid(), 'bob', 58, 58.0, 1.0);",
        )
        .unwrap();
        let afs = load_attributed_findings_for_repo(&conn, "o/r", RuleKind::Complexity).unwrap();
        assert_eq!(afs.len(), 1);
        assert_eq!(afs[0].finding.kind, RuleKind::Complexity);
        assert_eq!(afs[0].finding.rule_id, "wide-signature");
        assert_eq!(afs[0].finding.extra.as_deref(), Some("12 > 10"));
        assert_eq!(afs[0].attributions[0].student_id, "bob");
    }

    #[test]
    fn bulk_loader_dispatches_by_kind_for_static_analysis() {
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO static_analysis_findings
                (repo_full_name, analyzer, rule_id, severity, file_path,
                 start_line, end_line, message, fingerprint)
             VALUES
                ('o/r', 'pmd', 'UnusedPrivateMethod', 'INFO',
                 'src/main/java/A.java', 42, 99, 'Avoid unused.', 'fp1');
             INSERT INTO static_analysis_finding_attribution
                (finding_id, student_id, lines_authored, total_lines, weight)
             VALUES (last_insert_rowid(), 'carol', 58, 58, 1.0);",
        )
        .unwrap();
        let afs =
            load_attributed_findings_for_repo(&conn, "o/r", RuleKind::StaticAnalysis).unwrap();
        assert_eq!(afs.len(), 1);
        assert_eq!(afs[0].finding.kind, RuleKind::StaticAnalysis);
        assert_eq!(afs[0].finding.rule_id, "pmd:UnusedPrivateMethod");
        assert_eq!(afs[0].attributions[0].student_id, "carol");
    }

    #[test]
    fn unattributed_finding_surfaces_with_empty_attributions() {
        // Generated/copy-pasted file with no blame map: the finding is
        // still rendered (so the team sees the issue) but the
        // attribution row is suppressed.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, file_path, rule_name, violation_kind,
                 offending_import, severity, start_line, end_line, rule_kind)
             VALUES
                ('o/r', 'Gen.java', 'r', 'layer_dependency', 'com.x', 'INFO', 1, 1,
                 'layer_dependency');",
        )
        .unwrap();
        let afs = load_attributed_findings_for_repo(&conn, "o/r", RuleKind::Architecture).unwrap();
        assert_eq!(afs.len(), 1);
        assert!(afs[0].attributions.is_empty());
    }

    #[test]
    fn bulk_loader_groups_attributions_by_finding() {
        // Two authors share a single architecture violation; the
        // loader collapses them into one AttributedFinding with two
        // entries, sorted descending by blame share.
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, file_path, rule_name, violation_kind,
                 offending_import, severity, start_line, end_line, rule_kind)
             VALUES
                ('o/r', 'A.java', 'r', 'layer_dependency', 'com.x', 'WARNING', 1, 30,
                 'layer_dependency');
             INSERT INTO architecture_violation_attribution
                (violation_rowid, student_id, lines_authored, total_lines, weight)
             SELECT rowid, 'alice', 22, 30, 0.7333 FROM architecture_violations;
             INSERT INTO architecture_violation_attribution
                (violation_rowid, student_id, lines_authored, total_lines, weight)
             SELECT rowid, 'bob', 8, 30, 0.2667 FROM architecture_violations;",
        )
        .unwrap();
        let afs = load_attributed_findings_for_repo(&conn, "o/r", RuleKind::Architecture).unwrap();
        assert_eq!(afs.len(), 1);
        assert_eq!(afs[0].attributions.len(), 2);
        assert_eq!(afs[0].attributions[0].student_id, "alice");
        assert_eq!(afs[0].attributions[1].student_id, "bob");
    }

    #[test]
    fn bulk_loader_scopes_by_repo_full_name() {
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO architecture_violations
                (repo_full_name, file_path, rule_name, violation_kind,
                 offending_import, severity, start_line, end_line, rule_kind)
             VALUES
                ('o/r', 'A.java', 'r', 'layer_dependency', 'com.x', 'WARNING', 1, 1,
                 'layer_dependency'),
                ('o/other', 'A.java', 'r', 'layer_dependency', 'com.x', 'WARNING', 1, 1,
                 'layer_dependency');",
        )
        .unwrap();
        let afs = load_attributed_findings_for_repo(&conn, "o/r", RuleKind::Architecture).unwrap();
        assert_eq!(afs.len(), 1, "must not bleed from sibling repo");
    }
}
