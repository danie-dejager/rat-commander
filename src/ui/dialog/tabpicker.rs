//! The tab picker (Alt-J): the active panel's open tabs as a selectable list.
//!
//! This is the reliable way to switch tabs. `Ctrl-Tab` is only delivered by
//! terminals that report modified keys, so on its own it would silently do
//! nothing for a lot of users; a pickable list always works.

use super::widgets::*;
use super::{DialogResult, Submit};

pub struct TabPickerDialog {
    /// Which panel these tabs belong to.
    side: usize,
    /// One directory per open tab, in tab order.
    entries: Vec<VfsPath>,
    /// The tab currently being shown, marked in the list.
    current: usize,
    cursor: usize,
    offset: usize,
    view_h: usize,
}

impl TabPickerDialog {
    pub fn new(side: usize, entries: Vec<VfsPath>, current: usize) -> Self {
        TabPickerDialog { side, entries, current, cursor: current, offset: 0, view_h: 1 }
    }

    fn submit_current(&self) -> DialogResult {
        match self.entries.get(self.cursor) {
            // Picking the tab you are on is just a close.
            Some(_) if self.cursor == self.current => DialogResult::Cancel,
            Some(_) => DialogResult::Submit(Submit::SelectTab(self.side, self.cursor)),
            None => DialogResult::Cancel,
        }
    }

    fn box_rect(&self, area: Rect) -> Rect {
        let w = 76u16.min(area.width.saturating_sub(4));
        let h = (self.entries.len() as u16 + 2).min(area.height.saturating_sub(4)).max(3);
        centered(area, w, h)
    }

    pub(crate) fn handle_click(&mut self, area: Rect, col: u16, row: u16) -> DialogResult {
        let rect = self.box_rect(area);
        let inner = Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            width: rect.width.saturating_sub(2),
            height: rect.height.saturating_sub(2),
        };
        if col < inner.x
            || col >= inner.x + inner.width
            || row < inner.y
            || row >= inner.y + inner.height
        {
            return DialogResult::None;
        }
        let idx = self.offset + (row - inner.y) as usize;
        if idx < self.entries.len() {
            self.cursor = idx;
            return self.submit_current();
        }
        DialogResult::None
    }

    pub(crate) fn handle_scroll(&mut self, delta: isize) -> DialogResult {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = (self.cursor as isize + delta).clamp(0, max as isize) as usize;
        DialogResult::None
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        let max = self.entries.len().saturating_sub(1);
        let page = self.view_h.max(1);
        match key.code {
            KeyCode::Esc => DialogResult::Cancel,
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                DialogResult::None
            }
            KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(max);
                DialogResult::None
            }
            KeyCode::PageUp => {
                self.cursor = self.cursor.saturating_sub(page);
                DialogResult::None
            }
            KeyCode::PageDown => {
                self.cursor = (self.cursor + page).min(max);
                DialogResult::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                DialogResult::None
            }
            KeyCode::End => {
                self.cursor = max;
                DialogResult::None
            }
            KeyCode::Enter => self.submit_current(),
            _ => DialogResult::None,
        }
    }

    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let rect = self.box_rect(area);
        draw_shadow(f, rect, theme);
        f.render_widget(Clear, rect);
        let block = dialog_block(&crate::l10n::trd("Tabs"), theme);
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        self.view_h = inner.height as usize;
        self.offset = crate::util::scroll::scroll_to_visible(self.offset, self.cursor, self.view_h);
        self.offset = self.offset.min(self.entries.len().saturating_sub(self.view_h));

        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let lines: Vec<Line> = self
            .entries
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(self.view_h)
            .map(|(i, path)| {
                let mark = if i == self.current { "▶ " } else { "  " };
                let shown = format!("{}. {}", i + 1, path.display());
                let text =
                    format!("{mark}{}", ellipsize(&shown, inner.width.saturating_sub(2) as usize));
                let style = if i == self.cursor {
                    theme.dialog_selection
                } else if i == self.current {
                    base.fg(theme.dialog_title)
                } else {
                    base
                };
                Line::from(Span::styled(pad_right(&text, inner.width as usize), style))
            })
            .collect();
        f.render_widget(Paragraph::new(lines).style(base), inner);
    }
}
