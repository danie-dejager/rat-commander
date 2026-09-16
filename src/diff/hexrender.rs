//! Drawing the [`HexDiffView`]: the two files' bytes side by side (hex and
//! ASCII, only hex, or one file above the other as the width allows), sharing
//! one offset column and one cursor, with every byte that differs coloured.

use super::hex::{BYTES_PER_ROW, HexDiffView, Layout};
use crate::editor::hex::HexGeom;
use crate::ui::theme::Theme;
use crate::util::text::{ellipsize, pad_right};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Columns of one file's hex cells, the spaces between them included.
const HEX_W: u16 = 49;
/// Columns of one file's `|ascii|`.
const ASCII_W: u16 = 18;
/// Columns between the two files.
const SEP_W: u16 = 3;

/// What a block of cells on screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Hex,
    Ascii,
}

/// A block of cells: which file (0 left, 1 right), hex or ASCII, and where its
/// first cell is.
#[derive(Debug, Clone, Copy)]
struct Pane {
    side: usize,
    kind: Kind,
    x: u16,
    y: u16,
}

/// The layout that fits `width` columns, the panes it puts the bytes in, and
/// how many rows of bytes each file gets.
fn panes(body: Rect, off_w: u16) -> (Layout, Vec<Pane>, usize) {
    let (x0, y0) = (body.x + off_w + 2, body.y);
    let wide = off_w + 2 + 2 * (HEX_W + ASCII_W) + SEP_W;
    let hex_only = off_w + 2 + 2 * HEX_W + SEP_W;
    if body.width >= wide {
        let right = x0 + HEX_W + ASCII_W + SEP_W;
        let p = |side, kind, x| Pane { side, kind, x, y: y0 };
        let v = vec![
            p(0, Kind::Hex, x0),
            p(0, Kind::Ascii, x0 + HEX_W + 1),
            p(1, Kind::Hex, right),
            p(1, Kind::Ascii, right + HEX_W + 1),
        ];
        return (Layout::Wide, v, body.height as usize);
    }
    if body.width >= hex_only {
        let v = vec![
            Pane { side: 0, kind: Kind::Hex, x: x0, y: y0 },
            Pane { side: 1, kind: Kind::Hex, x: x0 + HEX_W + SEP_W, y: y0 },
        ];
        return (Layout::HexOnly, v, body.height as usize);
    }
    // One above the other, a rule naming the second between them.
    let rows = body.height.saturating_sub(1) / 2;
    let y1 = y0 + rows + 1;
    let v = vec![
        Pane { side: 0, kind: Kind::Hex, x: x0, y: y0 },
        Pane { side: 0, kind: Kind::Ascii, x: x0 + HEX_W + 1, y: y0 },
        Pane { side: 1, kind: Kind::Hex, x: x0, y: y1 },
        Pane { side: 1, kind: Kind::Ascii, x: x0 + HEX_W + 1, y: y1 },
    ];
    (Layout::Stacked, v, rows as usize)
}

/// Column of hex cell `j` from the start of a hex pane.
fn hex_x(j: u16) -> u16 {
    3 * j + u16::from(j >= 8)
}

/// The byte offset under screen cell (`col`, `row`), as last drawn.
pub(super) fn offset_at(v: &HexDiffView, col: u16, row: u16) -> Option<u64> {
    let off_w = HexGeom::for_len(v.len()).off_w;
    let (_, panes, rows) = panes(v.body, off_w);
    let bpr = BYTES_PER_ROW as u16;
    for p in panes {
        if row < p.y || row >= p.y + rows as u16 || col < p.x {
            continue;
        }
        let dx = col - p.x;
        let j = match p.kind {
            Kind::Hex => (0..bpr).find(|&j| dx >= hex_x(j) && dx < hex_x(j) + 2),
            Kind::Ascii => (dx < bpr).then_some(dx),
        };
        if let Some(j) = j {
            let off = v.top + (row - p.y) as u64 * BYTES_PER_ROW + j as u64;
            return (off < v.lens()[p.side]).then_some(off);
        }
    }
    None
}

