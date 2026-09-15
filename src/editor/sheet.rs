//! The editor's spreadsheet mode: a CSV or TSV file edited as a grid of cells.
//!
//! The text stays the one copy of the table. The grid is an index over it —
//! where each record starts — rebuilt whenever the buffer changes, and every
//! edit made in the grid is an ordinary replacement of the text a cell covers.
//! So undo, saving, search and replace see nothing new: they work on the text
//! as they always have, and the grid redraws from whatever the text now says.
//!
//! The editor's cursor stays the one position, too. The cursor cell is the cell
//! the text cursor is in, so a search hit, an undo or a jump to a line lands on
//! the cell it lands on in the text, and switching to the text (Alt-G) leaves
//! the cursor at the start of the cell that was selected.

use super::{EditorSignal, EditorState, buffer::EditorBuffer};
use crate::sheet::csv::{self, Dialect, Scanner};
use crate::sheet::grid::{self, GridRows, GridView};
use crate::ui::theme::Theme;
use crate::util::text::{ellipsize, pad_right};
use ratatui::Frame;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

/// Bytes the dialect is guessed from.
const SNIFF_BYTES: usize = 64 * 1024;
/// Records the column widths and the header guess are taken from.
const MEASURE_RECORDS: usize = 1000;

/// The grid over a table file, and whether it is the view showing.
pub(crate) struct SheetGrid {
    /// The grid (rather than the text) is on screen.
    pub(crate) on: bool,
    dialect: Dialect,
    index: SheetIndex,
    view: GridView,
    /// The buffer revision and cursor the grid position was last worked out
    /// for; either changing means the cursor cell has to be found again.
    synced: Option<(u64, usize)>,
    /// A cell being edited in the cell bar.
    edit: Option<CellEdit>,
    /// Whether the widths and the header row have been decided yet.
    measured: bool,
}

/// Where the records are, for one revision of the buffer.
#[derive(Default)]
struct SheetIndex {
    rev: u64,
    /// The char index each record starts at. Ends with the buffer's length when
    /// the text ends in a line break — the start of a record not yet written.
    records: Vec<usize>,
    /// The buffer's length, in chars.
    len: usize,
    /// Most fields any record has.
    ncols: usize,
    /// Records end in CR LF rather than LF.
    crlf: bool,
}

/// One cell of a record: the chars it covers, quotes included.
#[derive(Debug, Clone, Copy)]
struct CellSpan {
    start: usize,
    end: usize,
    quoted: bool,
}

/// A cell being edited: its place and the text so far.
struct CellEdit {
    row: usize,
    col: usize,
    value: String,
    /// The caret, as a char index into `value`.
    caret: usize,
}

impl SheetIndex {
    fn build(buf: &EditorBuffer, dialect: Dialect) -> Self {
        let mut starts = vec![0usize];
        let mut widest = 0;
        let mut scanner = Scanner::new(dialect);
        let mut base = 0;
        for chunk in buf.chunks() {
            scanner.feed_counting(chunk.as_bytes(), base, &mut starts, &mut widest);
            base += chunk.len();
        }
        let records: Vec<usize> = starts.into_iter().map(|b| buf.byte_to_char(b)).collect();
        let crlf = records.get(1).is_some_and(|&s| s >= 2 && buf.char_at(s - 2) == Some('\r'));
        SheetIndex { rev: buf.revision(), records, len: buf.len_chars(), ncols: widest, crlf }
    }

    /// Records in the table.
    fn count(&self) -> usize {
        let n = self.records.len();
        if self.records.last() == Some(&self.len) { n - 1 } else { n }
    }

