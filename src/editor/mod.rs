//! Internal `mcedit`-style text editor.
//!
//! The editor is a full-screen overlay (like the viewer). It owns an
//! [`EditorBuffer`] and all cursor/selection state; the app handles only the
//! async file save when the editor asks for it.

pub mod buffer;
pub mod hex;
mod inspector;
mod jsoncheck;
pub mod menu;
pub mod render;
mod sheet;
mod template;

use crate::config::{EditorOptions, WrapMode};
use crate::vfs::VfsPath;
use buffer::EditorBuffer;
use menu::{EditorAction, EditorMenu};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::path::Path;

/// What the app should do after the editor handles a key.
pub enum EditorSignal {
    Stay,
    Close,
    /// Persist the buffer; close the editor afterwards if `close_after`.
    Save {
        close_after: bool,
    },
    /// Open the "Save as" browser to write the buffer to a chosen path.
    SaveAs,
    /// The buffer is modified and the user asked to quit: the app should show a
    /// modal save/discard/cancel confirmation.
    ConfirmQuit,
    /// Open the modal search dialog (F7).
    OpenSearch,
    /// Open the modal search & replace dialog (F4).
    OpenReplace,
    /// Start a fresh, unnamed buffer (File → New).
    NewFile,
    /// Open the file browser for one of the editor's file actions.
    Browse(BrowseKind),
    /// Open the "go to line" prompt.
    OpenGotoLine,
    /// Open the block-sort options dialog.
    OpenSortBlock,
    /// Open the "paste output of a command" prompt.
    OpenPasteOutput,
    /// Open the editor options dialog (Options → General).
    OpenOptions,
    /// Persist the current editor options as the saved defaults.
    SaveSetup,
    /// Show the About box.
    About,
    /// Repaint the whole screen from scratch (Ctrl-L).
    RefreshScreen,
    /// Draw the buffer's GeoJSON on a map (Alt-M).
    OpenGeoMap,
    /// Pick the binary template for the hex view (F5).
    OpenTemplatePicker,
    /// Open this binary template in a text editor, at a line if given, coming
    /// back to this hex editor when that one closes.
    EditTemplate {
        path: std::path::PathBuf,
        line: Option<usize>,
    },
    /// Ask for a name and start a new binary template for this file.
    NewTemplate,
    /// Show this text: the JWT at the cursor, decoded.
    ShowJwt(String),
}

/// Which of the editor's file actions a browser was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseKind {
    /// Replace the buffer with the chosen file (File → Open).
    Open,
    /// Insert the chosen file at the cursor (File → Insert file).
    Insert,
    /// Write the marked block — or the whole buffer — to the chosen file.
    CopyTo,
}

/// Remembered search options for "find next" (`n`).
#[derive(Default, Clone)]
struct LastSearch {
    pattern: String,
    regex: bool,
    case_sensitive: bool,
    whole_words: bool,
    backwards: bool,
}

pub struct EditorState {
    pub name: String,
    pub path: VfsPath,
    buf: EditorBuffer,
    /// Cursor as an absolute char index.
    cursor: usize,
    /// Preferred column for vertical movement.
    goal_col: Option<usize>,
    pub dirty: bool,
    top_line: usize,
    left_col: usize,
    /// Live marking anchor (block being extended by the cursor).
    anchor: Option<usize>,
    /// Whether the live `anchor` was started by a Shift+move (so a plain move
    /// collapses it), as opposed to an explicit F3 mark (which a move extends).
    shift_marking: bool,
    /// Finalized block (start, end) in char indices.
    block: Option<(usize, usize)>,
    clipboard: String,
    /// Text the editor wants pushed to the *system* clipboard, drained by the
    /// app loop after a key is handled. Kept as state rather than written here
    /// so this stays a pure state machine with no terminal I/O — which is what
    /// lets every editor test run headlessly.
    pending_clip: Option<String>,
    last_search: LastSearch,
    /// Lines holding a match, from the search dialog's "Find all". Highlighted
    /// until the next Find all replaces the set — or until the editor closes,
    /// which drops it with the rest of this state. A plain "find next" leaves it
    /// alone, so the hits stay visible while you work through them.
    found_lines: std::collections::HashSet<usize>,
    status: String,
    view_rows: usize,
    view_cols: usize,
    /// Text body and footer (F-key bar) rects, recorded by the renderer for
    /// mouse hit-testing.
    text_area: Rect,
    footer_area: Rect,
    /// The status/menu-bar row, recorded by the renderer for menu hit-testing.
    menu_area: Rect,
    /// Where a left-drag selection began (char index), while a drag is active.
    mouse_anchor: Option<usize>,
    /// When `Some`, the editor is in (in-place, file-backed) hex mode.
    hex: Option<hex::HexEditor>,
    /// Incremental syntax highlighter (text mode), when a syntax matched.
    hl: Option<crate::syntax::Highlighter>,
    /// Virtual (display-only) word wrap: long logical lines are shown across
    /// several screen rows, each continued row ending in a `>` marker.
    wrap: bool,
    /// First visible *sub-row* within `top_line` when wrapping (0 otherwise).
    top_sub: usize,
    /// Whether the F1 keyboard-shortcut help overlay is showing.
    help_open: bool,
    /// Modifiers on the last key event, so the F-key bar can show the alternate
    /// F2 / F9 labels ("Save as" / "Wrap") while Shift or Ctrl is held.
    hint_mods: KeyModifiers,
    /// A fresh buffer with no filename yet (`rc /edit` with no file). Its first
    /// save is redirected to "Save as"; cleared once a path is chosen.
    unnamed: bool,
    /// Set by [`restore_position`](EditorState::restore_position) so the next
    /// render scrolls the restored cursor to the vertical center of the view.
    pending_center: bool,
    /// The open F9 pulldown menu, when one is showing.
    menu: Option<EditorMenu>,
    /// The persisted behaviour settings (Options → General).
    opts: EditorOptions,
    /// Typing replaces the character under the cursor instead of pushing it
    /// along (the Ins toggle).
    overwrite: bool,
    /// Bookmarked line numbers (Alt-K), jumped between with Alt-J / Alt-I.
    bookmarks: std::collections::HashSet<usize>,
    /// Whether the syntax theme picked by [`enable_syntax`](EditorState::enable_syntax)
    /// was the dark one — remembered so the Ctrl-S toggle can rebuild the
    /// highlighter without asking the app again.
    hl_dark: bool,
    /// The spreadsheet grid over a CSV or TSV file (Alt-G switches it with the
    /// text), when the file is one — or when it was asked for.
    sheet: Option<sheet::SheetGrid>,
    /// The live syntax check of a JSON file.
    json: Option<jsoncheck::JsonCheck>,
    /// The binary template run over the file in hex mode, and its panel.
    tpl: Option<template::TemplateState>,
    /// The template panel's rect, recorded by the renderer for the mouse.
    tpl_area: Rect,
    /// The data inspector beside the bytes in hex mode (F8).
    insp: inspector::InspectorState,
}

/// Above this size a file is opened straight into hex mode (text mode loads the
/// whole file, so it's reserved for reasonably sized files).
pub const MAX_TEXT_EDIT: u64 = crate::viewer::MAX_VIEW_BYTES as u64;

/// Editor keyboard shortcuts shown by the F1 help overlay: `(keys, description)`.
pub const EDITOR_HELP: &[(&str, &str)] = &[
    ("F1", "This help"),
    ("F2", "Save"),
    ("Shift-F2", "Save as…"),
    ("F3", "Start / end block mark"),
    ("F4", "Search & replace"),
    ("F5 / Shift-F5", "Copy block to the cursor / insert a file"),
    ("F6", "Move block to the cursor"),
    ("F7 / Shift-F7", "Search / search again"),
    ("F8", "Delete block"),
    ("F9", "Menu"),
    ("Shift-F9", "Toggle word wrap"),
    ("Ctrl-F9", "Toggle hex editor"),
    ("Alt-G", "Spreadsheet grid / text (CSV, TSV)"),
    ("Alt-E / Alt-Shift-E", "Next / previous JSON syntax error"),
    ("Alt-M", "Show and edit the GeoJSON in the file on a map"),
    ("Hex: F5 / Shift-F5", "Choose / rerun the binary template"),
    ("Hex: F6", "Template variable at the cursor / back to the bytes"),
    ("Hex: F8", "Data inspector: the bytes at the cursor as numbers, text, dates"),
    ("Hex: Tab / Shift-Tab", "Hex / ASCII column / inspector / template tree"),
    ("Hex: F3", "Template output / variables"),
    ("Inspector: Enter / b", "Edit the value / switch the byte order"),
    ("Tree: Enter / ← →", "Edit the value or open / close, parent"),
    ("Tree: + - *", "Open, close, open everything below"),
    ("Grid: Enter / F3", "Edit the cell / header row on or off"),
    ("Grid: F5 F6 / F8", "Insert row, column / delete row (Shift: column)"),
    ("F10 / Esc", "Quit (prompts if modified)"),
    ("Ins", "Toggle insert / overwrite"),
    ("Ctrl-C / X / V", "Copy / cut block to clipboard, paste"),
    ("Ctrl-Z / Ctrl-Y", "Undo / redo"),
    ("Ctrl-A", "Mark the whole file"),
    ("Ctrl-N / Ctrl-F", "New buffer / copy block to a file"),
    ("Ctrl-S / Ctrl-L", "Toggle syntax highlighting / repaint"),
    ("Alt-L / Alt-B", "Go to line / matching bracket"),
    ("Alt-P / Alt-T / Alt-U", "Format paragraph / sort block / paste output"),
    ("Alt-K / J / I / O", "Bookmark: toggle, next, previous, flush"),
    ("Shift + arrows", "Mark text while moving"),
    ("Ctrl-← / →", "Move by word"),
    ("Ctrl-Home / End", "Start / end of document"),
    ("Home / End", "Start / end of line"),
    ("PgUp / PgDn", "Page up / down"),
];

impl EditorState {
    pub fn new(name: String, path: VfsPath, text: &str) -> Self {
        let mut ed = EditorState {
            name,
            path,
            buf: EditorBuffer::from_str(text),
            cursor: 0,
            goal_col: None,
            dirty: false,
            top_line: 0,
            left_col: 0,
            anchor: None,
            shift_marking: false,
            block: None,
            clipboard: String::new(),
            pending_clip: None,
            last_search: LastSearch::default(),
            found_lines: std::collections::HashSet::new(),
            status: String::new(),
            view_rows: 1,
            view_cols: 1,
            text_area: Rect::default(),
            footer_area: Rect::default(),
            menu_area: Rect::default(),
            mouse_anchor: None,
            hex: None,
            hl: None,
            wrap: false,
            top_sub: 0,
            help_open: false,
            hint_mods: KeyModifiers::NONE,
            unnamed: false,
            pending_center: false,
            menu: None,
            opts: EditorOptions::default(),
            overwrite: false,
            bookmarks: std::collections::HashSet::new(),
            hl_dark: false,
            sheet: None,
            json: None,
            tpl: None,
            tpl_area: Rect::default(),
            insp: inspector::InspectorState::default(),
        };
        ed.detect_kind();
        ed
    }

