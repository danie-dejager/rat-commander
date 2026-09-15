//! The spreadsheet grid: a cursor cell, a view scrolled by whole rows and
//! columns, and the drawing of it all.
//!
//! The grid holds positions and column widths but never the table itself. Each
//! frame the owner asks [`GridView::layout`] which records will be on screen,
//! reads just those, and hands them to [`render`] — so a table paged from a
//! multi-gigabyte file costs no more to draw than a small one.
//!
//! Rows are *records*, numbered from 0 in the file. With [`GridView::header`]
//! on, record 0 holds the column titles: it stays pinned above the body as the
//! heading row, and the body starts at record 1. With it off, the heading row
//! shows the column letters instead.

use super::csv::{column_name, looks_numeric};
use crate::ui::theme::Theme;
use crate::util::scroll::scroll_to_visible;
use crate::util::text::{pad_left, pad_right, truncate_width};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::borrow::Cow;
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

/// Narrowest a column is measured to, so a column of one-letter codes still has
/// room for its letter heading.
pub const MIN_WIDTH: u16 = 3;
/// Widest a column is measured to. A long text column is cut rather than
/// pushing every other column off the screen; the cell bar shows it whole.
pub const MAX_AUTO_WIDTH: u16 = 40;
/// Widest a column can be made by hand.
pub const MAX_WIDTH: u16 = 250;
/// Width of a column nothing has measured yet.
const DEFAULT_WIDTH: u16 = 8;
/// What each column adds around its text: a space either side and the `│`
/// before it.
const CELL_CHROME: u16 = 3;

/// Where the cursor is, what part of the table is on screen, and how wide each
/// column is drawn.
#[derive(Debug, Clone, Default)]
pub struct GridView {
    /// The cursor's record.
    pub row: usize,
    /// The cursor's column.
    pub col: usize,
    /// First record drawn in the body (never the header record).
    pub top: usize,
    /// First column drawn.
    pub left: usize,
    /// Whether record 0 is column titles rather than data.
    pub header: bool,
    widths: Vec<u16>,
    /// Body rows on screen, from the last layout.
    pub page_rows: usize,
    /// Geometry of the last layout, for [`GridView::cell_at`] and [`render`].
    area: Rect,
    gutter: u16,
    /// Columns the last layout fitted: (column, x of its `│`, text width).
    shown: Vec<(usize, u16, u16)>,
}

impl GridView {
    pub fn new(header: bool) -> Self {
        GridView { header, top: usize::from(header), ..GridView::default() }
    }

    /// The first record of the body: 1 below a header, else 0.
    pub fn first_body(&self) -> usize {
        usize::from(self.header)
    }

    /// How wide column `col` is drawn.
    pub fn width(&self, col: usize) -> u16 {
        self.widths.get(col).copied().unwrap_or(DEFAULT_WIDTH)
    }

    /// Size the columns to fit `rows` — a sample of the table, measured once
    /// when it opens. Widths only grow, so measuring again as columns come into
    /// view never shifts the ones already laid out.
    pub fn measure(&mut self, rows: &[Vec<String>]) {
        for row in rows {
            for (c, cell) in row.iter().enumerate() {
                let w = (display(cell).width().min(MAX_AUTO_WIDTH as usize) as u16).max(MIN_WIDTH);
                if c >= self.widths.len() {
                    self.widths.resize(c + 1, 0);
                }
                self.widths[c] = self.widths[c].max(w);
            }
        }
    }

    /// Measure only the columns no earlier measurement reached — ones first
    /// seen in the rows now on screen.
    pub fn measure_new_columns(&mut self, rows: &[Vec<String>]) {
        let known = self.widths.len();
        let mut wide: Vec<u16> = Vec::new();
        for row in rows {
            for (c, cell) in row.iter().enumerate().skip(known) {
                let w = (display(cell).width().min(MAX_AUTO_WIDTH as usize) as u16).max(MIN_WIDTH);
                if c - known >= wide.len() {
                    wide.resize(c - known + 1, MIN_WIDTH);
                }
                wide[c - known] = wide[c - known].max(w);
            }
        }
        self.widths.extend(wide);
    }

