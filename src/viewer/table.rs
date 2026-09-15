//! The table view: a CSV or TSV file drawn as a spreadsheet.
//!
//! It is the text view's other face, the way a Markdown file's rendering is —
//! F8 switches between the two — and it is paged from the file the same way:
//! only a record-start index is kept, extended as far as the cursor has been,
//! and only the records on screen are ever read. A record is not a line here:
//! a quoted field may hold line breaks, so the index is built by the CSV
//! scanner rather than by looking for newlines.

use super::{FOUND_LINES_MAX, GotoMode, Source, ViewMode, ViewerSignal, ViewerState};
use crate::sheet::csv::{self, Dialect, Scanner};
use crate::sheet::grid::{self, GridRows, GridView};
use crate::ui::theme::Theme;
use ratatui::Frame;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::Style;
use std::collections::HashSet;

/// Bytes the dialect is guessed from.
const SNIFF_BYTES: usize = 64 * 1024;
/// Most of one record read for display. A stray quote can make the rest of a
/// file one record; reading it whole would load the file.
const RECORD_CAP: usize = 64 * 1024;
/// How much of the table the column widths are measured from when it opens.
const MEASURE_RECORDS: usize = 1000;
const MEASURE_BYTES: usize = 1024 * 1024;
/// Bytes indexed per step, as the line index does.
const CHUNK: usize = 256 * 1024;

/// A table's record index and its grid.
pub(crate) struct TableView {
    dialect: Dialect,
    /// Byte offset of the start of every record in `[0, scanned)`.
    starts: Vec<usize>,
    scanned: usize,
    scanner: Scanner,
    pub(crate) grid: GridView,
    /// Records holding a "Find all" hit.
    found: HashSet<usize>,
    /// Most fields any record read so far has had.
    ncols: usize,
}

impl TableView {
    fn open(src: &Source, name: &str) -> Self {
        let len = src.len();
        let sample = src.read_range(0, SNIFF_BYTES);
        let dialect = csv::sniff(&sample, len <= SNIFF_BYTES, name);
        let mut t = TableView {
            dialect,
            starts: vec![0],
            scanned: 0,
            scanner: Scanner::new(dialect),
            grid: GridView::new(false),
            found: HashSet::new(),
            ncols: 0,
        };
        while t.starts.len() <= MEASURE_RECORDS && t.scanned < MEASURE_BYTES && t.scan_chunk(src) {}
        let n = t.count(len).min(MEASURE_RECORDS);
        let rows: Vec<Vec<String>> = (0..n).map(|i| t.fields(src, i)).collect();
        let header = n > 1 && csv::guess_header(&rows[0]);
        t.grid = GridView::new(header);
        t.grid.measure(&rows);
        t
    }

    /// Index one more chunk; false once the whole file is.
    fn scan_chunk(&mut self, src: &Source) -> bool {
        let len = src.len();
        if self.scanned >= len {
            return false;
        }
        let buf = src.read_range(self.scanned, (self.scanned + CHUNK).min(len));
        if buf.is_empty() {
            self.scanned = len; // a failed read ends the index rather than looping
            return false;
        }
        self.scanner.feed(&buf, self.scanned, &mut self.starts);
        self.scanned += buf.len();
        true
    }

    /// Index far enough to know where record `i` ends.
    fn extend_to_record(&mut self, src: &Source, i: usize) {
        while self.starts.len() <= i.saturating_add(1) && self.scan_chunk(src) {}
    }

    fn extend_to_byte(&mut self, src: &Source, off: usize) {
        while self.scanned <= off && self.scan_chunk(src) {}
    }

    fn index_fully(&mut self, src: &Source) {
        while self.scan_chunk(src) {}
    }

    fn exact(&self, src: &Source) -> bool {
        self.scanned >= src.len()
    }

    /// Records known so far — all of them once the index is complete. A line
    /// break ending the file does not start one more.
    fn count(&self, len: usize) -> usize {
        let n = self.starts.len();
        if self.scanned >= len && self.starts.last() == Some(&len) { n - 1 } else { n }
    }