    /// The line break a new record ends with.
    fn eol(&self) -> &'static str {
        if self.crlf { "\r\n" } else { "\n" }
    }

    /// Record `row`'s text, from its start to where its line break begins.
    fn body(&self, buf: &EditorBuffer, row: usize) -> (usize, usize) {
        let start = self.records[row];
        let mut end = self.records.get(row + 1).copied().unwrap_or(self.len);
        if end > start && buf.char_at(end - 1) == Some('\n') {
            end -= 1;
            if end > start && buf.char_at(end - 1) == Some('\r') {
                end -= 1;
            }
        }
        (start, end)
    }

    /// Record `row`'s cells.
    fn spans(&self, buf: &EditorBuffer, row: usize, dialect: Dialect) -> Vec<CellSpan> {
        let (start, end) = self.body(buf, row);
        let text = buf.slice(start, end);
        let mut out = Vec::new();
        let (mut byte, mut ch) = (0, start);
        for f in csv::split(text.as_bytes(), dialect) {
            ch += text[byte..f.start].chars().count();
            let s = ch;
            ch += text[f.start..f.end].chars().count();
            byte = f.end;
            out.push(CellSpan { start: s, end: ch, quoted: f.quoted });
        }
        out
    }

    /// Record `row`'s values.
    fn values(&self, buf: &EditorBuffer, row: usize, dialect: Dialect) -> Vec<String> {
        if row >= self.count() {
            return Vec::new();
        }
        let (start, end) = self.body(buf, row);
        let text = buf.slice(start, end);
        csv::split(text.as_bytes(), dialect)
            .iter()
            .map(|f| csv::value(&text.as_bytes()[f.start..f.end], f.quoted).into_owned())
            .collect()
    }

    /// The cell holding char `at`: past the last record is the row after it.
    fn locate(&self, buf: &EditorBuffer, at: usize, dialect: Dialect) -> (usize, usize) {
        let count = self.count();
        let row = match self.records.binary_search(&at) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        if row >= count {
            return (count, 0);
        }
        let spans = self.spans(buf, row, dialect);
        let col = spans.iter().position(|s| at <= s.end).unwrap_or(spans.len().saturating_sub(1));
        (row, col)
    }

    /// Where the cursor goes for cell (`row`, `col`): the cell's first char, or
    /// the end of a record too short to have that cell.
    fn cell_start(&self, buf: &EditorBuffer, row: usize, col: usize, dialect: Dialect) -> usize {
        if row >= self.count() {
            return self.len;
        }
        match self.spans(buf, row, dialect).get(col) {
            Some(s) => s.start,
            None => self.body(buf, row).1,
        }
    }
}

impl SheetGrid {
    fn new(buf: &EditorBuffer, name: &str) -> Self {
        let mut sample = Vec::new();
        for chunk in buf.chunks() {
            sample.extend_from_slice(chunk.as_bytes());
            if sample.len() >= SNIFF_BYTES {
                break;
            }
        }
        let complete = sample.len() < SNIFF_BYTES;
        sample.truncate(SNIFF_BYTES);
        SheetGrid {
            on: true,
            dialect: csv::sniff(&sample, complete, name),
            index: SheetIndex::default(),
            view: GridView::new(false),
            synced: None,
            edit: None,
            measured: false,
        }
    }
}

impl EditorState {
    /// Decide from the file name whether this is a table, and give it a grid
    /// if so — shown straight away, since that is how a table opens. Run when a
    /// file is opened and when a buffer is saved under a new name.
    pub fn detect_kind(&mut self) {
        let is_sheet = crate::sheet::is_sheet_name(&self.name);
        if is_sheet && self.sheet.is_none() {
            self.sheet = Some(SheetGrid::new(&self.buf, &self.name));
        } else if !is_sheet {
            self.sheet = None;
        }
    }

    /// Whether the grid is the view on screen (hex mode, when on, wins).
    pub fn sheet_active(&self) -> bool {
        self.hex.is_none() && self.sheet.as_ref().is_some_and(|s| s.on)
    }

    /// Alt-G: switch between the grid and the text — for any file, so a table
    /// without a table's name can be edited as one too.
    pub(super) fn toggle_sheet(&mut self) {
        if self.hex.is_some() {
            return;
        }
        if self.sheet_active() {
            self.commit_cell_edit();
            if let Some(s) = self.sheet.as_mut() {
                s.on = false;
            }
            self.goal_col = None;
            return;
        }
        match self.sheet.as_mut() {
            Some(s) => {
                s.on = true;
                s.synced = None;
            }
            None => self.sheet = Some(SheetGrid::new(&self.buf, &self.name)),
        }
        self.clear_marks();
    }

    /// Bring the grid up to date with the buffer: re-index after an edit, and
    /// find the cursor cell again after the cursor moved.
    pub(super) fn sheet_sync(&mut self) {
        let rev = self.buf.revision();
        let Some(s) = self.sheet.as_mut() else { return };
        if s.index.rev != rev {
            s.index = SheetIndex::build(&self.buf, s.dialect);
        }
        if !s.measured {
            s.measured = true;
            let n = s.index.count().min(MEASURE_RECORDS);
            let rows: Vec<Vec<String>> =
                (0..n).map(|r| s.index.values(&self.buf, r, s.dialect)).collect();
            s.view = GridView::new(n > 1 && csv::guess_header(&rows[0]));
            s.view.measure(&rows);
        }
        if s.synced != Some((rev, self.cursor)) {
            let (row, col) = s.index.locate(&self.buf, self.cursor, s.dialect);
            s.view.row = row;
            s.view.col = col;
            s.synced = Some((rev, self.cursor));
        }
    }