    /// Make column `col` `delta` columns wider (narrower when negative).
    pub fn widen(&mut self, col: usize, delta: i32) {
        if col >= self.widths.len() {
            self.widths.resize(col + 1, DEFAULT_WIDTH);
        }
        let w = (i32::from(self.widths[col]) + delta).clamp(1, i32::from(MAX_WIDTH));
        self.widths[col] = w as u16;
    }

    /// Turn the header record on or off, keeping the body below it.
    pub fn set_header(&mut self, on: bool) {
        self.header = on;
        self.top = self.top.max(self.first_body());
        if !on && self.top == 1 {
            self.top = 0;
        }
    }

    /// Put the cursor on (`row`, `col`), clamped to the last record and column.
    pub fn move_to(&mut self, row: usize, col: usize, last_row: usize, last_col: usize) {
        self.row = row.min(last_row);
        self.col = col.min(last_col);
    }

    /// Scroll the body by `delta` records, taking the cursor along when it
    /// would otherwise leave the screen.
    pub fn scroll(&mut self, delta: isize, last_row: usize) {
        let first = self.first_body();
        let page = self.page_rows.max(1);
        let max_top = last_row.saturating_sub(page - 1).max(first);
        self.top = (self.top as isize).saturating_add(delta).clamp(first as isize, max_top as isize)
            as usize;
        if self.row >= first {
            self.row = self.row.clamp(self.top, (self.top + page - 1).min(last_row));
        }
    }

    /// Fit the grid to `area` for a table of `total` records (at least) and
    /// `ncols` columns: scroll so the cursor cell is on screen, and work out
    /// which columns fit. Returns the body records to read for [`render`].
    pub fn layout(&mut self, area: Rect, total: usize, ncols: usize) -> Range<usize> {
        self.area = area;
        self.gutter = (total.max(1).to_string().len() as u16).max(3) + 1;
        let body_h = area.height.saturating_sub(2) as usize;
        self.page_rows = body_h;
        let first = self.first_body();
        self.top = self.top.max(first);
        if self.row >= first {
            self.top = scroll_to_visible(self.top, self.row, body_h.max(1)).max(first);
        }

        let avail = area.width.saturating_sub(self.gutter);
        let span = |v: &Self, c: usize| v.width(c) + CELL_CHROME;
        self.left = self.left.min(self.col);
        // Scroll right until the cursor's column fits whole, or is the first.
        while self.left < self.col
            && (self.left..=self.col).map(|c| u32::from(span(self, c))).sum::<u32>()
                > u32::from(avail)
        {
            self.left += 1;
        }
        self.shown.clear();
        let mut x = area.x + self.gutter;
        let right = area.x + area.width;
        let mut c = self.left;
        while x < right && c < ncols.max(self.col + 1) {
            let text = self.width(c).min(right.saturating_sub(x + CELL_CHROME));
            self.shown.push((c, x, text));
            x = x.saturating_add(span(self, c));
            c += 1;
        }
        self.top..self.top + body_h
    }

    /// The cell under screen position (`x`, `y`), from the last layout: a
    /// record and a column. A click in the row numbers keeps the column.
    pub fn cell_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let a = self.area;
        if a.width == 0 || x < a.x || x >= a.x + a.width || y < a.y + 1 || y >= a.y + a.height {
            return None;
        }
        let row = if y == a.y + 1 {
            if !self.header {
                return None;
            }
            0
        } else {
            self.top + (y - a.y - 2) as usize
        };
        let col = self
            .shown
            .iter()
            .find(|&&(_, cx, w)| x >= cx && x < cx + w + CELL_CHROME)
            .map_or(self.col, |&(c, ..)| c);
        Some((row, col))
    }
}

/// What [`render`] draws, as read by the grid's owner for the last layout.
pub struct GridRows<'a> {
    /// Record 0's fields, when it is the header.
    pub header: Option<&'a [String]>,
    /// The body records from [`GridView::top`] on, as fields.
    pub rows: &'a [Vec<String>],
    /// Records in the table — a lower bound, and shown with `+`, until `exact`.
    pub total: usize,
    pub exact: bool,
    /// Whether a record holds a "Find all" hit.
    pub found: &'a dyn Fn(usize) -> bool,
}

