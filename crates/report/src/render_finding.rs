//! Unified bullet renderer for `AttributedFinding`s (W2.T5).
//!
//! All three scanners (architecture, complexity, static analysis)
//! emit `RuleFinding`s of the same shape; this module collapses their
//! three previously-separate inline bullet emitters into a single
//! entry point. The PER-KIND prose, severity-tag formatting and child
//! bullets differ — `render_attributed_finding` dispatches on
//! `finding.kind` and delegates to the matching helper. The output is
//! byte-identical to what `markdown.rs::write_student_*_block`
//! produced before the refactor; the snapshot test in
//! `crates/report/tests/snapshots.rs` is the regression gate.
//!
//! The kind-specific differences this module preserves:
//!
//! | Kind | Label | Prose source | Severity tag | Trailer | Child bullet |
//! |---|---|---|---|---|---|
//! | Architecture | `` `<basename>` `` | `humanize_rule_name(rule_id)` | `_(<severity>)_` | — | optional `evidence` |
//! | Complexity | `` `<class>.<method>()` `` (caller supplies) | `quality::i18n::rule_prose(rule_id)` | `_(<severity>)_` | `({measured} > {threshold})` from `extra` | — |
//! | StaticAnalysis | `` `<basename>` `` | `` `<analyzer:rule>` `` (i.e. `finding.rule_id`) | `· _<severity>_` | `— {message}` from `evidence` | — |
//!
//! `attribution` is `Option<&AuthorAttribution>` so the same renderer
//! can serve both the per-student bullets (where exactly one author
//! is shown per line) and unattributed findings (no `· N% of lines`
//! suffix).

use sprint_grader_core::finding::{AuthorAttribution, LineSpan, RuleFinding, RuleKind};

use crate::markdown::{
    format_blame_weight_suffix, humanize_rule_name, md_escape, round_to_int_if_integer,
};
use crate::url::{github_blob_url, line_anchor, line_suffix};

/// Single entry point for rendering one finding bullet — replaces the
/// three inline bullet emitters in `markdown.rs::write_student_*_block`.
///
/// `label_override` lets complexity callers pass a `` `Class.method()` ``
/// label; architecture and static-analysis callers pass `None` and the
/// renderer falls back to `` `<basename>` ``.
pub fn render_attributed_finding(
    finding: &RuleFinding,
    repo_full_name: &str,
    label_override: Option<&str>,
    attribution: Option<&AuthorAttribution>,
) -> String {
    match finding.kind {
        RuleKind::Architecture => {
            render_architecture_bullet(finding, repo_full_name, label_override, attribution)
        }
        RuleKind::Complexity => {
            render_complexity_bullet(finding, repo_full_name, label_override, attribution)
        }
        RuleKind::StaticAnalysis => {
            render_static_analysis_bullet(finding, repo_full_name, label_override, attribution)
        }
    }
}

fn render_architecture_bullet(
    finding: &RuleFinding,
    repo_full_name: &str,
    label_override: Option<&str>,
    attribution: Option<&AuthorAttribution>,
) -> String {
    let file_cell = file_cell_with_title(
        finding,
        repo_full_name,
        label_override.unwrap_or(&default_basename_label(&finding.file_repo_relative)),
    );
    let weight_suffix = weight_suffix_for(attribution);
    let mut out = format!(
        "- {} — {} _({})_{}\n",
        file_cell,
        humanize_rule_name(&finding.rule_id),
        severity_lower(finding),
        weight_suffix,
    );
    // Optional LLM-supplied explanation renders as a child bullet so
    // the reader sees the *why*, not just the tag.
    let prose = finding.evidence.trim();
    if !prose.is_empty() {
        out.push_str(&format!("  - {}\n", md_escape(prose)));
    }
    out
}