    /// Move the cursor cell to (`row`, `col`), clamped to the table plus the
    /// empty row below it and the empty column beside it, where typing adds one.
    fn sheet_go(&mut self, row: usize, col: usize) {
        let Some(s) = self.sheet.as_mut() else { return };
        let row = row.min(s.index.count());
        let col = col.min(s.index.ncols);
        s.view.row = row;
        s.view.col = col;
        self.cursor = s.index.cell_start(&self.buf, row, col, s.dialect);
        self.goal_col = None;
        s.synced = Some((self.buf.revision(), self.cursor));
    }

    /// The cursor cell, and the table's size in records and columns.
    fn sheet_cursor(&self) -> (usize, usize, usize, usize) {
        let s = self.sheet.as_ref().expect("only called with a grid");
        (s.view.row, s.view.col, s.index.count(), s.index.ncols)
    }

    fn cell_value(&self, row: usize, col: usize) -> String {
        let s = self.sheet.as_ref().expect("only called with a grid");
        s.index.values(&self.buf, row, s.dialect).get(col).cloned().unwrap_or_default()
    }

    /// Replace one cell's text with `value`, as a single undo step.
    ///
    /// A cell past the end of a short record is reached by adding the
    /// delimiters in between; a cell in the row below the table adds a record.
    /// Writing a cell's current value again changes nothing — not even the
    /// modified flag.
    fn sheet_set_cell(&mut self, row: usize, col: usize, value: &str) {
        self.sheet_sync();
        let Some(s) = self.sheet.as_ref() else { return };
        let (d, ix) = (s.dialect, &s.index);
        let delims = |n: usize| String::from(d.delim as char).repeat(n);
        let (at, end, text) = if row >= ix.count() {
            if value.is_empty() {
                return;
            }
            // A new record at the end, keeping the file's line breaks.
            let ends_open = ix.len > 0 && ix.records.last() != Some(&ix.len);
            let lead = if ends_open { ix.eol() } else { "" };
            let tail = if ends_open { "" } else { ix.eol() };
            let text = format!("{lead}{}{}{tail}", delims(col), csv::quote(value, d, false));
            (ix.len, ix.len, text)
        } else {
            let spans = ix.spans(&self.buf, row, d);
            match spans.get(col) {
                Some(sp) => {
                    if self.cell_value(row, col) == value {
                        return;
                    }
                    (sp.start, sp.end, csv::quote(value, d, sp.quoted).into_owned())
                }
                None if value.is_empty() => return,
                // Past the end of a short record: the delimiters in between,
                // then the value (a record always has at least one field).
                None => {
                    let at = ix.body(&self.buf, row).1;
                    let pad = delims(col + 1 - spans.len());
                    (at, at, format!("{pad}{}", csv::quote(value, d, false)))
                }
            }
        };
        self.replace_as_one_step(at, end, &text);
        self.sheet_sync();
        self.sheet_go(row, col);
        // A value longer than its column widens it.
        if let Some(s) = self.sheet.as_mut() {
            let values = s.index.values(&self.buf, row, s.dialect);
            s.view.measure(&[values]);
        }
    }