/// Draw the grid into the area of the last [`GridView::layout`]: the cell bar,
/// the heading row, and the body. `edit` puts a cell being edited in the cell
/// bar — its text and the caret, as a char index — and the returned position
/// is where the terminal's cursor goes for it.
pub fn render(
    f: &mut Frame,
    view: &GridView,
    data: &GridRows,
    theme: &Theme,
    edit: Option<(&str, usize)>,
) -> Option<Position> {
    let area = view.area;
    if area.width < 4 || area.height == 0 {
        return None;
    }
    let base = Style::default().fg(theme.text_fg).bg(theme.panel_bg);
    let dim = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    let head = Style::default().fg(theme.header_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD);
    let mark = theme.cursor_inactive;
    let found_bg = theme.cursor_inactive.bg.unwrap_or(theme.panel_bg);
    let width = area.width as usize;

    // The cell under the cursor, whole.
    let current: Option<&str> = if view.row == 0 && view.header {
        data.header.and_then(|h| h.get(view.col)).map(String::as_str)
    } else {
        view.row
            .checked_sub(view.top)
            .and_then(|i| data.rows.get(i))
            .and_then(|r| r.get(view.col))
            .map(String::as_str)
    };
    let reference = format!(" {}{} ", column_name(view.col), view.row + 1);
    let mut spans = vec![Span::styled(reference.clone(), mark)];
    let mut used = reference.width() + 1;
    spans.push(Span::styled(" ", base));
    let title = data.header.filter(|_| view.row != 0).and_then(|h| h.get(view.col));
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        let t = format!("{}: ", display(t));
        let (t, w) = truncate_width(&t, width.saturating_sub(used) / 2);
        used += w;
        spans.push(Span::styled(t, dim));
    }
    let mut caret = None;
    match edit {
        Some((text, at)) => {
            // Each character one cell of its own, so the caret maps straight
            // onto the screen; scrolled so the caret stays in view.
            let chars: Vec<String> = text.chars().map(|c| display_char(c).to_string()).collect();
            let at = at.min(chars.len());
            let room = width.saturating_sub(used + 1);
            let mut skip = 0;
            while skip < at && chars[skip..at].concat().width() > room {
                skip += 1;
            }
            let shown = chars[skip..].concat();
            let (shown, _) = truncate_width(&shown, room + 1);
            let before = chars[skip..at].concat().width();
            caret = Some(Position::new(area.x + (used + before) as u16, area.y));
            spans.push(Span::styled(shown, base));
        }
        None => {
            let value = display(current.unwrap_or(""));
            spans.push(Span::styled(cut(&value, width.saturating_sub(used)), base));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(base), Rect { height: 1, ..area });
    if area.height < 2 {
        return caret;
    }

    // One row of the grid: the row number, then each column that fits.
    let gutter = view.gutter as usize;
    let row_line = |number: Option<usize>,
                    cells: &dyn Fn(usize) -> (String, Style, bool),
                    fill: Style,
                    number_style: Style| {
        let mut spans = Vec::with_capacity(view.shown.len() * 2 + 2);
        let label = number.map_or(String::new(), |n| (n + 1).to_string());
        spans.push(Span::styled(pad_left(&label, gutter - 1), number_style));
        spans.push(Span::styled(" ", fill));
        let mut used = gutter;
        let sep_style = Style { fg: Some(theme.panel_border), ..fill };
        for &(c, _, w) in &view.shown {
            if used >= width {
                break;
            }
            let (text, style, right) = cells(c);
            let w = w as usize;
            let body =
                if right { pad_left(&cut(&text, w), w) } else { pad_right(&cut(&text, w), w) };
            let cell = format!(" {body} ");
            for (t, s) in [("│".to_string(), sep_style), (cell, style)] {
                let (t, tw) = truncate_width(&t, width.saturating_sub(used));
                used += tw;
                spans.push(Span::styled(t, s));
            }
        }
        // The table's right edge, after the last column.
        if used < width {
            spans.push(Span::styled("│", sep_style));
            used += 1;
        }
        if used < width {
            spans.push(Span::styled(" ".repeat(width - used), fill));
        }
        Line::from(spans)
    };

    let mut lines = Vec::with_capacity(area.height as usize - 1);
    // The heading row: the titles, or the column letters.
    let heading = |c: usize| -> (String, Style, bool) {
        let on_cursor = c == view.col;
        match data.header {
            Some(h) => {
                let text = display(h.get(c).map_or("", String::as_str)).into_owned();
                let style = if view.row == 0 && on_cursor {
                    theme.cursor
                } else if on_cursor {
                    mark.add_modifier(Modifier::BOLD)
                } else {
                    head
                };
                (text, style, false)
            }
            None => {
                let name = column_name(c);
                let w = view.width(c) as usize;
                let pad = w.saturating_sub(name.len()) / 2;
                let text = format!("{}{name}", " ".repeat(pad));
                (text, if on_cursor { mark } else { head }, false)
            }
        }
    };
    let heading_number = if view.header { Some(0) } else { None };
    let number_style = if view.header && view.row == 0 { mark } else { dim };
    lines.push(row_line(heading_number, &heading, base, number_style));

    for (i, rec) in data.rows.iter().enumerate().take(area.height as usize - 2) {
        let r = view.top + i;
        let fill = if (data.found)(r) { Style { bg: Some(found_bg), ..base } } else { base };
        let cell = |c: usize| -> (String, Style, bool) {
            let text = rec.get(c).map_or("", String::as_str);
            let style = if r == view.row && c == view.col { theme.cursor } else { fill };
            (display(text).into_owned(), style, looks_numeric(text))
        };
        let number_style =
            if r == view.row { mark } else { Style { fg: Some(theme.panel_border), ..fill } };
        lines.push(row_line(Some(r), &cell, fill, number_style));
    }
    let body = Rect { y: area.y + 1, height: area.height - 1, ..area };
    f.render_widget(Paragraph::new(lines).style(base), body);
    // Below the last record: the "more" marker when the count is not final.
    if !data.exact && data.rows.len() + 2 < area.height as usize {
        let y = area.y + 2 + data.rows.len() as u16;
        let row = Rect { y, height: 1, ..area };
        f.render_widget(Paragraph::new(Span::styled(format!(" {}+", data.total), dim)), row);
    }
    caret
}

