//! The binary template picker (F5 in the hex editor): every template, the
//! ones that fit the file first, filtered as you type. Enter uses the
//! highlighted one, F4 opens it for editing.

use super::palette::{fuzzy, highlight_spans};
use super::widgets::*;
use super::{DialogResult, Submit};
use crate::bt::header::{Origin, TemplateInfo};

enum Entry {
    /// Stop using a template.
    Off,
    Template {
        info: Box<TemplateInfo>,
        fits: bool,
    },
}

pub struct TemplatePickerDialog {
    entries: Vec<Entry>,
    /// The file name of the template in use, marked in the list.
    current: Option<String>,
    query: String,
    qcursor: usize,
    /// Indices into `entries` with their matched characters.
    filtered: Vec<(usize, Vec<usize>)>,
    sel: usize,
    offset: usize,
    list_area: Rect,
}

impl TemplatePickerDialog {
    /// `templates` in the order to list them: `fitting` (indices, best first)
    /// go before the rest, which are sorted by category and name.
    pub fn new(templates: &[TemplateInfo], fitting: &[usize], current: Option<String>) -> Self {
        let mut entries = vec![Entry::Off];
        for &i in fitting {
            entries.push(Entry::Template { info: Box::new(templates[i].clone()), fits: true });
        }
        let mut rest: Vec<&TemplateInfo> = templates
            .iter()
            .enumerate()
            .filter(|(i, _)| !fitting.contains(i))
            .map(|(_, t)| t)
            .collect();
        rest.sort_by(|a, b| {
            a.category
                .to_lowercase()
                .cmp(&b.category.to_lowercase())
                .then(a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()))
        });
        entries.extend(
            rest.into_iter().map(|t| Entry::Template { info: Box::new(t.clone()), fits: false }),
        );
        let mut d = TemplatePickerDialog {
            entries,
            current,
            query: String::new(),
            qcursor: 0,
            filtered: Vec::new(),
            sel: 0,
            offset: 0,
            list_area: Rect::default(),
        };
        d.refilter();
        // Start on the template in use, else on the best fit.
        d.sel = d
            .filtered
            .iter()
            .position(|(i, _)| matches!(&d.entries[*i], Entry::Template { info, .. } if Some(&info.file_name) == d.current.as_ref()))
            .unwrap_or(if fitting.is_empty() { 0 } else { 1 });
        d
    }

    fn label(e: &Entry) -> String {
        match e {
            Entry::Off => crate::l10n::tr("(No template)"),
            Entry::Template { info, .. } => info.file_name.clone(),
        }
    }

    fn refilter(&mut self) {
        let q = self.query.trim().to_string();
        let mut hits: Vec<(i32, usize, Vec<usize>)> = Vec::new();
        for (idx, e) in self.entries.iter().enumerate() {
            if q.is_empty() {
                hits.push((0, idx, Vec::new()));
                continue;
            }
            let label = Self::label(e);
            if let Some((score, pos)) = fuzzy(&q, &label) {
                hits.push((score + 1000, idx, pos));
            } else if let Entry::Template { info, .. } = e
                && (fuzzy(&q, &info.purpose).is_some() || fuzzy(&q, &info.category).is_some())
            {
                hits.push((0, idx, Vec::new()));
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

    fn submit(&self, edit: bool) -> DialogResult {
        let Some((i, _)) = self.filtered.get(self.sel) else { return DialogResult::None };
        match (&self.entries[*i], edit) {
            (Entry::Off, false) => DialogResult::Submit(Submit::EditorTemplate(None)),
            (Entry::Off, true) => DialogResult::None,
            (Entry::Template { info, .. }, false) => {
                DialogResult::Submit(Submit::EditorTemplate(Some(info.clone())))
            }
            (Entry::Template { info, .. }, true) => {
                DialogResult::Submit(Submit::EditorEditTemplate(info.clone()))
            }
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        let page = self.list_area.height.max(1) as isize;
        match key.code {
            KeyCode::Esc => DialogResult::Cancel,
            KeyCode::Enter => self.submit(false),
            KeyCode::F(4) => self.submit(true),
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
            return self.submit(false);
        }
        DialogResult::None
    }

    pub(crate) fn handle_scroll(&mut self, delta: isize) -> DialogResult {
        self.move_sel(delta);
        DialogResult::None
    }

    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let width = 96u16.min(area.width.saturating_sub(4)).max(30);
        let height = area.height.saturating_sub(4).clamp(8, 30);
        let rect = centered(area, width, height);
        draw_shadow(f, rect, theme);
        f.render_widget(Clear, rect);
        let title = format!(
            "{} ({})",
            crate::l10n::trd("Binary templates"),
            self.filtered.len().saturating_sub(1)
        );
        let block = dialog_block(&title, theme);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        if inner.width < 10 || inner.height < 5 {
            return;
        }
        let iw = inner.width as usize;
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let dim = base.fg(theme.panel_border);
        let field = Style::default().fg(theme.input_fg).bg(theme.input_bg);

        // Query line.
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

        // The list, with the highlighted template's details at the bottom.
        let list = Rect { y: inner.y + 1, height: inner.height.saturating_sub(3), ..inner };
        self.list_area = list;
        let visible = list.height as usize;
        self.offset = crate::util::scroll::scroll_to_visible(self.offset, self.sel, visible.max(1));
        let name_w = 26.min(iw / 3);
        let cat_w = 16.min(iw / 5);
        let tag_w = 10;
        let purpose_w = iw.saturating_sub(name_w + cat_w + tag_w + 3);
        let mut lines = Vec::with_capacity(visible);
        let fits_tag = crate::l10n::tr("fits");
        for (row, (i, hl)) in self.filtered.iter().enumerate().skip(self.offset).take(visible) {
            let e = &self.entries[*i];
            let selected = row == self.sel;
            let row_style = if selected { theme.button_focused } else { base };
            let hot = row_style.fg(theme.hotkey_fg).add_modifier(Modifier::BOLD);
            let mut spans = vec![Span::styled(" ", row_style)];
            match e {
                Entry::Off => {
                    let label = Self::label(e);
                    let len = label.chars().count();
                    spans.extend(highlight_spans(&label, hl, row_style, hot));
                    spans.push(Span::styled(" ".repeat(iw.saturating_sub(len + 1)), row_style));
                }
                Entry::Template { info, fits } => {
                    let current = self.current.as_deref() == Some(info.file_name.as_str());
                    let name = ellipsize(
                        &format!("{}{}", if current { "• " } else { "" }, info.file_name),
                        name_w,
                    );
                    let hl: Vec<usize> =
                        if current { hl.iter().map(|p| p + 2).collect() } else { hl.clone() };
                    let len = name.chars().count();
                    spans.extend(highlight_spans(&name, &hl, row_style, hot));
                    spans.push(Span::styled(" ".repeat(name_w + 1 - len.min(name_w)), row_style));
                    let other = if selected { row_style } else { dim };
                    spans.push(Span::styled(
                        pad_right(&ellipsize(&info.category, cat_w), cat_w + 1),
                        other,
                    ));
                    spans.push(Span::styled(
                        pad_right(&ellipsize(&info.purpose, purpose_w), purpose_w),
                        row_style,
                    ));
                    let tag = if *fits {
                        fits_tag.clone()
                    } else {
                        match info.origin {
                            Origin::BuiltIn => String::new(),
                            Origin::Modified => crate::l10n::tr("modified"),
                            Origin::User => crate::l10n::tr("user"),
                        }
                    };
                    let tag_style = if selected { row_style } else { base.fg(theme.dialog_title) };
                    spans.push(Span::styled(
                        format!(" {}", pad_right(&ellipsize(&tag, tag_w), tag_w)),
                        tag_style,
                    ));
                }
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines).style(base), list);

        let detail_y = list.y + list.height;
        let (mask, path) = match self.filtered.get(self.sel).map(|(i, _)| &self.entries[*i]) {
            Some(Entry::Template { info, .. }) => (
                format!(
                    "{}{} {}   {} {}",
                    if info.version.is_empty() {
                        String::new()
                    } else {
                        format!("v{}   ", info.version)
                    },
                    crate::l10n::tr("File mask:"),
                    info.masks.join(", "),
                    crate::l10n::tr("ID bytes:"),
                    info.id_text,
                ),
                info.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| crate::l10n::tr("(built in)")),
            ),
            _ => (String::new(), String::new()),
        };
        let keys = crate::l10n::tr("Enter: use   F4: edit   Esc: close");
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(pad_right(&ellipsize(&format!(" {mask}"), iw), iw), dim)),
                Line::from(Span::styled(
                    pad_right(&ellipsize(&format!(" {path}   {keys}"), iw), iw),
                    dim,
                )),
            ]),
            Rect { y: detail_y, height: 2, ..inner },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn info(name: &str, cat: &str) -> TemplateInfo {
        let mut i = crate::bt::header::parse_header(name, b"");
        i.category = cat.into();
        i
    }

    #[test]
    fn fitting_templates_come_first_and_typing_filters() {
        let ts =
            vec![info("ZIP.bt", "Archive"), info("PNG.bt", "Image"), info("7ZIP.bt", "Archive")];
        let mut d = TemplatePickerDialog::new(&ts, &[1], None);
        // (No template), then the fit, then the rest by category.
        let labels: Vec<String> =
            d.filtered.iter().map(|(i, _)| TemplatePickerDialog::label(&d.entries[*i])).collect();
        assert_eq!(labels[1..], ["PNG.bt", "7ZIP.bt", "ZIP.bt"]);
        assert_eq!(d.sel, 1, "starts on the best fit");
        for c in "zip".chars() {
            d.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let first = TemplatePickerDialog::label(&d.entries[d.filtered[0].0]);
        assert_eq!(first, "ZIP.bt");
        assert!(
            matches!(d.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), DialogResult::Submit(Submit::EditorTemplate(Some(i))) if i.file_name == "ZIP.bt")
        );
        assert!(matches!(
            d.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE)),
            DialogResult::Submit(Submit::EditorEditTemplate(_))
        ));
    }
}