    /// One edit of the text, undone on its own.
    fn replace_as_one_step(&mut self, start: usize, end: usize, text: &str) {
        self.buf.break_undo_group();
        self.buf.replace_range(start, end, text);
        self.buf.break_undo_group();
        self.dirty = true;
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(self.buf.char_to_line(start));
        }
    }

    /// F5: an empty record above the cursor's, as wide as the table.
    fn sheet_insert_row(&mut self) {
        self.sheet_sync();
        let (row, col, count, ncols) = self.sheet_cursor();
        let Some(s) = self.sheet.as_ref() else { return };
        let empty = String::from(s.dialect.delim as char).repeat(ncols.saturating_sub(1));
        if row >= count {
            // Below the table: an empty record at the end.
            let ends_open = s.index.len > 0 && s.index.records.last() != Some(&s.index.len);
            let lead = if ends_open { s.index.eol() } else { "" };
            let at = s.index.len;
            let text = format!("{lead}{empty}{}", s.index.eol());
            self.replace_as_one_step(at, at, &text);
        } else {
            let at = s.index.records[row];
            let text = format!("{empty}{}", s.index.eol());
            self.replace_as_one_step(at, at, &text);
        }
        self.sheet_sync();
        self.sheet_go(row, col);
    }

    /// F8: remove the cursor's record, line break and all.
    fn sheet_delete_row(&mut self) {
        self.sheet_sync();
        let (row, col, count, _) = self.sheet_cursor();
        if row >= count {
            return;
        }
        let Some(s) = self.sheet.as_ref() else { return };
        let ix = &s.index;
        let start = ix.records[row];
        let (start, end) = match ix.records.get(row + 1) {
            Some(&next) => (start, next),
            // The last record, with no line break after it: take the break
            // before it instead, so the record above does not gain one.
            None if row > 0 => (ix.body(&self.buf, row - 1).1, ix.len),
            None => (start, ix.len),
        };
        self.replace_as_one_step(start, end, "");
        self.sheet_sync();
        let (_, _, count, _) = self.sheet_cursor();
        self.sheet_go(row.min(count), col);
    }

    /// F6 / Shift-F8: insert an empty column left of the cursor's, or remove
    /// the cursor's column, in every record at once — one undo step.
    fn sheet_edit_column(&mut self, insert: bool) {
        self.sheet_sync();
        let (row, col, count, _) = self.sheet_cursor();
        let Some(s) = self.sheet.as_ref() else { return };
        let (d, ix) = (s.dialect, &s.index);
        if count == 0 {
            return;
        }
        let delim = d.delim as char;
        let mut out = String::new();
        for r in 0..count {
            let body_end = ix.body(&self.buf, r).1;
            let next = ix.records.get(r + 1).copied().unwrap_or(ix.len);
            let mut cells: Vec<String> = ix
                .spans(&self.buf, r, d)
                .iter()
                .map(|sp| self.buf.slice(sp.start, sp.end))
                .collect();
            if insert {
                if col <= cells.len() && !(cells.len() == 1 && cells[0].is_empty()) {
                    cells.insert(col, String::new());
                }
            } else if col < cells.len() {
                cells.remove(col);
            }
            out.push_str(&cells.join(&delim.to_string()));
            out.push_str(&self.buf.slice(body_end, next));
        }
        let end = ix.len;
        self.replace_as_one_step(0, end, &out);
        self.sheet_sync();
        let (_, _, _, ncols) = self.sheet_cursor();
        self.sheet_go(row, col.min(ncols.saturating_sub(1)));
    }

    /// F3: whether the first record is the header row.
    fn sheet_toggle_header(&mut self) {
        if let Some(s) = self.sheet.as_mut() {
            let on = !s.view.header;
            s.view.set_header(on);
        }
    }

    /// Start editing the cursor cell in the cell bar, with `initial` as its
    /// text — or, without one, the text it already has.
    fn begin_cell_edit(&mut self, initial: Option<String>) {
        let (row, col, ..) = self.sheet_cursor();
        let value = initial.unwrap_or_else(|| self.cell_value(row, col));
        let caret = value.chars().count();
        if let Some(s) = self.sheet.as_mut() {
            s.edit = Some(CellEdit { row, col, value, caret });
        }
    }

    /// Write a cell being edited back to the text.
    pub(super) fn commit_cell_edit(&mut self) {
        let Some(e) = self.sheet.as_mut().and_then(|s| s.edit.take()) else { return };
        self.sheet_set_cell(e.row, e.col, &e.value);
    }

    /// Whether a cell is being edited (the app keeps Esc from being held as a
    /// key prefix meanwhile, so it cancels at once).
    pub fn editing_cell(&self) -> bool {
        self.sheet_active() && self.sheet.as_ref().is_some_and(|s| s.edit.is_some())
    }

    /// Keys while a cell is being edited. `None` hands the key on to the grid,
    /// after the edit has been written back: a key that is not editing ends
    /// the edit and then does what it always does — F2 saves, Tab moves on.
    fn handle_cell_edit_key(&mut self, key: KeyEvent) -> Option<EditorSignal> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT) && !ctrl;
        let s = self.sheet.as_mut()?;
        let e = s.edit.as_mut()?;
        match key.code {
            KeyCode::Esc => s.edit = None,
            KeyCode::Enter if alt => {
                insert_at_caret(e, "\n");
            }
            KeyCode::Enter => {
                let (row, col) = (e.row, e.col);
                self.commit_cell_edit();
                self.sheet_go(row + 1, col);
            }
            KeyCode::Char('v') if ctrl => {
                let clip = self.clipboard.clone();
                if let Some(e) = self.sheet.as_mut().and_then(|s| s.edit.as_mut()) {
                    insert_at_caret(e, &clip);
                }
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                self.commit_cell_edit();
                return None;
            }
            _ => {
                if crate::ui::textedit::edit_key(&mut e.value, &mut e.caret, key)
                    == crate::ui::textedit::Edit::Ignored
                {
                    self.commit_cell_edit();
                    return None;
                }
            }
        }
        Some(EditorSignal::Stay)
    }

    /// Keys in the grid.
    pub(super) fn handle_sheet_key(&mut self, key: KeyEvent) -> EditorSignal {
        self.sheet_sync();
        if self.editing_cell()
            && let Some(signal) = self.handle_cell_edit_key(key)
        {
            return signal;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT) && !ctrl;
        let back_tab = key.code == KeyCode::BackTab || (key.code == KeyCode::Tab && shift);
        let (row, col, count, ncols) = self.sheet_cursor();
        let page = self.sheet.as_ref().map_or(1, |s| s.view.page_rows.saturating_sub(1).max(1));
        match key.code {
            KeyCode::F(10) | KeyCode::Esc => {
                return if self.dirty { EditorSignal::ConfirmQuit } else { EditorSignal::Close };
            }
            KeyCode::F(2) if shift || ctrl => return EditorSignal::SaveAs,
            KeyCode::F(2) => return EditorSignal::Save { close_after: false },
            KeyCode::F(3) => self.sheet_toggle_header(),
            KeyCode::F(4) => return EditorSignal::OpenReplace,
            KeyCode::F(7) if shift => self.search_again(),
            KeyCode::F(7) => return EditorSignal::OpenSearch,
            KeyCode::F(5) => self.sheet_insert_row(),
            KeyCode::F(6) => self.sheet_edit_column(true),
            KeyCode::F(8) if shift => self.sheet_edit_column(false),
            KeyCode::F(8) => self.sheet_delete_row(),
            KeyCode::Char('z') if ctrl => self.undo(),
            KeyCode::Char('y') if ctrl => self.redo(),
            KeyCode::Char('c') if ctrl => self.sheet_copy(false),
            KeyCode::Char('x') if ctrl => self.sheet_copy(true),
            KeyCode::Char('v') if ctrl => self.sheet_paste(),
            KeyCode::Char('n') if ctrl => return EditorSignal::NewFile,
            KeyCode::Char('f') if ctrl => return EditorSignal::Browse(super::BrowseKind::CopyTo),
            KeyCode::Char('s') if ctrl => self.toggle_syntax(),
            KeyCode::Char('l') if ctrl => return EditorSignal::RefreshScreen,
            KeyCode::Char('g') if alt => self.toggle_sheet(),
            KeyCode::Char('l') if alt => return EditorSignal::OpenGotoLine,
            _ if back_tab => {
                if col > 0 {
                    self.sheet_go(row, col - 1);
                } else if row > 0 {
                    let last = self.sheet_row_len(row - 1).saturating_sub(1);
                    self.sheet_go(row - 1, last);
                }
            }
            KeyCode::Tab => {
                if col + 1 < self.sheet_row_len(row) {
                    self.sheet_go(row, col + 1);
                } else if row < count {
                    self.sheet_go(row + 1, 0);
                }
            }
            KeyCode::Left if ctrl => self.sheet_widen(-1),
            KeyCode::Right if ctrl => self.sheet_widen(1),
            KeyCode::Up => self.sheet_go(row.saturating_sub(1), col),
            KeyCode::Down => self.sheet_go(row + 1, col),
            KeyCode::PageUp => self.sheet_go(row.saturating_sub(page), col),
            KeyCode::PageDown => self.sheet_go(row + page, col),
            KeyCode::Left => self.sheet_go(row, col.saturating_sub(1)),
            KeyCode::Right => self.sheet_go(row, (col + 1).min(ncols)),
            KeyCode::Home if ctrl => self.sheet_go(0, 0),
            KeyCode::End if ctrl => self.sheet_go(count.saturating_sub(1), col),
            KeyCode::Home => self.sheet_go(row, 0),
            KeyCode::End => self.sheet_go(row, self.sheet_row_len(row).saturating_sub(1)),
            KeyCode::Enter => self.begin_cell_edit(None),
            KeyCode::Backspace => self.begin_cell_edit(Some(String::new())),
            KeyCode::Delete => self.sheet_set_cell(row, col, ""),
            // A typed character starts an edit that replaces the cell, as a
            // spreadsheet does (AltGr, reported as Ctrl+Alt, composes one too).
            KeyCode::Char(c) if ctrl == key.modifiers.contains(KeyModifiers::ALT) => {
                self.begin_cell_edit(Some(c.to_string()));
            }
            _ => {}
        }
        EditorSignal::Stay
    }

    /// Fields in record `row` (none in the row below the table).
    fn sheet_row_len(&self, row: usize) -> usize {
        let Some(s) = self.sheet.as_ref() else { return 0 };
        if row >= s.index.count() {
            return 0;
        }
        s.index.spans(&self.buf, row, s.dialect).len()
    }

    fn sheet_widen(&mut self, delta: i32) {
        if let Some(s) = self.sheet.as_mut() {
            let col = s.view.col;
            s.view.widen(col, delta);
        }
    }

    /// Ctrl-C / Ctrl-X: the cursor cell's value to the clipboard, and for a cut,
    /// out of the cell.
    fn sheet_copy(&mut self, cut: bool) {
        let (row, col, ..) = self.sheet_cursor();
        let value = self.cell_value(row, col);
        self.clipboard = value.clone();
        self.pending_clip = Some(value);
        if cut {
            self.sheet_set_cell(row, col, "");
        }
    }

    /// Ctrl-V: the clipboard into the cursor cell.
    fn sheet_paste(&mut self) {
        if self.clipboard.is_empty() {
            self.status = "The clipboard is empty".to_string();
            return;
        }
        let (row, col, ..) = self.sheet_cursor();
        let text = self.clipboard.clone();
        self.sheet_set_cell(row, col, &text);
    }

    /// Menu actions that act on the grid.
    pub(super) fn sheet_action(&mut self, action: super::menu::EditorAction) {
        use super::menu::EditorAction as A;
        if !self.sheet_active() {
            return;
        }
        self.sheet_sync();
        self.commit_cell_edit();
        match action {
            A::SheetInsertRow => self.sheet_insert_row(),
            A::SheetDeleteRow => self.sheet_delete_row(),
            A::SheetInsertCol => self.sheet_edit_column(true),
            A::SheetDeleteCol => self.sheet_edit_column(false),
            A::SheetHeader => self.sheet_toggle_header(),
            A::ClipCopy => self.sheet_copy(false),
            A::ClipCut => self.sheet_copy(true),
            A::ClipPaste => self.sheet_paste(),
            _ => {}
        }
    }

    /// The mouse on the grid: the wheel scrolls, a click picks a cell, and a
    /// click on the cursor cell edits it.
    pub(super) fn handle_sheet_mouse(&mut self, ev: MouseEvent) -> EditorSignal {
        self.sheet_sync();
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.commit_cell_edit();
                let delta = if ev.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                let Some(s) = self.sheet.as_mut() else { return EditorSignal::Stay };
                let last = s.index.count();
                s.view.scroll(delta, last);
                let (row, col) = (s.view.row, s.view.col);
                self.sheet_go(row, col);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(hit) = self.sheet.as_ref().and_then(|s| s.view.cell_at(ev.column, ev.row))
                else {
                    return EditorSignal::Stay;
                };
                let editing = self.editing_cell();
                self.commit_cell_edit();
                let (row, col, ..) = self.sheet_cursor();
                if hit == (row, col) && !editing {
                    self.begin_cell_edit(None);
                } else {
                    self.sheet_go(hit.0, hit.1);
                }
            }
            _ => {}
        }
        EditorSignal::Stay
    }
}

