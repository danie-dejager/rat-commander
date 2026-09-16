//! The data inspector in the hex editor (F8): a panel beside (or below) the
//! bytes reading the bytes at the cursor as each common type — integers,
//! floats, LEB128, a UTF-8 or UTF-16 character, dates, a GUID — in the byte
//! order `b` picks. It follows the cursor, and a value typed into a row goes
//! into the hex editor's unsaved edits.
//!
//! The panel shares the side of the bytes with the template panel, and Tab
//! steps the keys round all of them: hex column, ASCII column, inspector,
//! template tree.

use super::{EditorSignal, EditorState};
use crate::bt::inspect::{self, Field, MAX_WIDTH};
use crate::ui::theme::Theme;
use crate::util::text::{ellipsize, pad_right};
use ratatui::Frame;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Width the panels need to sit beside the bytes rather than below them.
const SIDE_MIN: u16 = 44;
/// Rows the inspector wants: its title and one per type.
const ROWS: u16 = Field::ALL.len() as u16 + 1;
/// Width of the type-name column.
const LABEL_W: usize = 10;
/// Rows the template tree keeps below the inspector when both are beside the
/// bytes and they can't both have what they want.
const TREE_KEEPS: u16 = 12;

#[derive(Debug, Default)]
pub struct InspectorState {
    pub(super) shown: bool,
    pub(super) focus: bool,
    pub(super) big_endian: bool,
    /// The selected row, an index into [`Field::ALL`].
    sel: usize,
    scroll: usize,
    /// The value being typed, and the caret in it.
    edit: Option<(String, usize)>,
    /// The panel's rect and its rows', recorded by the renderer for the mouse.
    pub(super) area: Rect,
    list_area: Rect,
}

/// Where the hex editor's body goes: the bytes, and the panels beside or
/// below them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexLayout {
    pub hex: Rect,
    pub inspector: Option<Rect>,
    pub template: Option<Rect>,
}

/// Split the hex editor's body between the bytes and the panels showing:
/// side by side when there's room, stacked when there's height (the inspector
/// above the template tree when both show), else only the panel with the
/// focus, in the bytes' place.
pub(super) fn hex_layout(
    body: Rect,
    hex_w: u16,
    inspector: bool,
    template: bool,
    inspector_focus: bool,
    tree_focus: bool,
) -> HexLayout {
    let bytes_only = HexLayout { hex: body, inspector: None, template: None };
    if !(inspector || template) || body.width == 0 || body.height == 0 {
        return bytes_only;
    }
    let share = |r: Rect| -> (Option<Rect>, Option<Rect>) {
        if !template {
            return (Some(r), None);
        }
        if !inspector {
            return (None, Some(r));
        }
        // All its rows if the tree keeps a dozen, else at least half.
        let top = ROWS.min((r.height / 2).max(r.height.saturating_sub(TREE_KEEPS))).max(1);
        let rest = (r.height > top).then_some(Rect { y: r.y + top, height: r.height - top, ..r });
        (Some(Rect { height: top, ..r }), rest)
    };
    if body.width >= hex_w + SIDE_MIN {
        let (i, t) = share(Rect { x: body.x + hex_w, width: body.width - hex_w, ..body });
        return HexLayout { hex: Rect { width: hex_w, ..body }, inspector: i, template: t };
    }
    if body.height >= 11 {
        // The inspector alone needs no more than its rows; the tree takes
        // what the bytes leave.
        let mut hex_h = (body.height * 45 / 100).max(4);
        if !template {
            hex_h = hex_h.max(body.height.saturating_sub(ROWS));
        }
        let (i, t) = share(Rect { y: body.y + hex_h, height: body.height - hex_h, ..body });
        return HexLayout { hex: Rect { height: hex_h, ..body }, inspector: i, template: t };
    }
    let hidden = Rect { height: 0, ..body };
    if inspector && inspector_focus {
        return HexLayout { hex: hidden, inspector: Some(body), template: None };
    }
    if template && tree_focus {
        return HexLayout { hex: hidden, inspector: None, template: Some(body) };
    }
    bytes_only
}

/// Where the keys go in hex mode, in the order Tab steps through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HexFocus {
    Hex,
    Ascii,
    Inspector,
    Tree,
}

