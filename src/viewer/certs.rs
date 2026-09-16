//! The viewer over a certificate or key file: the [`Report`]'s tabs as lists
//! of labelled, toned rows. F8 switches to the file's raw text and back, Enter
//! goes to the line a row came from, and "Find all" narrows the lists.

use super::search::Needle;
use super::{ViewMode, ViewerSignal, ViewerState};
use crate::certs::{Label, Report, Row, Tone};
use crate::ui::theme::Theme;
use crate::util::text::{ellipsize, pad_right};
use ratatui::Frame;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub struct CertView {
    pub report: Report,
    /// The tab up, as an index into the report's tabs.
    pub tab: usize,
    /// Per tab, the highlighted row and the first on screen, counted in the
    /// rows showing.
    sel: Vec<usize>,
    top: Vec<usize>,
    /// "Find all": the term, and per tab the rows kept.
    filter: Option<(String, Vec<Vec<usize>>)>,
    /// Rows of list on screen, recorded by the renderer.
    pub(crate) page: usize,
    pub(crate) list_area: Rect,
    pub(crate) strip_row: u16,
    pub(crate) tab_hits: Vec<(u16, u16, usize)>,
}

impl CertView {
    pub fn new(report: Report) -> Self {
        let n = report.tabs.len();
        CertView {
            report,
            tab: 0,
            sel: vec![0; n],
            top: vec![0; n],
            filter: None,
            page: 1,
            list_area: Rect::default(),
            strip_row: 0,
            tab_hits: Vec::new(),
        }
    }

    /// Rows of tab `t` that are showing.
    pub fn len(&self, t: usize) -> usize {
        match &self.filter {
            Some((_, rows)) => rows[t].len(),
            None => self.report.tabs[t].1.len(),
        }
    }

    /// Showing row `i` of tab `t`.
    pub fn row(&self, t: usize, i: usize) -> Option<&Row> {
        let idx = match &self.filter {
            Some((_, rows)) => *rows[t].get(i)?,
            None => i,
        };
        self.report.tabs[t].1.get(idx)
    }

    pub fn selected(&self) -> usize {
        self.sel[self.tab]
    }

    pub fn set_tab(&mut self, t: usize) {
        if t < self.report.tabs.len() {
            self.tab = t;
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        let n = self.report.tabs.len() as isize;
        self.tab = (self.tab as isize + delta).rem_euclid(n.max(1)) as usize;
    }

    pub fn select(&mut self, i: usize) {
        self.sel[self.tab] = i.min(self.len(self.tab).saturating_sub(1));
    }

    fn move_by(&mut self, delta: isize) {
        self.select((self.selected() as isize).saturating_add(delta).max(0) as usize);
    }

    fn scroll_into_view(&mut self, rows: usize) -> usize {
        let t = self.tab;
        let len = self.len(t);
        self.sel[t] = self.sel[t].min(len.saturating_sub(1));
        let top = crate::util::scroll::scroll_to_visible(self.top[t], self.sel[t], rows);
        self.top[t] = top.min(len.saturating_sub(rows.max(1)));
        self.top[t]
    }

    fn scroll_by(&mut self, delta: isize) {
        let t = self.tab;
        let rows = self.page.max(1);
        let max_top = self.len(t).saturating_sub(rows);
        self.top[t] = ((self.top[t] as isize).saturating_add(delta).max(0) as usize).min(max_top);
        let top = self.top[t];
        self.sel[t] = self.sel[t].clamp(top, top + rows - 1).min(self.len(t).saturating_sub(1));
    }

    /// What a search looks through in a row: its label and its value.
    fn haystack(row: &Row) -> String {
        let label = match &row.label {
            Label::Key(k) => crate::l10n::tr(k),
            Label::Text(t) => t.clone(),
        };
        format!("{label} {}", row.value)
    }

    pub fn filter_term(&self) -> Option<&str> {
        self.filter.as_ref().map(|(t, _)| t.as_str())
    }

    /// Narrow every tab to the rows matching `needle` — and the heading of
    /// each object that has one, so a match still says what it belongs to.
    pub fn set_filter(&mut self, term: &str, needle: &Needle) {
        let rows = self
            .report
            .tabs
            .iter()
            .map(|(_, rows)| {
                let mut kept = Vec::new();
                let mut heading = None;
                for (i, row) in rows.iter().enumerate() {
                    if row.depth == 0 {
                        heading = Some(i);
                    }
                    if needle.find(Self::haystack(row).as_bytes(), 0).is_some() {
                        if let Some(h) = heading.filter(|&h| h != i && kept.last() < Some(&h)) {
                            kept.push(h);
                        }
                        kept.push(i);
                    }
                }
                kept
            })
            .collect();
        self.filter = Some((term.to_string(), rows));
        self.sel.iter_mut().for_each(|s| *s = 0);
        self.top.iter_mut().for_each(|s| *s = 0);
    }

    pub fn clear_filter(&mut self) -> bool {
        let Some((_, rows)) = self.filter.take() else { return false };
        for (t, kept) in rows.iter().enumerate() {
            self.sel[t] = kept.get(self.sel[t]).copied().unwrap_or(0);
            self.top[t] = self.sel[t].saturating_sub(self.page / 2);
        }
        true
    }

    /// Highlight the next row of this tab matching `needle` (or the one
    /// before), wrapping round.
    pub fn find(&mut self, needle: &Needle, backwards: bool) -> bool {
        let len = self.len(self.tab);
        let from = self.selected();
        for step in 1..=len {
            let i = if backwards { (from + len - step % len) % len } else { (from + step) % len };
            if self
                .row(self.tab, i)
                .is_some_and(|r| needle.find(Self::haystack(r).as_bytes(), 0).is_some())
            {
                self.select(i);
                return true;
            }
        }
        false
    }
}

impl ViewerState {
    /// Attach what a certificate or key file holds, and show it.
    pub fn set_certs(&mut self, report: Report) {
        self.certs = Some(Box::new(CertView::new(report)));
        self.show_certs = true;
    }

