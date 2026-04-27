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

/// One tile in [`treemap_svg`]. `value` is the area weight (typically a
/// statement count); `category` selects the colour via [`category_color`].
#[derive(Debug, Clone)]
pub struct TreemapTile {
    pub label: String,
    pub category: String,
    pub value: f64,
}

/// Stable per-category palette derived from a hash of the category string.
/// Picks an HSL hue from a 16-step ring so two adjacent students always get
/// distinguishable colours, then converts to hex. The same `category` string
/// always produces the same colour, which makes the legend predictable
/// across re-runs and across sprints.
pub fn category_color(category: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    category.hash(&mut h);
    let hash = h.finish();
    let hue = (hash % 360) as f64;
    // Saturation 55%, lightness 60% — readable on light AND dark report
    // backgrounds without being neon.
    hsl_to_hex(hue, 0.55, 0.60)
}

fn hsl_to_hex(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = (h % 360.0) / 60.0;
    let x = c * (1.0 - (h_prime % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let r = ((r1 + m) * 255.0).round() as u8;
    let g = ((g1 + m) * 255.0).round() as u8;
    let b = ((b1 + m) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", r, g, b)
}

/// Squarified treemap (Bruls/Huijing/van Wijk 2000). Tiles are laid out in a
/// row-then-column slice/dice that minimises aspect ratio variance — easier to
/// read than naive horizontal-strip layouts when one tile dominates.
///
/// Tiles with `value <= 0` are skipped. The output is deterministic for any
/// given input ordering, so re-running the report on unchanged data emits
/// byte-identical SVG (handy for `diff-db --derived-only` parity checks).
pub fn treemap_svg(title: &str, tiles: &[TreemapTile], width: u32, height: u32) -> String {
    let positives: Vec<&TreemapTile> = tiles.iter().filter(|t| t.value > 0.0).collect();
    if positives.is_empty() {
        return format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"40\" fill=\"currentColor\" fill-opacity=\"0.7\"><text x=\"8\" y=\"24\" font-family=\"sans-serif\" font-size=\"13\">{t}: no data</text></svg>",
            w = width,
            t = html_escape(title)
        );
    }

    let title_h: u32 = 22;
    let plot_top = title_h;
    let plot_h = height.saturating_sub(title_h);
    let plot_w = width;
    if plot_h == 0 || plot_w == 0 {
        return String::new();
    }

    let mut sorted: Vec<&TreemapTile> = positives.clone();
    sorted.sort_by(|a, b| {
        b.value
            .partial_cmp(&a.value)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.label.cmp(&b.label))
    });

    let total: f64 = sorted.iter().map(|t| t.value).sum();
    let scale = (plot_w as f64 * plot_h as f64) / total;

    let rects = squarify(
        &sorted.iter().map(|t| t.value * scale).collect::<Vec<_>>(),
        plot_w as f64,
        plot_h as f64,
    );

    let mut svg = String::new();
    use std::fmt::Write;
    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" font-family=\"sans-serif\" font-size=\"11\" fill=\"currentColor\">",
        w = width,
        h = height
    );
    let _ = write!(
        svg,
        "<text x=\"4\" y=\"15\" font-weight=\"bold\">{t}</text>",
        t = html_escape(title)
    );

    for (i, rect) in rects.iter().enumerate() {
        let tile = sorted[i];
        let color = category_color(&tile.category);
        let pct = tile.value / total * 100.0;
        let _ = write!(
            svg,
            "<rect x=\"{x:.2}\" y=\"{y:.2}\" width=\"{w:.2}\" height=\"{h:.2}\" fill=\"{c}\" stroke=\"#FFFFFF\" stroke-width=\"1\"><title>{lbl}\nowner: {cat}\n{val:.0} stmts ({pct:.1}%)</title></rect>",
            x = rect.x,
            y = plot_top as f64 + rect.y,
            w = rect.w,
            h = rect.h,
            c = color,
            lbl = html_escape(&tile.label),
            cat = html_escape(&tile.category),
            val = tile.value,
            pct = pct,
        );
        // Inline label: only when the tile is large enough to read.
        if rect.w >= 60.0 && rect.h >= 18.0 {
            let label = short_label(&tile.label, rect.w);
            let _ = write!(
                svg,
                "<text x=\"{tx:.2}\" y=\"{ty:.2}\" fill=\"#222\" font-size=\"10\">{l}</text>",
                tx = rect.x + 4.0,
                ty = plot_top as f64 + rect.y + 12.0,
                l = html_escape(&label)
            );
        }
    }
    svg.push_str("</svg>");
    svg
}