pub fn render(f: &mut Frame, area: Rect, v: &mut HexDiffView, theme: &Theme) {
    if area.height < 3 || area.width < 12 {
        return;
    }
    let status = Rect { height: 1, ..area };
    let footer = Rect { y: area.y + area.height - 1, height: 1, ..area };
    let body = Rect { y: area.y + 1, height: area.height - 2, ..area };
    let geom = HexGeom::for_len(v.len());
    let (layout, panes, rows) = panes(body, geom.off_w);
    v.body = body;
    v.layout = layout;
    v.view_rows = rows.max(1);

    // Scroll to the cursor.
    let cur_row = v.cursor / BYTES_PER_ROW;
    let mut top_row = v.top / BYTES_PER_ROW;
    if cur_row < top_row {
        top_row = cur_row;
    } else if cur_row >= top_row + v.view_rows as u64 {
        top_row = cur_row + 1 - v.view_rows as u64;
    }
    v.top = top_row * BYTES_PER_ROW;

    let normal = Style::default().fg(theme.text_fg).bg(theme.panel_bg);
    f.render_widget(Paragraph::new("").style(normal), body);
    let field = v.field();
    render_status(f, status, v, field, theme);

    let data = v.rows(v.top, rows * BYTES_PER_ROW as usize);
    let offset_style = Style::default().fg(theme.header_fg).bg(theme.panel_bg);
    let differs_style =
        Style::default().fg(theme.error_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD);
    let sep = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    let absent = Style::default().bg(super::render::mix(theme.panel_bg, theme.panel_border, 0.5));
    let byte = |side: usize, i: usize| data[side].get(i).copied();

    // The offset column, once per file's block of rows.
    let blocks: Vec<u16> = {
        let mut ys: Vec<u16> = panes.iter().map(|p| p.y).collect();
        ys.dedup();
        ys
    };
    for &y in &blocks {
        for r in 0..rows {
            let base = r * BYTES_PER_ROW as usize;
            let off = v.top + base as u64;
            if off >= v.len() {
                break;
            }
            let row_differs =
                (base..base + BYTES_PER_ROW as usize).any(|i| byte(0, i) != byte(1, i));
            let style = if row_differs { differs_style } else { offset_style };
            let text = format!("{off:0w$X}", w = geom.off_w as usize);
            f.buffer_mut().set_string(body.x, y + r as u16, text, style);
        }
    }
    if layout == Layout::Stacked && rows > 0 {
        let y = body.y + rows as u16;
        let label = format!(" {} ", v.names[1]);
        let w = body.width as usize;
        let rule = format!("──{label}{}", "─".repeat(w.saturating_sub(label.chars().count() + 2)));
        f.buffer_mut().set_string(body.x, y, ellipsize(&rule, w), sep);
    }

    for p in &panes {
        let other = 1 - p.side;
        for r in 0..rows {
            let base = r * BYTES_PER_ROW as usize;
            let y = p.y + r as u16;
            if v.top + base as u64 >= v.len() {
                break;
            }
            let mut spans = Vec::with_capacity(2 * BYTES_PER_ROW as usize + 2);
            if p.kind == Kind::Ascii {
                spans.push(Span::styled("|", sep));
            }
            for j in 0..BYTES_PER_ROW as usize {
                let off = v.top + (base + j) as u64;
                let b = byte(p.side, base + j);
                let style = if off == v.cursor && off < v.len() {
                    theme.cursor
                } else if b.is_none() {
                    absent
                } else if b != byte(other, base + j) {
                    differs_style
                } else {
                    normal
                };
                let cell = match (p.kind, b) {
                    (Kind::Hex, Some(b)) => format!("{b:02X}"),
                    (Kind::Hex, None) => "  ".to_string(),
                    (Kind::Ascii, Some(b)) if (0x20..0x7f).contains(&b) => (b as char).to_string(),
                    (Kind::Ascii, Some(_)) => ".".to_string(),
                    (Kind::Ascii, None) => " ".to_string(),
                };
                spans.push(Span::styled(cell, style));
                if p.kind == Kind::Hex {
                    spans.push(Span::styled(if j == 7 { "  " } else { " " }, sep));
                }
            }
            let x = if p.kind == Kind::Ascii { p.x - 1 } else { p.x };
            match p.kind {
                Kind::Ascii => spans.push(Span::styled("|", sep)),
                // Between the files: a rule after the first one's last column.
                Kind::Hex if p.side == 0 && layout == Layout::HexOnly => {
                    spans.push(Span::styled(" │ ", sep));
                }
                Kind::Hex => {}
            }
            let line = Line::from(spans);
            let width = (body.x + body.width).saturating_sub(x);
            f.render_widget(Paragraph::new(line), Rect { x, y, width, height: 1 });
        }
    }
    if layout == Layout::Wide {
        let x = panes[1].x + ASCII_W - 1;
        for r in 0..rows as u16 {
            f.buffer_mut().set_string(x, body.y + r, " │ ", sep);
        }
    }
    render_footer(f, footer, v, theme);
}