    /// The cursor's 0-based `(line, column)` — the unit remembered across
    /// sessions so a file re-opens where it was left.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let line = self.buf.char_to_line(self.cursor);
        let col = self.cursor - self.buf.line_to_char(line);
        (line, col)
    }

    /// Restore a remembered cursor `(line, column)` (clamped to the buffer, which
    /// may have changed since) and request that the next render scroll it to the
    /// vertical center. A no-op in hex mode, whose cursor is a byte offset.
    pub fn restore_position(&mut self, line: usize, col: usize) {
        if self.hex.is_some() {
            return;
        }
        let last_line = self.buf.len_lines().saturating_sub(1);
        let line = line.min(last_line);
        let col = col.min(self.buf.line_len(line));
        self.cursor = self.buf.line_to_char(line) + col;
        self.pending_center = true;
    }

    /// A fresh, unnamed buffer (`rc /edit` with no file). It carries no path, so
    /// the first save is routed through "Save as" to obtain a filename. The name
    /// is a display-only placeholder for the status line.
    pub fn new_unnamed() -> Self {
        let mut ed = EditorState::new("[No Name]".to_string(), VfsPath::local_cwd(), "");
        ed.unnamed = true;
        ed
    }

    /// Whether this is a fresh buffer with no filename yet.
    pub fn is_unnamed(&self) -> bool {
        self.unnamed
    }

    /// Clear the unnamed flag once the buffer has been assigned a path (via
    /// "Save as"), so subsequent saves write in place.
    pub fn set_named(&mut self) {
        self.unnamed = false;
    }

    /// Track held Shift/Ctrl from a key event to drive the F-key bar's alternate
    /// labels. Fed every key event by the event loop *only* when the terminal's
    /// enhanced keyboard protocol reports standalone modifier presses/releases —
    /// so on terminals without it the labels never change (rather than sticking).
    /// Held state is tracked from the modifier keys' own press/release events
    /// (a modifier-key release still reports the modifier as set, so mirroring
    /// `key.modifiers` would never clear).
    pub fn note_key(&mut self, key: KeyEvent) {
        use ratatui::crossterm::event::ModifierKeyCode;
        if let KeyCode::Modifier(m) = key.code {
            let bit = match m {
                ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift => KeyModifiers::SHIFT,
                ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl => {
                    KeyModifiers::CONTROL
                }
                _ => return,
            };
            if key.kind == KeyEventKind::Release {
                self.hint_mods.remove(bit);
            } else {
                self.hint_mods.insert(bit);
            }
        }
    }

    /// The F-key bar labels for the current mode and modifier state. Holding
    /// Shift or Ctrl in text mode swaps in the alternates those modifiers reach:
    /// "Save as", "Insert file", "Search again", and F9's two view toggles.
    pub fn footer_labels(&self) -> [String; 10] {
        let shift = self.hint_mods.contains(KeyModifiers::SHIFT);
        let ctrl = self.hint_mods.contains(KeyModifiers::CONTROL);
        let src = if self.hex.is_some() {
            self.hex_fkey_labels()
        } else if self.sheet_active() {
            let mut labels = crate::ui::fkeys::SHEET_LABELS;
            if shift || ctrl {
                labels[1] = "Save as"; // F2
            }
            if shift {
                labels[6] = "Again"; // F7
                labels[7] = "DelCol"; // F8
            }
            if ctrl {
                labels[8] = "Hex"; // F9
            }
            labels
        } else {
            let mut labels = crate::ui::fkeys::EDITOR_LABELS;
            if shift || ctrl {
                labels[1] = "Save as"; // F2
            }
            if shift {
                labels[4] = "InsFil"; // F5
                labels[6] = "Again"; // F7
                labels[8] = "Wrap"; // F9
            }
            if ctrl {
                labels[8] = "Hex"; // F9
            }
            labels
        };
        // Translate each label into the active language (RTL-reshaped for display).
        src.map(crate::l10n::trd)
    }

    /// Whether the F1 shortcut-help overlay is currently shown.
    pub fn help_open(&self) -> bool {
        self.help_open
    }

    /// Whether virtual word wrap is on (for the renderer / status line).
    pub fn wrap(&self) -> bool {
        self.wrap
    }

    /// Turn on syntax highlighting if a syntax matches the file name and the
    /// content is within the size cap. `dark` selects a fitting bundled theme.
    pub fn enable_syntax(&mut self, dark: bool) {
        if self.buf.len_chars() <= crate::syntax::HL_MAX_BYTES {
            self.hl = crate::syntax::Highlighter::for_file(&self.name, dark);
        }
    }

    /// Ensure the highlighter has processed lines up to (and including) `upto`.
    fn ensure_hl(&mut self, upto: usize) {
        let total = self.buf.len_lines();
        let Some(hl) = self.hl.as_mut() else {
            return;
        };
        // Disjoint field borrows: `hl` (self.hl) vs. self.buf.
        while hl.processed() < upto && hl.processed() < total {
            let i = hl.processed();
            let display = self.buf.line_text(i);
            hl.process_next(&display);
        }
    }

    /// Per-character foreground colors for `line` (length `len`), or `None` when
    /// highlighting is off.
    fn line_fg(
        &self,
        line: usize,
        len: usize,
        default: ratatui::style::Color,
    ) -> Option<Vec<ratatui::style::Color>> {
        self.hl.as_ref().map(|hl| hl.line_fg(line, len, default))
    }

    /// Open a (local) file directly in hex mode without loading it into memory —
    /// used for files too large to load as text.
    pub fn new_hex(name: String, path: VfsPath) -> std::io::Result<Self> {
        let hex = hex::HexEditor::open(&path.path)?;
        let mut s = Self::new(name, path, "");
        s.hex = Some(hex);
        s.start_templates();
        Ok(s)
    }

    pub fn is_hex(&self) -> bool {
        self.hex.is_some()
    }

    /// Flush pending in-place hex edits to the file (the app's save path calls
    /// this instead of writing the text buffer when in hex mode).
    pub fn flush_hex(&mut self) -> std::io::Result<()> {
        if let Some(h) = self.hex.as_mut() {
            h.save()?;
        }
        self.dirty = false;
        Ok(())
    }

    /// Hex-mode search / replace. `hex` ⇒ the strings are hex bytes (e.g.
    /// "48 65"); otherwise they are literal ASCII bytes. Replace is overwrite-
    /// only, so the replacement must equal the search length.
    pub fn apply_hex_search_replace(
        &mut self,
        replace: bool,
        search: &str,
        replacement: &str,
        hex: bool,
        backwards: bool,
    ) {
        let parse = |s: &str| -> Option<Vec<u8>> {
            if hex { parse_hex_bytes(s) } else { Some(s.as_bytes().to_vec()) }
        };
        let pat = match parse(search) {
            Some(v) if !v.is_empty() => v,
            _ => {
                self.status = "Invalid search bytes".to_string();
                return;
            }
        };
        let Some(h) = self.hex.as_mut() else {
            return;
        };
        if replace {
            let rep = match parse(replacement) {
                Some(v) => v,
                None => {
                    self.status = "Invalid replacement bytes".to_string();
                    return;
                }
            };
            if rep.len() != pat.len() {
                self.status = "Replacement must be the same length (overwrite-only)".to_string();
                return;
            }
            let n = h.replace_all(&pat, &rep);
            self.dirty = self.hex.as_ref().unwrap().dirty;
            self.status = format!("Replaced {n} occurrence(s)");
        } else {
            let from = if backwards { h.cursor } else { (h.cursor + 1).min(h.len) };
            let found = h.find(&pat, from, backwards);
            match found {
                Some(off) => {
                    h.cursor = off;
                    h.nibble_low = false;
                }
                None => self.status = "Not found".to_string(),
            }
        }
    }

    pub fn contents(&self) -> String {
        self.buf.text()
    }

    /// The text as it stands, for reading on another thread.
    pub fn text_snapshot(&self) -> ropey::Rope {
        self.buf.snapshot()
    }

    /// The cursor as a byte offset into the text.
    pub fn cursor_byte(&self) -> usize {
        self.buf.snapshot().char_to_byte(self.cursor.min(self.buf.len_chars()))
    }

    /// Make an edit of the GeoJSON map's: a replacement as an undo step of its
    /// own, or an undo or redo of one. The cursor stays on the text it was on.
    pub fn apply_map_edit(&mut self, edit: crate::geo::edit::TextEdit) {
        use crate::geo::edit::TextEdit;
        let (start, end, len) = match &edit {
            TextEdit::Replace { start, end, text } => (*start, *end, text.len()),
            TextEdit::Undo { start, end, len } | TextEdit::Redo { start, end, len } => {
                (*start, *end, *len)
            }
        };
        let (s, e) = (self.buf.byte_to_char(start), self.buf.byte_to_char(end));
        match edit {
            TextEdit::Replace { text, .. } => {
                self.buf.break_undo_group();
                self.buf.replace_range(s, e, &text);
                self.buf.break_undo_group();
            }
            TextEdit::Undo { .. } => {
                if self.buf.undo().is_none() {
                    return;
                }
            }
            TextEdit::Redo { .. } => {
                if self.buf.redo().is_none() {
                    return;
                }
            }
        }
        let new_end = self.buf.byte_to_char(start + len);
        self.cursor = if self.cursor >= e { self.cursor - e + new_end } else { self.cursor.min(s) };
        self.dirty = true;
        self.goal_col = None;
        self.clear_marks();
        let line = self.buf.char_to_line(s);
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(line);
        }
    }

    /// Put the cursor at byte offset `byte` of the text, centred on screen.
    pub fn goto_byte(&mut self, byte: usize) {
        self.pre_move(false);
        self.cursor = self.buf.byte_to_char(byte);
        self.goal_col = None;
        self.pending_center = true;
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
        self.status = "Saved".to_string();
    }

    /// Show a one-line message on the footer row (replacing the F-key bar until
    /// the next key). Used by the app for outcomes it, not the editor, knows.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
    }

    // -- Geometry helpers --------------------------------------------------

    fn cur_line(&self) -> usize {
        self.buf.char_to_line(self.cursor)
    }

    fn cur_col(&self) -> usize {
        self.cursor - self.buf.line_to_char(self.cur_line())
    }

    fn line_start_char(&self, line: usize) -> usize {
        self.buf.line_to_char(line)
    }

    // -- Word-wrap geometry (only active when `self.wrap`) ------------------

    /// Content width of a *continued* wrapped row; the final column is reserved
    /// for the `>` continuation marker.
    fn wrap_seg(&self) -> usize {
        self.view_cols.saturating_sub(1).max(1)
    }

    /// Start offsets (chars within the line) of each visual sub-row of logical
    /// `line`. Always starts with `0`; its length is the number of visual rows.
    /// With wrap off, a width ≤ 1, or a line that fits, this is `[0]`.
    fn line_breaks(&self, line: usize) -> Vec<usize> {
        let len = self.buf.line_len(line);
        if !self.wrap || self.view_cols <= 1 || len <= self.view_cols {
            return vec![0];
        }
        let chars: Vec<char> = self.buf.line_text(line).chars().collect();
        let segw = self.wrap_seg();
        let mut starts = vec![0usize];
        let mut s = 0usize;
        while len - s > self.view_cols {
            let hard = s + segw;
            // Prefer breaking just after the last space/tab within (s, hard);
            // otherwise hard-break. Either way the break is > s (progress).
            let brk = chars[s..hard]
                .iter()
                .rposition(|c| *c == ' ' || *c == '\t')
                .map(|pos| s + pos + 1)
                .unwrap_or(hard);
            starts.push(brk);
            s = brk;
        }
        starts
    }

    /// The cursor's `(logical line, visual sub-row, visual column)`.
    fn cursor_visual(&self) -> (usize, usize, usize) {
        let line = self.cur_line();
        let col = self.cur_col();
        let breaks = self.line_breaks(line);
        let sub = breaks.iter().rposition(|&b| b <= col).unwrap_or(0);
        (line, sub, col - breaks[sub])
    }

    /// Char index at visual column `vcol` of sub-row `sub` on `line` (clamped to
    /// the sub-row's content width).
    fn char_at_subrow(&self, line: usize, sub: usize, vcol: usize) -> usize {
        let breaks = self.line_breaks(line);
        let sub = sub.min(breaks.len() - 1);
        let start = breaks[sub];
        let end = breaks.get(sub + 1).copied().unwrap_or(self.buf.line_len(line));
        self.line_start_char(line) + start + vcol.min(end - start)
    }

    /// The visual row after `(line, sub)`, or `None` at the document end.
    fn vis_next(&self, line: usize, sub: usize) -> Option<(usize, usize)> {
        if sub + 1 < self.line_breaks(line).len() {
            Some((line, sub + 1))
        } else if line + 1 < self.buf.len_lines() {
            Some((line + 1, 0))
        } else {
            None
        }
    }

    /// The visual row before `(line, sub)`, or `None` at the document start.
    fn vis_prev(&self, line: usize, sub: usize) -> Option<(usize, usize)> {
        if sub > 0 {
            Some((line, sub - 1))
        } else if line > 0 {
            Some((line - 1, self.line_breaks(line - 1).len() - 1))
        } else {
            None
        }
    }

    /// Move the cursor by `delta` *visual* rows (word-wrap mode), keeping the
    /// goal visual column.
    fn move_vertical_wrapped(&mut self, delta: isize) {
        let (line, sub, vcol) = self.cursor_visual();
        let goal = self.goal_col.unwrap_or(vcol);
        self.goal_col = Some(goal);
        let mut pos = (line, sub);
        if delta >= 0 {
            for _ in 0..delta {
                match self.vis_next(pos.0, pos.1) {
                    Some(p) => pos = p,
                    None => break,
                }
            }
        } else {
            for _ in 0..(-delta) {
                match self.vis_prev(pos.0, pos.1) {
                    Some(p) => pos = p,
                    None => break,
                }
            }
        }
        self.cursor = self.char_at_subrow(pos.0, pos.1, goal);
    }

    // -- Key handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> EditorSignal {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        self.status.clear();

        // The open F9 menu takes every key until it closes or fires an action.
        if self.menu.is_some() {
            return self.handle_menu_key(key);
        }

        // The F1 help overlay swallows the next key (any key closes it).
        if self.help_open {
            self.help_open = false;
            return EditorSignal::Stay;
        }
        if key.code == KeyCode::F(1) {
            self.help_open = true;
            return EditorSignal::Stay;
        }

        // F9 opens the pulldown menu (mcedit's); its Shift/Ctrl variants keep the
        // two view toggles that used to live on the bare key.
        if key.code == KeyCode::F(9) {
            // A cell being edited is written back before the menu or a mode
            // switch can act on the table.
            self.commit_cell_edit();
            if ctrl {
                self.toggle_hex();
            } else if shift {
                self.toggle_wrap();
            } else {
                self.open_menu(0);
            }
            return EditorSignal::Stay;
        }
        if self.hex.is_some() {
            return self.handle_hex_key(key);
        }
        if self.sheet_active() {
            return self.handle_sheet_key(key);
        }

        // Run the edit, then — if the buffer changed — invalidate the syntax
        // highlight from the first affected line so only the suffix re-highlights.
        let pre_rev = self.buf.revision();
        let pre_line = self.cur_line();
        let sig = self.handle_text_key(key, ctrl);
        if self.buf.revision() != pre_rev {
            let from = pre_line.min(self.cur_line());
            if let Some(hl) = self.hl.as_mut() {
                hl.invalidate(from);
            }
        }
        sig
    }

    // -- The F9 pulldown menu ----------------------------------------------

    /// Open the menu bar on menu `active` (0 = File).
    fn open_menu(&mut self, active: usize) {
        let mode = if self.is_hex() {
            menu::MenuMode::Hex
        } else if self.sheet_active() {
            menu::MenuMode::Sheet
        } else {
            menu::MenuMode::Text
        };
        self.menu =
            Some(menu::editor_menu(active, mode, self.json_checked(), self.template_panel()));
    }

    /// Whether the F9 menu is currently open (the renderer draws it over the
    /// status row, and the app keeps Esc from being taken as a key prefix).
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// The open menu, for the renderer.
    pub(crate) fn menu_mut(&mut self) -> Option<&mut EditorMenu> {
        self.menu.as_mut()
    }

    /// Route a key to the open menu, running whatever it activates.
    fn handle_menu_key(&mut self, key: KeyEvent) -> EditorSignal {
        use crate::ui::pulldown::MenuSignal;
        let signal = self.menu.as_mut().expect("only called with a menu open").handle_key(key);
        match signal {
            MenuSignal::Stay => EditorSignal::Stay,
            MenuSignal::Close => {
                self.menu = None;
                EditorSignal::Stay
            }
            MenuSignal::Activate(action) => {
                self.menu = None;
                self.run_menu_action(action)
            }
        }
    }

    /// Carry out a menu item. Most act on the buffer here and now; the rest ask
    /// the app for a dialog through an [`EditorSignal`].
    fn run_menu_action(&mut self, action: EditorAction) -> EditorSignal {
        use EditorAction as A;
        match action {
            A::Separator => {}

            // -- File --
            A::OpenFile => return EditorSignal::Browse(BrowseKind::Open),
            A::NewFile => return EditorSignal::NewFile,
            A::Save => return EditorSignal::Save { close_after: false },
            A::SaveAs => return EditorSignal::SaveAs,
            A::InsertFile => return EditorSignal::Browse(BrowseKind::Insert),
            A::CopyToFile => return EditorSignal::Browse(BrowseKind::CopyTo),
            A::About => return EditorSignal::About,
            A::Quit => {
                return if self.dirty { EditorSignal::ConfirmQuit } else { EditorSignal::Close };
            }

            // -- Edit --
            A::Undo => self.undo(),
            A::Redo => self.redo(),
            A::ToggleInsert => self.toggle_overwrite(),
            A::ToggleMark => self.toggle_mark(),
            A::MarkAll => self.mark_all(),
            A::Unmark => {
                self.clear_marks();
                self.status = "Unmarked".to_string();
            }
            A::CopyBlock => self.copy_block(),
            A::MoveBlock => self.move_block(),
            A::DeleteBlock => self.delete_block(),
            A::ClipCopy | A::ClipCut | A::ClipPaste if self.sheet_active() => {
                self.sheet_action(action);
            }
            A::ClipCopy => self.copy_to_clipboard(),
            A::ClipCut => self.cut_to_clipboard(),
            A::ClipPaste => self.paste(),
            A::DocStart => {
                self.pre_move(false);
                self.cursor = 0;
                self.goal_col = None;
            }
            A::DocEnd => {
                self.pre_move(false);
                self.cursor = self.buf.len_chars();
                self.goal_col = None;
            }

            // -- Search --
            A::Search => return EditorSignal::OpenSearch,
            A::SearchAgain => self.search_again(),
            A::Replace => return EditorSignal::OpenReplace,
            A::BookmarkToggle => self.bookmark_toggle(),
            A::BookmarkNext => self.bookmark_jump(true),
            A::BookmarkPrev => self.bookmark_jump(false),
            A::BookmarkFlush => self.bookmark_flush(),
            A::NextError => self.jump_json_error(true),
            A::PrevError => self.jump_json_error(false),

            // -- Command --
            A::GotoLine => return EditorSignal::OpenGotoLine,
            A::MatchBracket => self.goto_matching_bracket(),
            A::ToggleSyntax => self.toggle_syntax(),
            A::ToggleWrap => self.toggle_wrap(),
            A::ToggleHex => self.toggle_hex(),
            A::ToggleInspector => self.toggle_inspector(),
            A::ToggleSheet => self.toggle_sheet(),
            A::RefreshScreen => return EditorSignal::RefreshScreen,
            A::GeoMap => return EditorSignal::OpenGeoMap,
            A::DecodeJwt => {
                let (line, col) = self.cursor_line_col();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                match crate::certs::jwt::token_at(&self.buf.line_text(line), col) {
                    None => self.status = "No JWT at the cursor".to_string(),
                    Some(token) => match crate::certs::jwt::decode(&token, now) {
                        Ok(text) => return EditorSignal::ShowJwt(text),
                        Err(e) => self.status = format!("Not a JWT: {e}"),
                    },
                }
            }
            A::TemplateMenu => {}
            A::ChooseTemplate => return EditorSignal::OpenTemplatePicker,
            A::RerunTemplate => {
                if let Some(t) = self.tpl.as_mut()
                    && matches!(t.choice, template::TemplateChoice::Off)
                {
                    t.choice = template::TemplateChoice::Auto;
                }
                self.run_template();
            }
            A::JumpToVariable => self.jump_to_template_variable(),
            A::EditTemplate => match self.active_template().and_then(|i| i.path) {
                Some(path) => {
                    return EditorSignal::EditTemplate { path, line: self.template_error_line() };
                }
                None => self.status = "No template file to edit".to_string(),
            },
            A::NewTemplate => return EditorSignal::NewTemplate,
            A::CloseTemplate => self.set_template(None),

            // -- Format --
            A::InsertDateTime => self.insert_date_time(),
            A::FormatParagraph => self.format_paragraph(),
            A::SortBlock => return EditorSignal::OpenSortBlock,
            A::PasteOutput => return EditorSignal::OpenPasteOutput,
            A::SheetInsertRow
            | A::SheetDeleteRow
            | A::SheetInsertCol
            | A::SheetDeleteCol
            | A::SheetHeader => self.sheet_action(action),

            // -- Options --
            A::Options => return EditorSignal::OpenOptions,
            A::SaveSetup => return EditorSignal::SaveSetup,
        }
        EditorSignal::Stay
    }

    // -- Settings ----------------------------------------------------------

    /// Apply the persisted editor options (at open time, and again whenever the
    /// options dialog is accepted).
    pub fn set_options(&mut self, opts: EditorOptions, dark: bool) {
        self.wrap = opts.wrap_mode == WrapMode::Dynamic;
        if !self.wrap {
            self.top_sub = 0;
        }
        self.buf.set_group_undo(opts.group_undo);
        self.hl_dark = dark;
        if self.insp.shown != opts.hex_inspector {
            self.toggle_inspector();
        }
        self.insp.big_endian = opts.hex_inspector_big_endian;
        self.opts = opts;
        // Idempotent, so this is also how a freshly opened editor gets its
        // highlighter: build one if it should have one, drop it if not.
        if self.opts.syntax_highlighting {
            if self.hl.is_none() {
                self.enable_syntax(dark);
            }
        } else {
            self.hl = None;
        }
        self.goal_col = None;
    }

    /// The editor's current options (the dialog opens on these).
    pub fn options(&self) -> &EditorOptions {
        &self.opts
    }

    /// Whether F2 should raise a confirmation before writing the file.
    pub fn confirm_before_saving(&self) -> bool {
        self.opts.confirm_before_saving
    }

    /// Whether the cursor position in this file should be remembered.
    pub fn save_file_position(&self) -> bool {
        self.opts.save_file_position
    }

    /// Typing replaces rather than inserts (shown in the status line).
    pub fn overwrite(&self) -> bool {
        self.overwrite
    }

    /// Whether `line` carries a bookmark (the renderer tints it).
    pub(crate) fn line_bookmarked(&self, line: usize) -> bool {
        self.bookmarks.contains(&line)
    }

    /// Whether tab characters are drawn as a visible arrow.
    pub(crate) fn show_tabs(&self) -> bool {
        self.opts.visible_tabs
    }

    /// Whether whitespace at the end of a line is marked.
    pub(crate) fn show_trailing_spaces(&self) -> bool {
        self.opts.visible_trailing_spaces
    }

    fn toggle_overwrite(&mut self) {
        self.overwrite = !self.overwrite;
        self.status =
            if self.overwrite { "Overwrite mode".to_string() } else { "Insert mode".to_string() };
    }

    /// Toggle the display-only word wrap (Shift-F9). Hex mode has no wrapping.
    fn toggle_wrap(&mut self) {
        if self.hex.is_some() {
            return;
        }
        self.wrap = !self.wrap;
        self.left_col = 0;
        self.top_sub = 0;
        self.goal_col = None;
        self.status =
            if self.wrap { "Word wrap ON".to_string() } else { "Word wrap OFF".to_string() };
    }

    /// Toggle syntax colouring, rebuilding the highlighter when turning it on.
    fn toggle_syntax(&mut self) {
        if self.hl.is_some() {
            self.hl = None;
            self.status = "Syntax highlighting OFF".to_string();
            return;
        }
        self.enable_syntax(self.hl_dark);
        self.status = if self.hl.is_some() {
            "Syntax highlighting ON".to_string()
        } else {
            "No syntax matches this file".to_string()
        };
    }

    fn undo(&mut self) {
        match self.buf.undo() {
            Some(c) => {
                self.cursor = c.min(self.buf.len_chars());
                self.dirty = true;
                self.clear_marks();
            }
            None => self.status = "Nothing to undo".to_string(),
        }
    }

    fn redo(&mut self) {
        match self.buf.redo() {
            Some(c) => {
                self.cursor = c.min(self.buf.len_chars());
                self.dirty = true;
                self.clear_marks();
            }
            None => self.status = "Nothing to redo".to_string(),
        }
    }

    // -- Bookmarks ---------------------------------------------------------

    fn bookmark_toggle(&mut self) {
        let line = self.cur_line();
        if self.bookmarks.remove(&line) {
            self.status = format!("Bookmark cleared on line {}", line + 1);
        } else {
            self.bookmarks.insert(line);
            self.status = format!("Bookmark set on line {}", line + 1);
        }
    }

    /// Jump to the next (or previous) bookmarked line, wrapping around.
    fn bookmark_jump(&mut self, forward: bool) {
        if self.bookmarks.is_empty() {
            self.status = "No bookmarks".to_string();
            return;
        }
        let cur = self.cur_line();
        let mut lines: Vec<usize> = self.bookmarks.iter().copied().collect();
        lines.sort_unstable();
        let target = if forward {
            lines.iter().find(|&&l| l > cur).copied().or_else(|| lines.first().copied())
        } else {
            lines.iter().rev().find(|&&l| l < cur).copied().or_else(|| lines.last().copied())
        };
        if let Some(line) = target {
            self.goto_line(line);
        }
    }

    fn bookmark_flush(&mut self) {
        let n = self.bookmarks.len();
        self.bookmarks.clear();
        self.status = format!("Cleared {n} bookmark(s)");
    }

    /// Put the cursor at the start of `line` (0-based, clamped).
    pub fn goto_line(&mut self, line: usize) {
        let line = line.min(self.buf.len_lines().saturating_sub(1));
        self.pre_move(false);
        self.cursor = self.line_start_char(line);
        self.goal_col = None;
        self.pending_center = true;
    }

    // -- Command menu actions ----------------------------------------------

    /// Repeat the last search in the same direction (Shift-F7).
    fn search_again(&mut self) {
        if self.last_search.pattern.is_empty() {
            self.status = "No previous search".to_string();
            return;
        }
        self.search_next();
    }

    /// Jump between a bracket and its partner. Looks at the character under the
    /// cursor, then — so it also works from just past a closing bracket — the one
    /// before it.
    fn goto_matching_bracket(&mut self) {
        const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
        let at = |i: usize| self.buf.char_at(i);
        let (pos, ch) = match at(self.cursor).filter(|c| is_bracket(*c)) {
            Some(c) => (self.cursor, c),
            None => match self.cursor.checked_sub(1).and_then(at).filter(|c| is_bracket(*c)) {
                Some(c) => (self.cursor - 1, c),
                None => {
                    self.status = "No bracket at the cursor".to_string();
                    return;
                }
            },
        };
        let (open, close, forward) = match PAIRS.iter().find(|(o, _)| *o == ch) {
            Some((o, c)) => (*o, *c, true),
            None => {
                let (o, c) = PAIRS.iter().find(|(_, c)| *c == ch).expect("ch is a bracket");
                (*o, *c, false)
            }
        };
        let n = self.buf.len_chars();
        let mut depth = 0i32;
        let mut i = pos;
        loop {
            match at(i) {
                Some(c) if c == open => depth += 1,
                Some(c) if c == close => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                self.cursor = i;
                self.goal_col = None;
                return;
            }
            if forward {
                i += 1;
                if i >= n {
                    break;
                }
            } else {
                if i == 0 {
                    break;
                }
                i -= 1;
            }
        }
        self.status = "No matching bracket".to_string();
    }

    // -- Format menu actions -----------------------------------------------

    /// Insert the current date and time at the cursor, in ISO order.
    fn insert_date_time(&mut self) {
        let (date, time) = crate::rename::date_time_now();
        let stamp = format!(
            "{}-{}-{} {}:{}:{}",
            &date[0..4],
            &date[4..6],
            &date[6..8],
            &time[0..2],
            &time[2..4],
            &time[4..6],
        );
        self.insert_text(&stamp);
    }

    /// Re-wrap the paragraph around the cursor (blank lines delimit it) to the
    /// configured line length, keeping its first line's indentation. Done as one
    /// buffer edit, so one undo step puts it back.
    fn format_paragraph(&mut self) {
        let total = self.buf.len_lines();
        if total == 0 {
            return;
        }
        let blank = |l: usize| self.buf.line_text(l).trim().is_empty();
        let cur = self.cur_line();
        if blank(cur) {
            self.status = "The cursor is not in a paragraph".to_string();
            return;
        }
        let mut first = cur;
        while first > 0 && !blank(first - 1) {
            first -= 1;
        }
        let mut last = cur;
        while last + 1 < total && !blank(last + 1) {
            last += 1;
        }
        // The paragraph's own indentation is preserved on every wrapped line.
        let head = self.buf.line_text(first);
        let indent: String = head.chars().take_while(|c| c.is_whitespace()).collect();
        let words: Vec<String> = (first..=last)
            .flat_map(|l| {
                self.buf.line_text(l).split_whitespace().map(|w| w.to_string()).collect::<Vec<_>>()
            })
            .collect();
        if words.is_empty() {
            return;
        }
        let width = self.opts.word_wrap_line_length.max(indent.chars().count() + 8);
        let mut out = String::new();
        let mut line = indent.clone();
        let mut empty = true;
        for w in words {
            let extra = if empty { 0 } else { 1 };
            if !empty && line.chars().count() + extra + w.chars().count() > width {
                out.push_str(&line);
                out.push('\n');
                line = indent.clone();
                empty = true;
            }
            if !empty {
                line.push(' ');
            }
            line.push_str(&w);
            empty = false;
        }
        out.push_str(&line);

        let start = self.line_start_char(first);
        let end = self.line_start_char(last) + self.buf.line_len(last);
        self.clear_marks();
        self.cursor = self.buf.replace_range(start, end, &out);
        self.dirty = true;
        self.goal_col = None;
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(first);
        }
    }

    /// Sort the marked block's lines (Format → Sort). With no block the whole
    /// buffer is sorted. One buffer edit, so it undoes in one step.
    pub fn sort_block(&mut self, reverse: bool, ignore_case: bool, unique: bool) {
        let (start, end) = match self.block_range() {
            // Grow the block out to whole lines: sorting half a line is nonsense.
            Some((s, e)) => {
                let (ls, le) =
                    (self.buf.char_to_line(s), self.buf.char_to_line(e.saturating_sub(1).max(s)));
                (self.line_start_char(ls), self.line_start_char(le) + self.buf.line_len(le))
            }
            None => (0, self.buf.len_chars()),
        };
        let text = self.buf.slice(start, end);
        let mut lines: Vec<String> = text.split('\n').map(|l| l.to_string()).collect();
        let key = |l: &String| if ignore_case { l.to_lowercase() } else { l.clone() };
        lines.sort_by_key(&key);
        if unique {
            lines.dedup_by(|a, b| key(a) == key(b));
        }
        if reverse {
            lines.reverse();
        }
        let out = lines.join("\n");
        self.clear_marks();
        self.cursor = start;
        self.buf.replace_range(start, end, &out);
        self.dirty = true;
        self.goal_col = None;
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(self.buf.char_to_line(start));
        }
        self.status = format!("Sorted {} line(s)", out.split('\n').count());
    }

    /// Insert `text` at the cursor (the app's hook for "paste output of…" and
    /// "insert file"), as one undo step.
    pub fn insert_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let pre_line = self.cur_line();
        self.insert_text(text);
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(pre_line);
        }
    }

    /// The text File → "Copy to file" should write: the marked block, or the
    /// whole buffer when nothing is marked.
    pub fn block_or_all(&self) -> String {
        match self.block_range() {
            Some((s, e)) => self.buf.slice(s, e),
            None => self.buf.text(),
        }
    }

    /// Replace the whole buffer with `text` (File → Open reuses the open
    /// editor). Resets the view, marks and undo history.
    pub fn load_text(&mut self, name: String, path: VfsPath, text: &str) {
        self.name = name;
        self.path = path;
        self.buf = EditorBuffer::from_str(text);
        self.buf.set_group_undo(self.opts.group_undo);
        self.cursor = 0;
        self.top_line = 0;
        self.top_sub = 0;
        self.left_col = 0;
        self.goal_col = None;
        self.dirty = false;
        self.unnamed = false;
        self.clear_marks();
        self.bookmarks.clear();
        self.found_lines.clear();
        self.hl = None;
        if self.opts.syntax_highlighting {
            self.enable_syntax(self.hl_dark);
        }
        self.sheet = None;
        self.json = None;
        // File → Open from hex mode opens the new file as text.
        self.hex = None;
        self.stop_templates();
        self.detect_kind();
    }

    /// The first bytes of the file in hex mode, for matching templates.
    pub fn file_head(&mut self) -> Vec<u8> {
        match self.hex.as_mut() {
            Some(h) => h.window(0, crate::bt::header::ID_WINDOW),
            None => Vec::new(),
        }
    }

    fn mark_all(&mut self) {
        let n = self.buf.len_chars();
        self.anchor = None;
        self.shift_marking = false;
        self.block = (n > 0).then_some((0, n));
        self.status = format!("Marked {n} chars");
    }

    /// Copy the block to the clipboard and delete it (Ctrl-X).
    fn cut_to_clipboard(&mut self) {
        if self.block_range().is_none() {
            self.status = "No block is marked".to_string();
            return;
        }
        self.copy_to_clipboard();
        self.delete_block();
    }

    /// Route a mouse event: a left click positions the cursor, a left-drag marks
    /// a block (like F3), the wheel scrolls, and the F-key bar acts as buttons.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> EditorSignal {
        let (col, row) = (ev.column, ev.row);

        // While the F9 menu is open it owns the pointer: a click either picks an
        // item or (anywhere else) closes the menu.
        if self.menu.is_some() {
            if !matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                return EditorSignal::Stay;
            }
            use crate::ui::pulldown::MenuSignal;
            let area = Rect { height: 1, ..self.menu_area };
            let signal = self.menu.as_mut().expect("checked above").click(area, col, row);
            return match signal {
                MenuSignal::Stay => EditorSignal::Stay,
                MenuSignal::Close => {
                    self.menu = None;
                    EditorSignal::Stay
                }
                MenuSignal::Activate(action) => {
                    self.menu = None;
                    self.run_menu_action(action)
                }
            };
        }

        // Any click dismisses the help overlay.
        if self.help_open && matches!(ev.kind, MouseEventKind::Down(_)) {
            self.help_open = false;
            return EditorSignal::Stay;
        }

        // A click on the F-key bar acts as that function key (when the bar is
        // actually showing — a status message replaces it).
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && row == self.footer_area.y
            && self.status.is_empty()
        {
            let hex_labels = self.hex_fkey_labels();
            let labels: &[&str] = if self.is_hex() {
                &hex_labels
            } else if self.sheet_active() {
                &crate::ui::fkeys::SHEET_LABELS
            } else {
                &crate::ui::fkeys::EDITOR_LABELS
            };
            return match crate::ui::fkeys::index_at(self.footer_area, labels, col, row) {
                // Pass the click's modifiers through, so clicking the bar while
                // holding Shift/Ctrl triggers the alternate action (Save as / Wrap).
                Some(i) => self.handle_key(KeyEvent::new(KeyCode::F(i as u8 + 1), ev.modifiers)),
                None => EditorSignal::Stay,
            };
        }

        if self.is_hex() {
            return self.handle_hex_mouse(ev);
        }
        if self.sheet_active() {
            return self.handle_sheet_mouse(ev);
        }

        match ev.kind {
            MouseEventKind::ScrollUp => {
                self.status.clear();
                self.move_vertical(-3);
            }
            MouseEventKind::ScrollDown => {
                self.status.clear();
                self.move_vertical(3);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.status.clear();
                if let Some(c) = self.char_at_screen(col, row) {
                    self.cursor = c;
                    self.goal_col = None;
                    self.clear_marks();
                    self.mouse_anchor = Some(c);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(c) = self.char_at_screen(col, row) {
                    // Begin marking on the first drag away from the press point.
                    if self.anchor.is_none()
                        && let Some(a) = self.mouse_anchor
                        && a != c
                    {
                        self.anchor = Some(a);
                    }
                    self.cursor = c;
                    self.goal_col = None;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Finalize a drag selection so it sticks (like a second F3).
                if let Some(a) = self.anchor.take() {
                    self.block = Some(order(a, self.cursor));
                }
                self.mouse_anchor = None;
            }
            _ => {}
        }
        EditorSignal::Stay
    }

    /// Mouse handling in hex mode: the wheel scrolls and a click places the byte
    /// cursor on the clicked hex/ASCII cell.
    fn handle_hex_mouse(&mut self, ev: MouseEvent) -> EditorSignal {
        // Both see every event: a click in one panel takes the keys from the
        // other.
        let in_tree = self.template_mouse(ev);
        let in_inspector = self.inspector_mouse(ev);
        if in_tree || in_inspector {
            return EditorSignal::Stay;
        }
        match ev.kind {
            MouseEventKind::ScrollUp => {
                if let Some(h) = self.hex.as_mut() {
                    h.move_rows(-3);
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(h) = self.hex.as_mut() {
                    h.move_rows(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((off, ascii)) = self.hex_cell_at(ev.column, ev.row)
                    && let Some(h) = self.hex.as_mut()
                {
                    h.cursor = off;
                    h.ascii_pane = ascii;
                    h.nibble_low = false;
                }
            }
            _ => {}
        }
        EditorSignal::Stay
    }

    /// Map a screen point to a char index in text mode (clamped to the buffer).
    fn char_at_screen(&self, col: u16, row: u16) -> Option<usize> {
        let a = self.text_area;
        if a.width == 0
            || a.height == 0
            || col < a.x
            || col >= a.x + a.width
            || row < a.y
            || row >= a.y + a.height
        {
            return None;
        }
        let lines = self.buf.len_lines();
        if lines == 0 {
            return Some(0);
        }
        if self.wrap {
            // Walk visual rows from the scroll position to the clicked row.
            let target = (row - a.y) as usize;
            let mut pos = (self.top_line, self.top_sub);
            for _ in 0..target {
                match self.vis_next(pos.0, pos.1) {
                    Some(p) => pos = p,
                    None => return Some(self.buf.len_chars()),
                }
            }
            return Some(self.char_at_subrow(pos.0, pos.1, (col - a.x) as usize));
        }
        let line = (self.top_line + (row - a.y) as usize).min(lines - 1);
        let col_in = (self.left_col + (col - a.x) as usize).min(self.buf.line_len(line));
        Some(self.line_start_char(line) + col_in)
    }

    /// Map a screen point in hex mode to `(byte offset, ascii_pane)`, by the
    /// layout `render_hex` draws ([`hex::HexGeom`]). `None` when the click
    /// misses a real byte.
    fn hex_cell_at(&self, col: u16, row: u16) -> Option<(u64, bool)> {
        let a = self.text_area;
        let h = self.hex.as_ref()?;
        if row < a.y || row >= a.y + a.height || col < a.x || col >= a.x + a.width {
            return None;
        }
        let (j, ascii) = hex::HexGeom::for_len(h.len).cell_at(col - a.x)?;
        let off = h.top + (row - a.y) as u64 * hex::BYTES_PER_ROW + j as u64;
        (off < h.len).then_some((off, ascii))
    }

    fn handle_text_key(&mut self, key: KeyEvent, ctrl: bool) -> EditorSignal {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // AltGr is Ctrl+Alt on some layouts, so a key that composes a character
        // must not be mistaken for an Alt shortcut.
        let alt = key.modifiers.contains(KeyModifiers::ALT) && !ctrl;
        match key.code {
            KeyCode::F(10) | KeyCode::Esc => {
                if self.dirty {
                    return EditorSignal::ConfirmQuit;
                }
                return EditorSignal::Close;
            }
            // Shift-F2 / Ctrl-F2 → Save as; plain F2 saves to the current path.
            KeyCode::F(2) if shift || ctrl => return EditorSignal::SaveAs,
            KeyCode::F(2) => return EditorSignal::Save { close_after: false },
            KeyCode::F(3) => self.toggle_mark(),
            // Shift-F5 inserts a file at the cursor (mcedit's F15).
            KeyCode::F(5) if shift => return EditorSignal::Browse(BrowseKind::Insert),
            KeyCode::F(5) => self.copy_block(),
            KeyCode::F(6) => self.move_block(),
            KeyCode::F(8) => self.delete_block(),
            KeyCode::F(7) if shift => self.search_again(),
            KeyCode::F(7) => return EditorSignal::OpenSearch,
            KeyCode::F(4) => return EditorSignal::OpenReplace,
            KeyCode::Insert => self.toggle_overwrite(),
            KeyCode::Char('z') if ctrl => self.undo(),
            KeyCode::Char('y') if ctrl => self.redo(),
            KeyCode::Char('c') if ctrl => self.copy_to_clipboard(),
            KeyCode::Char('x') if ctrl => self.cut_to_clipboard(),
            KeyCode::Char('v') if ctrl => self.paste(),
            KeyCode::Char('a') if ctrl => self.mark_all(),
            KeyCode::Char('n') if ctrl => return EditorSignal::NewFile,
            KeyCode::Char('f') if ctrl => return EditorSignal::Browse(BrowseKind::CopyTo),
            KeyCode::Char('s') if ctrl => self.toggle_syntax(),
            KeyCode::Char('l') if ctrl => return EditorSignal::RefreshScreen,
            // The Alt shortcuts mirror mcedit's (M-l, M-b, M-p, M-t, M-u, and the
            // four bookmark keys).
            KeyCode::Char('l') if alt => return EditorSignal::OpenGotoLine,
            KeyCode::Char('b') if alt => self.goto_matching_bracket(),
            KeyCode::Char('p') if alt => self.format_paragraph(),
            KeyCode::Char('t') if alt => return EditorSignal::OpenSortBlock,
            KeyCode::Char('u') if alt => return EditorSignal::OpenPasteOutput,
            KeyCode::Char('k') if alt => self.bookmark_toggle(),
            KeyCode::Char('j') if alt => self.bookmark_jump(true),
            KeyCode::Char('i') if alt => self.bookmark_jump(false),
            KeyCode::Char('o') if alt => self.bookmark_flush(),
            KeyCode::Char('g') if alt => self.toggle_sheet(),
            KeyCode::Char('m') if alt => return EditorSignal::OpenGeoMap,
            KeyCode::Char('e') if alt && !shift => self.jump_json_error(true),
            KeyCode::Char('e' | 'E') if alt => self.jump_json_error(false),

            KeyCode::Up => {
                self.pre_move(shift);
                self.move_vertical(-1);
            }
            KeyCode::Down => {
                self.pre_move(shift);
                self.move_vertical(1);
            }
            // Ctrl-←/→ jump by word; plain ←/→ move one character.
            KeyCode::Left if ctrl => {
                self.pre_move(shift);
                self.word_left();
            }
            KeyCode::Right if ctrl => {
                self.pre_move(shift);
                self.word_right();
            }
            KeyCode::Left => {
                self.pre_move(shift);
                self.move_left();
            }
            KeyCode::Right => {
                self.pre_move(shift);
                self.move_right();
            }
            // Ctrl-Home/End jump to the start/end of the document.
            KeyCode::Home if ctrl => {
                self.pre_move(shift);
                self.cursor = 0;
                self.goal_col = None;
            }
            KeyCode::End if ctrl => {
                self.pre_move(shift);
                self.cursor = self.buf.len_chars();
                self.goal_col = None;
            }
            KeyCode::Home => {
                self.pre_move(shift);
                self.cursor = self.line_start_char(self.cur_line());
                self.goal_col = None;
            }
            KeyCode::End => {
                self.pre_move(shift);
                let line = self.cur_line();
                self.cursor = self.line_start_char(line) + self.buf.line_len(line);
                self.goal_col = None;
            }
            KeyCode::PageUp => {
                self.pre_move(shift);
                self.move_vertical(-(self.view_rows as isize - 1));
            }
            KeyCode::PageDown => {
                self.pre_move(shift);
                self.move_vertical(self.view_rows as isize - 1);
            }

            KeyCode::Enter => self.newline(),
            KeyCode::Tab => self.insert_tab(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char(c) => self.type_char(c),
            _ => {}
        }
        EditorSignal::Stay
    }

    /// Enter: a newline, carrying the current line's indentation onto it when
    /// "Return does autoindent" is on.
    fn newline(&mut self) {
        if !self.opts.return_does_autoindent {
            return self.insert_text("\n");
        }
        // Only the indentation *before* the cursor is copied, so pressing Enter
        // in the middle of a run of leading spaces doesn't over-indent.
        let line = self.cur_line();
        let col = self.cur_col();
        let indent: String = self
            .buf
            .line_text(line)
            .chars()
            .take(col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.insert_text(&format!("\n{indent}"));
    }

    /// Tab: spaces up to the next tab stop, or a literal tab when "Fill tabs
    /// with spaces" is off.
    fn insert_tab(&mut self) {
        if !self.opts.fill_tabs_with_spaces {
            return self.insert_text("\t");
        }
        let w = self.opts.tab_spacing.max(1);
        let n = w - self.cur_col() % w;
        self.insert_text(&" ".repeat(n));
    }

    /// Type one character: overwriting the one under the cursor in overwrite
    /// mode, and hard-wrapping the line afterwards in typewriter mode.
    fn type_char(&mut self, c: char) {
        let over = self.overwrite && self.buf.char_at(self.cursor).is_some_and(|ch| ch != '\n');
        if over {
            self.finalize_marks();
            let pos = self.cursor;
            self.cursor = self.buf.replace_range(pos, pos + 1, &c.to_string());
            self.dirty = true;
            self.goal_col = None;
        } else {
            self.insert_text(&c.to_string());
        }
        if self.opts.wrap_mode == WrapMode::Typewriter {
            self.typewriter_wrap();
        }
    }

    /// Typewriter wrap: once the line the cursor is on runs past the wrap
    /// column, turn the last space before that column into a newline. The space
    /// is *replaced*, so the cursor's char index is unaffected.
    fn typewriter_wrap(&mut self) {
        let limit = self.opts.word_wrap_line_length.max(8);
        let line = self.cur_line();
        if self.buf.line_len(line) <= limit {
            return;
        }
        let chars: Vec<char> = self.buf.line_text(line).chars().collect();
        let Some(brk) =
            chars[..=limit.min(chars.len() - 1)].iter().rposition(|c| *c == ' ' || *c == '\t')
        else {
            return; // one long unbroken word: leave it alone
        };
        let start = self.line_start_char(line);
        // Only break behind the cursor — otherwise typing at the start of a long
        // line would keep re-breaking text the user is not writing.
        if start + brk >= self.cursor {
            return;
        }
        self.buf.replace_range(start + brk, start + brk + 1, "\n");
        self.adjust_block_delete(start + brk, start + brk + 1);
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(line);
        }
    }

    /// Toggle between text and hex modes. Switching is only allowed when the
    /// current mode has no unsaved changes, so the two backing stores can't
    /// diverge (and the in-place file is never clobbered by stale text).
    fn toggle_hex(&mut self) {
        if let Some(h) = self.hex.as_ref() {
            // Leaving hex mode → text mode.
            if h.dirty {
                self.status = "Save (F2) before leaving hex mode".to_string();
                return;
            }
            if h.len > MAX_TEXT_EDIT {
                self.status = "File too large for text mode".to_string();
                return;
            }
            let reload = h.saved_any;
            let path = self.path.path.clone();
            self.hex = None;
            self.stop_templates();
            // Re-read the file so the text view reflects any saved hex edits.
            if reload && let Ok(data) = std::fs::read(&path) {
                self.buf = EditorBuffer::from_str(&String::from_utf8_lossy(&data));
                self.cursor = 0;
                self.top_line = 0;
                self.left_col = 0;
                self.clear_marks();
            }
        } else {
            // Entering hex mode (local files only).
            if self.path.scheme != "file" {
                self.status = "Hex mode requires a local file".to_string();
                return;
            }
            // A fresh buffer's path is the working directory, not a file: hex
            // mode edits a file in place, so it needs a name first.
            if self.unnamed {
                self.status = "Save the buffer (F2) before switching to hex mode".to_string();
                return;
            }
            if self.dirty {
                self.status = "Save (F2) before switching to hex mode".to_string();
                return;
            }
            match hex::HexEditor::open(Path::new(&self.path.path)) {
                Ok(h) => {
                    let ro = h.readonly;
                    self.hex = Some(h);
                    self.start_templates();
                    // No persistent banner; only note the read-only case.
                    if ro {
                        self.status = "read-only file".to_string();
                    }
                }
                Err(e) => self.status = format!("cannot open for hex: {e}"),
            }
        }
    }

    /// Key handling while in hex mode.
    fn handle_hex_key(&mut self, key: KeyEvent) -> EditorSignal {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if let Some(signal) = self.inspector_key(key) {
            return signal;
        }
        if let Some(signal) = self.template_tree_key(key) {
            return signal;
        }
        if let Some(signal) = self.template_hex_key(key) {
            return signal;
        }
        match key.code {
            KeyCode::F(10) | KeyCode::Esc => {
                return if self.dirty { EditorSignal::ConfirmQuit } else { EditorSignal::Close };
            }
            // Saving routes through the app, which flushes the overlay in place.
            KeyCode::F(2) => return EditorSignal::Save { close_after: false },
            KeyCode::F(7) => return EditorSignal::OpenSearch,
            KeyCode::F(4) => return EditorSignal::OpenReplace,
            _ => {}
        }

        let rows = self.view_rows.max(1) as i64;
        let readonly = self.hex.as_ref().map(|h| h.readonly).unwrap_or(true);
        let mut typed = false;
        if let Some(h) = self.hex.as_mut() {
            match key.code {
                KeyCode::Up => h.move_rows(-1),
                KeyCode::Down => h.move_rows(1),
                KeyCode::Left => h.move_by(-1),
                KeyCode::Right => h.move_by(1),
                KeyCode::Home if ctrl => h.goto_start(),
                KeyCode::End if ctrl => h.goto_end(),
                KeyCode::Home => h.row_start(),
                KeyCode::End => h.row_end(),
                KeyCode::PageUp => h.move_rows(-(rows - 1).max(1)),
                KeyCode::PageDown => h.move_rows((rows - 1).max(1)),
                KeyCode::Backspace => h.move_by(-1),
                // Only a plainly typed character edits a byte: a Ctrl/Alt
                // shortcut that hex mode has no answer for must be ignored, not
                // written into the file.
                KeyCode::Char(c) if !ctrl && !alt => {
                    typed = true;
                    if !readonly {
                        if h.ascii_pane {
                            h.input_ascii(c);
                        } else {
                            let _ = h.input_hex(c);
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(h) = &self.hex {
            self.dirty = h.dirty;
        }
        if typed && readonly {
            self.status = "read-only file".to_string();
        }
        EditorSignal::Stay
    }

    /// Apply the result of the modal search / search-and-replace dialog.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_search_replace(
        &mut self,
        replace: bool,
        search: &str,
        replacement: &str,
        regex: bool,
        case_sensitive: bool,
        whole_words: bool,
        backwards: bool,
        find_all: bool,
    ) {
        self.last_search = LastSearch {
            pattern: search.to_string(),
            regex,
            case_sensitive,
            whole_words,
            backwards,
        };
        if replace {
            let n = self.replace_all(search, replacement, regex, case_sensitive, whole_words);
            self.status = format!("Replaced {n} occurrence(s)");
        } else if find_all {
            let n = self.find_all();
            self.status = if n == 0 {
                format!("Not found: {search}")
            } else {
                format!("{n} line(s) highlighted")
            };
            // Land on the first hit too, so the highlight isn't off-screen.
            if n > 0 {
                self.cursor = 0;
                self.search_next();
            }
        } else {
            self.search_next();
        }
    }

    /// Whether `line` holds a "Find all" match (the renderer tints it).
    pub(crate) fn line_found(&self, line: usize) -> bool {
        self.found_lines.contains(&line)
    }

    /// Highlight every line holding a match of the remembered search, replacing
    /// any previous set. Returns the number of lines marked.
    fn find_all(&mut self) -> usize {
        let ls = self.last_search.clone();
        self.found_lines.clear();
        if ls.pattern.is_empty() {
            return 0;
        }
        let Some(re) = Self::build_regex(&ls.pattern, ls.regex, ls.case_sensitive, ls.whole_words)
        else {
            self.status = "Invalid search pattern".to_string();
            return 0;
        };
        let text = self.buf.text();
        // Walk the matches in order, carrying a running line count, so the whole
        // pass stays linear rather than re-counting from the top for each hit.
        let (mut line, mut scanned) = (0usize, 0usize);
        for m in re.find_iter(&text) {
            line += text[scanned..m.start()].bytes().filter(|&b| b == b'\n').count();
            scanned = m.start();
            // A match spanning newlines is attributed to the line it starts on.
            self.found_lines.insert(line);
        }
        self.found_lines.len()
    }

    /// Build a regex from the given options.
    fn build_regex(
        pattern: &str,
        regex: bool,
        case_sensitive: bool,
        whole_words: bool,
    ) -> Option<regex::Regex> {
        let mut pat = if regex { pattern.to_string() } else { regex::escape(pattern) };
        if whole_words {
            pat = format!(r"\b(?:{pat})\b");
        }
        regex::RegexBuilder::new(&pat)
            .case_insensitive(!case_sensitive)
            // The buffer is searched as one string, so without this `^` and `$`
            // would anchor to the start and end of the whole file — in an editor
            // they can only sensibly mean "start/end of a line". `.` still stops
            // at a newline (that would need `dot_matches_new_line`), so a pattern
            // cannot silently swallow whole lines.
            .multi_line(true)
            .build()
            .ok()
    }

    /// Find the next (or previous) match of the remembered search.
    fn search_next(&mut self) {
        let ls = self.last_search.clone();
        if ls.pattern.is_empty() {
            return;
        }
        let Some(re) = Self::build_regex(&ls.pattern, ls.regex, ls.case_sensitive, ls.whole_words)
        else {
            self.status = "Invalid search pattern".to_string();
            return;
        };
        let text = self.buf.text();
        // Work in byte offsets, then convert to a char index.
        let cur_byte = char_to_byte(&text, self.cursor);
        let found = if ls.backwards {
            re.find_iter(&text)
                .filter(|m| m.start() < cur_byte)
                .last()
                .or_else(|| re.find_iter(&text).last())
        } else {
            re.find_at(&text, (cur_byte + 1).min(text.len())).or_else(|| re.find(&text))
        };
        match found {
            Some(m) => {
                self.cursor = text[..m.start()].chars().count();
                self.goal_col = None;
            }
            None => self.status = format!("Not found: {}", ls.pattern),
        }
    }

    /// Replace all matches; returns the count. Done as a single buffer edit so
    /// it is one undo step.
    fn replace_all(
        &mut self,
        search: &str,
        replacement: &str,
        regex: bool,
        case_sensitive: bool,
        whole_words: bool,
    ) -> usize {
        if search.is_empty() {
            return 0;
        }
        let Some(re) = Self::build_regex(search, regex, case_sensitive, whole_words) else {
            self.status = "Invalid search pattern".to_string();
            return 0;
        };
        let text = self.buf.text();
        let count = re.find_iter(&text).count();
        if count == 0 {
            return 0;
        }
        // In literal mode replacement is verbatim; in regex mode allow $1 refs.
        let new_text = if regex {
            re.replace_all(&text, replacement).into_owned()
        } else {
            re.replace_all(&text, regex::NoExpand(replacement)).into_owned()
        };
        let len = self.buf.len_chars();
        self.cursor = self.buf.replace_range(0, len, &new_text).min(new_text.chars().count());
        self.cursor = self.cursor.min(self.buf.len_chars());
        self.dirty = true;
        self.clear_marks();
        // The whole buffer was rewritten — re-highlight from the top.
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(0);
        }
        count
    }

    // -- Movement ----------------------------------------------------------

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.goal_col = None;
    }

    fn move_right(&mut self) {
        if self.cursor < self.buf.len_chars() {
            self.cursor += 1;
        }
        self.goal_col = None;
    }

    fn move_vertical(&mut self, delta: isize) {
        if self.wrap {
            return self.move_vertical_wrapped(delta);
        }
        let line = self.cur_line();
        let goal = self.goal_col.unwrap_or_else(|| self.cur_col());
        self.goal_col = Some(goal);
        let max_line = self.buf.len_lines().saturating_sub(1);
        let target = (line as isize + delta).clamp(0, max_line as isize) as usize;
        let col = goal.min(self.buf.line_len(target));
        self.cursor = self.line_start_char(target) + col;
    }

    /// Selection bookkeeping run before a cursor movement: with Shift held, start
    /// (or keep extending) a live selection. Releasing Shift *finalizes* the
    /// live selection into a fixed block so it stays marked while the cursor
    /// moves around (rather than collapsing). An F3 mark keeps extending.
    fn pre_move(&mut self, shift: bool) {
        if shift {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
                self.block = None;
                self.shift_marking = true;
            }
        } else if self.shift_marking {
            self.finalize_marks();
        } else if !self.opts.persistent_selection {
            // With persistent selection off, moving the cursor drops the mark —
            // the GUI-editor behaviour mcedit's option switches to.
            self.clear_marks();
        }
    }

    /// Whether `c` is part of a word (letters, digits, underscore).
    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Move to the start of the next word (skipping the current word, then any
    /// separators), crossing line breaks like most editors.
    fn word_right(&mut self) {
        let n = self.buf.len_chars();
        let is_word = |i: usize| self.buf.char_at(i).map(Self::is_word_char).unwrap_or(false);
        while self.cursor < n && is_word(self.cursor) {
            self.cursor += 1;
        }
        while self.cursor < n && !is_word(self.cursor) {
            self.cursor += 1;
        }
        self.goal_col = None;
    }

    /// Move to the start of the current or previous word.
    fn word_left(&mut self) {
        let is_word = |i: usize| self.buf.char_at(i).map(Self::is_word_char).unwrap_or(false);
        while self.cursor > 0 && !is_word(self.cursor - 1) {
            self.cursor -= 1;
        }
        while self.cursor > 0 && is_word(self.cursor - 1) {
            self.cursor -= 1;
        }
        self.goal_col = None;
    }

    // -- Editing -----------------------------------------------------------

    fn insert_text(&mut self, text: &str) {
        self.finalize_marks();
        let pos = self.cursor;
        let len = text.chars().count();
        self.cursor = self.buf.insert(pos, text);
        self.adjust_block_insert(pos, len);
        self.dirty = true;
        self.goal_col = None;
    }

    fn backspace(&mut self) {
        self.finalize_marks();
        // "Backspace through tabs": inside a line's leading spaces, one press
        // removes a whole indent step rather than a single space.
        if self.opts.backspace_through_tabs {
            let line = self.cur_line();
            let start = self.line_start_char(line);
            let col = self.cursor - start;
            let indent_only =
                col > 0 && self.buf.slice(start, self.cursor).chars().all(|c| c == ' ');
            if indent_only {
                let w = self.opts.tab_spacing.max(1);
                let back = if col.is_multiple_of(w) { w } else { col % w }.min(col);
                let (d0, d1) = (self.cursor - back, self.cursor);
                self.cursor = self.buf.delete(d0, d1);
                self.adjust_block_delete(d0, d1);
                self.dirty = true;
                self.goal_col = None;
                return;
            }
        }
        if self.cursor > 0 {
            let (d0, d1) = (self.cursor - 1, self.cursor);
            self.cursor = self.buf.delete(d0, d1);
            self.adjust_block_delete(d0, d1);
            self.dirty = true;
        }
        self.goal_col = None;
    }

    fn delete_forward(&mut self) {
        self.finalize_marks();
        if self.cursor < self.buf.len_chars() {
            let (d0, d1) = (self.cursor, self.cursor + 1);
            self.buf.delete(d0, d1);
            self.adjust_block_delete(d0, d1);
            self.dirty = true;
        }
        self.goal_col = None;
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            self.status = "The clipboard is empty".to_string();
            return;
        }
        self.finalize_marks();
        let text = self.clipboard.clone();
        let pos = self.cursor;
        let len = text.chars().count();
        self.cursor = self.buf.insert(pos, &text);
        if !self.opts.cursor_after_inserted_block {
            self.cursor = pos;
        }
        self.adjust_block_insert(pos, len);
        self.dirty = true;
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(self.buf.char_to_line(pos));
        }
    }

    // -- Block operations --------------------------------------------------

    fn clear_marks(&mut self) {
        self.anchor = None;
        self.block = None;
        self.shift_marking = false;
    }

    /// Turn a live (anchor-based) selection into a fixed block so it survives
    /// cursor moves and edits. A zero-length selection is dropped. No-op when
    /// there's no live anchor (a fixed block is left as-is).
    fn finalize_marks(&mut self) {
        if let Some(a) = self.anchor.take() {
            let (s, e) = order(a, self.cursor);
            self.block = (s != e).then_some((s, e));
        }
        self.shift_marking = false;
    }

    /// Shift the fixed block to keep the *same text* marked after inserting `len`
    /// chars at `pos`: text before the block moves it; text inside grows it; text
    /// after it is unaffected. (Live anchors are finalized before any edit.)
    fn adjust_block_insert(&mut self, pos: usize, len: usize) {
        if let Some((s, e)) = self.block {
            let s2 = if pos <= s { s + len } else { s };
            let e2 = if pos < e { e + len } else { e };
            self.block = Some((s2, e2));
        }
    }

    /// Shift the fixed block to keep the same text marked after deleting
    /// `[d0, d1)`. The block shrinks by whatever overlap was removed and is
    /// dropped if the whole marked range is gone.
    fn adjust_block_delete(&mut self, d0: usize, d1: usize) {
        if let Some((s, e)) = self.block {
            let len = d1 - d0;
            let map = |x: usize| {
                if x <= d0 {
                    x
                } else if x >= d1 {
                    x - len
                } else {
                    d0 // a marker inside the removed range collapses to its start
                }
            };
            let (s2, e2) = (map(s), map(e));
            self.block = (s2 < e2).then_some((s2, e2));
        }
    }

    fn toggle_mark(&mut self) {
        // An explicit F3 mark is never a Shift-selection (plain moves extend it).
        self.shift_marking = false;
        if self.anchor.is_some() {
            // Finalize the live block.
            let a = self.anchor.take().unwrap();
            self.block = Some(order(a, self.cursor));
        } else if self.block.is_some() {
            self.block = None;
        } else {
            self.anchor = Some(self.cursor);
            self.block = None;
        }
    }

    /// The current block range (fixed, or live from the anchor).
    fn block_range(&self) -> Option<(usize, usize)> {
        if let Some((s, e)) = self.block {
            Some((s, e))
        } else {
            self.anchor.map(|a| order(a, self.cursor))
        }
    }

    /// F5: insert a copy of the marked block at the cursor (mc-editor "Copy
    /// block"). The original block stays marked.
    fn copy_block(&mut self) {
        let Some((s, e)) = self.block_range() else {
            self.status = "No block is marked".to_string();
            return;
        };
        let text = self.buf.slice(s, e);
        let len = text.chars().count();
        // Finalize a live selection so it tracks the insertion that follows.
        self.finalize_marks();
        let pos = self.cursor;
        self.cursor = self.buf.insert(pos, &text);
        if !self.opts.cursor_after_inserted_block {
            self.cursor = pos;
        }
        self.adjust_block_insert(pos, len);
        self.dirty = true;
        self.status = format!("Copied {len} chars to the cursor");
    }

    /// Take any text the editor wants on the system clipboard. The app loop
    /// calls this after handling a key and does the actual OSC 52 write.
    pub fn take_pending_clip(&mut self) -> Option<String> {
        self.pending_clip.take()
    }

    /// Ctrl-C: copy the marked block to the internal clipboard (paste with Ctrl-V)
    /// and offer it to the system clipboard too, so it can leave the program.
    fn copy_to_clipboard(&mut self) {
        if let Some((s, e)) = self.block_range() {
            self.clipboard = self.buf.slice(s, e);
            self.pending_clip = Some(self.clipboard.clone());
            self.status = format!("Copied {} chars to clipboard", e - s);
        } else {
            self.status = "No block is marked".to_string();
        }
    }

    fn delete_block(&mut self) {
        if let Some((s, e)) = self.block_range() {
            self.buf.delete(s, e);
            self.cursor = s;
            self.dirty = true;
            self.clear_marks();
        }
    }

    fn move_block(&mut self) {
        let Some((s, e)) = self.block_range() else {
            return;
        };
        if self.cursor >= s && self.cursor <= e {
            self.status = "Move target is inside the block".to_string();
            return;
        }
        let text = self.buf.slice(s, e);
        let block_len = e - s;
        self.buf.delete(s, e);
        // Adjust the insertion point for the removed block.
        let insert_at = if self.cursor > e { self.cursor - block_len } else { self.cursor };
        self.cursor = self.buf.insert(insert_at, &text);
        self.dirty = true;
        self.clear_marks();
    }
}

/// Whether `c` is one of the bracket characters "go to matching bracket" pairs.
fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}')
}

fn order(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Parse a hex-byte string like "48 65 6c" or "48656c" into bytes.
fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() || !cleaned.len().is_multiple_of(2) {
        return None;
    }
    (0..cleaned.len()).step_by(2).map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok()).collect()
}

/// Convert a char index into a byte offset within `text`.
fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> EditorState {
        EditorState::new("t".into(), VfsPath::local("/tmp/x"), text)
    }

    #[test]
    fn restore_position_sets_cursor_and_clamps() {
        let mut e = ed("aaaa\nbbbb\ncccc\ndddd");
        e.restore_position(2, 3);
        assert_eq!(e.cursor_line_col(), (2, 3));
        // The char index is line start + column (line 2 starts at char 10).
        assert_eq!(e.cursor, 10 + 3);
        // A line/column past the end clamps to the last line and its length.
        e.restore_position(999, 999);
        assert_eq!(e.cursor_line_col(), (3, 4), "clamped to last line, end of line");
    }

    #[test]
    fn restore_position_centers_the_cursor_on_render() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        // 40 lines; restore onto line 20 and render into a 12-row area (status +
        // 10 text rows + footer), so the cursor should sit ~5 rows from the top.
        let text: String = (0..40).map(|i| format!("line{i}\n")).collect();
        let mut e = ed(&text);
        e.restore_position(20, 0);
        assert_eq!(e.top_line, 0, "not scrolled until the first render");
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(30, 12)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), &mut e, &theme)).unwrap();
        assert_eq!(e.view_rows, 10);
        assert_eq!(e.top_line, 15, "cursor line 20 centered in a 10-row view");
        assert_eq!(e.cur_line(), 20, "the cursor itself stays on the restored line");
        // The one-shot centering doesn't re-fire on the next render.
        assert!(!e.pending_center);
    }

    #[test]
    fn block_selection_follows_theme_not_hardcoded_cyan() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        // Mark a block from the start of "hello" (F3, then extend a few chars).
        let mut e = ed("hello");
        e.handle_key(key(KeyCode::F(3)));
        for _ in 0..3 {
            e.handle_key(key(KeyCode::Right));
        }
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(20, 6)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), &mut e, &theme)).unwrap();
        let b = t.backend().buffer();
        // The first selected cell ('h') is painted with the theme's selection
        // bar, not a hardcoded colour.
        let cell = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .find(|&(x, y)| b[(x, y)].symbol() == "h")
            .expect("'h' rendered");
        assert_eq!(Some(b[cell].bg), theme.cursor.bg, "selection uses the theme cursor bar");
        assert_eq!(Some(b[cell].fg), theme.cursor.fg, "selection uses the theme cursor fg");
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn ctrl_home_end_jump_to_document_ends() {
        let mut e = ed("line0\nline1\nline2");
        e.handle_key(key(KeyCode::Down));
        e.handle_key(key(KeyCode::Right));
        assert_ne!(e.cursor, 0);
        e.handle_key(key_mod(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(e.cursor, 0);
        assert_eq!(e.cur_line(), 0);
        e.handle_key(key_mod(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(e.cursor, e.buf.len_chars());
        assert_eq!(e.cur_line(), 2);
    }

    #[test]
    fn ctrl_arrows_jump_by_word() {
        // "foo bar  baz": words start at 0, 4, 9 (double space before "baz").
        let mut e = ed("foo bar  baz");
        e.handle_key(key_mod(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(e.cursor, 4, "start of \"bar\"");
        e.handle_key(key_mod(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(e.cursor, 9, "start of \"baz\"");
        e.handle_key(key_mod(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(e.cursor, 4, "back to start of \"bar\"");
        e.handle_key(key_mod(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(e.cursor, 0, "back to start of \"foo\"");
        // Word movement crosses line breaks.
        let mut e = ed("a\nbb");
        e.handle_key(key_mod(KeyCode::Right, KeyModifiers::CONTROL)); // past "a"
        e.handle_key(key_mod(KeyCode::Right, KeyModifiers::CONTROL)); // onto "bb"
        assert_eq!(e.cur_line(), 1);
    }

    #[test]
    fn shift_arrows_select_without_f3() {
        let mut e = ed("abcdef");
        e.handle_key(key_mod(KeyCode::Right, KeyModifiers::SHIFT));
        e.handle_key(key_mod(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(e.block_range(), Some((0, 2)), "Shift+Right marks a block");
        e.handle_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)); // Ctrl-C → clipboard
        assert_eq!(e.clipboard, "ab");
        // A plain move now *keeps* the selection (it finalizes to a fixed block).
        e.handle_key(key(KeyCode::Right));
        assert_eq!(e.block_range(), Some((0, 2)), "Shift-selection persists across plain moves");

        // Shift+Ctrl-Right selects a whole word ("foo " up to the next word).
        let mut e = ed("foo bar");
        e.handle_key(key_mod(KeyCode::Right, KeyModifiers::SHIFT | KeyModifiers::CONTROL));
        assert_eq!(e.block_range(), Some((0, 4)));
    }

    #[test]
    fn shift_selection_persists_and_f5_copies_to_cursor() {
        let mut e = ed("hello world");
        // Shift-select "hello".
        for _ in 0..5 {
            e.handle_key(key_mod(KeyCode::Right, KeyModifiers::SHIFT));
        }
        assert_eq!(e.block_range(), Some((0, 5)));
        // Move the cursor away with plain arrows — the selection must stay.
        for _ in 0..3 {
            e.handle_key(key(KeyCode::Right));
        }
        assert_eq!(e.block_range(), Some((0, 5)), "selection persists across plain moves");
        // F5 copies the marked block to the cursor position (mc-editor style).
        e.handle_key(key(KeyCode::End)); // cursor → 11
        e.handle_key(key(KeyCode::F(5)));
        assert_eq!(e.contents(), "hello worldhello");
        assert_eq!(e.block_range(), Some((0, 5)), "the original block stays marked");
    }

    #[test]
    fn block_stays_anchored_to_text_across_edits() {
        let marked = |e: &EditorState| -> Option<String> {
            e.block_range().map(|(s, end)| e.buf.slice(s, end))
        };
        // Mark "cde" in "abcdefg" → fixed block [2,5).
        let mut e = ed("abcdefg");
        for _ in 0..2 {
            e.handle_key(key(KeyCode::Right));
        }
        e.handle_key(key(KeyCode::F(3))); // anchor at 2
        for _ in 0..3 {
            e.handle_key(key(KeyCode::Right)); // cursor → 5
        }
        e.handle_key(key(KeyCode::F(3))); // finalize block [2,5)
        assert_eq!(marked(&e).as_deref(), Some("cde"));

        // Insert before the block — the same text stays marked.
        e.handle_key(key_mod(KeyCode::Home, KeyModifiers::CONTROL));
        e.handle_key(key(KeyCode::Char('X')));
        e.handle_key(key(KeyCode::Char('Y')));
        assert_eq!(e.contents(), "XYabcdefg");
        assert_eq!(marked(&e).as_deref(), Some("cde"), "tracks an insert before the block");

        // Delete before the block — still the same text.
        e.handle_key(key_mod(KeyCode::Home, KeyModifiers::CONTROL));
        e.handle_key(key(KeyCode::Delete)); // remove 'X'
        assert_eq!(e.contents(), "Yabcdefg");
        assert_eq!(marked(&e).as_deref(), Some("cde"), "tracks a delete before the block");

        // Editing *inside* the block keeps the selection (it grows to stay
        // contiguous), it does not clear it. Block is [3,6) ("cde"); insert 'Z'
        // between 'c' and 'd' (index 4).
        e.handle_key(key_mod(KeyCode::Home, KeyModifiers::CONTROL));
        for _ in 0..4 {
            e.handle_key(key(KeyCode::Right));
        }
        e.handle_key(key(KeyCode::Char('Z')));
        assert_eq!(e.contents(), "YabcZdefg");
        assert_eq!(
            marked(&e).as_deref(),
            Some("cZde"),
            "an edit inside the block keeps it marked (and contiguous)"
        );
    }

    #[test]
    fn f3_marking_still_extends_with_plain_arrows() {
        // Regression: an F3 mark must keep extending on *plain* arrows.
        let mut e = ed("abcdef");
        e.handle_key(key(KeyCode::F(3)));
        e.handle_key(key(KeyCode::Right));
        e.handle_key(key(KeyCode::Right));
        assert_eq!(e.block_range(), Some((0, 2)));
    }

    fn tmpfile(bytes: &[u8]) -> std::path::PathBuf {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("rc_edhex_{}_{nanos}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn the_jwt_under_the_cursor_decodes_into_a_dialog() {
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let mut e = EditorState::new(
            "req.http".into(),
            VfsPath::local_cwd(),
            &format!("GET /\nAuthorization: Bearer {token}\n"),
        );
        e.cursor = e.buf.line_to_char(1) + 30;
        match e.run_menu_action(EditorAction::DecodeJwt) {
            EditorSignal::ShowJwt(text) => assert!(text.contains("John Doe"), "{text}"),
            _ => panic!("no JWT decoded"),
        }
        e.cursor = 1;
        assert!(matches!(e.run_menu_action(EditorAction::DecodeJwt), EditorSignal::Stay));
        assert_eq!(e.status, "No JWT at the cursor");
    }

    #[test]
    fn hex_mode_edits_file_in_place() {
        let p = tmpfile(b"hello");
        let mut e = EditorState::new("h".into(), VfsPath::local(&p), "hello");
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::CONTROL)); // enter hex
        assert!(e.is_hex());
        // Overwrite first byte 'h' (0x68) with 'H' (0x48).
        e.handle_key(key(KeyCode::Char('4')));
        e.handle_key(key(KeyCode::Char('8')));
        assert!(e.dirty, "byte edit marks dirty");
        assert_eq!(std::fs::read(&p).unwrap(), b"hello", "not written until save");

        e.flush_hex().unwrap(); // the app's save path calls this
        assert!(!e.dirty);
        assert_eq!(std::fs::read(&p).unwrap(), b"Hello", "in-place byte write");

        // Toggle back to text reflects the saved change.
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::CONTROL));
        assert!(!e.is_hex());
        assert_eq!(e.contents(), "Hello");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn hex_search_and_replace() {
        let p = tmpfile(b"hello hello hello");
        let mut e = EditorState::new("h".into(), VfsPath::local(&p), "x");
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::CONTROL));
        // ASCII search moves the cursor to the next match.
        e.apply_hex_search_replace(false, "hello", "", false, false);
        // Cursor was at 0; next match starts at offset 6.
        // (find searches from cursor+1)
        // Replace-all (equal length) overwrites every occurrence.
        e.apply_hex_search_replace(true, "hello", "HELLO", false, false);
        e.flush_hex().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"HELLO HELLO HELLO");

        // Hex-byte search input ("68 65" = "he") parses and finds.
        e.apply_hex_search_replace(false, "48 45 4C 4C 4F", "", true, false);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn hex_toggle_blocked_with_unsaved_text() {
        let p = tmpfile(b"abc");
        let mut e = EditorState::new("h".into(), VfsPath::local(&p), "abc");
        e.handle_key(key(KeyCode::Char('x'))); // dirty text
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::CONTROL));
        assert!(!e.is_hex(), "can't enter hex with unsaved text edits");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn hex_color_tints_the_hash_in_editor() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let p = tmpfile(b"");
        let mut e = EditorState::new("c.css".into(), VfsPath::local(&p), "a: #00ff80;");
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(40, 8)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), &mut e, &theme)).unwrap();
        let b = t.backend().buffer();
        let hash = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .find(|&(x, y)| b[(x, y)].symbol() == "#")
            .expect("'#' rendered");
        assert_eq!(
            b[hash].fg,
            ratatui::style::Color::Rgb(0x00, 0xff, 0x80),
            "hash tinted with its color"
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn hex_view_renders_offset_and_ascii() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let p = tmpfile(b"hello world example bytes 0123456789ABCDEF");
        let mut e = EditorState::new("h".into(), VfsPath::local(&p), "x");
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::CONTROL));
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(90, 12)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), &mut e, &theme)).unwrap();
        let b = t.backend().buffer();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
        }
        assert!(s.contains("00000000"), "offset column");
        assert!(s.contains("HEX"), "hex status indicator");
        assert!(s.contains("hello world"), "ascii pane shows content");
        // The F-key bar (not a mode banner) is shown, with supported functions.
        assert!(s.contains("Save") && s.contains("PullDn"), "F-key bar in hex mode");
        assert!(!s.contains("Hex mode"), "no persistent mode banner");
        std::fs::remove_file(&p).ok();
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE }
    }

    /// Place the text body at (0,1) and the F-key bar at row 7, as the renderer
    /// would, so mouse hit-testing has geometry to work with.
    fn with_layout(e: &mut EditorState) {
        e.text_area = Rect::new(0, 1, 20, 5);
        e.footer_area = Rect::new(0, 7, 20, 1);
        e.view_rows = 5;
        e.view_cols = 20;
    }

    #[test]
    fn click_moves_cursor() {
        let mut e = ed("abcdef\nghijkl\nmnopqr");
        with_layout(&mut e);
        // Row 1 is the first text line; column 3 → char index 3 on line 0.
        e.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 3, 1));
        assert_eq!(e.cur_line(), 0);
        assert_eq!(e.cur_col(), 3);
        // Second text line, column 2.
        e.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 2));
        assert_eq!(e.cur_line(), 1);
        assert_eq!(e.cur_col(), 2);
    }

    #[test]
    fn drag_marks_a_block_but_a_click_does_not() {
        let mut e = ed("abcdef\nghijkl");
        with_layout(&mut e);
        // Press at col 0, drag to col 3, release → block [0,3) like F3.
        e.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 1));
        assert_eq!(e.block_range(), None, "a bare press starts no selection");
        e.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 3, 1));
        assert_eq!(e.block_range(), Some((0, 3)), "dragging extends a live block");
        e.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 3, 1));
        assert_eq!(e.block_range(), Some((0, 3)), "release finalizes the block");
        e.handle_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)); // Ctrl-C → clipboard
        assert_eq!(e.clipboard, "abc");

        // A plain click (down then up, no drag) leaves no selection and the
        // arrow keys do not extend one.
        e.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 1));
        e.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 1, 1));
        e.handle_key(key(KeyCode::Right));
        assert_eq!(e.block_range(), None, "a click leaves no anchor to extend");
    }

    #[test]
    fn wheel_scrolls_the_cursor() {
        let mut e = ed("l0\nl1\nl2\nl3\nl4\nl5");
        with_layout(&mut e);
        assert_eq!(e.cur_line(), 0);
        e.handle_mouse(mouse(MouseEventKind::ScrollDown, 1, 3));
        assert_eq!(e.cur_line(), 3, "wheel down advances three lines");
        e.handle_mouse(mouse(MouseEventKind::ScrollUp, 1, 3));
        assert_eq!(e.cur_line(), 0, "wheel up rewinds three lines");
    }

    #[test]
    fn fkey_bar_click_acts_as_that_key() {
        let mut e = ed("abcdef");
        with_layout(&mut e);
        // Footer width 20, 10 labels → 2 cells each; F3 ("Mark") spans cols 4-5.
        e.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 7));
        assert!(e.anchor.is_some(), "clicking F3 starts a mark");
        // F10 ("Quit") spans cols 18-19; with no unsaved changes it closes.
        let sig = e.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 18, 7));
        assert!(matches!(sig, EditorSignal::Close), "clicking F10 quits");
    }

    #[test]
    fn typing_and_undo() {
        let mut e = ed("");
        for c in "hi".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(e.contents(), "hi");
        assert!(e.dirty);

        e.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(e.contents(), "h");
        e.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(e.contents(), "hi");
    }

    #[test]
    fn mark_and_delete_block() {
        let mut e = ed("abcdef");
        e.handle_key(key(KeyCode::F(3))); // start mark at 0
        for _ in 0..3 {
            e.handle_key(key(KeyCode::Right)); // cursor -> 3
        }
        e.handle_key(key(KeyCode::F(8))); // delete block [0,3)
        assert_eq!(e.contents(), "def");
    }

    #[test]
    fn clipboard_copy_and_paste() {
        let mut e = ed("abc");
        e.handle_key(key(KeyCode::F(3)));
        for _ in 0..3 {
            e.handle_key(key(KeyCode::Right));
        }
        e.handle_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)); // Ctrl-C copies "abc"
        // cursor at end; Ctrl-V paste duplicates.
        e.handle_key(key_mod(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(e.contents(), "abcabc");
    }

    #[test]
    fn clipboard_keys_also_offer_the_block_to_the_system_clipboard() {
        // Ctrl-C records the block for the app loop to push out over OSC 52,
        // and taking it clears it so one copy is not written twice.
        let mut e = ed("abc");
        e.handle_key(key(KeyCode::F(3)));
        for _ in 0..3 {
            e.handle_key(key(KeyCode::Right));
        }
        e.handle_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(e.take_pending_clip().as_deref(), Some("abc"));
        assert_eq!(e.take_pending_clip(), None, "draining it leaves nothing behind");

        // Ctrl-X cuts, and offers the same text (it routes through copy).
        let mut e = ed("abc");
        e.handle_key(key(KeyCode::F(3)));
        for _ in 0..3 {
            e.handle_key(key(KeyCode::Right));
        }
        e.handle_key(key_mod(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(e.take_pending_clip().as_deref(), Some("abc"));
        assert_eq!(e.contents(), "", "and the block is gone from the buffer");

        // A block copy that never reaches the clipboard must not leak out either.
        let mut e = ed("abcdef");
        e.handle_key(key(KeyCode::F(3)));
        for _ in 0..2 {
            e.handle_key(key(KeyCode::Right));
        }
        e.handle_key(key(KeyCode::F(3)));
        e.handle_key(key(KeyCode::End));
        e.handle_key(key(KeyCode::F(5)));
        assert_eq!(e.take_pending_clip(), None, "F5 never touches the system clipboard");
    }

    #[test]
    fn f5_copies_block_to_cursor() {
        // Mark "ab" (F3 … move … F3 to finalize), move the cursor to the end,
        // F5 inserts a copy there; the original block stays marked.
        let mut e = ed("abcdef");
        e.handle_key(key(KeyCode::F(3)));
        for _ in 0..2 {
            e.handle_key(key(KeyCode::Right));
        }
        e.handle_key(key(KeyCode::F(3))); // finalize block [0,2)
        e.handle_key(key(KeyCode::End)); // cursor → 6, block persists
        e.handle_key(key(KeyCode::F(5)));
        assert_eq!(e.contents(), "abcdefab");
        assert_eq!(e.block_range(), Some((0, 2)), "original block stays marked");
        assert_eq!(e.clipboard, "", "F5 does not touch the clipboard");
    }

    #[test]
    fn literal_replace_all() {
        let mut e = ed("a b a b");
        e.apply_search_replace(true, "a", "X", false, false, false, false, false);
        assert_eq!(e.contents(), "X b X b");
    }

    #[test]
    fn regex_replace_with_groups() {
        let mut e = ed("name: bob");
        e.apply_search_replace(true, r"(\w+): (\w+)", "$2=$1", true, true, false, false, false);
        assert_eq!(e.contents(), "bob=name");
    }

    #[test]
    fn line_anchors_match_each_line_not_the_whole_buffer() {
        // `^` / `$` are per-line, which is the only reading that makes sense in an
        // editor — the buffer is searched as one string, so without multi-line
        // mode these would only ever match at the very start/end of the file.
        let mut e = ed("foo one
bar two
foo three");

        // "Find next" searches from just past the cursor (so it never re-finds the
        // match you are sitting on), so from the top `^foo` lands on the *third*
        // line — a match only a per-line anchor can produce. Anchored to the whole
        // buffer, `^foo` could only ever match at byte 0.
        e.apply_search_replace(false, "^foo", "", true, false, false, false, false);
        assert_eq!(e.cur_line(), 2, "the anchor matches a later line's start");
        // Searching on wraps back to the first line's anchor.
        e.apply_search_replace(false, "^foo", "", true, false, false, false, false);
        assert_eq!((e.cur_line(), e.cur_col()), (0, 0));

        // `$` likewise anchors to each line's end.
        let mut e = ed("aa x
bb
cc x");
        e.apply_search_replace(false, "x$", "", true, false, false, false, false);
        assert_eq!(e.cur_line(), 0);
        e.apply_search_replace(false, "x$", "", true, false, false, false, false);
        assert_eq!(e.cur_line(), 2);

        // Find all: every line starting with "foo", not just the file's first.
        let mut e = ed("foo one
bar two
foo three");
        e.apply_search_replace(false, "^foo", "", true, false, false, false, true);
        assert!(e.line_found(0) && e.line_found(2), "both lines start with foo");
        assert!(!e.line_found(1));

        // Replace all: anchored replacements apply per line.
        let mut e = ed("foo 1
xfoo 2
foo 3");
        e.apply_search_replace(true, "^foo", "BAR", true, false, false, false, false);
        assert_eq!(
            e.contents(),
            "BAR 1
xfoo 2
BAR 3",
            "only line-initial foo is replaced"
        );
    }

    #[test]
    fn multi_line_mode_leaves_literals_and_dot_alone() {
        // A literal search is escaped, so `^` is still just a caret.
        let mut e = ed("a
b^c
d");
        e.apply_search_replace(false, "^", "", false, false, false, false, false);
        assert_eq!((e.cur_line(), e.cur_col()), (1, 1), "the literal caret is found");

        // `.` still stops at a newline, so a pattern can't swallow whole lines.
        let mut e = ed("aa
bb");
        e.apply_search_replace(true, "a.*b", "X", true, false, false, false, false);
        assert_eq!(
            e.contents(),
            "aa
bb",
            "no match spans the newline"
        );

        // An anchored empty-ish pattern still replaces once per line, not once
        // for the file.
        let mut e = ed("p
q
r");
        e.apply_search_replace(true, "^", ">", true, false, false, false, false);
        assert_eq!(
            e.contents(),
            ">p
>q
>r"
        );
    }

    #[test]
    fn find_all_highlights_every_matching_line_and_persists() {
        let mut e = ed("alpha hit\nbeta\ngamma HIT\ndelta\nhit again");
        // Case-insensitive by default, so all three lines match.
        e.apply_search_replace(false, "hit", "", false, false, false, false, true);
        for line in [0, 2, 4] {
            assert!(e.line_found(line), "line {line} holds a match");
        }
        for line in [1, 3] {
            assert!(!e.line_found(line), "line {line} has no match");
        }
        assert!(e.status.contains('3'), "the count is reported: {}", e.status);

        // A plain "find next" afterwards leaves the highlight alone — the whole
        // point is to keep the hits visible while stepping through them.
        e.apply_search_replace(false, "beta", "", false, false, false, false, false);
        assert!(e.line_found(0) && e.line_found(2), "a later search keeps the highlight");

        // The next Find all replaces the set rather than adding to it.
        e.apply_search_replace(false, "beta", "", false, false, false, false, true);
        assert!(e.line_found(1), "the new term's line is highlighted");
        assert!(!e.line_found(0) && !e.line_found(2), "the previous highlight is gone");

        // A term that matches nothing clears the highlight and says so.
        e.apply_search_replace(false, "nothing here", "", false, false, false, false, true);
        assert!((0..5).all(|l| !e.line_found(l)), "no line stays highlighted");
        assert!(e.status.contains("Not found"), "status: {}", e.status);
    }

    #[test]
    fn find_all_honours_the_search_options() {
        let mut e = ed("Hit\nhit\nhitting");
        // Case-sensitive: only the exact-case line matches.
        e.apply_search_replace(false, "hit", "", false, true, false, false, true);
        assert!(!e.line_found(0) && e.line_found(1) && e.line_found(2));
        // Whole words: "hitting" no longer counts.
        e.apply_search_replace(false, "hit", "", false, true, true, false, true);
        assert!(e.line_found(1) && !e.line_found(2), "whole-words excludes 'hitting'");
        // Regex works too, with `^`/`$` anchoring per line.
        e.apply_search_replace(false, "^h.t$", "", true, true, false, false, true);
        assert!(e.line_found(1), "the regex matches the whole line 'hit'");
        assert!(!e.line_found(0), "and stays case-sensitive");
        assert!(!e.line_found(2), "'hitting' is longer than the anchored pattern");
    }

    #[test]
    fn case_insensitive_search_moves_cursor() {
        let mut e = ed("one TWO three");
        e.apply_search_replace(false, "two", "", false, false, false, false, false);
        // Cursor should land on "TWO" (char index 4).
        assert_eq!(e.cur_line(), 0);
        assert_eq!(e.cur_col(), 4);
    }

    #[test]
    fn vertical_movement_keeps_goal_column() {
        let mut e = ed("longline\nx\nshort");
        // Move to col 5 on line 0.
        for _ in 0..5 {
            e.handle_key(key(KeyCode::Right));
        }
        e.handle_key(key(KeyCode::Down)); // line 1 "x" -> clamps to col 1
        assert_eq!(e.cur_line(), 1);
        e.handle_key(key(KeyCode::Down)); // line 2 "short" -> goal col 5 restored
        assert_eq!(e.cur_line(), 2);
        assert_eq!(e.cur_col(), 5);
    }

    #[test]
    fn save_as_keys_emit_signal() {
        let mut e = ed("hi");
        assert!(matches!(
            e.handle_key(key_mod(KeyCode::F(2), KeyModifiers::SHIFT)),
            EditorSignal::SaveAs
        ));
        assert!(matches!(
            e.handle_key(key_mod(KeyCode::F(2), KeyModifiers::CONTROL)),
            EditorSignal::SaveAs
        ));
        assert!(matches!(
            e.handle_key(key(KeyCode::F(2))),
            EditorSignal::Save { close_after: false }
        ));
    }

    #[test]
    fn f1_opens_help_and_next_key_dismisses_it() {
        let mut e = ed("hi");
        assert!(!e.help_open());
        e.handle_key(key(KeyCode::F(1)));
        assert!(e.help_open(), "F1 opens the shortcut help");
        // The dismiss key is swallowed (not inserted into the buffer).
        e.handle_key(key(KeyCode::Char('x')));
        assert!(!e.help_open(), "any key closes the help overlay");
        assert_eq!(e.contents(), "hi", "the dismiss key is consumed, not typed");
    }

    #[test]
    fn word_wrap_toggles_with_shift_f9() {
        let mut e = ed("hello");
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::SHIFT));
        assert!(e.wrap, "Shift-F9 turns word wrap on");
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::SHIFT));
        assert!(!e.wrap, "and off again");
        // Plain F9 now opens the menu instead of toggling anything.
        e.handle_key(key(KeyCode::F(9)));
        assert!(e.menu_open());
        assert!(!e.wrap);
    }

    #[test]
    fn wrap_breaks_a_long_line_at_spaces() {
        let mut e = ed("aaaaaaaaaa bbbbbbbbbb cccccccccc"); // 32 chars
        e.wrap = true;
        e.view_cols = 12; // segment width 11
        let breaks = e.line_breaks(0);
        // Three visual rows, breaking just after each space.
        assert_eq!(breaks, vec![0, 11, 22], "wrapped at the spaces: {breaks:?}");
        // A line that fits is a single visual row.
        let mut s = ed("short");
        s.wrap = true;
        s.view_cols = 12;
        assert_eq!(s.line_breaks(0), vec![0]);
    }

    #[test]
    fn wrap_arrows_move_by_visual_row() {
        let mut e = ed("aaaaaaaaaa bbbbbbbbbb cccccccccc");
        e.wrap = true;
        e.view_cols = 12;
        e.cursor = 0; // line 0, sub-row 0, column 0
        e.handle_key(key(KeyCode::Down));
        assert_eq!(e.cursor, 11, "Down steps to the next visual row (offset 11)");
        e.handle_key(key(KeyCode::Down));
        assert_eq!(e.cursor, 22, "Down again → third visual row (offset 22)");
        e.handle_key(key(KeyCode::Up));
        assert_eq!(e.cursor, 11, "Up returns one visual row");
    }

    #[test]
    fn wrap_renders_continuation_marker() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut e = ed("aaaaaaaaaa bbbbbbbbbb cccccccccc");
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::SHIFT)); // wrap on
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(14, 8)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), &mut e, &theme)).unwrap();
        let b = t.backend().buffer();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
        }
        assert!(s.contains('>'), "continued wrapped rows show a `>` marker");
    }

    #[test]
    fn fbar_labels_switch_while_modifier_held() {
        let mut e = ed("hi");
        use ratatui::crossterm::event::ModifierKeyCode;
        let press = |code| KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press);
        let release =
            |code| KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Release);

        let plain = e.footer_labels();
        assert_eq!(plain[1], "Save");
        assert_eq!(plain[8], "PullDn");
        // Pressing (holding) Ctrl flips F2/F9 to the alternates Ctrl reaches.
        e.note_key(press(KeyCode::Modifier(ModifierKeyCode::LeftControl)));
        let held = e.footer_labels();
        assert_eq!(held[1], "Save as");
        assert_eq!(held[8], "Hex");
        // Releasing it restores the defaults (a modifier-key release event still
        // reports the modifier as set, so this must come from the release kind).
        e.note_key(release(KeyCode::Modifier(ModifierKeyCode::LeftControl)));
        assert_eq!(e.footer_labels()[1], "Save");
        assert_eq!(e.footer_labels()[8], "PullDn");
        // Shift reaches a different set: Save as, Insert file, Search again, Wrap.
        e.note_key(press(KeyCode::Modifier(ModifierKeyCode::RightShift)));
        let held = e.footer_labels();
        assert_eq!(held[1], "Save as");
        assert_eq!(held[4], "InsFil");
        assert_eq!(held[6], "Again");
        assert_eq!(held[8], "Wrap");
        e.note_key(release(KeyCode::Modifier(ModifierKeyCode::RightShift)));
        assert_eq!(e.footer_labels()[1], "Save");
    }
    // -- F9 menu and the actions it drives ---------------------------------

    /// Run a menu action directly, as choosing it from the F9 menu would.
    fn act(e: &mut EditorState, a: menu::EditorAction) -> EditorSignal {
        e.run_menu_action(a)
    }

    #[test]
    fn f9_opens_the_menu_and_esc_closes_it() {
        let mut e = ed("hello");
        e.handle_key(key(KeyCode::F(9)));
        assert!(e.menu_open(), "F9 opens the pulldown");
        // Keys go to the menu, not the buffer, while it is open.
        e.handle_key(key(KeyCode::Down));
        assert_eq!(e.contents(), "hello", "typing into the menu never edits the file");
        e.handle_key(key(KeyCode::Esc));
        assert!(!e.menu_open(), "Esc closes it");
        // ...and Esc did not reach the editor as a quit request.
        assert!(matches!(e.handle_key(key(KeyCode::Char('!'))), EditorSignal::Stay));
        assert_eq!(e.contents(), "!hello");
    }

    #[test]
    fn menu_file_items_leave_through_signals() {
        let mut e = ed("x");
        assert!(matches!(
            act(&mut e, menu::EditorAction::OpenFile),
            EditorSignal::Browse(BrowseKind::Open)
        ));
        assert!(matches!(act(&mut e, menu::EditorAction::NewFile), EditorSignal::NewFile));
        assert!(matches!(act(&mut e, menu::EditorAction::About), EditorSignal::About));
        // Quit on a clean buffer closes; on a modified one it asks first.
        assert!(matches!(act(&mut e, menu::EditorAction::Quit), EditorSignal::Close));
        e.handle_key(key(KeyCode::Char('y')));
        assert!(matches!(act(&mut e, menu::EditorAction::Quit), EditorSignal::ConfirmQuit));
    }

    #[test]
    fn mark_all_and_cut_to_clipboard() {
        let mut e = ed("one\ntwo");
        act(&mut e, menu::EditorAction::MarkAll);
        assert_eq!(e.block, Some((0, 7)));
        act(&mut e, menu::EditorAction::ClipCut);
        assert_eq!(e.contents(), "", "cut removes the block");
        e.paste();
        assert_eq!(e.contents(), "one\ntwo", "and the clipboard still holds it");
        // Unmark drops the mark without touching the text.
        act(&mut e, menu::EditorAction::MarkAll);
        act(&mut e, menu::EditorAction::Unmark);
        assert!(e.block_range().is_none());
        assert_eq!(e.contents(), "one\ntwo");
    }

    #[test]
    fn insert_toggles_overwrite_typing() {
        let mut e = ed("abcd");
        e.handle_key(key(KeyCode::Insert));
        assert!(e.overwrite());
        e.handle_key(key(KeyCode::Char('X')));
        assert_eq!(e.contents(), "Xbcd", "overwrite replaces the character under the cursor");
        // At the end of a line there is nothing to replace, so it inserts.
        e.cursor = 4;
        e.handle_key(key(KeyCode::Char('!')));
        assert_eq!(e.contents(), "Xbcd!");
        e.handle_key(key(KeyCode::Insert));
        assert!(!e.overwrite());
        e.cursor = 0;
        e.handle_key(key(KeyCode::Char('Z')));
        assert_eq!(e.contents(), "ZXbcd!");
    }

    // -- Typing options ----------------------------------------------------

    /// An editor with `opts` applied (no syntax highlighting, so tests stay fast).
    fn ed_opts(text: &str, opts: EditorOptions) -> EditorState {
        let mut e = ed(text);
        e.set_options(EditorOptions { syntax_highlighting: false, ..opts }, false);
        e
    }

    #[test]
    fn return_autoindents_only_up_to_the_cursor() {
        let mut e = ed_opts("    body", EditorOptions::default());
        e.cursor = 8; // end of the line
        e.handle_key(key(KeyCode::Enter));
        assert_eq!(e.contents(), "    body\n    ", "the line's indent is carried over");

        // Turned off, Enter is a bare newline.
        let mut e = ed_opts(
            "    body",
            EditorOptions { return_does_autoindent: false, ..EditorOptions::default() },
        );
        e.cursor = 8;
        e.handle_key(key(KeyCode::Enter));
        assert_eq!(e.contents(), "    body\n");

        // Pressing Enter inside the indentation copies only what precedes it.
        let mut e = ed_opts("    body", EditorOptions::default());
        e.cursor = 2;
        e.handle_key(key(KeyCode::Enter));
        assert_eq!(e.contents(), "  \n    body");
    }

    #[test]
    fn tab_advances_to_the_next_tab_stop() {
        let mut e = ed_opts("", EditorOptions { tab_spacing: 4, ..EditorOptions::default() });
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(e.contents(), "    ");
        e.insert_text("ab");
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(e.contents(), "    ab  ", "column 6 → two spaces reach column 8");

        // With "fill tabs with spaces" off it types a real tab.
        let mut e =
            ed_opts("", EditorOptions { fill_tabs_with_spaces: false, ..EditorOptions::default() });
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(e.contents(), "\t");
    }

    #[test]
    fn backspace_through_tabs_removes_a_whole_indent_step() {
        let opts = EditorOptions {
            backspace_through_tabs: true,
            tab_spacing: 4,
            ..EditorOptions::default()
        };
        let mut e = ed_opts("        code", opts.clone());
        e.cursor = 8;
        e.handle_key(key(KeyCode::Backspace));
        assert_eq!(e.contents(), "    code", "one press eats a whole tab stop");
        // Past the indentation it is an ordinary backspace again.
        e.cursor = 8;
        e.handle_key(key(KeyCode::Backspace));
        assert_eq!(e.contents(), "    cod");

        // Off (the default), backspace always removes one character.
        let mut e = ed_opts("        code", EditorOptions::default());
        e.cursor = 8;
        e.handle_key(key(KeyCode::Backspace));
        assert_eq!(e.contents(), "       code");
    }

    #[test]
    fn typewriter_wrap_breaks_the_line_for_real() {
        let opts = EditorOptions {
            wrap_mode: WrapMode::Typewriter,
            word_wrap_line_length: 10,
            ..EditorOptions::default()
        };
        let mut e = ed_opts("", opts);
        for c in "aaa bbb ccc ddd".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        assert!(e.contents().contains('\n'), "a newline was written into the buffer");
        for line in e.contents().lines() {
            assert!(line.chars().count() <= 11, "line {line:?} stays near the wrap column");
        }
    }

    #[test]
    fn group_undo_takes_back_a_whole_run_of_typing() {
        let mut e = ed_opts("", EditorOptions { group_undo: true, ..EditorOptions::default() });
        for c in "hello".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key_mod(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(e.contents(), "", "one undo takes back the whole run");

        // Off (the default) each character undoes on its own.
        let mut e = ed_opts("", EditorOptions::default());
        for c in "hello".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key_mod(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(e.contents(), "hell");
    }

    #[test]
    fn persistent_selection_can_be_turned_off() {
        // On (the default): a plain move keeps the block marked.
        let mut e = ed_opts("hello", EditorOptions::default());
        e.mark_all();
        e.handle_key(key(KeyCode::Right));
        assert!(e.block_range().is_some());

        let mut e = ed_opts(
            "hello",
            EditorOptions { persistent_selection: false, ..EditorOptions::default() },
        );
        e.mark_all();
        e.handle_key(key(KeyCode::Right));
        assert!(e.block_range().is_none(), "the mark is dropped by a plain move");
    }

    #[test]
    fn cursor_after_inserted_block_can_be_turned_off() {
        let mut e = ed_opts("ab", EditorOptions::default());
        e.clipboard = "XY".to_string();
        e.cursor = 1;
        e.paste();
        assert_eq!((e.contents().as_str(), e.cursor), ("aXYb", 3));

        let mut e = ed_opts(
            "ab",
            EditorOptions { cursor_after_inserted_block: false, ..EditorOptions::default() },
        );
        e.clipboard = "XY".to_string();
        e.cursor = 1;
        e.paste();
        assert_eq!((e.contents().as_str(), e.cursor), ("aXYb", 1));
    }

    // -- Bookmarks, brackets, goto -----------------------------------------

    #[test]
    fn bookmarks_toggle_jump_and_flush() {
        let mut e = ed("l0\nl1\nl2\nl3\nl4");
        e.goto_line(1);
        e.bookmark_toggle();
        e.goto_line(3);
        e.bookmark_toggle();
        assert!(e.line_bookmarked(1) && e.line_bookmarked(3));

        // Next from line 3 wraps around to line 1; previous steps back.
        e.bookmark_jump(true);
        assert_eq!(e.cursor_line_col().0, 1);
        e.bookmark_jump(true);
        assert_eq!(e.cursor_line_col().0, 3);
        e.bookmark_jump(false);
        assert_eq!(e.cursor_line_col().0, 1);

        // Toggling the same line again clears it; flush clears the rest.
        e.bookmark_toggle();
        assert!(!e.line_bookmarked(1));
        e.bookmark_flush();
        assert!(!e.line_bookmarked(3));
        e.bookmark_jump(true);
        assert_eq!(e.status, "No bookmarks");
    }

    #[test]
    fn matching_bracket_jumps_both_ways() {
        let mut e = ed("fn f(a, (b), c) {}");
        e.cursor = 4; // the opening '('
        e.goto_matching_bracket();
        assert_eq!(e.cursor, 14, "skips the nested pair and lands on the closing ')'");
        e.goto_matching_bracket();
        assert_eq!(e.cursor, 4, "and back again");

        // Just past a closing bracket counts as being on it.
        e.cursor = 11; // ')' of "(b)"
        e.goto_matching_bracket();
        assert_eq!(e.cursor, 8);

        e.cursor = 1; // 'n' — no bracket here
        e.goto_matching_bracket();
        assert_eq!(e.cursor, 1);
        assert_eq!(e.status, "No bracket at the cursor");
    }

    #[test]
    fn goto_line_clamps_and_centers() {
        let mut e = ed("a\nb\nc");
        e.goto_line(1);
        assert_eq!(e.cursor_line_col(), (1, 0));
        e.goto_line(999);
        assert_eq!(e.cursor_line_col(), (2, 0), "past the end clamps to the last line");
    }

    // -- Format menu -------------------------------------------------------

    #[test]
    fn format_paragraph_rewraps_only_its_own_paragraph() {
        let opts = EditorOptions { word_wrap_line_length: 20, ..EditorOptions::default() };
        let mut e = ed_opts("  one two three four five six seven\n\nuntouched line", opts);
        e.cursor = 3;
        e.format_paragraph();
        let text = e.contents();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() > 3, "the paragraph was split over several lines");
        for l in lines.iter().take_while(|l| !l.is_empty()) {
            assert!(l.chars().count() <= 20, "{l:?} fits the wrap column");
            assert!(l.starts_with("  "), "{l:?} keeps the paragraph's indent");
        }
        assert_eq!(*lines.last().unwrap(), "untouched line", "the next paragraph is left alone");
        // One undo puts the whole reflow back.
        e.undo();
        assert_eq!(e.contents(), "  one two three four five six seven\n\nuntouched line");
    }

    #[test]
    fn sort_block_sorts_the_marked_lines() {
        let mut e = ed("c\na\nb\nzzz");
        e.block = Some((0, 5)); // "c\na\nb"
        e.sort_block(false, false, false);
        assert_eq!(e.contents(), "a\nb\nc\nzzz", "only the marked lines move");

        // Reverse, case-insensitive and unique all apply; no block = whole buffer.
        let mut e = ed("b\nA\na\nB");
        e.sort_block(true, true, true);
        assert_eq!(e.contents(), "b\nA");
    }

    #[test]
    fn insert_date_time_writes_a_timestamp() {
        let mut e = ed("");
        e.insert_date_time();
        let text = e.contents();
        assert_eq!(text.len(), 19, "YYYY-MM-DD HH:MM:SS");
        assert!(
            text.chars().enumerate().all(|(i, c)| match i {
                4 | 7 => c == '-',
                10 => c == ' ',
                13 | 16 => c == ':',
                _ => c.is_ascii_digit(),
            }),
            "unexpected timestamp shape: {text}"
        );
    }

    // -- File-menu helpers the app calls back into -------------------------

    #[test]
    fn block_or_all_and_insert_at_cursor() {
        let mut e = ed("hello world");
        assert_eq!(e.block_or_all(), "hello world", "no block ⇒ the whole buffer");
        e.block = Some((0, 5));
        assert_eq!(e.block_or_all(), "hello");

        e.cursor = 5;
        e.insert_at_cursor(" there");
        assert_eq!(e.contents(), "hello there world");
    }

    #[test]
    fn load_text_replaces_the_buffer_and_its_history() {
        let mut e = ed("old");
        e.handle_key(key(KeyCode::Char('x')));
        e.bookmark_toggle();
        assert!(e.dirty);
        e.load_text("new.txt".into(), VfsPath::local("/tmp/new.txt"), "fresh");
        assert_eq!(e.contents(), "fresh");
        assert_eq!(e.name, "new.txt");
        assert!(!e.dirty, "a just-loaded file is unmodified");
        assert_eq!(e.cursor, 0);
        assert!(!e.line_bookmarked(0), "bookmarks belong to the file that had them");
        assert!(e.buf.undo().is_none(), "the previous file's undo history is gone");
    }

    #[test]
    fn hex_mode_menu_greys_out_the_text_actions() {
        let p = tmpfile(b"abc");
        let mut e = EditorState::new("h".into(), VfsPath::local(&p), "abc");
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::CONTROL));
        assert!(e.is_hex());
        // F9 still opens the menu in hex mode — built for hex, so the text-only
        // entries are greyed out (see `editor::menu`'s own tests).
        e.handle_key(key(KeyCode::F(9)));
        assert!(e.menu_open());
        std::fs::remove_file(&p).ok();
    }

    /// A fresh buffer carries the working directory as its path, so Ctrl-F9 used
    /// to hand a *directory* to the hex editor: on Unix that opens read-only and
    /// reports the directory's size while reading back nothing, and the renderer
    /// then indexed past the end of its (empty) window and panicked.
    #[test]
    fn hex_mode_is_refused_on_a_fresh_unnamed_buffer() {
        let mut e = EditorState::new_unnamed();
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::CONTROL));
        assert!(!e.is_hex(), "an unnamed buffer has no file to hex-edit");
        assert!(e.status.contains("hex mode"), "the refusal is explained: {}", e.status);
    }

    /// A zero-byte file is a valid hex-mode target: no bytes to show, no panic.
    #[test]
    fn hex_mode_renders_an_empty_file() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let p = std::env::temp_dir().join(format!("rc_hex_empty_{}", std::process::id()));
        std::fs::write(&p, b"").unwrap();
        let mut e = EditorState::new("empty".into(), VfsPath::local(&p), "");
        e.handle_key(key_mod(KeyCode::F(9), KeyModifiers::CONTROL));
        assert!(e.is_hex(), "an existing empty file enters hex mode: {}", e.status);
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), &mut e, &theme)).unwrap();
        std::fs::remove_file(&p).ok();
    }
}
