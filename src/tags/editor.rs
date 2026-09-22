//! The editor's tag mode: an audio file's tags edited as a list of fields.
//!
//! F4 on an audio file opens the ordinary editor, but with the bytes behind it
//! rather than text — the file is binary, so it is the hex editor underneath —
//! and this view in front. Alt-T switches between the two, so the bytes are
//! never taken away, only covered by something more useful for the job.
//!
//! Its text is English, like the spreadsheet grid and the hex view it sits
//! beside: none of the editor's body views go through the catalogs, and a tag's
//! field names are closer to data than to chrome.
//!
//! The values live here rather than in the buffer, because a tag is not a range
//! of the file: writing one moves everything after it. So a save goes through
//! [`super::write`], which rewrites the container, instead of through the hex
//! editor's in-place byte patching.

use super::{Field, Tags};
use crate::ui::textedit::{self, Edit};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use std::path::Path;

/// One row of the view: an editable field, or a heading / read-only line.
pub(crate) enum Row {
    /// A well-known field and its index into [`TagEditor::values`].
    Field(Field, usize),
    /// A heading, with its English text.
    Heading(&'static str),
    /// An item the program has no name for: shown, kept, not editable.
    Extra(usize),
    /// The embedded-picture count.
    Pictures(usize),
}

/// The tags of the file being edited, and where the cursor is in them.
pub(crate) struct TagEditor {
    /// The tag view (rather than the bytes) is on screen.
    pub(crate) on: bool,
    /// What was read, kept so a cancelled edit and the dirty check have
    /// something to compare against.
    original: Tags,
    /// The current value of each well-known field, in [`Field::ALL`] order.
    values: Vec<String>,
    /// Selected row.
    cursor: usize,
    /// First visible row.
    top: usize,
    /// The field being typed into: its row, the text so far, and the caret.
    edit: Option<(usize, String, usize)>,
    /// Rows the last render showed, so the mouse can find them.
    area: Rect,
}

impl TagEditor {
    /// Read `path`'s tags. `None` when it has none that can be read — the
    /// caller then opens the file the way it would any other.
    pub(crate) fn open(path: &Path) -> Option<Self> {
        let original = super::read(path)?;
        let values = original.fields.iter().map(|(_, v)| v.clone()).collect();
        Some(TagEditor {
            on: true,
            original,
            values,
            cursor: 0,
            top: 0,
            edit: None,
            area: Rect::default(),
        })
    }

    /// Whether any value differs from what was read.
    pub(crate) fn dirty(&self) -> bool {
        self.original.fields.iter().map(|(_, v)| v).ne(self.values.iter())
    }

    /// The fields to write, paired back up with which field each one is.
    pub(crate) fn to_write(&self) -> Vec<(Field, String)> {
        Field::ALL.iter().copied().zip(self.values.iter().cloned()).collect()
    }

    /// The kind of tag these values came from.
    pub(crate) fn tag_type(&self) -> lofty::tag::TagType {
        self.original.tag_type
    }

    /// Take the current values as the saved ones, after a successful write.
    pub(crate) fn mark_saved(&mut self) {
        for ((_, orig), now) in self.original.fields.iter_mut().zip(&self.values) {
            orig.clone_from(now);
        }
    }

    /// Re-read the file after it has been rewritten, so the view matches what
    /// is now on disk (a save can normalise a value, or create the tag).
    pub(crate) fn reload(&mut self, path: &Path) {
        if let Some(t) = super::read(path) {
            self.values = t.fields.iter().map(|(_, v)| v.clone()).collect();
            self.original = t;
        }
    }

    /// Whether a field is being typed into, so the caller knows the caret is here.
    pub(crate) fn editing(&self) -> bool {
        self.edit.is_some()
    }

    /// Every row the view shows, rebuilt per render — cheap, and it keeps the
    /// row numbering and the drawing from ever disagreeing.
    pub(crate) fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> =
            Vec::with_capacity(Field::ALL.len() + self.original.extra.len() + 3);
        for (i, f) in Field::ALL.iter().enumerate() {
            rows.push(Row::Field(*f, i));
        }
        if self.original.pictures > 0 {
            rows.push(Row::Pictures(self.original.pictures));
        }
        if !self.original.extra.is_empty() {
            rows.push(Row::Heading("Other tags in this file (kept, not edited)"));
            for i in 0..self.original.extra.len() {
                rows.push(Row::Extra(i));
            }
        }
        rows
    }