/// The status row; `field` names the first file's field at the cursor.
fn render_status(f: &mut Frame, area: Rect, v: &HexDiffView, field: Option<String>, theme: &Theme) {
    let trd = crate::l10n::trd;
    let (n, capped, inside) = v.runs_status();
    let plus = if capped { "+" } else { "" };
    let pos = inside.map(|i| format!(" [{}/{n}]", i + 1)).unwrap_or_default();
    let scanning = if v.busy() {
        format!("   {} {}%", trd("Scanning…"), v.progress())
    } else {
        String::new()
    };
    let [la, lb] = v.lens();
    let sizes = if la != lb { format!("   {} {la} / {lb}", trd("Size")) } else { String::new() };
    let field = field.map(|f| format!("   {f}")).unwrap_or_default();
    let tail = format!(
        "   {n}{plus} {}{pos}{scanning}   {} 0x{:X}{sizes}{field} ",
        trd("diff(s)"),
        trd("Offset"),
        v.cursor
    );
    let half = (area.width as usize).saturating_sub(tail.chars().count() + 8) / 2;
    let text = format!(
        " {}  ⇄  {}{tail}",
        ellipsize(&v.names[0], half.max(4)),
        ellipsize(&v.names[1], half.max(4))
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            pad_right(&text, area.width as usize),
            theme.menubar.add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

fn render_footer(f: &mut Frame, area: Rect, v: &HexDiffView, theme: &Theme) {
    let hint = if !v.status.is_empty() {
        v.status.clone()
    } else if v.waiting() {
        "Scanning for the next difference…".to_string()
    } else {
        "↑↓←→ move   Ctrl-↑↓ / n N next / previous difference   F5 go to offset   Esc close"
            .to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            pad_right(&format!(" {hint}"), area.width as usize),
            theme.fkey_label,
        ))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::super::hex::Origin;
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn view(a: &[u8], b: &[u8]) -> HexDiffView {
        let origins = [Origin::Mem(a.to_vec().into()), Origin::Mem(b.to_vec().into())];
        let mut v = HexDiffView::open(["left.bin".into(), "right.bin".into()], origins).unwrap();
        while v.poll() {
            std::thread::yield_now();
        }
        v
    }

    fn screen(v: &mut HexDiffView, w: u16, h: u16) -> (String, ratatui::buffer::Buffer) {
        let theme = Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| render(f, f.area(), v, &theme)).unwrap();
        let b = t.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
            s.push('\n');
        }
        (s, b)
    }

    /// Print the binary compare of `$HEXDIFF_A` and `$HEXDIFF_B` at
    /// `$HEXDIFF_SIZE` (`WxH`), after stepping `$HEXDIFF_STEPS` differences on:
    /// `HEXDIFF_A=a HEXDIFF_B=b cargo test --bin rc diff::hexrender::tests::preview -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn preview() {
        let path = |k: &str| std::path::PathBuf::from(std::env::var(k).expect(k));
        let (a, b) = (path("HEXDIFF_A"), path("HEXDIFF_B"));
        let (w, h) = std::env::var("HEXDIFF_SIZE")
            .ok()
            .and_then(|s| {
                s.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            })
            .unwrap_or((160, 30));
        let file_name = |p: &std::path::Path| p.file_name().unwrap().to_string_lossy().into_owned();
        let names = [file_name(&a), file_name(&b)];
        let mut v = HexDiffView::open(names, [Origin::File(a), Origin::File(b)]).unwrap();
        while v.busy() {
            v.poll();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = screen(&mut v, w, h);
        let steps: usize =
            std::env::var("HEXDIFF_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        for _ in 0..steps {
            v.handle_key(ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char('n'),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ));
        }
        println!("{}", screen(&mut v, w, h).0);
    }

    #[test]
    fn both_files_show_side_by_side_with_the_differences_coloured() {
        let a: Vec<u8> = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ\0".to_vec();
        let mut b = a.clone();
        b[3] = b'x';
        b.push(0xff);
        let mut v = view(&a, &b);
        let (s, buf) = screen(&mut v, 160, 10);
        assert!(s.contains("left.bin  ⇄  right.bin"), "{s}");
        assert!(s.contains("2 diff(s)") && s.contains("Size 27 / 28"), "{s}");
        assert!(s.contains("41 42 43 44") && s.contains("41 42 43 78"), "{s}");
        assert!(s.contains("|ABCDEFGHIJKLMNOP|") && s.contains("|ABCxEFGHIJKLMNOP|"), "{s}");
        let theme = Theme::mc();
        // Offset 3 on the left is at hex column 3 of the left pane.
        let (_, panes, _) = panes(Rect::new(0, 1, 160, 8), 8);
        let x = panes[0].x + hex_x(3);
        assert_eq!(buf[(x, 1)].fg, theme.error_fg, "the differing byte is coloured");
        assert_ne!(buf[(panes[0].x + hex_x(2), 1)].fg, theme.error_fg);
        // The longer file's extra byte against nothing on the other side.
        let x = panes[2].x + hex_x(11);
        assert_eq!(buf[(x, 2)].symbol(), "F");
        assert_eq!(buf[(x + 1, 2)].symbol(), "F");
    }

    #[test]
    fn narrower_screens_drop_the_ascii_then_stack_the_files() {
        let a = vec![0u8; 64];
        let mut b = a.clone();
        b[20] = 1;
        let mut v = view(&a, &b);
        let _ = screen(&mut v, 160, 12);
        assert_eq!(v.layout, Layout::Wide);
        let (s, _) = screen(&mut v, 120, 12);
        assert_eq!(v.layout, Layout::HexOnly);
        assert!(!s.contains('|'), "no ASCII columns:\n{s}");
        let (s, _) = screen(&mut v, 80, 12);
        assert_eq!(v.layout, Layout::Stacked);
        assert!(s.contains("right.bin ─"), "a rule names the second file:\n{s}");
        assert_eq!(v.view_rows, 4);
    }

    #[test]
    fn a_click_puts_the_cursor_on_either_files_byte() {
        let a = vec![0u8; 64];
        let mut v = view(&a, &a);
        let _ = screen(&mut v, 160, 12);
        let (_, panes, _) = panes(v.body, 8);
        let at = |x, y| offset_at(&v, x, y);
        assert_eq!(at(panes[0].x + hex_x(5), panes[0].y + 1), Some(21));
        assert_eq!(at(panes[1].x + 2, panes[1].y), Some(2), "left ASCII");
        assert_eq!(at(panes[3].x + 15, panes[3].y + 3), Some(63), "right ASCII");
        assert_eq!(at(panes[2].x + hex_x(0), panes[2].y + 4), None, "past the end");
        assert_eq!(at(panes[0].x + 2, panes[0].y), None, "between cells");
    }
}