fn render_complexity_bullet(
    finding: &RuleFinding,
    repo_full_name: &str,
    label_override: Option<&str>,
    attribution: Option<&AuthorAttribution>,
) -> String {
    // Complexity always carries a `Class.method()` label supplied by
    // the caller (the RuleFinding alone doesn't know the method name).
    // Fall back to the basename if the caller didn't override so the
    // renderer stays usable in isolation (e.g. golden-file tests).
    let label_owned;
    let label = match label_override {
        Some(s) => s,
        None => {
            label_owned = default_basename_label(&finding.file_repo_relative);
            &label_owned
        }
    };
    let file_cell = file_cell_with_title(finding, repo_full_name, label);

    let prose = sprint_grader_quality::i18n::rule_prose(&finding.rule_id);
    let measured_tail = match finding.extra.as_deref() {
        Some(s) => format_measured_tail(s),
        None => String::new(),
    };
    let weight_suffix = weight_suffix_for(attribution);
    format!(
        "- {} — {} _({})_{}{}\n",
        file_cell,
        prose,
        severity_lower(finding),
        measured_tail,
        weight_suffix,
    )
}

fn render_static_analysis_bullet(
    finding: &RuleFinding,
    repo_full_name: &str,
    label_override: Option<&str>,
    attribution: Option<&AuthorAttribution>,
) -> String {
    let file_cell = file_cell_no_title(
        finding,
        repo_full_name,
        label_override.unwrap_or(&default_basename_label(&finding.file_repo_relative)),
    );
    let weight_suffix = weight_suffix_for(attribution);
    // First line of the message only — multi-line PMD/Checkstyle
    // messages would otherwise blow out the markdown rendering.
    let first_line = finding.evidence.lines().next().unwrap_or("");
    format!(
        "- {} — `{}` · _{}_ — {}{}\n",
        file_cell,
        finding.rule_id,
        severity_lower(finding),
        md_escape(first_line),
        weight_suffix,
    )
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn default_basename_label(file_repo_relative: &str) -> String {
    let basename = file_repo_relative
        .rsplit('/')
        .next()
        .unwrap_or(file_repo_relative);
    format!("`{}`", basename)
}

/// `[<label> :L42-L99](url#L42-L99 "full/path/to/file.java")`. Falls
/// back to `<label> :L42-L99` (no link) when the repo identifier isn't
/// in `<org>/<repo>` form.
fn file_cell_with_title(finding: &RuleFinding, repo_full_name: &str, label: &str) -> String {
    let span = bullet_span(&finding.span);
    let suffix = line_suffix(span);
    let anchor = line_anchor(span);
    let url = github_blob_url(repo_full_name, "HEAD", &finding.file_repo_relative, None);
    if url.is_empty() {
        format!("{}{}", label, suffix)
    } else {
        format!(
            "[{}{}]({}{} \"{}\")",
            label,
            suffix,
            url,
            anchor,
            md_escape(&finding.file_repo_relative),
        )
    }
}

/// `[<label> :L42-L99](url#L42-L99)` — no title attribute. Static
/// analysis's existing bullet shape omits the hover title because the
/// message column already carries the full prose.
fn file_cell_no_title(finding: &RuleFinding, repo_full_name: &str, label: &str) -> String {
    let span = bullet_span(&finding.span);
    let suffix = line_suffix(span);
    let anchor = line_anchor(span);
    let url = github_blob_url(repo_full_name, "HEAD", &finding.file_repo_relative, None);
    if url.is_empty() {
        format!("{}{}", label, suffix)
    } else {
        format!("[{}{}]({}{})", label, suffix, url, anchor)
    }
}

/// `Some` when `span.start >= 1` so we never emit `#L0` anchors for
/// findings whose line range was unknown. The existing renderer's
/// helpers (`line_suffix`, `line_anchor`) already short-circuit on
/// `None`; mirror their contract here so the converted output is
/// byte-identical.
fn bullet_span(span: &LineSpan) -> Option<LineSpan> {
    if span.start == 0 {
        None
    } else {
        Some(*span)
    }
}

fn weight_suffix_for(attribution: Option<&AuthorAttribution>) -> String {
    match attribution {
        Some(a) => format_blame_weight_suffix(a.blame_share),
        None => String::new(),
    }
}

fn severity_lower(finding: &RuleFinding) -> String {
    finding.severity.to_string().to_lowercase()
}

/// Format a complexity overflow string. The RuleFinding's `extra` is
/// the string `"12 > 10"` already (built by
/// `quality::testability::Finding::into_rule_finding`), but the
/// existing renderer applies `round_to_int_if_integer` to each side so
/// `12.0 > 10.0` collapses to `12 > 10`. Re-parse the two halves to
/// preserve that rendering.
fn format_measured_tail(extra: &str) -> String {
    let mut parts = extra.split(" > ");
    match (parts.next(), parts.next()) {
        (Some(m), Some(t)) => {
            let m = m.parse::<f64>().ok().map(round_to_int_if_integer);
            let t = t.parse::<f64>().ok().map(round_to_int_if_integer);
            match (m, t) {
                (Some(m), Some(t)) => format!(" ({} > {})", m, t),
                _ => format!(" ({})", extra),
            }
        }
        _ => format!(" ({})", extra),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_grader_core::finding::{LineSpan, RuleFinding, RuleKind, Severity};

    fn af(kind: RuleKind, rule_id: &str, evidence: &str, extra: Option<&str>) -> RuleFinding {
        RuleFinding {
            rule_id: rule_id.to_string(),
            kind,
            severity: Severity::Warning,
            repo_full_name: "udg/spring-x".to_string(),
            file_repo_relative: "src/main/java/Foo.java".to_string(),
            span: LineSpan::range(42, 99),
            evidence: evidence.to_string(),
            extra: extra.map(|s| s.to_string()),
        }
    }

    #[test]
    fn architecture_bullet_matches_legacy_shape() {
        // Matches the line emitted by the old write_student_architecture_block.
        let f = af(RuleKind::Architecture, "VALIDATION_IN_UI", "", None);
        let bullet = render_attributed_finding(
            &f,
            "udg/spring-x",
            None,
            Some(&AuthorAttribution::new("alice", 1.0)),
        );
        assert!(
            bullet.contains("[`Foo.java` :L42-L99]"),
            "label + line suffix: {bullet}"
        );
        assert!(
            bullet.contains(
                "https://github.com/udg/spring-x/blob/HEAD/src/main/java/Foo.java#L42-L99"
            ),
            "url with anchor: {bullet}"
        );
        assert!(
            bullet.contains("\"src/main/java/Foo.java\""),
            "title attribute carries full path: {bullet}"
        );
        // humanize_rule_name turns "VALIDATION_IN_UI" → "Validation in ui"
        // (the prose-key form); the snapshot test pins the exact text.
        assert!(
            bullet.contains(" — "),
            "em-dash separator before prose: {bullet}"
        );
        assert!(
            bullet.contains(" _(warning)_"),
            "severity in italic parens: {bullet}"
        );
        assert!(
            bullet.contains(" · 100% of lines"),
            "weight suffix: {bullet}"
        );
    }

    #[test]
    fn architecture_bullet_emits_child_evidence_when_present() {
        let f = af(
            RuleKind::Architecture,
            "x",
            "Validation belongs in service layer.",
            None,
        );
        let bullet = render_attributed_finding(
            &f,
            "udg/spring-x",
            None,
            Some(&AuthorAttribution::new("alice", 1.0)),
        );
        assert!(
            bullet.contains("  - Validation belongs in service layer."),
            "explanation surfaces as nested bullet: {bullet}"
        );
    }

    #[test]
    fn architecture_bullet_skips_empty_evidence() {
        let f = af(RuleKind::Architecture, "x", "   ", None);
        let bullet = render_attributed_finding(
            &f,
            "udg/spring-x",
            None,
            Some(&AuthorAttribution::new("alice", 1.0)),
        );
        assert!(
            !bullet.contains("  - "),
            "whitespace-only evidence must not emit a child bullet: {bullet}"
        );
    }

    #[test]
    fn complexity_bullet_uses_label_override_and_measured_tail() {
        let f = af(
            RuleKind::Complexity,
            "wide-signature",
            "ignored",
            Some("12 > 10"),
        );
        let bullet = render_attributed_finding(
            &f,
            "udg/spring-x",
            Some("`LoginController.authenticate()`"),
            Some(&AuthorAttribution::new("alice", 1.0)),
        );
        assert!(
            bullet.contains("[`LoginController.authenticate()` :L42-L99]"),
            "label override used verbatim: {bullet}"
        );
        assert!(
            bullet.contains(" (12 > 10)"),
            "measured > threshold tail: {bullet}"
        );
        assert!(
            bullet.contains(" · 100% of lines"),
            "weight suffix preserved: {bullet}"
        );
    }

    #[test]
    fn static_analysis_bullet_uses_namespaced_rule_id_and_no_title() {
        let f = af(
            RuleKind::StaticAnalysis,
            "pmd:UnusedPrivateMethod",
            "Avoid unused private methods such as 'helper()'.",
            None,
        );
        let bullet = render_attributed_finding(
            &f,
            "udg/spring-x",
            None,
            Some(&AuthorAttribution::new("alice", 1.0)),
        );
        assert!(
            bullet.contains("[`Foo.java` :L42-L99]"),
            "basename label: {bullet}"
        );
        assert!(
            !bullet.contains("\"src/main/java/Foo.java\""),
            "static-analysis bullet has no title attribute: {bullet}"
        );
        assert!(
            bullet.contains("`pmd:UnusedPrivateMethod` · _warning_"),
            "namespaced rule id + dot-separator severity tag: {bullet}"
        );
        assert!(
            bullet.contains("Avoid unused private methods such as 'helper()'."),
            "message renders verbatim: {bullet}"
        );
    }

    #[test]
    fn static_analysis_bullet_truncates_message_to_first_line() {
        let f = af(
            RuleKind::StaticAnalysis,
            "pmd:X",
            "First line\nSecond line",
            None,
        );
        let bullet = render_attributed_finding(
            &f,
            "udg/spring-x",
            None,
            Some(&AuthorAttribution::new("alice", 1.0)),
        );
        assert!(bullet.contains("First line"));
        assert!(
            !bullet.contains("Second line"),
            "multi-line messages collapse to first line: {bullet}"
        );
    }

    #[test]
    fn no_attribution_omits_weight_suffix() {
        let f = af(RuleKind::Architecture, "x", "", None);
        let bullet = render_attributed_finding(&f, "udg/spring-x", None, None);
        assert!(
            !bullet.contains(" of lines"),
            "no attribution => no weight suffix: {bullet}"
        );
    }

    #[test]
    fn span_with_zero_start_emits_no_anchor_or_suffix() {
        let mut f = af(RuleKind::Architecture, "x", "", None);
        f.span = LineSpan::single(0);
        let bullet = render_attributed_finding(
            &f,
            "udg/spring-x",
            None,
            Some(&AuthorAttribution::new("alice", 1.0)),
        );
        assert!(!bullet.contains(":L0"), "L0 anchor never emitted: {bullet}");
        assert!(
            !bullet.contains("#L"),
            "no line anchor on URL when span unknown: {bullet}"
        );
    }

    #[test]
    fn empty_repo_full_name_falls_back_to_plain_label() {
        // The github_blob_url debug-assert short-circuits on a
        // non-org-qualified repo and returns the empty URL; the bullet
        // must drop the markdown link rather than emit `[label]()`.
        let f = af(RuleKind::Architecture, "x", "", None);
        let bullet = render_attributed_finding(
            &f,
            "bare-repo",
            None,
            Some(&AuthorAttribution::new("alice", 1.0)),
        );
        assert!(!bullet.contains("[]"), "no empty markdown link: {bullet}");
        assert!(!bullet.contains("()"), "no empty href: {bullet}");
        assert!(
            bullet.contains("`Foo.java` :L42-L99"),
            "bare label retained: {bullet}"
        );
    }
}