/// Put `text` into the edited value at the caret.
fn insert_at_caret(e: &mut CellEdit, text: &str) {
    let at = e.value.char_indices().nth(e.caret).map_or(e.value.len(), |(b, _)| b);
    e.value.insert_str(at, text);
    e.caret += text.chars().count();
}

/// Draw the grid into the editor's text area. Returns where the terminal's
/// cursor goes: on the caret, while a cell is being edited.
pub(super) fn render(
    f: &mut Frame,
    area: Rect,
    ed: &mut EditorState,
    theme: &Theme,
) -> Option<Position> {
    ed.sheet_sync();
    let center = std::mem::take(&mut ed.pending_center);
    let EditorState { sheet, buf, found_lines, .. } = ed;
    let s = sheet.as_mut()?;
    let base = Style::default().fg(theme.text_fg).bg(theme.panel_bg);
    f.render_widget(ratatui::widgets::Block::default().style(base), area);
    let count = s.index.count();
    if center {
        let half = (area.height as usize).saturating_sub(2) / 2;
        s.view.top = s.view.row.saturating_sub(half);
    }
    // One row and one column past the table, where typing adds to it.
    let range = s.view.layout(area, count + 1, s.index.ncols + 1);
    let mut rows: Vec<Vec<String>> = (range.start..range.end.min(count + 1))
        .map(|r| s.index.values(buf, r, s.dialect))
        .collect();
    let mut header = (s.view.header && count > 0).then(|| s.index.values(buf, 0, s.dialect));
    // The cell being edited shows what it is being changed to.
    if let Some(e) = &s.edit {
        let target = if e.row == 0 && s.view.header {
            header.as_mut()
        } else {
            e.row.checked_sub(s.view.top).and_then(|i| rows.get_mut(i))
        };
        if let Some(cells) = target {
            if cells.len() <= e.col {
                cells.resize(e.col + 1, String::new());
            }
            cells[e.col] = e.value.clone();
        }
    }
    s.view.measure_new_columns(&rows);
    s.view.layout(area, count + 1, s.index.ncols + 1);
    let ix = &s.index;
    let found = |r: usize| r < count && found_lines.contains(&buf.char_to_line(ix.records[r]));
    let data = GridRows {
        header: header.as_deref(),
        rows: &rows,
        total: count,
        exact: true,
        found: &found,
    };
    let edit = s.edit.as_ref().map(|e| (e.value.as_str(), e.caret));
    grid::render(f, &s.view, &data, theme, edit)
}

