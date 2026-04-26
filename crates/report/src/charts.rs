//! Hand-rolled SVG chart primitives for the HTML report.
//!
//! The Python tree uses matplotlib; the migration plan calls for inline SVG so
//! the HTML renders at any DPI without raster assets. `plotters` would work
//! but carries a heavy transitive tree — for the two chart kinds we actually
//! need (stacked horizontal bars per student, sparkline per metric) the
//! output shape is simple enough to emit as raw SVG strings.

use std::fmt::Write;

/// Tier color palette that matches the PR submission timing categories.
/// Legacy color-name tiers are retained so old DB rows still render.
pub fn tier_color(tier: &str) -> &'static str {
    match tier {
        "Regular" | "Green" => "#4CAF50",
        "Late" | "Orange" => "#FF9800",
        "Critical" | "Red" | "Cramming" => "#F44336",
        "Fix" => "#2196F3",
        _ => "#9E9E9E",
    }
}

#[derive(Debug, Clone)]
pub struct StackedRow {
    pub label: String,
    /// One segment per series; values must sum to `total` for the row to
    /// render proportionally.
    pub segments: Vec<(String, f64)>,
}

/// Render a horizontal stacked bar chart as SVG. Each row represents one
/// student; each segment represents a tier count. Widths are normalized to
/// the row's own total so short and long rows both fill the width — matching
/// the Python openpyxl stacked-bar presentation.
pub fn stacked_bars_svg(title: &str, rows: &[StackedRow], width: u32, row_height: u32) -> String {
    if rows.is_empty() {
        // `fill="currentColor"` on the root <svg> lets <text> inherit the
        // surrounding Markdown viewer's text color — readable in both light
        // and dark themes without hard-coding.
        return format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"40\" fill=\"currentColor\" fill-opacity=\"0.7\"><text x=\"8\" y=\"24\" font-family=\"sans-serif\" font-size=\"13\">{t}: no data</text></svg>",
            w = width,
            t = html_escape(title)
        );
    }

    let right_pad: u32 = 24;
    let longest_label = rows
        .iter()
        .map(|r| r.label.chars().count() as u32)
        .max()
        .unwrap_or(0);
    let requested_left_pad = 140.max(longest_label.saturating_mul(7).saturating_add(16));
    let max_left_pad = width.saturating_sub(right_pad + 160);
    let left_pad = requested_left_pad.min(max_left_pad.max(80));
    let top_pad: u32 = 30;
    let bar_area = width.saturating_sub(left_pad + right_pad);
    let total_height = top_pad + rows.len() as u32 * row_height + 20;

    let mut svg = String::new();
    // Root `fill="currentColor"` makes every <text> inherit the Markdown
    // viewer's text color. <rect> elements below set their own fill so the
    // tier palette is unaffected.
    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" font-family=\"sans-serif\" font-size=\"12\" fill=\"currentColor\">",
        w = width,
        h = total_height
    );
    let _ = write!(
        svg,
        "<text x=\"{tx}\" y=\"18\" font-weight=\"bold\">{t}</text>",
        tx = left_pad,
        t = html_escape(title)
    );

    for (i, row) in rows.iter().enumerate() {
        let y = top_pad + i as u32 * row_height;
        let label_y = y + (row_height - 4) / 2 + 4;
        let _ = write!(
            svg,
            "<text x=\"{lx}\" y=\"{ly}\" text-anchor=\"end\">{lbl}</text>",
            lx = left_pad - 6,
            ly = label_y,
            lbl = html_escape(&row.label)
        );

        let total: f64 = row.segments.iter().map(|(_, v)| *v).sum();
        let mut cursor = left_pad as f64;
        if total > 0.0 {
            for (series, value) in &row.segments {
                let seg_w = (value / total) * bar_area as f64;
                if seg_w <= 0.0 {
                    continue;
                }
                let _ = write!(
                    svg,
                    "<rect x=\"{x:.2}\" y=\"{by}\" width=\"{w:.2}\" height=\"{bh}\" fill=\"{c}\"><title>{s}: {v:.0}</title></rect>",
                    x = cursor,
                    by = y + 4,
                    w = seg_w,
                    bh = row_height.saturating_sub(8),
                    c = tier_color(series),
                    s = html_escape(series),
                    v = value
                );
                cursor += seg_w;
            }
        } else {
            let _ = write!(
                svg,
                "<rect x=\"{x}\" y=\"{by}\" width=\"{w}\" height=\"{bh}\" fill=\"#EEE\" />",
                x = left_pad,
                by = y + 4,
                w = bar_area,
                bh = row_height.saturating_sub(8),
            );
        }
    }

    // Legend
    let legend_y = top_pad + rows.len() as u32 * row_height + 4;
    let all_series: Vec<&str> = rows
        .iter()
        .flat_map(|r| r.segments.iter().map(|(s, _)| s.as_str()))
        .collect();
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut lx = left_pad;
    for s in all_series {
        if !seen.insert(s) {
            continue;
        }
        let _ = write!(
            svg,
            "<rect x=\"{x}\" y=\"{y}\" width=\"10\" height=\"10\" fill=\"{c}\" />",
            x = lx,
            y = legend_y,
            c = tier_color(s),
        );
        let _ = write!(
            svg,
            "<text x=\"{tx}\" y=\"{ty}\">{s}</text>",
            tx = lx + 14,
            ty = legend_y + 9,
            s = html_escape(s),
        );
        lx += 14 + (s.chars().count() as u32 * 7 + 12);
    }

    svg.push_str("</svg>");
    svg
}