    /// The bytes of record `i`, at most [`RECORD_CAP`] of them.
    fn record(&mut self, src: &Source, i: usize) -> (usize, Vec<u8>) {
        self.extend_to_record(src, i);
        if i >= self.count(src.len()) {
            return (src.len(), Vec::new());
        }
        let start = self.starts[i];
        let end = self.starts.get(i + 1).copied().unwrap_or(src.len());
        (start, src.read_range(start, end.min(start + RECORD_CAP)))
    }

    /// Record `i` as its fields' values.
    fn fields(&mut self, src: &Source, i: usize) -> Vec<String> {
        let (_, bytes) = self.record(src, i);
        let d = self.dialect;
        let out: Vec<String> = csv::split(&bytes, d)
            .iter()
            .map(|f| csv::value(&bytes[f.start..f.end], f.quoted).into_owned())
            .collect();
        self.ncols = self.ncols.max(out.len());
        out
    }

    /// The record holding byte `off`.
    fn record_of_byte(&mut self, src: &Source, off: usize) -> usize {
        self.extend_to_byte(src, off);
        let i = match self.starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        i.min(self.count(src.len()).saturating_sub(1))
    }

    /// The cell holding byte `off`: its record, and the field it falls in.
    fn cell_of_byte(&mut self, src: &Source, off: usize) -> (usize, usize) {
        let row = self.record_of_byte(src, off);
        let (start, bytes) = self.record(src, row);
        let at = off.saturating_sub(start);
        let fields = csv::split(&bytes, self.dialect);
        let col = fields.iter().position(|f| at <= f.end).unwrap_or(fields.len().saturating_sub(1));
        self.ncols = self.ncols.max(fields.len());
        (row, col)
    }

    /// Put the cursor on (`row`, `col`), indexing as far as that row and
    /// clamping to the table.
    fn go(&mut self, src: &Source, row: usize, col: usize) {
        self.extend_to_record(src, row);
        let last_row = self.count(src.len()).saturating_sub(1);
        self.grid.move_to(row, col, last_row, self.ncols.saturating_sub(1));
    }
}

impl ViewerState {
    /// Whether the table is on screen: a table file in text mode with the
    /// table (rather than its raw text) chosen.
    pub(crate) fn table_active(&self) -> bool {
        self.is_sheet && self.sheet_render && self.mode == ViewMode::Text
    }

    /// Build the table view on first use.
    pub(super) fn ensure_table(&mut self) {
        if self.table.is_none() {
            self.table = Some(TableView::open(&self.src, &self.name));
        }
    }

    /// The table's cursor and size for the header: the cursor's record, the
    /// records known, and whether that count is final.
    pub(crate) fn table_status(&self) -> Option<(usize, usize, bool)> {
        let t = self.table.as_ref()?;
        Some((t.grid.row, t.count(self.src.len()), t.exact(&self.src)))
    }

    /// F8: switch between the table and the raw text, keeping the place — the
    /// record under the cursor becomes the top line, and back.
    pub(super) fn toggle_table(&mut self) {
        if self.table_active() {
            if let Some(t) = self.table.as_ref() {
                let off = t.starts.get(t.grid.row).copied().unwrap_or(0);
                self.sheet_render = false;
                self.extend_to_byte(off + 1);
                self.top = self.byte_to_line(off).min(self.max_top());
            } else {
                self.sheet_render = false;
            }
            return;
        }
        // The table has no follow mode: it would redraw under the cursor.
        self.follow = None;
        self.sheet_render = true;
        self.extend_to_line(self.top + 1);
        let off = self.line_starts.get(self.top).copied().unwrap_or(0);
        self.table_reveal_byte(off);
        if let Some(t) = self.table.as_mut() {
            t.grid.col = 0;
        }
    }

