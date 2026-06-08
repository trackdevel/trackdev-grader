//! Excel cell text limits (`rust_xlsxwriter` rejects strings > 32,767 chars).

use std::borrow::Cow;

/// OOXML / Excel per-cell string cap.
pub const EXCEL_MAX_CELL_CHARS: usize = 32_767;

const TRUNC_SUFFIX: &str = "… [truncated for Excel]";

/// Truncate at a Unicode scalar boundary so `write_string` stays within Excel's limit.
pub fn truncate_excel_cell(s: &str) -> Cow<'_, str> {
    let n = s.chars().count();
    if n <= EXCEL_MAX_CELL_CHARS {
        return Cow::Borrowed(s);
    }
    let suffix_len = TRUNC_SUFFIX.chars().count();
    let take = EXCEL_MAX_CELL_CHARS.saturating_sub(suffix_len);
    let mut out: String = s.chars().take(take).collect();
    out.push_str(TRUNC_SUFFIX);
    debug_assert!(out.chars().count() <= EXCEL_MAX_CELL_CHARS);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_string_passes_through() {
        assert_eq!(truncate_excel_cell("hello").as_ref(), "hello");
    }

    #[test]
    fn at_limit_passes_through() {
        let s: String = "a".repeat(EXCEL_MAX_CELL_CHARS);
        assert_eq!(
            truncate_excel_cell(&s).chars().count(),
            EXCEL_MAX_CELL_CHARS
        );
    }

    #[test]
    fn over_limit_is_truncated_with_notice() {
        let s: String = "x".repeat(40_000);
        let t = truncate_excel_cell(&s);
        assert!(t.chars().count() <= EXCEL_MAX_CELL_CHARS);
        assert!(t.ends_with(TRUNC_SUFFIX));
    }
}