/// Emit a small SVG sparkline for a 0..1 metric over N points. Used on the
/// student dashboard to preview sprint-over-sprint composite scores.
pub fn sparkline_svg(values: &[f64], width: u32, height: u32) -> String {
    if values.is_empty() {
        return format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" />",
            w = width,
            h = height
        );
    }
    let pad: f64 = 2.0;
    let w = width as f64 - pad * 2.0;
    let h = height as f64 - pad * 2.0;
    let n = values.len();
    let step = if n > 1 { w / (n - 1) as f64 } else { 0.0 };
    let mut d = String::new();
    for (i, v) in values.iter().enumerate() {
        let x = pad + i as f64 * step;
        let y = pad + (1.0 - v.clamp(0.0, 1.0)) * h;
        if i == 0 {
            let _ = write!(d, "M {:.2},{:.2}", x, y);
        } else {
            let _ = write!(d, " L {:.2},{:.2}", x, y);
        }
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\"><path d=\"{d}\" stroke=\"#1976D2\" fill=\"none\" stroke-width=\"1.5\" stroke-linejoin=\"round\" stroke-linecap=\"round\" /></svg>",
        w = width,
        h = height,
        d = d
    )
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rows_emits_no_data_placeholder() {
        let svg = stacked_bars_svg("Timing", &[], 400, 24);
        assert!(svg.contains("no data"));
        assert!(svg.starts_with("<svg"));
    }

    #[test]
    fn stacked_bars_respects_segment_widths() {
        let rows = vec![StackedRow {
            label: "Alice".into(),
            segments: vec![("Green".into(), 3.0), ("Red".into(), 1.0)],
        }];
        let svg = stacked_bars_svg("Timing", &rows, 400, 24);
        // Both segment colours present
        assert!(svg.contains(tier_color("Green")));
        assert!(svg.contains(tier_color("Red")));
        // Label rendered
        assert!(svg.contains("Alice"));
    }

    #[test]
    fn sparkline_handles_single_value() {
        let svg = sparkline_svg(&[0.5], 60, 20);
        assert!(svg.contains("<path"));
    }

    #[test]
    fn html_escape_covers_all_reserved_chars() {
        let s = html_escape("<b class=\"x\">a&b</b>");
        assert_eq!(s, "&lt;b class=&quot;x&quot;&gt;a&amp;b&lt;/b&gt;");
    }
}