    /// Move the table cursor to the cell holding byte `off` — where a search
    /// hit, a byte-offset Goto or a find-file hit lands.
    pub(super) fn table_reveal_byte(&mut self, off: usize) {
        self.ensure_table();
        let (Some(t), src) = (self.table.as_mut(), &self.src) else { return };
        let (row, col) = t.cell_of_byte(src, off);
        t.grid.row = row;
        t.grid.col = col;
    }

    /// F3 on a find-file content hit: open at the record holding that line.
    pub fn goto_hit_line(&mut self, line: usize) {
        if !self.table_active() {
            self.goto(&line.to_string(), GotoMode::Line);
            return;
        }
        let li = line.saturating_sub(1);
        self.extend_to_line(li + 1);
        let off = self.line_starts.get(li).copied().unwrap_or(0);
        self.table_reveal_byte(off);
    }

    /// Goto in the table: a row by number or by percentage, or the record
    /// holding a byte offset.
    pub(super) fn goto_table(&mut self, v: &str, mode: GotoMode) -> bool {
        self.ensure_table();
        let (Some(t), src) = (self.table.as_mut(), &self.src) else { return false };
        let row = match mode {
            GotoMode::Line => {
                let Ok(n) = v.parse::<usize>() else { return false };
                n.saturating_sub(1)
            }
            GotoMode::Percent => {
                let Ok(p) = v.parse::<f64>() else { return false };
                t.index_fully(src);
                let last = t.count(src.len()).saturating_sub(1);
                (last as f64 * p.clamp(0.0, 100.0) / 100.0).round() as usize
            }
            GotoMode::DecimalOffset | GotoMode::HexOffset => {
                let parsed = if mode == GotoMode::DecimalOffset {
                    v.parse::<usize>().ok()
                } else {
                    let hex = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")).unwrap_or(v);
                    usize::from_str_radix(hex, 16).ok()
                };
                let Some(off) = parsed else { return false };
                t.record_of_byte(src, off)
            }
        };
        let col = t.grid.col;
        t.go(src, row, col);
        true
    }

    /// "Find all" in the table: mark every record holding a match, and move to
    /// the first.
    pub(super) fn table_find_all(&mut self) {
        self.ensure_table();
        if let Some(t) = self.table.as_mut() {
            t.found.clear();
        }
        let Some(needle) = self.needle() else { return };
        let mut at = 0;
        let mut first = None;
        while let Some(off) = self.scan(&needle, at) {
            first.get_or_insert(off);
            let (Some(t), src) = (self.table.as_mut(), &self.src) else { return };
            let row = t.record_of_byte(src, off);
            t.found.insert(row);
            if t.found.len() >= FOUND_LINES_MAX {
                break;
            }
            // On to the next record: one hit marks a record.
            at = t.starts.get(row + 1).copied().unwrap_or(off + 1).max(off + 1);
        }
        if let Some(off) = first {
            self.last_match = Some(off);
            self.table_reveal_byte(off);
        }
    }

    /// Whether record `row` holds a "Find all" hit.
    #[cfg(test)]
    pub(crate) fn table_found(&self, row: usize) -> bool {
        self.table.as_ref().is_some_and(|t| t.found.contains(&row))
    }

