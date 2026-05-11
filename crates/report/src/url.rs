//! GitHub blob-URL builder. Single source of truth for `github.com/o/r/blob/...`
//! URLs anywhere in the workspace — every renderer routes through here so the
//! `is_repo_relative` debug-assert catches absolute filesystem paths at the
//! boundary (the static-analysis URL bug captured by W1.T4).

use sprint_grader_core::finding::LineSpan;
use sprint_grader_core::paths::is_repo_relative;

const GITHUB_BLOB_PREFIX: &str = "https://github.com";

/// Build a `https://github.com/<org>/<repo>/blob/<git_ref>/<file>#L<a>-L<b>` URL.
///
/// `repo_full_name` MUST be `<org>/<repo>`; we return an empty string when it
/// is not, mirroring the previous `github_file_url -> Option<String>` shape
/// so callers can fall back to a plain code-formatted basename.
///
/// `repo_relative_file` MUST be a repo-relative POSIX path; debug-asserts
/// via `is_repo_relative`. In release builds we log a warning and emit the
/// URL anyway so production never panics — but the assert catches the bug
/// during testing.
pub fn github_blob_url(
    repo_full_name: &str,
    git_ref: &str,
    repo_relative_file: &str,
    span: Option<LineSpan>,
) -> String {
    if !repo_full_name.contains('/') {
        return String::new();
    }
    if !is_repo_relative(repo_relative_file) {
        debug_assert!(
            is_repo_relative(repo_relative_file),
            "github_blob_url: file path is not repo-relative: {repo_relative_file:?}"
        );
        tracing::warn!(
            target: "report::url",
            file = repo_relative_file,
            repo = repo_full_name,
            "github_blob_url called with non-repo-relative path; URL will be malformed"
        );
    }
    let mut out =
        format!("{GITHUB_BLOB_PREFIX}/{repo_full_name}/blob/{git_ref}/{repo_relative_file}");
    out.push_str(&line_anchor(span));
    out
}

/// `#L42` or `#L42-L99`; empty when `span` is `None`.
///
/// A range with `end == start` collapses to the single-line form (`#L42`)
/// to match GitHub's canonical URL shape.
pub fn line_anchor(span: Option<LineSpan>) -> String {
    let Some(s) = span else { return String::new() };
    match s.end {
        Some(end) if end > s.start => format!("#L{}-L{}", s.start, end),
        _ => format!("#L{}", s.start),
    }
}

/// ` :L42` or ` :L42-L99` — for inline display next to a filename in markdown.
/// Empty when `span` is `None`.
pub fn line_suffix(span: Option<LineSpan>) -> String {
    let Some(s) = span else { return String::new() };
    match s.end {
        Some(end) if end > s.start => format!(" :L{}-L{}", s.start, end),
        _ => format!(" :L{}", s.start),
    }
}

/// Convenience constructor: build an `Option<LineSpan>` from the
/// `(Option<i64>, Option<i64>)` shape that the database rows use. Filters
/// out non-positive starts (which the schema stores as `0` for unknown).
pub fn span_from_db(start: Option<i64>, end: Option<i64>) -> Option<LineSpan> {
    let s = start?;
    if s <= 0 {
        return None;
    }
    let start = u32::try_from(s).ok()?;
    let end = end.and_then(|e| if e >= s { u32::try_from(e).ok() } else { None });
    Some(LineSpan { start, end })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_with_no_span_is_plain_blob_url() {
        let url = github_blob_url("o/r", "HEAD", "src/Foo.java", None);
        assert_eq!(url, "https://github.com/o/r/blob/HEAD/src/Foo.java");
    }

    #[test]
    fn url_with_single_line_span_appends_anchor() {
        let url = github_blob_url("o/r", "HEAD", "src/Foo.java", Some(LineSpan::single(42)));
        assert_eq!(url, "https://github.com/o/r/blob/HEAD/src/Foo.java#L42");
    }

    #[test]
    fn url_with_range_appends_dual_anchor() {
        let url = github_blob_url("o/r", "HEAD", "src/Foo.java", Some(LineSpan::range(42, 99)));
        assert_eq!(url, "https://github.com/o/r/blob/HEAD/src/Foo.java#L42-L99");
    }

    #[test]
    fn url_collapses_equal_start_end_to_single_anchor() {
        let url = github_blob_url("o/r", "HEAD", "Foo.java", Some(LineSpan::range(7, 7)));
        assert_eq!(url, "https://github.com/o/r/blob/HEAD/Foo.java#L7");
    }

    #[test]
    fn url_uses_provided_git_ref() {
        let url = github_blob_url("o/r", "abc1234", "Foo.java", None);
        assert_eq!(url, "https://github.com/o/r/blob/abc1234/Foo.java");
    }

    #[test]
    fn url_is_empty_when_repo_name_lacks_org_prefix() {
        assert_eq!(github_blob_url("just-a-name", "HEAD", "Foo.java", None), "");
    }

    #[test]
    fn line_anchor_handles_each_span_shape() {
        assert_eq!(line_anchor(None), "");
        assert_eq!(line_anchor(Some(LineSpan::single(42))), "#L42");
        assert_eq!(line_anchor(Some(LineSpan::range(42, 99))), "#L42-L99");
        assert_eq!(line_anchor(Some(LineSpan::range(42, 42))), "#L42");
    }

    #[test]
    fn line_suffix_handles_each_span_shape() {
        assert_eq!(line_suffix(None), "");
        assert_eq!(line_suffix(Some(LineSpan::single(42))), " :L42");
        assert_eq!(line_suffix(Some(LineSpan::range(42, 99))), " :L42-L99");
    }

    #[test]
    fn span_from_db_handles_unknown_and_swapped_endpoints() {
        assert_eq!(span_from_db(None, None), None);
        assert_eq!(span_from_db(Some(0), Some(10)), None);
        assert_eq!(span_from_db(Some(-1), None), None);
        assert_eq!(span_from_db(Some(42), None), Some(LineSpan::single(42)));
        assert_eq!(
            span_from_db(Some(42), Some(99)),
            Some(LineSpan::range(42, 99))
        );
        // end < start is treated as no end (range collapses to single-line).
        assert_eq!(span_from_db(Some(42), Some(10)), Some(LineSpan::single(42)));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "not repo-relative")]
    fn debug_asserts_on_absolute_path() {
        let _ = github_blob_url("o/r", "HEAD", "/home/u/Foo.java", None);
    }
}