/// A cell's text as one screen line: a line break as `↵`, other control
/// characters (tabs among them) as spaces.
pub fn display(s: &str) -> Cow<'_, str> {
    if !s.chars().any(char::is_control) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace("\r\n", "\n").chars().map(display_char).collect())
}

fn display_char(c: char) -> char {
    match c {
        '\n' | '\r' => '↵',
        c if c.is_control() => ' ',
        c => c,
    }
}

/// `s` in at most `w` columns, ending in `…` when it had to be cut.
fn cut(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let (mut t, _) = truncate_width(s, w - 1);
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The screen column `needle` starts at in a drawn row.
    fn column(row: &str, needle: &str) -> u16 {
        row[..row.find(needle).unwrap()].chars().count() as u16
    }

    fn rows(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect()
    }

    /// Lay out and draw a table, returning each screen row as text and the
    /// buffer for style checks.
    fn draw(
        view: &mut GridView,
        table: &[Vec<String>],
        w: u16,
        h: u16,
        edit: Option<(&str, usize)>,
    ) -> (Vec<String>, ratatui::buffer::Buffer, Option<Position>) {
        let theme = Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let ncols = table.iter().map(Vec::len).max().unwrap_or(0);
        let mut caret = None;
        t.draw(|f| {
            let range = view.layout(f.area(), table.len(), ncols);
            let body: Vec<Vec<String>> = table
                .get(range.start.min(table.len())..range.end.min(table.len()))
                .unwrap_or(&[])
                .to_vec();
            let header = view.header.then(|| table[0].as_slice());
            let data = GridRows {
                header,
                rows: &body,
                total: table.len(),
                exact: true,
                found: &|_| false,
            };
            caret = render(f, view, &data, &theme, edit);
        })
        .unwrap();
        let b = t.backend().buffer().clone();
        let text =
            (0..h).map(|y| (0..w).map(|x| b[(x, y)].symbol().to_string()).collect()).collect();
        (text, b, caret)
    }

    #[test]
    fn a_table_draws_its_titles_separators_and_right_aligned_numbers() {
        let table = rows(&[&["name", "qty"], &["apple", "12"], &["fig", "3"]]);
        let mut view = GridView::new(true);
        view.measure(&table);
        view.row = 1;
        let (text, b, _) = draw(&mut view, &table, 30, 5, None);
        assert!(text[0].contains("A2") && text[0].contains("name: apple"), "{:?}", text[0]);
        assert!(text[1].contains("│ name  │ qty │"), "{:?}", text[1]);
        assert!(text[2].contains("│ apple │  12 │"), "numbers are right-aligned: {:?}", text[2]);
        assert!(text[3].contains("│ fig   │   3 │"), "{:?}", text[3]);
        assert!(
            text[2].trim_start().starts_with('2'),
            "row numbers count the header: {:?}",
            text[2]
        );
        let theme = Theme::mc();
        // The cursor cell, and only it, has the cursor's colours.
        assert_eq!(b[(column(&text[2], "apple"), 2)].bg, theme.cursor.bg.unwrap());
        assert_ne!(b[(column(&text[3], "fig"), 3)].bg, theme.cursor.bg.unwrap());
        assert!(b[(column(&text[1], "name"), 1)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn without_a_header_the_columns_are_lettered_and_long_cells_cut() {
        let table = rows(&[&["a very long value indeed", "x\ny"]]);
        let mut view = GridView::new(false);
        view.widen(0, 2);
        let (text, _, _) = draw(&mut view, &table, 40, 4, None);
        assert!(text[1].contains('A') && text[1].contains('B'), "{:?}", text[1]);
        assert!(text[2].contains("a very lo…"), "cut with an ellipsis: {:?}", text[2]);
        assert!(text[2].contains("x↵y"), "a line break shows as ↵: {:?}", text[2]);
    }

    #[test]
    fn wide_characters_keep_the_separators_in_line() {
        let table = rows(&[&["日本語", "z"], &["ab", "z"]]);
        let mut view = GridView::new(false);
        view.measure(&table);
        let (_, b, _) = draw(&mut view, &table, 30, 4, None);
        let seps = |y: u16| (0..30).filter(|&x| b[(x, y)].symbol() == "│").collect::<Vec<_>>();
        assert_eq!(seps(2).len(), 3, "two columns and the right edge");
        assert_eq!(seps(2), seps(3), "a double-width title does not push the columns along");
    }

    #[test]
    fn the_cursor_column_is_scrolled_into_view() {
        let table = rows(&[&["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc", "dddddddddd"]]);
        let mut view = GridView::new(false);
        view.measure(&table);
        view.col = 3;
        let (text, _, _) = draw(&mut view, &table, 30, 4, None);
        assert!(view.left > 0);
        assert!(text[2].contains("dddddddddd"), "{:?}", text[2]);
        assert_eq!(view.cell_at(view.shown[0].1 + 2, 2), Some((0, view.left)));
    }

    #[test]
    fn scrolling_keeps_the_cursor_on_screen_and_the_header_pinned() {
        let table: Vec<Vec<String>> =
            (0..100).map(|i| vec![if i == 0 { "title".into() } else { i.to_string() }]).collect();
        let mut view = GridView::new(true);
        let (text, _, _) = draw(&mut view, &table, 20, 6, None);
        assert_eq!(view.top, 1);
        assert!(text[1].contains("title"));
        view.row = 50;
        let (text, _, _) = draw(&mut view, &table, 20, 6, None);
        assert!(text[1].contains("title"), "the titles stay put");
        assert!(text[5].contains("50"), "{text:?}");
        view.scroll(-10, 99);
        assert_eq!(view.row, view.top + view.page_rows - 1, "the cursor came along");
    }

    #[test]
    fn an_edited_cell_shows_in_the_cell_bar_with_the_caret_on_it() {
        let table = rows(&[&["one", "two"]]);
        let mut view = GridView::new(false);
        let (text, _, caret) = draw(&mut view, &table, 30, 4, Some(("hello", 2)));
        assert!(text[0].contains("A1") && text[0].contains("hello"));
        let at = text[0].find("hello").unwrap() as u16;
        assert_eq!(caret, Some(Position::new(at + 2, 0)));
    }
}