    /// Keys while the table is up. The cursor moves by cell; the viewer's own
    /// keys (search, goto, the mode cycle, quitting) work as they always do.
    pub(super) fn handle_table_key(&mut self, key: KeyEvent) -> ViewerSignal {
        self.ensure_table();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let back_tab = key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT));
        match key.code {
            KeyCode::F(8) => {
                self.toggle_table();
                return ViewerSignal::Stay;
            }
            KeyCode::F(4) => {
                // To the hex view, at the record the cursor is on.
                let off = self
                    .table
                    .as_ref()
                    .and_then(|t| t.starts.get(t.grid.row).copied())
                    .unwrap_or(0);
                let signal = self.handle_plain_view_key(key);
                if self.mode == ViewMode::Hex {
                    self.top = (off / 16).min(self.max_top());
                }
                return signal;
            }
            KeyCode::Char('n') => {
                self.find_next();
                return ViewerSignal::Stay;
            }
            KeyCode::F(1)
            | KeyCode::F(3)
            | KeyCode::F(5)
            | KeyCode::F(7)
            | KeyCode::F(10)
            | KeyCode::Esc
            | KeyCode::Char('q') => return self.handle_plain_view_key(key),
            _ => {}
        }
        let (Some(t), src) = (self.table.as_mut(), &self.src) else { return ViewerSignal::Stay };
        let (row, col) = (t.grid.row, t.grid.col);
        let page = t.grid.page_rows.saturating_sub(1).max(1);
        match key.code {
            _ if back_tab => {
                if col > 0 {
                    t.go(src, row, col - 1);
                } else if row > 0 {
                    let last = t.fields(src, row - 1).len().saturating_sub(1);
                    t.go(src, row - 1, last);
                }
            }
            KeyCode::Tab => {
                if col + 1 < t.fields(src, row).len() {
                    t.go(src, row, col + 1);
                } else if row + 1 < {
                    t.extend_to_record(src, row + 1);
                    t.count(src.len())
                } {
                    t.go(src, row + 1, 0);
                }
            }
            KeyCode::Up => t.go(src, row.saturating_sub(1), col),
            KeyCode::Down => t.go(src, row + 1, col),
            KeyCode::PageUp => t.go(src, row.saturating_sub(page), col),
            KeyCode::PageDown => t.go(src, row + page, col),
            KeyCode::Left if ctrl => t.grid.widen(col, -1),
            KeyCode::Right if ctrl => t.grid.widen(col, 1),
            KeyCode::Char('<') => t.grid.widen(col, -1),
            KeyCode::Char('>') => t.grid.widen(col, 1),
            KeyCode::Left => t.go(src, row, col.saturating_sub(1)),
            KeyCode::Right => {
                t.fields(src, row);
                t.go(src, row, col + 1);
            }
            KeyCode::Home if ctrl => t.go(src, 0, 0),
            KeyCode::End if ctrl => {
                t.index_fully(src);
                t.go(src, usize::MAX, col);
            }
            KeyCode::Home => t.go(src, row, 0),
            KeyCode::End => {
                let last = t.fields(src, row).len().saturating_sub(1);
                t.go(src, row, last);
            }
            KeyCode::F(2) => {
                let on = !t.grid.header;
                t.grid.set_header(on);
            }
            _ => {}
        }
        ViewerSignal::Stay
    }

    /// The mouse on the table: the wheel scrolls, a click picks a cell.
    pub(super) fn handle_table_mouse(&mut self, ev: MouseEvent) {
        self.ensure_table();
        let (Some(t), src) = (self.table.as_mut(), &self.src) else { return };
        match ev.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let delta = if ev.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                t.extend_to_record(src, t.grid.top + t.grid.page_rows + 3);
                let last = t.count(src.len()).saturating_sub(1);
                t.grid.scroll(delta, last);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((row, col)) = t.grid.cell_at(ev.column, ev.row)
                    && row < t.count(src.len())
                {
                    t.go(src, row, col);
                }
            }
            _ => {}
        }
    }
}