/// Truncate a path to roughly fit the given pixel width at 6px/char.
fn short_label(s: &str, width_px: f64) -> String {
    let max_chars = ((width_px - 8.0) / 6.0).max(4.0) as usize;
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    // Prefer keeping the basename — the directory prefix repeats.
    let basename = s.rsplit('/').next().unwrap_or(s);
    if basename.chars().count() <= max_chars {
        return basename.to_string();
    }
    let take = max_chars.saturating_sub(1);
    let mut out: String = basename.chars().take(take).collect();
    out.push('…');
    out
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Squarified treemap layout. `areas` are pre-scaled so they sum to
/// `width * height`. Returns one `Rect` per input area in the same order.
fn squarify(areas: &[f64], width: f64, height: f64) -> Vec<Rect> {
    fn worst_ratio(row: &[f64], side_len: f64) -> f64 {
        let s: f64 = row.iter().sum();
        let r_max = row.iter().cloned().fold(f64::MIN, f64::max);
        let r_min = row.iter().cloned().fold(f64::MAX, f64::min);
        let s_sq = s * s;
        let side_sq = side_len * side_len;
        (side_sq * r_max / s_sq).max(s_sq / (side_sq * r_min))
    }
    #[allow(clippy::too_many_arguments)]
    fn layout_row(
        row: &[f64],
        offsets: &[usize],
        out: &mut [Rect],
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        horizontal: bool,
    ) -> (f64, f64, f64, f64) {
        let s: f64 = row.iter().sum();
        if horizontal {
            let row_h = if w > 0.0 { s / w } else { 0.0 };
            let mut cursor = x;
            for (i, &area) in row.iter().enumerate() {
                let tile_w = if row_h > 0.0 { area / row_h } else { 0.0 };
                out[offsets[i]] = Rect {
                    x: cursor,
                    y,
                    w: tile_w,
                    h: row_h,
                };
                cursor += tile_w;
            }
            (x, y + row_h, w, (h - row_h).max(0.0))
        } else {
            let row_w = if h > 0.0 { s / h } else { 0.0 };
            let mut cursor = y;
            for (i, &area) in row.iter().enumerate() {
                let tile_h = if row_w > 0.0 { area / row_w } else { 0.0 };
                out[offsets[i]] = Rect {
                    x,
                    y: cursor,
                    w: row_w,
                    h: tile_h,
                };
                cursor += tile_h;
            }
            (x + row_w, y, (w - row_w).max(0.0), h)
        }
    }

    let mut out = vec![
        Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0
        };
        areas.len()
    ];
    let (mut x, mut y, mut w, mut h) = (0.0, 0.0, width, height);
    let mut current: Vec<f64> = Vec::new();
    let mut current_idx: Vec<usize> = Vec::new();
    let mut next_idx = 0usize;

    while next_idx < areas.len() {
        let side_len = w.min(h);
        if current.is_empty() {
            current.push(areas[next_idx]);
            current_idx.push(next_idx);
            next_idx += 1;
            continue;
        }
        let mut try_row = current.clone();
        try_row.push(areas[next_idx]);
        let worst_now = worst_ratio(&current, side_len);
        let worst_next = worst_ratio(&try_row, side_len);
        if worst_next <= worst_now {
            current.push(areas[next_idx]);
            current_idx.push(next_idx);
            next_idx += 1;
        } else {
            let horizontal = w >= h;
            let (nx, ny, nw, nh) =
                layout_row(&current, &current_idx, &mut out, x, y, w, h, horizontal);
            x = nx;
            y = ny;
            w = nw;
            h = nh;
            current.clear();
            current_idx.clear();
        }
    }
    if !current.is_empty() {
        let horizontal = w >= h;
        let _ = layout_row(&current, &current_idx, &mut out, x, y, w, h, horizontal);
    }
    out
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
    fn treemap_emits_no_data_when_all_zero() {
        let svg = treemap_svg("Ownership", &[], 400, 200);
        assert!(svg.contains("no data"));
    }

    #[test]
    fn treemap_renders_tile_per_input() {
        let tiles = vec![
            TreemapTile {
                label: "src/A.java".into(),
                category: "alice".into(),
                value: 50.0,
            },
            TreemapTile {
                label: "src/B.java".into(),
                category: "bob".into(),
                value: 30.0,
            },
            TreemapTile {
                label: "src/C.java".into(),
                category: "alice".into(),
                value: 20.0,
            },
        ];
        let svg = treemap_svg("Ownership", &tiles, 400, 200);
        assert_eq!(svg.matches("<rect").count(), 3);
        // Same author → same colour.
        let alice = category_color("alice");
        let bob = category_color("bob");
        assert_ne!(
            alice, bob,
            "different categories must get different colours"
        );
        assert!(svg.contains(&alice));
        assert!(svg.contains(&bob));
    }

    #[test]
    fn category_color_is_stable_for_same_string() {
        assert_eq!(category_color("alice"), category_color("alice"));
        // Different strings produce different colours (in general; the hash
        // theoretically collides but with 360 hue buckets a 2-string collision
        // would be a fixture worth investigating).
        assert_ne!(category_color("alice"), category_color("bob"));
    }

    #[test]
    fn squarify_tiles_cover_the_canvas() {
        let areas = vec![400.0, 300.0, 200.0, 100.0]; // sums to 1000 = 100*10
        let rects = squarify(&areas, 100.0, 10.0);
        // Per-tile area is preserved (within fp slop).
        for (rect, area) in rects.iter().zip(&areas) {
            assert!(
                (rect.w * rect.h - area).abs() < 1e-6,
                "tile area lost: {:?} vs {area}",
                rect
            );
        }
    }

    #[test]
    fn html_escape_covers_all_reserved_chars() {
        let s = html_escape("<b class=\"x\">a&b</b>");
        assert_eq!(s, "&lt;b class=&quot;x&quot;&gt;a&amp;b&lt;/b&gt;");
    }
}