const FOCUS_ORDER: [HexFocus; 4] =
    [HexFocus::Hex, HexFocus::Ascii, HexFocus::Inspector, HexFocus::Tree];

impl EditorState {
    /// Whether keys go to the inspector.
    pub fn inspector_focus(&self) -> bool {
        self.hex.is_some() && self.insp.shown && self.insp.focus
    }

    fn hex_focus(&self) -> HexFocus {
        if self.inspector_focus() {
            HexFocus::Inspector
        } else if self.template_focus() {
            HexFocus::Tree
        } else if self.hex.as_ref().is_some_and(|h| h.ascii_pane) {
            HexFocus::Ascii
        } else {
            HexFocus::Hex
        }
    }

    /// Tab (or Shift-Tab, `back`) in hex mode: on to the next of the hex
    /// column, the ASCII column, the inspector and the template tree that is
    /// showing. Into the tree the cursor's variable is selected, as F6 does;
    /// out of it the byte cursor goes to the variable selected there.
    pub(super) fn step_hex_focus(&mut self, back: bool) {
        let from = self.hex_focus();
        match from {
            HexFocus::Tree => self.leave_template_tree(),
            HexFocus::Inspector => {
                self.insp.focus = false;
                self.insp.edit = None;
            }
            HexFocus::Hex | HexFocus::Ascii => {}
        }
        let mut i = FOCUS_ORDER.iter().position(|&f| f == from).unwrap_or(0);
        for _ in 0..FOCUS_ORDER.len() {
            i = if back { i + FOCUS_ORDER.len() - 1 } else { i + 1 };
            i %= FOCUS_ORDER.len();
            match FOCUS_ORDER[i] {
                to @ (HexFocus::Hex | HexFocus::Ascii) => {
                    if let Some(h) = self.hex.as_mut() {
                        h.set_pane(to == HexFocus::Ascii);
                    }
                    return;
                }
                HexFocus::Inspector if self.insp.shown => {
                    self.insp.focus = true;
                    return;
                }
                // Until a run has results there is nothing to go to.
                HexFocus::Tree if self.template_panel() => {
                    self.jump_to_template_variable();
                    if self.template_focus() {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// Show or hide the inspector (F8).
    pub(super) fn toggle_inspector(&mut self) {
        self.insp.shown = !self.insp.shown;
        if !self.insp.shown {
            self.insp.focus = false;
            self.insp.edit = None;
        }
        self.opts.hex_inspector = self.insp.shown;
    }

    /// The bytes of the row selected in the inspector, while it has the keys,
    /// for the hex view to mark.
    pub(super) fn inspector_range(&mut self) -> Option<(u64, u64)> {
        if !self.inspector_focus() {
            return None;
        }
        let big = self.insp.big_endian;
        let field = Field::ALL[self.insp.sel];
        let h = self.hex.as_mut()?;
        let bytes = h.window(h.cursor, MAX_WIDTH);
        inspect::decode(field, &bytes, big).map(|(_, n)| (h.cursor, n as u64))
    }

    /// Keys for the inspector: F8 and Tab wherever the keys are, the rest
    /// while it has them. `None` leaves the key to the template tree or the
    /// bytes (saving, searching, quitting).
    pub(super) fn inspector_key(&mut self, key: KeyEvent) -> Option<EditorSignal> {
        if self.insp.edit.is_some() {
            self.inspector_edit_key(key);
            return Some(EditorSignal::Stay);
        }
        if self.editing_template_value() {
            return None;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::F(8) => {
                self.toggle_inspector();
                return Some(EditorSignal::Stay);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.step_hex_focus(shift || key.code == KeyCode::BackTab);
                return Some(EditorSignal::Stay);
            }
            _ => {}
        }
        if !self.inspector_focus() {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let last = Field::ALL.len() - 1;
        let page = self.insp.list_area.height.saturating_sub(1).max(1) as usize;
        let sel = &mut self.insp.sel;
        match key.code {
            KeyCode::Up => *sel = sel.saturating_sub(1),
            KeyCode::Down => *sel = (*sel + 1).min(last),
            KeyCode::PageUp => *sel = sel.saturating_sub(page),
            KeyCode::PageDown => *sel = (*sel + page).min(last),
            KeyCode::Home => *sel = 0,
            KeyCode::End => *sel = last,
            // The byte cursor moves under the inspector: by a byte, or with
            // Ctrl by the width of the selected value.
            KeyCode::Left | KeyCode::Right => {
                let step =
                    if ctrl { self.inspector_range().map_or(1, |(_, n)| n.max(1)) } else { 1 };
                if let Some(h) = self.hex.as_mut() {
                    let step = step as i64;
                    h.move_by(if key.code == KeyCode::Left { -step } else { step });
                }
            }
            KeyCode::Enter => self.begin_inspector_edit(),
            KeyCode::Char('b' | 'B') if !ctrl && !alt => self.toggle_byte_order(),
            KeyCode::Esc => self.insp.focus = false,
            KeyCode::F(2)
            | KeyCode::F(3)
            | KeyCode::F(4)
            | KeyCode::F(5)
            | KeyCode::F(6)
            | KeyCode::F(7)
            | KeyCode::F(10) => return None,
            _ => {}
        }
        Some(EditorSignal::Stay)
    }

    fn toggle_byte_order(&mut self) {
        self.insp.big_endian = !self.insp.big_endian;
        self.opts.hex_inspector_big_endian = self.insp.big_endian;
    }

    fn begin_inspector_edit(&mut self) {
        let big = self.insp.big_endian;
        let field = Field::ALL[self.insp.sel];
        let Some(h) = self.hex.as_mut() else { return };
        if h.readonly {
            self.status = "read-only file".to_string();
            return;
        }
        let bytes = h.window(h.cursor, MAX_WIDTH);
        match inspect::decode(field, &bytes, big) {
            Some((text, _)) => {
                let caret = text.chars().count();
                self.insp.edit = Some((text, caret));
            }
            None => self.status = "Not enough bytes before the end of the file".to_string(),
        }
    }

    fn inspector_edit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.insp.edit = None,
            KeyCode::Enter => self.commit_inspector_edit(),
            _ => {
                if let Some((text, caret)) = self.insp.edit.as_mut() {
                    let _ = crate::ui::textedit::edit_key(text, caret, key);
                }
            }
        }
    }

    /// Write the value being typed over the bytes at the cursor, into the hex
    /// editor's unsaved edits; a value that can't be written keeps the field
    /// open and says why.
    fn commit_inspector_edit(&mut self) {
        let Some((text, caret)) = self.insp.edit.take() else { return };
        let big = self.insp.big_endian;
        let field = Field::ALL[self.insp.sel];
        let Some(h) = self.hex.as_mut() else { return };
        let bytes = h.window(h.cursor, MAX_WIDTH);
        match inspect::encode(field, &text, big, &bytes) {
            Ok(out) => {
                let at = h.cursor;
                if !h.set_bytes(at, &out) {
                    self.status = "read-only file".to_string();
                    return;
                }
                self.dirty = h.dirty;
            }
            Err(msg) => {
                self.status = msg;
                self.insp.edit = Some((text, caret));
            }
        }
    }

    /// Mouse events over the inspector. Returns whether the event was for it.
    pub(super) fn inspector_mouse(&mut self, ev: MouseEvent) -> bool {
        let a = self.insp.area;
        let inside = self.hex.is_some()
            && self.insp.shown
            && ev.column >= a.x
            && ev.column < a.x + a.width
            && ev.row >= a.y
            && ev.row < a.y + a.height;
        if !inside {
            if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.insp.focus = false;
                self.insp.edit = None;
            }
            return false;
        }
        let last = Field::ALL.len() - 1;
        match ev.kind {
            MouseEventKind::ScrollUp => self.insp.sel = self.insp.sel.saturating_sub(3),
            MouseEventKind::ScrollDown => self.insp.sel = (self.insp.sel + 3).min(last),
            MouseEventKind::Down(MouseButton::Left) => {
                let was_focused = self.insp.focus;
                self.insp.focus = true;
                let list = self.insp.list_area;
                if ev.row >= list.y && ev.row < list.y + list.height {
                    let i = (self.insp.scroll + (ev.row - list.y) as usize).min(last);
                    // A click on the selected row edits it.
                    if was_focused && i == self.insp.sel && self.insp.edit.is_none() {
                        self.begin_inspector_edit();
                    } else {
                        self.insp.edit = None;
                        self.insp.sel = i;
                    }
                } else if ev.row < list.y {
                    // The title row names the byte order: a click switches it.
                    self.insp.edit = None;
                    self.toggle_byte_order();
                }
            }
            _ => {}
        }
        true
    }
}

/// Draw the inspector. Returns the caret position while a value is being typed.
pub(super) fn render_panel(
    f: &mut Frame,
    area: Rect,
    ed: &mut EditorState,
    theme: &Theme,
) -> Option<Position> {
    let normal = Style::default().fg(theme.text_fg).bg(theme.panel_bg);
    let dim = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    let header =
        Style::default().fg(theme.header_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD);
    let side = area.x > ed.text_area.x;
    let focus = ed.inspector_focus();
    let big = ed.insp.big_endian;
    let tr = crate::l10n::tr;
    let title = format!(
        "{}  {}",
        tr("Inspector"),
        if big { tr("big-endian") } else { tr("little-endian") }
    );

    // A divider from the bytes: a column when beside them, a rule when below.
    let inner = if side {
        let rule: Vec<Line> =
            (0..area.height).map(|_| Line::from(Span::styled("│", dim))).collect();
        f.render_widget(Paragraph::new(rule), Rect { width: 1, ..area });
        Rect { x: area.x + 1, width: area.width.saturating_sub(1), ..area }
    } else {
        area
    };
    let w = inner.width as usize;
    if inner.height == 0 || w < 8 {
        return None;
    }
    f.render_widget(Paragraph::new("").style(normal), inner);
    let head = if side {
        Line::from(Span::styled(pad_right(&ellipsize(&title, w), w), header))
    } else {
        let label = format!(" {title} ");
        let text = format!("──{label}{}", "─".repeat(w.saturating_sub(label.chars().count() + 2)));
        Line::from(Span::styled(ellipsize(&text, w), dim))
    };
    f.render_widget(Paragraph::new(head), Rect { height: 1, ..inner });

    let list = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    ed.insp.list_area = list;
    let visible = list.height as usize;
    if visible == 0 {
        return None;
    }
    let st = &mut ed.insp;
    st.scroll = crate::util::scroll::scroll_to_visible(st.scroll, st.sel, visible);
    let (scroll, sel) = (st.scroll, st.sel);
    let edit = st.edit.clone();
    let h = ed.hex.as_mut()?;
    let bytes = h.window(h.cursor, MAX_WIDTH);

    let value_w = w.saturating_sub(LABEL_W);
    let selected = if focus { theme.cursor } else { theme.cursor_inactive };
    let mut caret = None;
    let mut lines = Vec::with_capacity(visible);
    for (k, &field) in Field::ALL.iter().enumerate().skip(scroll).take(visible) {
        let label = pad_right(field.label(), LABEL_W);
        let row_style = if k == sel { selected } else { normal };
        if k == sel
            && let Some((text, at)) = &edit
        {
            let x = inner.x as usize + LABEL_W + (*at).min(value_w.saturating_sub(1));
            caret = Some(Position::new(x as u16, list.y + (k - scroll) as u16));
            lines.push(Line::from(vec![
                Span::styled(label, row_style),
                Span::styled(
                    pad_right(&ellipsize(text, value_w), value_w),
                    normal.add_modifier(Modifier::UNDERLINED),
                ),
            ]));
            continue;
        }
        let (value, style) = match inspect::decode(field, &bytes, big) {
            Some((v, _)) => (v, row_style),
            None => ("—".to_string(), if k == sel { row_style } else { dim }),
        };
        lines.push(Line::from(vec![
            Span::styled(label, row_style),
            Span::styled(pad_right(&ellipsize(&value, value_w), value_w), style),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(normal), list);
    caret
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::VfsPath;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::{Path, PathBuf};

    fn tmp(tag: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rc_insp_{tag}_{}.bin", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn hex_editor(p: &Path) -> EditorState {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let mut e = EditorState::new_hex(name, VfsPath::local(p)).unwrap();
        e.run_template_now();
        e
    }

    fn screen(e: &mut EditorState, w: u16, h: u16) -> String {
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), e, &theme)).unwrap();
        let b = t.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_panels_go_beside_below_or_instead() {
        let hex_w = 77;
        let wide = Rect::new(0, 1, 140, 30);
        // The tree alone, as before the inspector.
        let l = hex_layout(wide, hex_w, false, true, false, false);
        assert_eq!((l.hex.width, l.template.unwrap().x, l.template.unwrap().width), (77, 77, 63));
        assert_eq!(l.inspector, None);
        // Both beside the bytes: the inspector on top, the tree below it.
        let l = hex_layout(wide, hex_w, true, true, false, false);
        let (i, t) = (l.inspector.unwrap(), l.template.unwrap());
        assert_eq!((i.x, i.y, i.height), (77, 1, 18));
        assert_eq!((t.x, t.y, t.height), (77, 19, 12));
        let l = hex_layout(Rect::new(0, 1, 140, 60), hex_w, true, true, false, false);
        assert_eq!((l.inspector.unwrap().height, l.template.unwrap().height), (24, 36));
        let l = hex_layout(Rect::new(0, 1, 140, 16), hex_w, true, true, false, false);
        assert_eq!((l.inspector.unwrap().height, l.template.unwrap().height), (8, 8));
        // The inspector alone takes the whole side.
        let l = hex_layout(wide, hex_w, true, false, false, false);
        assert_eq!((l.inspector.unwrap().height, l.template), (30, None));
        // Narrow: below the bytes, which keep all the rows the inspector
        // doesn't need, and never less than they have with the tree.
        let l = hex_layout(Rect::new(0, 1, 80, 60), hex_w, true, false, false, false);
        assert_eq!(
            (l.hex.height, l.inspector.unwrap().y, l.inspector.unwrap().height),
            (36, 37, 24)
        );
        let l = hex_layout(Rect::new(0, 1, 80, 40), hex_w, true, false, false, false);
        assert_eq!((l.hex.height, l.inspector.unwrap().height), (18, 22));
        let l = hex_layout(Rect::new(0, 1, 80, 30), hex_w, false, true, false, false);
        assert_eq!((l.hex.height, l.template.unwrap().y, l.template.unwrap().height), (13, 14, 17));
        // Tiny: only the panel with the keys, in the bytes' place.
        let tiny = Rect::new(0, 1, 60, 8);
        assert_eq!(hex_layout(tiny, hex_w, true, true, false, false).hex.height, 8);
        let l = hex_layout(tiny, hex_w, true, true, true, false);
        assert_eq!((l.hex.height, l.inspector.unwrap().height, l.template), (0, 8, None));
        let l = hex_layout(tiny, hex_w, true, true, false, true);
        assert_eq!((l.hex.height, l.inspector, l.template.unwrap().height), (0, None, 8));
        assert_eq!(hex_layout(wide, hex_w, false, false, true, true).inspector, None);
    }

    #[test]
    fn f8_shows_the_bytes_at_the_cursor_in_either_byte_order() {
        let p = tmp("show", &[0x34, 0x12, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff]);
        let mut e = hex_editor(&p);
        assert!(!screen(&mut e, 160, 40).contains("Inspector"));
        e.handle_key(key(KeyCode::F(8)));
        let s = screen(&mut e, 160, 40);
        assert!(s.contains("Inspector  little-endian"), "{s}");
        assert!(s.contains("uint16    4660"), "0x1234 read little-endian:\n{s}");
        assert!(s.contains("int32     4660"));
        assert!(s.contains("Inspct"), "F8 on the key bar");
        // Following the cursor: at offset 4 every byte is 0xFF.
        e.hex.as_mut().unwrap().cursor = 4;
        let s = screen(&mut e, 160, 40);
        assert!(s.contains("int32     -1") && s.contains("uint32    4294967295"), "{s}");
        assert!(s.contains("int64     —"), "past the end:\n{s}");
        // Into the inspector with Tab (hex → ASCII → inspector), `b` for big-endian.
        e.hex.as_mut().unwrap().cursor = 0;
        e.handle_key(key(KeyCode::Tab));
        e.handle_key(key(KeyCode::Tab));
        assert!(e.inspector_focus());
        e.handle_key(key(KeyCode::Char('b')));
        let s = screen(&mut e, 160, 40);
        assert!(s.contains("big-endian") && s.contains("uint16    13330"), "0x3412:\n{s}");
        assert!(s.contains("pane:INSPECTOR"), "{s}");
        assert!(e.options().hex_inspector && e.options().hex_inspector_big_endian);
        // F8 again hides it and gives the keys back to the bytes.
        e.handle_key(key(KeyCode::F(8)));
        assert!(!e.inspector_focus() && !screen(&mut e, 160, 40).contains("Inspector"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_value_typed_in_the_inspector_overwrites_the_bytes_it_covers() {
        let p = tmp("edit", &[0u8; 8]);
        let mut e = hex_editor(&p);
        e.handle_key(key(KeyCode::F(8)));
        let _ = screen(&mut e, 160, 40);
        e.handle_key(key(KeyCode::Tab));
        e.handle_key(key(KeyCode::Tab));
        e.handle_key(key(KeyCode::Char('b')));
        // Down to uint16 (binary, int8, uint8, int16, uint16).
        for _ in 0..4 {
            e.handle_key(key(KeyCode::Down));
        }
        e.handle_key(key(KeyCode::Enter));
        e.handle_key(key(KeyCode::Backspace));
        for c in "0xBEEF".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        let s = screen(&mut e, 160, 40);
        assert!(s.contains("0xBEEF"), "the field shows what is typed:\n{s}");
        e.handle_key(key(KeyCode::Enter));
        assert_eq!(e.hex.as_mut().unwrap().window(0, 3), vec![0xbe, 0xef, 0x00]);
        assert!(e.dirty);
        // A value that doesn't fit keeps the field open and says why.
        e.handle_key(key(KeyCode::Enter));
        for _ in 0..10 {
            e.handle_key(key(KeyCode::Backspace));
        }
        for c in "70000".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key(KeyCode::Enter));
        assert!(e.status.contains("doesn't fit"), "{}", e.status);
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.hex.as_mut().unwrap().window(0, 2), vec![0xbe, 0xef]);
        // Ctrl-→ steps over the selected value's bytes.
        e.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(e.hex.as_ref().unwrap().cursor, 2);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn tab_steps_through_the_inspector_and_the_tree() {
        let p = std::env::temp_dir().join(format!("rc_insp_tab_{}.zip", std::process::id()));
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            z.start_file("hello.txt", opts).unwrap();
            std::io::Write::write_all(&mut z, b"hello template").unwrap();
            z.finish().unwrap();
        }
        let mut e = hex_editor(&p);
        assert!(e.template_panel());
        e.handle_key(key(KeyCode::F(8)));
        let _ = screen(&mut e, 160, 40);
        let at = |e: &EditorState| e.hex_focus();
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&e), HexFocus::Ascii);
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&e), HexFocus::Inspector);
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&e), HexFocus::Tree);
        assert!(!e.inspector_focus(), "one panel has the keys at a time");
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&e), HexFocus::Hex);
        let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        e.handle_key(back);
        assert_eq!(at(&e), HexFocus::Tree);
        e.handle_key(back);
        assert_eq!(at(&e), HexFocus::Inspector);
        // F6 from the inspector goes to the tree, taking the keys with it.
        e.handle_key(key(KeyCode::F(6)));
        assert_eq!(at(&e), HexFocus::Tree);
        // Both panels show beside the bytes, and a click moves the keys.
        let s = screen(&mut e, 160, 40);
        assert!(s.contains("Inspector") && s.contains("Name"), "{s}");
        let row = e.insp.list_area.y + 2;
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: e.insp.list_area.x + 2,
            row,
            modifiers: KeyModifiers::NONE,
        };
        e.handle_mouse(click);
        assert_eq!((at(&e), e.insp.sel), (HexFocus::Inspector, 2));
        let tree = e.tpl_area;
        e.handle_mouse(MouseEvent { column: tree.x + 5, row: tree.y + 3, ..click });
        assert_eq!(at(&e), HexFocus::Tree);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_read_only_file_refuses_an_edit() {
        let p = tmp("ro", &[1, 2, 3, 4]);
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&p, perms).unwrap();
        let mut e = hex_editor(&p);
        if !e.hex.as_ref().unwrap().readonly {
            // Running as root: the file opened for writing anyway.
            std::fs::remove_file(&p).ok();
            return;
        }
        e.handle_key(key(KeyCode::F(8)));
        let _ = screen(&mut e, 160, 40);
        e.handle_key(key(KeyCode::Tab));
        e.handle_key(key(KeyCode::Tab));
        e.handle_key(key(KeyCode::Enter));
        assert_eq!(e.status, "read-only file");
        assert!(e.insp.edit.is_none());
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&p, perms).unwrap();
        std::fs::remove_file(&p).ok();
    }
}