/// Draw the table into the viewer's content area.
pub(super) fn render(f: &mut Frame, area: Rect, v: &mut ViewerState, theme: &Theme) {
    v.ensure_table();
    let (Some(t), src) = (v.table.as_mut(), &v.src) else { return };
    let base = Style::default().fg(theme.text_fg).bg(theme.panel_bg);
    f.render_widget(ratatui::widgets::Block::default().style(base), area);
    let len = src.len();
    // Index as far as a screen below the cursor, so the layout knows the rows
    // it is about to scroll through.
    t.extend_to_record(src, t.grid.row.max(t.grid.top) + area.height as usize);
    let total = t.count(len);
    t.grid.row = t.grid.row.min(total.saturating_sub(1));
    let range = t.grid.layout(area, total, t.ncols.max(1));
    let header = (t.grid.header && total > 0).then(|| t.fields(src, 0));
    let rows: Vec<Vec<String>> =
        (range.start..range.end.min(total)).map(|i| t.fields(src, i)).collect();
    // Columns first seen on this screen get measured, and the layout redone
    // with their widths: the rows to read do not depend on the widths.
    t.grid.measure_new_columns(&rows);
    t.grid.layout(area, total, t.ncols.max(1));
    let found = &t.found;
    let data = GridRows {
        header: header.as_deref(),
        rows: &rows,
        total,
        exact: t.scanned >= len,
        found: &|r| found.contains(&r),
    };
    grid::render(f, &t.grid, &data, theme, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEOPLE: &str = "name,age,city\nann,31,oslo\nbob,42,\"new\nyork\"\ncyd,27,rome\n";

    fn press(v: &mut ViewerState, code: KeyCode) -> ViewerSignal {
        v.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn cursor(v: &ViewerState) -> (usize, usize) {
        let g = &v.table.as_ref().expect("the table was built").grid;
        (g.row, g.col)
    }

    fn draw(v: &mut ViewerState, w: u16, h: u16) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let theme = Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| super::super::render::render(f, f.area(), v, &theme, None)).unwrap();
        let b = t.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| b[(x, y)].symbol().to_string()).collect()).collect()
    }

    fn search(v: &mut ViewerState, term: &str, find_all: bool) {
        v.apply_search(&crate::ui::dialog::SearchReplaceParams {
            replace: false,
            search: term.into(),
            replacement: String::new(),
            regex: false,
            case_sensitive: false,
            whole_words: false,
            backwards: false,
            hex: false,
            find_all,
        });
    }

    #[test]
    fn a_csv_file_opens_as_a_table_with_its_titles_pinned() {
        let mut v = ViewerState::new("people.csv".into(), PEOPLE.as_bytes().to_vec());
        assert!(v.table_active());
        let rows = draw(&mut v, 60, 10);
        assert!(rows[0].contains("[Table]") && rows[0].contains("1/4 rows"), "{:?}", rows[0]);
        assert!(rows[2].contains("name") && rows[2].contains("city"), "{:?}", rows[2]);
        assert!(rows[3].contains("│ ann  │"), "{:?}", rows[3]);
        assert!(
            rows[4].contains("new↵york"),
            "a quoted line break stays in its cell: {:?}",
            rows[4]
        );
        assert_eq!(v.footer_labels()[1], "Header");
        assert_eq!(v.footer_labels()[7], "Raw");
        // Plain text files are untouched.
        assert!(!ViewerState::new("people.txt".into(), PEOPLE.into()).table_active());
    }

    #[test]
    fn the_cursor_moves_by_cell_and_tab_runs_on_to_the_next_record() {
        let mut v = ViewerState::new("people.csv".into(), PEOPLE.as_bytes().to_vec());
        draw(&mut v, 60, 10);
        press(&mut v, KeyCode::Down);
        press(&mut v, KeyCode::Right);
        assert_eq!(cursor(&v), (1, 1));
        press(&mut v, KeyCode::End);
        assert_eq!(cursor(&v), (1, 2));
        press(&mut v, KeyCode::Tab);
        assert_eq!(cursor(&v), (2, 0), "Tab from the last field starts the next record");
        press(&mut v, KeyCode::BackTab);
        assert_eq!(cursor(&v), (1, 2));
        v.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(cursor(&v).0, 3, "the record after a quoted line break is one record");
        press(&mut v, KeyCode::Down);
        assert_eq!(cursor(&v).0, 3, "no record past the last");
        press(&mut v, KeyCode::F(2));
        assert!(!v.table.as_ref().unwrap().grid.header, "F2 turns the header row off");
    }

    #[test]
    fn f8_switches_to_the_raw_text_and_back_keeping_the_place() {
        let long = format!("{PEOPLE}{}", "dan,50,bern\n".repeat(40));
        let mut v = ViewerState::new("people.csv".into(), long.into_bytes());
        draw(&mut v, 60, 10);
        press(&mut v, KeyCode::Down);
        press(&mut v, KeyCode::Down);
        press(&mut v, KeyCode::Down);
        press(&mut v, KeyCode::F(8));
        assert!(!v.table_active());
        assert_eq!(v.top, 4, "record 3 starts on line 5, after the quoted break");
        assert_eq!(v.footer_labels()[7], "Table");
        press(&mut v, KeyCode::F(8));
        assert!(v.table_active());
        assert_eq!(cursor(&v), (3, 0));
    }

    #[test]
    fn f4_cycles_through_hex_and_the_map_back_to_the_table() {
        let mut v = ViewerState::new("people.csv".into(), PEOPLE.as_bytes().to_vec());
        press(&mut v, KeyCode::F(4));
        assert_eq!(v.mode, ViewMode::Hex);
        assert!(!v.table_active());
        press(&mut v, KeyCode::F(4));
        press(&mut v, KeyCode::F(4));
        assert!(v.table_active());
    }

    #[test]
    fn goto_and_search_land_on_cells() {
        let mut v = ViewerState::new("people.csv".into(), PEOPLE.as_bytes().to_vec());
        draw(&mut v, 60, 10);
        assert!(v.goto("3", GotoMode::Line));
        assert_eq!(cursor(&v).0, 2);
        assert!(v.goto("100", GotoMode::Percent));
        assert_eq!(cursor(&v).0, 3);
        assert!(!v.goto("x", GotoMode::Line));
        search(&mut v, "york", false);
        assert_eq!(cursor(&v), (2, 2), "the hit inside a quoted field is its cell");
        search(&mut v, "o", true);
        assert!(v.table_found(1) && v.table_found(2) && v.table_found(3), "oslo, bob/york, rome");
        assert!(!v.table_found(0), "the titles hold no o");
        assert_eq!(cursor(&v), (1, 2), "Find all moves to the first hit");
    }

    #[test]
    fn a_click_picks_a_cell_and_the_wheel_scrolls() {
        let body: String = std::iter::once("n,sq\n".to_string())
            .chain((1..200).map(|i| format!("{i},{}\n", i * i)))
            .collect();
        let mut v = ViewerState::new("sq.csv".into(), body.into_bytes());
        let rows = draw(&mut v, 40, 12);
        // Row 4 of the screen is the second body record; click its second column.
        let x = rows[4].chars().enumerate().filter(|&(_, c)| c == '│').nth(1).unwrap().0 as u16 + 2;
        v.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(cursor(&v), (2, 1));
        for _ in 0..5 {
            v.handle_mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 5,
                row: 5,
                modifiers: KeyModifiers::NONE,
            });
        }
        let t = v.table.as_ref().unwrap();
        assert_eq!(t.grid.top, 16);
        assert!(t.grid.row >= t.grid.top, "the cursor came along");
    }

    #[test]
    fn a_quoted_line_break_across_an_index_chunk_is_still_one_record() {
        // A quoted field that opens just before the first 256 KiB chunk ends
        // and closes after it.
        let mut data = b"id,text\n".to_vec();
        let mut i = 0;
        while data.len() < CHUNK - 10 {
            data.extend_from_slice(format!("{i},plain\n").as_bytes());
            i += 1;
        }
        data.extend_from_slice(b"x,\"spans\nthe\nseam\"\nlast,one\n");
        let path = std::env::temp_dir().join(format!(
            "rc-table-{}-{}.csv",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::write(&path, &data).unwrap();
        let mut v = ViewerState::open_file("big.csv".into(), path.clone(), Some(path)).unwrap();
        draw(&mut v, 40, 10);
        v.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        let (row, _) = cursor(&v);
        let fields = {
            let (Some(t), src) = (v.table.as_mut(), &v.src) else { unreachable!() };
            (t.fields(src, row - 1), t.fields(src, row))
        };
        assert_eq!(fields.0, ["x", "spans\nthe\nseam"]);
        assert_eq!(fields.1, ["last", "one"]);
        assert_eq!(row, i + 2, "the titles, every plain record, the spanning one and the last");
    }
}