    /// The value of row `i` for display, and whether it is being edited.
    pub(crate) fn row_text(&self, row: &Row) -> (String, String) {
        match row {
            Row::Field(f, i) => {
                let shown = match &self.edit {
                    Some((r, text, _)) if self.rows_index_of_field(*i) == *r => text.clone(),
                    _ => self.values[*i].clone(),
                };
                (f.label().to_string(), shown)
            }
            Row::Heading(t) => (String::new(), t.to_string()),
            Row::Extra(i) => {
                let (k, v) = &self.original.extra[*i];
                (k.clone(), v.clone())
            }
            Row::Pictures(n) => ("Pictures".to_string(), n.to_string()),
        }
    }

    /// Where field `i` sits among the rows. The field rows come first and in
    /// order, so this is the index itself.
    fn rows_index_of_field(&self, i: usize) -> usize {
        i
    }

    /// The caret column within the value, when a field is being typed into.
    pub(crate) fn caret(&self) -> Option<(usize, usize)> {
        self.edit.as_ref().map(|(row, _, cur)| (*row, *cur))
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn top(&self) -> usize {
        self.top
    }

    pub(crate) fn set_view(&mut self, area: Rect, top: usize) {
        self.area = area;
        self.top = top;
    }

    /// Start editing the selected row, if it is an editable field.
    fn begin_edit(&mut self) {
        let rows = self.rows();
        if let Some(Row::Field(_, i)) = rows.get(self.cursor) {
            let text = self.values[*i].clone();
            let caret = text.chars().count();
            self.edit = Some((self.cursor, text, caret));
        }
    }

    /// Finish editing, keeping what was typed.
    fn commit_edit(&mut self) {
        if let Some((row, text, _)) = self.edit.take() {
            let rows = self.rows();
            if let Some(Row::Field(_, i)) = rows.get(row) {
                self.values[*i] = text;
            }
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        let mut next = (self.cursor as isize + delta).clamp(0, n as isize - 1) as usize;
        // Headings are not landing places; step past one in the direction of travel.
        let rows = self.rows();
        while matches!(rows.get(next), Some(Row::Heading(_))) {
            let step = if delta >= 0 { 1 } else { -1 };
            let moved = next as isize + step;
            if moved < 0 || moved >= n as isize {
                return;
            }
            next = moved as usize;
        }
        self.cursor = next;
    }

    /// Handle one key. Returns whether it was taken.
    pub(crate) fn key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // While a field is being typed into, the editing keys belong to it.
        if self.edit.is_some() {
            match key.code {
                KeyCode::Enter => {
                    self.commit_edit();
                    return true;
                }
                KeyCode::Esc => {
                    self.edit = None;
                    return true;
                }
                KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                    // Moving off the row keeps what was typed, like a grid.
                    self.commit_edit();
                    let d = if matches!(key.code, KeyCode::Up | KeyCode::BackTab) { -1 } else { 1 };
                    self.move_cursor(d);
                    return true;
                }
                _ => {}
            }
            if let Some((_, text, caret)) = self.edit.as_mut()
                && textedit::edit_key(text, caret, key) != Edit::Ignored
            {
                return true;
            }
            return true;
        }
        match key.code {
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Home if !ctrl => self.cursor = 0,
            KeyCode::End if !ctrl => self.move_cursor(self.rows().len() as isize),
            KeyCode::Enter => self.begin_edit(),
            // Clear the selected field outright.
            KeyCode::Delete => {
                let rows = self.rows();
                if let Some(Row::Field(_, i)) = rows.get(self.cursor) {
                    self.values[*i].clear();
                }
            }
            // Typing a printable character starts editing with it, so a value
            // can be replaced without pressing Enter first.
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                let rows = self.rows();
                if let Some(Row::Field(_, _)) = rows.get(self.cursor) {
                    self.edit = Some((self.cursor, c.to_string(), 1));
                }
            }
            _ => return false,
        }
        true
    }

    /// A click at `row` on screen selects that row, and a second click on the
    /// row already selected starts editing it.
    pub(crate) fn click(&mut self, row: u16) {
        if self.area.height == 0 || row < self.area.y {
            return;
        }
        let idx = self.top + (row - self.area.y) as usize;
        let rows = self.rows();
        if idx >= rows.len() || matches!(rows[idx], Row::Heading(_)) {
            return;
        }
        if self.cursor == idx && self.edit.is_none() {
            self.begin_edit();
        } else {
            self.commit_edit();
            self.cursor = idx;
        }
    }
}
