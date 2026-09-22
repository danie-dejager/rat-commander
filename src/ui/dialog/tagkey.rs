//! The tag-key picker (F5 on the editor's tag page): every key the file's tag
//! format can hold that it does not hold already, filtered as you type.
//!
//! A tag key is not something to be typed from memory — there are around a
//! hundred of them, spelled as the format spells them — so they are listed and
//! searched instead. The list is what the *file's own* tag type supports:
//! offering an MP4 a key only Vorbis comments have would write something the
//! file cannot keep, and the value would vanish on the next read.

use super::palette::{fuzzy, highlight_spans};
use super::widgets::*;
use super::{DialogResult, Submit};
use lofty::tag::ItemKey;

pub struct TagKeyDialog {
    /// Every key that may be added, as `(key, label)`.
    keys: Vec<(ItemKey, String)>,
    query: String,
    qcursor: usize,
    /// Indices into `keys`, with the characters the query matched.
    filtered: Vec<(usize, Vec<usize>)>,
    sel: usize,
    offset: usize,
    list_area: Rect,
}

impl TagKeyDialog {
    pub fn new(keys: Vec<(ItemKey, String)>) -> Self {
        let mut d = TagKeyDialog {
            keys,
            query: String::new(),
            qcursor: 0,
            filtered: Vec::new(),
            sel: 0,
            offset: 0,
            list_area: Rect::default(),
        };
        d.refilter();
        d
    }

    /// Whether there is anything at all to pick.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn refilter(&mut self) {
        let q = self.query.trim().to_string();
        let mut hits: Vec<(i32, usize, Vec<usize>)> = Vec::new();
        for (i, (_, label)) in self.keys.iter().enumerate() {
            if q.is_empty() {
                hits.push((0, i, Vec::new()));
            } else if let Some((score, pos)) = fuzzy(&q, label) {
                hits.push((score, i, pos));
            }
        }
        if !q.is_empty() {
            hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        self.filtered = hits.into_iter().map(|(_, i, p)| (i, p)).collect();
        self.sel = 0;
        self.offset = 0;
    }

    fn move_sel(&mut self, delta: isize) {
        let max = self.filtered.len() as isize - 1;
        self.sel = (self.sel as isize + delta).clamp(0, max.max(0)) as usize;
    }

    fn submit(&self) -> DialogResult {
        let Some((i, _)) = self.filtered.get(self.sel) else { return DialogResult::None };
        let (key, label) = &self.keys[*i];
        DialogResult::Submit(Submit::AddTagKey(*key, label.clone()))
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        let page = self.list_area.height.max(1) as isize;
        match key.code {
            KeyCode::Esc => DialogResult::Cancel,
            KeyCode::Enter => self.submit(),
            KeyCode::Up => {
                self.move_sel(-1);
                DialogResult::None
            }
            KeyCode::Down | KeyCode::Tab => {
                self.move_sel(1);
                DialogResult::None
            }
            KeyCode::PageUp => {
                self.move_sel(-page);
                DialogResult::None
            }
            KeyCode::PageDown => {
                self.move_sel(page);
                DialogResult::None
            }
            _ => {
                let before = self.query.clone();
                edit_text(&mut self.query, &mut self.qcursor, key);
                if self.query != before {
                    self.refilter();
                }
                DialogResult::None
            }
        }
    }

    pub(crate) fn handle_click(&mut self, _area: Rect, col: u16, row: u16) -> DialogResult {
        let a = self.list_area;
        if col < a.x || col >= a.x + a.width || row < a.y || row >= a.y + a.height {
            return DialogResult::None;
        }
        let idx = self.offset + (row - a.y) as usize;
        if idx < self.filtered.len() {
            self.sel = idx;
            return self.submit();
        }
        DialogResult::None
    }

    pub(crate) fn handle_scroll(&mut self, delta: isize) -> DialogResult {
        self.move_sel(delta);
        DialogResult::None
    }

    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let width = 56u16.min(area.width.saturating_sub(4)).max(24);
        let height = area.height.saturating_sub(4).clamp(8, 26);
        let rect = centered(area, width, height);
        draw_shadow(f, rect, theme);
        f.render_widget(Clear, rect);
        let title = format!("{} ({})", crate::l10n::trd("Add tag"), self.filtered.len());
        let block = dialog_block(&title, theme);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        if inner.width < 10 || inner.height < 4 {
            return;
        }
        let iw = inner.width as usize;
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let field = Style::default().fg(theme.input_fg).bg(theme.input_bg);

        // The query line.
        let qrow = Rect { height: 1, ..inner };
        let prompt = "  ";
        let avail = iw.saturating_sub(prompt.len() + 1);
        let start = self.qcursor.saturating_sub(avail.saturating_sub(1));
        let shown: String = self.query.chars().skip(start).take(avail).collect();
        let pad = " ".repeat(iw.saturating_sub(prompt.len() + shown.chars().count()));
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prompt, Style::default().fg(theme.dialog_title).bg(theme.input_bg)),
                Span::styled(shown, field),
                Span::styled(pad, field),
            ])),
            qrow,
        );
        f.set_cursor_position(Position::new(
            qrow.x + (prompt.len() + self.qcursor - start).min(iw - 1) as u16,
            qrow.y,
        ));

        let list = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
        self.list_area = list;
        let visible = list.height as usize;
        self.offset = crate::util::scroll::scroll_to_visible(self.offset, self.sel, visible.max(1));

        let mut lines = Vec::with_capacity(visible);
        for (row, (i, hl)) in self.filtered.iter().enumerate().skip(self.offset).take(visible) {
            let selected = row == self.sel;
            let style = if selected { theme.button_focused } else { base };
            let hot = style.fg(theme.hotkey_fg).add_modifier(Modifier::BOLD);
            let label = &self.keys[*i].1;
            let mut spans = vec![Span::styled(" ", style)];
            spans.extend(highlight_spans(label, hl, style, hot));
            let used = 1 + label.chars().count();
            spans.push(Span::styled(" ".repeat(iw.saturating_sub(used)), style));
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), list);
    }
}
