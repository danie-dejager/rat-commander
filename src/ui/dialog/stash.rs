//! The stash picker: the repository's stashes, with the verbs on them.
//!
//! Save is a form of its own (*Git → Stash save…*), but showing, applying,
//! popping and dropping all act on a stash you first have to pick — so they
//! live here, as keys on the list, rather than as four more entries in the Git
//! menu. That is not only tidier: menu accelerators have to be unique within a
//! menu *in every one of the eighteen translations*, and four more entries
//! would spend four scarce letters eighteen times over.

use super::widgets::*;
use super::{DialogResult, Submit};
use crate::git::ops::{self, StashEntry};

/// What a key on the list does to the stash under the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `git stash show -p` — look before you leap.
    Show,
    /// Apply and keep the stash.
    Apply,
    /// Apply and remove it.
    Pop,
}

pub struct StashDialog {
    entries: Vec<StashEntry>,
    cursor: usize,
    offset: usize,
    view_h: usize,
}

impl StashDialog {
    pub fn new(entries: Vec<StashEntry>) -> Self {
        StashDialog { entries, cursor: 0, offset: 0, view_h: 1 }
    }

    fn current(&self) -> Option<&StashEntry> {
        self.entries.get(self.cursor)
    }

    fn run(&self, verb: Verb) -> DialogResult {
        let Some(e) = self.current() else { return DialogResult::Cancel };
        let (title, args) = match verb {
            Verb::Show => ("stash show", ops::stash_show_args(&e.name)),
            Verb::Apply => ("stash apply", ops::stash_apply_args(&e.name)),
            Verb::Pop => ("stash pop", ops::stash_pop_args(&e.name)),
        };
        DialogResult::Submit(Submit::GitRun { title: title.into(), args })
    }

    /// Dropping throws work away for good, so it asks first — the same way
    /// `git rm` and `git restore` do.
    fn drop_current(&self) -> DialogResult {
        let Some(e) = self.current() else { return DialogResult::Cancel };
        DialogResult::Submit(Submit::ConfirmDropStash {
            label: format!("{} — {}", e.name, e.subject),
            args: ops::stash_drop_args(&e.name),
        })
    }

    fn box_rect(&self, area: Rect) -> Rect {
        let w = 76u16.min(area.width.saturating_sub(4));
        // Two extra rows: the border and the key hint at the bottom.
        let h = (self.entries.len() as u16 + 3).min(area.height.saturating_sub(4)).max(4);
        centered(area, w, h)
    }

    fn list_rect(&self, area: Rect) -> Rect {
        let rect = self.box_rect(area);
        Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            width: rect.width.saturating_sub(2),
            height: rect.height.saturating_sub(3),
        }
    }

    pub(crate) fn handle_click(&mut self, area: Rect, col: u16, row: u16) -> DialogResult {
        let inner = self.list_rect(area);
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
            // A click shows the stash; nothing is applied without a keypress,
            // since a misclick must not change the working tree.
            return self.run(Verb::Show);
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
            KeyCode::Enter => self.run(Verb::Show),
            KeyCode::Char('a') | KeyCode::Char('A') => self.run(Verb::Apply),
            KeyCode::Char('p') | KeyCode::Char('P') => self.run(Verb::Pop),
            KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => self.drop_current(),
            _ => DialogResult::None,
        }
    }

    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let rect = self.box_rect(area);
        draw_shadow(f, rect, theme);
        f.render_widget(Clear, rect);
        let block = dialog_block(&crate::l10n::trd("Stashes"), theme);
        f.render_widget(block, rect);

        let inner = self.list_rect(area);
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
            .map(|(i, e)| {
                let text = format!("{}  {}  {}", e.name, e.when, e.subject);
                let text = ellipsize(&text, inner.width as usize);
                let style = if i == self.cursor { theme.dialog_selection } else { base };
                Line::from(Span::styled(pad_right(&text, inner.width as usize), style))
            })
            .collect();
        f.render_widget(Paragraph::new(lines).style(base), inner);

        // The verbs are only discoverable from here, so always show them.
        let hint = Rect { y: rect.y + rect.height - 2, height: 1, ..inner };
        let tr = crate::l10n::trd;
        let text = format!(
            "Enter {}   a {}   p {}   d {}",
            tr("show"),
            tr("apply"),
            tr("pop"),
            tr("drop")
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                ellipsize(&text, inner.width as usize),
                base.fg(theme.dialog_title),
            )))
            .style(base),
            hint,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn dialog() -> StashDialog {
        StashDialog::new(vec![
            StashEntry {
                name: "stash@{0}".into(),
                when: "2 hours ago".into(),
                subject: "WIP on main".into(),
            },
            StashEntry {
                name: "stash@{1}".into(),
                when: "3 days ago".into(),
                subject: "on feature/x".into(),
            },
        ])
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn args_of(r: DialogResult) -> Vec<String> {
        match r {
            DialogResult::Submit(Submit::GitRun { args, .. }) => args,
            _ => panic!("expected a git run"),
        }
    }

    /// Each verb acts on the stash under the cursor, by its ref — which is why
    /// moving the cursor first has to change what runs.
    #[test]
    fn the_verbs_act_on_the_stash_under_the_cursor() {
        let mut d = dialog();
        assert_eq!(args_of(d.handle_key(key('a'))), ["stash", "apply", "stash@{0}"]);
        assert_eq!(args_of(d.handle_key(key('p'))), ["stash", "pop", "stash@{0}"]);

        d.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(args_of(d.handle_key(key('a'))), ["stash", "apply", "stash@{1}"]);
        assert_eq!(
            args_of(d.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))),
            ["stash", "show", "-p", "--stat", "stash@{1}"]
        );
    }

    /// Dropping is the one verb that destroys work, so it asks first rather
    /// than running straight away.
    #[test]
    fn dropping_asks_before_it_runs() {
        let mut d = dialog();
        match d.handle_key(key('d')) {
            DialogResult::Submit(Submit::ConfirmDropStash { label, args }) => {
                assert!(label.contains("stash@{0}"), "the box names what is going: {label}");
                assert_eq!(args, ["stash", "drop", "stash@{0}"]);
            }
            _ => panic!("expected a confirmation"),
        }
    }

    #[test]
    fn navigation_stays_inside_the_list_and_esc_closes() {
        let mut d = dialog();
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        d.handle_key(up);
        assert_eq!(d.cursor, 0, "cannot go above the first");
        for _ in 0..5 {
            d.handle_key(down);
        }
        assert_eq!(d.cursor, 1, "nor past the last");
        assert!(matches!(
            d.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            DialogResult::Cancel
        ));
    }

    /// An empty list must not let a verb build an argv with no ref in it.
    #[test]
    fn an_empty_list_has_nothing_to_act_on() {
        let mut d = StashDialog::new(Vec::new());
        assert!(matches!(d.handle_key(key('a')), DialogResult::Cancel));
        assert!(matches!(d.handle_key(key('d')), DialogResult::Cancel));
    }
}