/// The status row over the grid: the file, where the cursor cell is in the
/// table, and the delimiter the table is read with.
pub(super) fn render_status(f: &mut Frame, area: Rect, ed: &EditorState, theme: &Theme) {
    let Some(s) = ed.sheet.as_ref() else { return };
    let dirty = if ed.dirty { "[+]" } else { "   " };
    let delim = match s.dialect.delim {
        b'\t' => "Tab".to_string(),
        d => (d as char).to_string(),
    };
    let name = ellipsize(&ed.name, area.width.saturating_sub(48) as usize);
    let text = format!(
        " {name} {dirty}  Row {}/{}  Col {}/{}  {delim} ",
        s.view.row + 1,
        s.index.count(),
        csv::column_name(s.view.col),
        s.index.ncols,
    );
    f.render_widget(
        Paragraph::new(Line::from(ratatui::text::Span::styled(
            pad_right(&text, area.width as usize),
            theme.menubar.add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::VfsPath;
    use ratatui::Terminal;
    use ratatui::backend::{Backend, TestBackend};

    fn ed(name: &str, text: &str) -> EditorState {
        EditorState::new(name.into(), VfsPath::local("/tmp/x"), text)
    }

    fn press(e: &mut EditorState, code: KeyCode) -> EditorSignal {
        e.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn press_mod(e: &mut EditorState, code: KeyCode, mods: KeyModifiers) -> EditorSignal {
        e.handle_key(KeyEvent::new(code, mods))
    }

    fn typed(e: &mut EditorState, s: &str) {
        for c in s.chars() {
            press(e, KeyCode::Char(c));
        }
    }

    fn cell(e: &mut EditorState) -> (usize, usize) {
        e.sheet_sync();
        let v = &e.sheet.as_ref().unwrap().view;
        (v.row, v.col)
    }

    fn draw(e: &mut EditorState, w: u16, h: u16) -> (Vec<String>, Option<Position>) {
        let theme = Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| super::super::render::render(f, f.area(), e, &theme)).unwrap();
        let cursor = t.backend_mut().get_cursor_position().ok();
        let b = t.backend().buffer();
        let rows =
            (0..h).map(|y| (0..w).map(|x| b[(x, y)].symbol().to_string()).collect()).collect();
        (rows, cursor)
    }

    #[test]
    fn a_csv_file_opens_in_the_grid_and_alt_g_switches_to_the_text_in_place() {
        let mut e = ed("t.csv", "name,qty\napple,3\nfig,12\n");
        assert!(e.sheet_active());
        assert!(!ed("t.txt", "a,b\n").sheet_active());
        let (rows, _) = draw(&mut e, 50, 8);
        assert!(rows[0].contains("Row 1/3") && rows[0].contains("Col A/2"), "{:?}", rows[0]);
        assert!(rows[2].contains("name") && rows[3].contains("apple"), "{rows:?}");
        assert!(rows[7].contains("InsR"), "the grid's own F-key bar: {:?}", rows[7]);
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::Right);
        press_mod(&mut e, KeyCode::Char('g'), KeyModifiers::ALT);
        assert!(!e.sheet_active());
        assert_eq!(e.cursor_line_col(), (2, 4), "the text cursor is on the cell that was selected");
        press(&mut e, KeyCode::Up);
        press_mod(&mut e, KeyCode::Char('g'), KeyModifiers::ALT);
        assert!(e.sheet_active());
        assert_eq!(cell(&mut e), (1, 0), "and back, on the cell the text cursor moved to (apple)");
    }

    #[test]
    fn editing_a_cell_writes_it_back_quoted_as_one_undo_step() {
        let mut e = ed("t.csv", "a,b\nc,d\n");
        press(&mut e, KeyCode::Right);
        press(&mut e, KeyCode::Enter);
        assert!(e.editing_cell());
        press(&mut e, KeyCode::Backspace);
        typed(&mut e, "x,y");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.contents(), "a,\"x,y\"\nc,d\n");
        assert!(e.dirty);
        assert_eq!(cell(&mut e), (1, 1), "Enter moves down");
        press_mod(&mut e, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(e.contents(), "a,b\nc,d\n", "one undo takes the whole cell back");
    }

    #[test]
    fn typing_replaces_the_cell_and_esc_or_an_unchanged_value_leaves_it_alone() {
        let mut e = ed("t.csv", "a,b\n");
        typed(&mut e, "Q");
        press(&mut e, KeyCode::Tab);
        assert_eq!(e.contents(), "Q,b\n", "a typed character starts a replacing edit");
        assert_eq!(cell(&mut e), (0, 1), "Tab commits and moves right");
        typed(&mut e, "zzz");
        press(&mut e, KeyCode::Esc);
        assert!(!e.editing_cell());
        assert_eq!(e.contents(), "Q,b\n", "Esc cancels");
        let mut clean = ed("t.csv", "a,b\n");
        press(&mut clean, KeyCode::Enter);
        press(&mut clean, KeyCode::Enter);
        assert!(!clean.dirty, "committing a cell unchanged is no edit");
    }

    #[test]
    fn cells_past_the_table_add_to_it() {
        let mut e = ed("t.csv", "a,b\nc\n");
        // The short second record gains the delimiters up to the new cell.
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::Right);
        press(&mut e, KeyCode::Right);
        typed(&mut e, "z");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.contents(), "a,b\nc,,z\n");
        // The row below the table adds a record, with a line break of its own.
        press_mod(&mut e, KeyCode::End, KeyModifiers::CONTROL);
        press(&mut e, KeyCode::Down);
        assert_eq!(cell(&mut e).0, 2);
        typed(&mut e, "new");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.contents(), "a,b\nc,,z\n,,new\n");
        // A file without a final line break keeps having none.
        let mut open = ed("t.csv", "a,b");
        press(&mut open, KeyCode::Down);
        typed(&mut open, "c");
        press(&mut open, KeyCode::Enter);
        assert_eq!(open.contents(), "a,b\nc");
    }

    #[test]
    fn rows_and_columns_are_inserted_and_deleted_as_one_step_each() {
        let mut e = ed("t.csv", "a,b\r\nc,d\r\n");
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::F(5));
        assert_eq!(e.contents(), "a,b\r\n,\r\nc,d\r\n", "an empty record, CR LF like the rest");
        assert_eq!(cell(&mut e).0, 1);
        press(&mut e, KeyCode::F(8));
        assert_eq!(e.contents(), "a,b\r\nc,d\r\n");
        press(&mut e, KeyCode::Right);
        press(&mut e, KeyCode::F(6));
        assert_eq!(e.contents(), "a,,b\r\nc,,d\r\n");
        press_mod(&mut e, KeyCode::F(8), KeyModifiers::SHIFT);
        assert_eq!(e.contents(), "a,b\r\nc,d\r\n");
        for _ in 0..4 {
            press_mod(&mut e, KeyCode::Char('z'), KeyModifiers::CONTROL);
        }
        assert_eq!(e.contents(), "a,b\r\nc,d\r\n", "four operations, four undo steps");
        press_mod(&mut e, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(e.contents(), "a,b\r\nc,d\r\n");
        // Deleting the last record of a file without a final break takes the
        // break before it.
        let mut open = ed("t.csv", "a\nb");
        press(&mut open, KeyCode::Down);
        press(&mut open, KeyCode::F(8));
        assert_eq!(open.contents(), "a");
    }

    #[test]
    fn delete_clears_a_cell_and_the_clipboard_moves_values() {
        let mut e = ed("t.csv", "one,two\n");
        press_mod(&mut e, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(e.take_pending_clip().as_deref(), Some("one"));
        press(&mut e, KeyCode::Right);
        press_mod(&mut e, KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert_eq!(e.contents(), "one,one\n");
        press(&mut e, KeyCode::Delete);
        assert_eq!(e.contents(), "one,\n");
    }

    #[test]
    fn a_search_hit_lands_on_its_cell() {
        let mut e = ed("t.csv", "id,city\n1,oslo\n2,rome\n");
        e.apply_search_replace(false, "rome", "", false, false, false, false, false);
        assert_eq!(cell(&mut e), (2, 1));
    }

    #[test]
    fn the_edited_value_shows_in_the_cell_bar_with_the_terminal_cursor_on_the_caret() {
        let mut e = ed("t.csv", "name,qty\nfig,3\n");
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::Enter);
        typed(&mut e, "s");
        let (rows, cursor) = draw(&mut e, 40, 7);
        assert!(rows[1].contains("name: figs"), "{:?}", rows[1]);
        assert!(rows[3].contains("│ figs │"), "the cell mirrors the edit: {:?}", rows[3]);
        let x = rows[1].find("figs").unwrap() as u16 + 4;
        assert_eq!(cursor, Some(Position::new(x, 1)));
    }

    #[test]
    fn the_menu_row_actions_and_f3_act_on_the_grid() {
        use super::super::menu::EditorAction as A;
        let mut e = ed("t.csv", "1,2\n3,4\n");
        e.sheet_sync();
        assert!(!e.sheet.as_ref().unwrap().view.header, "numbers are no column titles");
        press(&mut e, KeyCode::F(3));
        assert!(e.sheet.as_ref().unwrap().view.header);
        e.run_menu_action(A::SheetInsertCol);
        assert_eq!(e.contents(), ",1,2\n,3,4\n");
        e.run_menu_action(A::SheetDeleteRow);
        assert_eq!(e.contents(), ",3,4\n");
    }
}
