//! `insta` filters for non-deterministic fields in REPORT.md
//! (T-T0.4).
//!
//! `REPORT.md` contains several fields that change run-to-run even on
//! the same fixture:
//! * ISO timestamps from `Utc::now()` calls in the renderers.
//! * Elapsed-seconds and durations.
//! * Tempdir absolute paths in any error/observation strings.
//! * The realised threshold band (T-P2.6) when hidden_thresholds is on.
//! * The `fitted_at` cell in the cumulative summary (T-P2.1).
//!
//! Each filter is a `(regex, replacement)` pair. Apply them via
//! `insta::with_settings!` or by passing them to `insta::Settings::add_filter`.

/// Returns the canonical filter set. Use as:
///
/// ```ignore
/// let mut s = insta::Settings::clone_current();
/// for (re, repl) in sprint_grader_blackbox::snapshot::filters() {
///     s.add_filter(re, repl);
/// }
/// s.bind(|| insta::assert_snapshot!(rendered));
/// ```
pub fn filters() -> Vec<(&'static str, &'static str)> {
    vec![
        // ISO-8601 timestamps with optional fractional seconds and Z/±hh:mm offset
        (
            r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})",
            "<timestamp>",
        ),
        // Bare ISO date (e.g. fitted_at fallback, "today" stamp)
        (r"\b\d{4}-\d{2}-\d{2}\b", "<date>"),
        // Elapsed-seconds floats followed by 's'
        (r"\b\d+\.\d{1,3}s\b", "<duration>"),
        // /tmp tempdir prefixes
        (r"/tmp/[a-zA-Z0-9_.]+", "<tmp>"),
        (r"/var/folders/[a-zA-Z0-9_./]+", "<tmp>"),
    ]
}

/// Apply `filters()` to a string in-place. Useful when the caller
/// wants to assert on the post-filter body without going through
/// `insta::Settings` (e.g. exact-match scenarios that are not yet
/// snapshot-driven).
pub fn scrub(s: &str) -> String {
    let mut out = s.to_string();
    for (re, repl) in filters() {
        let r = regex::Regex::new(re).expect("compile snapshot filter");
        out = r.replace_all(&out, repl).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_iso_timestamp() {
        assert_eq!(scrub("at 2026-04-26T10:15:30Z done"), "at <timestamp> done");
    }

    #[test]
    fn scrubs_iso_date_alone() {
        assert_eq!(scrub("today=2026-04-26"), "today=<date>");
    }

    #[test]
    fn scrubs_durations() {
        assert_eq!(scrub("ran in 12.34s"), "ran in <duration>");
    }

    #[test]
    fn scrubs_tempdir_paths() {
        assert_eq!(scrub("at /tmp/blackbox.abc.123/data"), "at <tmp>/data");
    }
}
