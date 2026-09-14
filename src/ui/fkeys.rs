//! The F1..F10 shortcut hint row at the bottom of the screen.

use crate::ui::theme::{GradRole, GradZone, Theme};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthChar;

/// Labels for the function-key row in panel mode (Midnight Commander order).
pub const PANEL_LABELS: [&str; 10] =
    ["Help", "Menu", "View", "Edit", "Copy", "RenMov", "Mkdir", "Delete", "PullDn", "Quit"];

/// Labels for the internal editor's function-key row (mcedit order).
pub const EDITOR_LABELS: [&str; 10] =
    ["Help", "Save", "Mark", "Replac", "Copy", "Move", "Search", "Delete", "PullDn", "Quit"];

/// Labels for the editor's hex mode (only the supported functions are shown).
pub const HEX_LABELS: [&str; 10] =
    ["", "Save", "", "Replac", "", "", "Search", "", "PullDn", "Quit"];

/// The function-key index (0-based — `i` means F`i+1`) at screen column `col`
/// on the bar row `row`, or `None` if the click misses the row or lands on an
/// empty (disabled) segment. Mirrors the segment layout used by [`render`].
pub fn index_at<S: AsRef<str>>(area: Rect, labels: &[S], col: u16, row: u16) -> Option<usize> {
    if row != area.y || col < area.x || col >= area.x + area.width {
        return None;
    }
    let n = labels.len().max(1);
    let total = area.width as usize;
    let base = total / n;
    let extra = total % n;
    let mut x = area.x as usize;
    for (i, label) in labels.iter().enumerate() {
        let seg = base + usize::from(i < extra);
        if seg == 0 {
            continue;
        }
        if (col as usize) >= x && (col as usize) < x + seg {
            return (!label.as_ref().is_empty()).then_some(i);
        }
        x += seg;
    }
    None
}

/// The panel function-key bar labels in the active language (RTL-reshaped for
/// display).
pub fn panel_labels() -> [String; 10] {
    PANEL_LABELS.map(crate::l10n::trd)
}