    /// The certificate view, when it is showing (vs. the raw text).
    pub(crate) fn active_certs(&self) -> Option<&CertView> {
        self.show_certs.then_some(self.certs.as_deref()).flatten()
    }

    pub(crate) fn active_certs_mut(&mut self) -> Option<&mut CertView> {
        if self.show_certs { self.certs.as_deref_mut() } else { None }
    }

    pub(super) fn handle_certs_key(&mut self, key: KeyEvent) -> ViewerSignal {
        let shift_tab = key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT));
        let Some(view) = self.active_certs_mut() else { return ViewerSignal::Stay };
        let page = view.page.saturating_sub(1).max(1) as isize;
        match key.code {
            _ if shift_tab => view.cycle_tab(-1),
            KeyCode::Tab => view.cycle_tab(1),
            KeyCode::Char(c @ '1'..='9') => view.set_tab(c as usize - '1' as usize),
            KeyCode::Up => view.move_by(-1),
            KeyCode::Down => view.move_by(1),
            KeyCode::PageUp => view.move_by(-page),
            KeyCode::PageDown => view.move_by(page),
            KeyCode::Home => view.select(0),
            KeyCode::End => view.select(usize::MAX),
            KeyCode::Esc if view.clear_filter() => {}
            // To the line of the file the row came from, in the raw text.
            KeyCode::Enter => {
                if let Some(line) = view.row(view.tab, view.selected()).and_then(|r| r.line) {
                    self.show_certs = false;
                    self.mode = ViewMode::Text;
                    self.goto_hit_line(line + 1);
                }
            }
            KeyCode::F(8) => self.show_certs = false,
            KeyCode::Char('n') => self.find_next(),
            KeyCode::F(1)
            | KeyCode::F(3)
            | KeyCode::F(7)
            | KeyCode::F(10)
            | KeyCode::Esc
            | KeyCode::Char('q') => return self.handle_plain_view_key(key),
            _ => {}
        }
        ViewerSignal::Stay
    }

    pub(super) fn handle_certs_mouse(&mut self, ev: MouseEvent) {
        let Some(view) = self.active_certs_mut() else { return };
        match ev.kind {
            MouseEventKind::ScrollDown => view.scroll_by(3),
            MouseEventKind::ScrollUp => view.scroll_by(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                if ev.row == view.strip_row
                    && let Some(&(_, _, t)) =
                        view.tab_hits.iter().find(|(a, b, _)| ev.column >= *a && ev.column < *b)
                {
                    view.set_tab(t);
                    return;
                }
                let a = view.list_area;
                if a.height > 0 && ev.row >= a.y && ev.row < a.y + a.height {
                    let i = view.top[view.tab] + (ev.row - a.y) as usize;
                    if i < view.len(view.tab) {
                        view.select(i);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The header while the certificate view shows.
pub fn render_header(f: &mut Frame, area: Rect, v: &ViewerState, theme: &Theme) {
    let Some(view) = v.active_certs() else { return };
    let trd = crate::l10n::trd;
    let len = view.len(view.tab);
    let at = if len == 0 { 0 } else { view.selected() + 1 };
    let filter =
        view.filter_term().map(|t| format!("  {}: \"{t}\"", trd("Filter"))).unwrap_or_default();
    let tab = trd(view.report.tabs[view.tab].0.label());
    let text = format!(
        " {}: {}  [{}]  {}  {tab} {at}/{len}{filter}",
        trd("View"),
        ellipsize(&v.name, 24),
        trd("Certificates"),
        view.report.summary,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            pad_right(&ellipsize(&text, area.width as usize), area.width as usize),
            theme.menubar.add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

/// The tab strip, then the rows of the tab that is up: labels in a column,
/// values after, each toned.
pub fn render(f: &mut Frame, area: Rect, v: &mut ViewerState, theme: &Theme) {
    let base = Style::default().fg(theme.text_fg).bg(theme.panel_bg);
    f.render_widget(ratatui::widgets::Block::default().style(base), area);
    let Some(view) = v.active_certs_mut() else { return };
    if area.height < 3 || area.width < 8 {
        return;
    }
    let strip = Rect { height: 1, ..area };
    let list =
        Rect { x: area.x + 1, y: area.y + 1, width: area.width - 1, height: area.height - 1 };
    view.page = list.height as usize;
    view.list_area = list;
    view.strip_row = strip.y;
    render_tabs(f, strip, view, theme);

    let width = list.width as usize;
    let top = view.scroll_into_view(list.height as usize);
    let len = view.len(view.tab);
    let label_text = |r: &Row| match &r.label {
        Label::Key(k) => crate::l10n::trd(k),
        Label::Text(t) => t.clone(),
    };
    // The label column fits the longest label of the tab, within reason.
    let label_w = (0..len)
        .filter_map(|i| view.row(view.tab, i))
        .map(|r| label_text(r).width() + 2 * r.depth as usize)
        .max()
        .unwrap_or(0)
        .clamp(6, 30);
    let heading =
        Style::default().fg(theme.header_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(theme.header_fg).bg(theme.panel_bg);
    let tone_style = |t: Tone| match t {
        Tone::Normal => base,
        Tone::Good => Style::default().fg(theme.exec_fg).bg(theme.panel_bg),
        Tone::Warn => Style::default().fg(theme.hotkey_fg).bg(theme.panel_bg),
        Tone::Bad => {
            Style::default().fg(theme.error_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD)
        }
        Tone::Dim => Style::default().fg(theme.panel_border).bg(theme.panel_bg),
    };
    let mut lines = Vec::with_capacity(list.height as usize);
    for i in top..(top + list.height as usize).min(len) {
        let Some(row) = view.row(view.tab, i) else { break };
        let indent = "  ".repeat(row.depth as usize);
        let label =
            pad_right(&ellipsize(&format!("{indent}{}", label_text(row)), label_w), label_w);
        let value = ellipsize(&row.value, width.saturating_sub(label_w + 2));
        let selected = i == view.selected();
        let (ls, vs) = if selected {
            (theme.cursor, theme.cursor)
        } else if row.depth == 0 {
            (heading, tone_style(row.tone).add_modifier(Modifier::BOLD))
        } else {
            (label_style, tone_style(row.tone))
        };
        let used = label_w + 2 + value.width();
        let fill = if selected { theme.cursor } else { base };
        lines.push(Line::from(vec![
            Span::styled(label, ls),
            Span::styled("  ", fill),
            Span::styled(value, vs),
            Span::styled(" ".repeat(width.saturating_sub(used)), fill),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(base), list);
}

fn render_tabs(f: &mut Frame, area: Rect, view: &mut CertView, theme: &Theme) {
    let normal = Style::default().fg(theme.panel_fg).bg(theme.panel_bg);
    let count_style = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let mut x = area.x;
    for (t, (tab, _)) in view.report.tabs.iter().enumerate() {
        let title = crate::l10n::trd(tab.label());
        let count = view.len(t).to_string();
        let style = if t == view.tab { theme.cursor } else { normal };
        let cs = if t == view.tab { theme.cursor } else { count_style };
        let w = (title.width() + count.len() + 4) as u16;
        spans.push(Span::styled(format!(" {title}"), style));
        spans.push(Span::styled(format!(" {count}"), cs));
        spans.push(Span::styled(" ", style));
        spans.push(Span::styled(" ", normal));
        hits.push((x, x + w - 1, t));
        x = x.saturating_add(w);
    }
    view.tab_hits = hits;
    f.render_widget(Paragraph::new(Line::from(spans)).style(normal), area);
}

#[cfg(test)]
mod tests {
    use crate::certs::testdata;
    use crate::viewer::{ViewerSignal, ViewerState};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    const MARCH_2025: i64 = 1_740_787_200;

    fn viewer() -> ViewerState {
        let text = format!("{}{}{}", testdata::LEAF, testdata::INTERMEDIATE, testdata::LEAF_KEY);
        let mut v = ViewerState::new("site.pem".into(), text.clone().into_bytes());
        v.set_certs(crate::certs::inspect("site.pem", text.as_bytes(), MARCH_2025).unwrap());
        v
    }

    fn screen(v: &mut ViewerState, w: u16, h: u16) -> String {
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| crate::viewer::render::render(f, f.area(), v, &theme, None)).unwrap();
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

    /// Print the certificate view of `$CERT_VIEW` at `$CERT_SIZE` (`WxH`) on
    /// tab `$CERT_TAB` (1-based):
    /// `CERT_VIEW=file cargo test --bin rc viewer::certs::tests::preview -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn preview() {
        let path = std::path::PathBuf::from(std::env::var("CERT_VIEW").expect("CERT_VIEW"));
        let (w, h) = std::env::var("CERT_SIZE")
            .ok()
            .and_then(|s| {
                s.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            })
            .unwrap_or((140, 30));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let data = std::fs::read(&path).unwrap();
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
                as i64;
        let mut v = ViewerState::new(name.clone(), data.clone());
        v.set_certs(crate::certs::inspect(&name, &data, now).expect("not a certificate file"));
        let tab = std::env::var("CERT_TAB").unwrap_or_else(|_| "1".into());
        v.handle_key(key(KeyCode::Char(tab.chars().next().unwrap())));
        println!("{}", screen(&mut v, w, h));
    }

    #[test]
    fn the_view_shows_tabs_rows_and_switches_to_the_raw_text() {
        let mut v = viewer();
        let s = screen(&mut v, 120, 20);
        assert!(s.contains("[Certificates]") && s.contains("2 certificates, 1 private key"), "{s}");
        assert!(s.contains("Summary 3") && s.contains("Chain"), "the tab strip:\n{s}");
        assert!(s.contains("CN=www.example.test"), "{s}");
        assert!(s.contains("Raw"), "F8 on the key bar:\n{s}");
        // The Certificates tab, and down to the leaf's issuer.
        v.handle_key(key(KeyCode::Char('2')));
        v.handle_key(key(KeyCode::Down));
        v.handle_key(key(KeyCode::Down));
        let s = screen(&mut v, 120, 20);
        assert!(s.contains("Issuer") && s.contains("CN=Rat Test Intermediate"), "{s}");
        // Enter goes to the certificate's line in the raw text.
        v.handle_key(key(KeyCode::Enter));
        assert!(v.active_certs().is_none());
        let s = screen(&mut v, 120, 20);
        assert!(s.contains("-----BEGIN CERTIFICATE-----"), "{s}");
        assert!(s.contains("Certs"), "F8 goes back:\n{s}");
        v.handle_key(key(KeyCode::F(8)));
        assert!(v.active_certs().is_some());
        assert!(matches!(v.handle_key(key(KeyCode::Char('q'))), ViewerSignal::Close));
    }

    #[test]
    fn find_all_narrows_the_rows_keeping_what_they_belong_to() {
        let mut v = viewer();
        v.handle_key(key(KeyCode::Char('2')));
        v.apply_search(&crate::ui::dialog::SearchReplaceParams {
            replace: false,
            search: "OCSP".into(),
            replacement: String::new(),
            regex: false,
            case_sensitive: false,
            whole_words: false,
            backwards: false,
            hex: false,
            find_all: true,
        });
        let view = v.active_certs().unwrap();
        assert_eq!(view.len(view.tab), 2, "the leaf's heading and its access row");
        assert_eq!(view.row(view.tab, 0).unwrap().depth, 0);
        v.handle_key(key(KeyCode::Esc));
        assert!(v.active_certs().unwrap().filter_term().is_none(), "Esc drops the filter first");
    }
}