/// Render a function-key hint row using the supplied labels. The segments are
/// distributed so the row spans the full width of `area`. The labels are drawn
/// as a gradient — the bar's own, or the theme's accent ramp on truecolor —
/// otherwise in the classic two-tone look.
pub fn render<S: AsRef<str>>(f: &mut Frame, area: Rect, labels: &[S], theme: &Theme) {
    // Claim the row, so a body gradient can't repaint the bar (and the bar's own
    // can't reach past it) when the two share a color. The cells below carry the
    // bar's ramp already, so the screen pass leaves them be.
    crate::ui::gradient::mark_zone(GradZone::Fkeys, area);
    crate::ui::gradient::mark_painted(area);
    let n = labels.len().max(1);
    let total = area.width as usize;
    let base = total / n;
    let extra = total % n; // spread the remainder over the first segments

    // Build a per-cell list of (text, is_number, start column). Everything is
    // measured in display columns, not characters, so a wide (CJK) label fills
    // its segment without overrunning it — an overrun would shove the later
    // segments right and clip F10 off the end of the bar.
    let mut cells: Vec<(String, bool, usize)> = Vec::with_capacity(total);
    let mut col = 0usize;
    for (i, label) in labels.iter().enumerate() {
        let seg = base + usize::from(i < extra);
        if seg == 0 {
            continue;
        }
        let num = (i + 1).to_string();
        let num_w = num.chars().count().min(seg);
        for ch in num.chars().take(num_w) {
            push_cell(&mut cells, &mut col, ch, true);
        }
        // Fit the label into the columns the number leaves, padded out to the
        // segment's edge so the next one starts where `index_at` expects it.
        for ch in crate::util::text::pad_right(label.as_ref(), seg - num_w).chars() {
            push_cell(&mut cells, &mut col, ch, false);
        }
    }

    let spans: Vec<Span> = cells
        .iter()
        .map(|(text, is_num, at)| {
            let style = if *is_num {
                // Numbers always sit on their solid, contrasting key-cap color.
                theme.fkey_num
            } else {
                match theme.bar_bg(GradRole::FkeyLabelBg, *at, total) {
                    Some(bg) => Style::default().bg(bg).fg(theme.bar_fg),
                    None => theme.fkey_label,
                }
            };
            Span::styled(text.as_str(), style)
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Append `ch` to the bar's cell list at `col`, advancing `col` by the number of
/// columns it occupies. A zero-width character (a combining mark) rides along in
/// the cell of the character it modifies, so a span is never split mid-grapheme.
fn push_cell(cells: &mut Vec<(String, bool, usize)>, col: &mut usize, ch: char, is_num: bool) {
    match UnicodeWidthChar::width(ch).unwrap_or(0) {
        0 => {
            if let Some((text, _, _)) = cells.last_mut() {
                text.push(ch);
            }
        }
        w => {
            cells.push((ch.to_string(), is_num, *col));
            *col += w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use unicode_width::UnicodeWidthStr;

    /// A column the bar never wrote to, pre-filled so the assertions can tell
    /// "left as it found it" from "deliberately drawn blank".
    const UNPAINTED: &str = "\u{00b7}";

    /// Draw a bar `w` columns wide with `labels` and hand back the row.
    fn bar(w: u16, labels: &[&str]) -> Buffer {
        let spec = crate::ui::theme::active_specs().into_iter().next().expect("a built-in theme");
        let theme = crate::ui::theme::Theme::from_spec(&spec, true);
        let mut t = Terminal::new(TestBackend::new(w, 1)).unwrap();
        t.draw(|f| {
            crate::ui::gradient::reset();
            let area = f.area();
            f.buffer_mut().set_string(0, 0, UNPAINTED.repeat(w as usize), Style::default());
            render(f, area, labels, &theme);
        })
        .unwrap();
        t.backend().buffer().clone()
    }

    /// The bar's text, with the cell ratatui blanks after each double-width
    /// glyph dropped, so the result reads as it does on screen.
    fn row_text(buf: &Buffer, w: u16) -> String {
        let mut out = String::new();
        let mut skip = 0;
        for x in 0..w {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, 0)].symbol();
            skip = sym.width().saturating_sub(1);
            out.push_str(sym);
        }
        out
    }

    /// The column each segment starts at, as [`index_at`] divides the bar up.
    fn starts(w: u16, n: usize) -> Vec<usize> {
        let (base, extra) = (w as usize / n, w as usize % n);
        (0..n)
            .scan(0, |x, i| Some(std::mem::replace(x, *x + base + usize::from(i < extra))))
            .collect()
    }

    /// Assert that `labels` lay out on a `w`-wide bar the way [`index_at`] reads
    /// it: every segment's number sits at the column the click map assigns it,
    /// and the bar paints every column up to the right edge.
    fn assert_bar_lines_up(w: u16, labels: &[&str]) {
        let buf = bar(w, labels);
        for (i, x) in starts(w, labels.len()).into_iter().enumerate() {
            let digit = (i + 1).to_string().chars().next().unwrap().to_string();
            assert_eq!(
                buf[(x as u16, 0)].symbol(),
                digit,
                "F{} starts at column {x} on a {w}-wide bar, not {:?}",
                i + 1,
                buf[(x as u16, 0)].symbol()
            );
        }
        // The bar covers every column up to the right edge. A label clipped
        // mid-glyph used to leave the last column untouched — a notch of bare
        // screen where the bar should have been.
        for x in 0..w {
            assert_ne!(buf[(x, 0)].symbol(), UNPAINTED, "column {x} of a {w}-wide bar is bare");
        }
    }

    #[test]
    fn index_at_maps_columns_to_function_keys() {
        // 20 wide / 10 labels → 2 cells each: F1 at 0-1, F2 at 2-3, … F10 at 18-19.
        let area = Rect::new(0, 5, 20, 1);
        let labels = super::PANEL_LABELS;
        assert_eq!(index_at(area, &labels, 0, 5), Some(0));
        assert_eq!(index_at(area, &labels, 5, 5), Some(2)); // F3
        assert_eq!(index_at(area, &labels, 19, 5), Some(9)); // F10
        // Wrong row, or off the right edge → no hit.
        assert_eq!(index_at(area, &labels, 5, 4), None);
        assert_eq!(index_at(area, &labels, 20, 5), None);
        // Empty (disabled) segments report no hit.
        let hex = super::HEX_LABELS; // index 0 ("") and 2 ("") are blank
        assert_eq!(index_at(area, &hex, 0, 5), None);
        assert_eq!(index_at(area, &hex, 2, 5), Some(1)); // F2 "Save"
        assert_eq!(index_at(area, &hex, 4, 5), None); // F3 blank
    }

    /// Double-width (CJK) labels are fitted by display width, not by character
    /// count, so each segment keeps to its share of the row. Counting characters
    /// made a segment up to twice as wide as its share, which shoved the later
    /// keys rightwards until F10 fell off the end of the bar.
    #[test]
    fn wide_labels_keep_every_key_in_its_segment() {
        // The Japanese panel labels: two to four double-width characters each.
        let ja = [
            "ヘルプ",
            "メニュー",
            "表示",
            "編集",
            "コピー",
            "名変移",
            "作成",
            "削除",
            "メニュ",
            "終了",
        ];
        for w in [41u16, 60, 80, 100, 120, 203] {
            assert_bar_lines_up(w, &ja);
        }
        // F10's label is drawn in full, and the bar ends exactly at the edge.
        let row = row_text(&bar(80, &ja), 80);
        assert!(row.contains("10終了"), "F10 is on the bar: {row:?}");
        assert_eq!(row.width(), 80, "the bar spans exactly 80 columns");
    }

    /// Zero-width combining marks ride along with the character they modify
    /// rather than each claiming a column, which otherwise left the bar short of
    /// the right edge and dropped the marks at render time.
    #[test]
    fn combining_marks_do_not_consume_columns() {
        // Devanagari, with combining vowel signs and a nukta-like mark.
        let hi = ["मदद", "मेनू", "देखें", "संपादन", "कॉपी", "नाम", "फोल्डर", "मिटाएँ", "मेनू", "बाहर"];
        for w in [61u16, 80, 100] {
            assert_bar_lines_up(w, &hi);
        }
        let row = row_text(&bar(80, &hi), 80);
        assert!(row.contains('\u{0901}'), "the combining mark survives: {row:?}");
        assert_eq!(row.width(), 80, "the bar spans exactly 80 columns");
    }

    /// Plain ASCII labels are unaffected at any width.
    #[test]
    fn ascii_labels_line_up_at_every_width() {
        for w in [20u16, 33, 50, 80, 132, 200] {
            assert_bar_lines_up(w, &PANEL_LABELS);
        }
    }

    /// A bar with fewer columns than keys still fills the row and stops at the
    /// edge: the keys that have no column left get no segment, and "10" is cut
    /// to one digit rather than overrunning into a neighbour's.
    #[test]
    fn a_bar_narrower_than_its_keys_stays_within_the_row() {
        for labels in [&PANEL_LABELS, &HEX_LABELS] {
            for w in [1u16, 4, 9, 10, 13, 19] {
                let row = row_text(&bar(w, labels), w);
                assert!(!row.contains(UNPAINTED), "a {w}-wide bar fills the row: {row:?}");
                assert_eq!(row.width(), w as usize, "a {w}-wide bar spans {w} columns");
            }
        }
    }
}
