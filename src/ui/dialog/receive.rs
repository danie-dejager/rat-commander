//! The "Receive files over LAN" dialog: a QR code for the upload page (pixel
//! graphics when available, half-block cell art otherwise), the URL, where the
//! files go, and what has arrived — with a progress bar while a file is on its
//! way. It stays open, receiving, until dismissed; closing it stops the server.

use super::DialogResult;
use super::widgets::*;
use crate::util::qr::Qr;
use ratatui::style::Color;
use std::time::Instant;

pub struct ReceiveDialog {
    /// The upload page's URL (also encoded in the QR).
    pub url: String,
    /// The directory files are saved into, as shown.
    pub dir: String,
    qr: Qr,
    /// Files saved so far, and their bytes.
    pub received: usize,
    pub bytes: u64,
    /// The upload in flight: its name, bytes so far, size, and when it began.
    current: Option<(String, u64, u64, Instant)>,
    /// The last file saved.
    last: Option<String>,
    /// The last upload that failed, and why — until the next one starts.
    failed: Option<(String, String)>,
}

impl ReceiveDialog {
    /// Build the dialog for `url`. `None` if the URL is somehow too long to
    /// encode as a QR (the caller then falls back to showing just the URL).
    pub fn new(url: String, dir: String) -> Option<Self> {
        let qr = Qr::encode(&url)?;
        Some(ReceiveDialog {
            url,
            dir,
            qr,
            received: 0,
            bytes: 0,
            current: None,
            last: None,
            failed: None,
        })
    }

    pub fn on_progress(&mut self, name: String, received: u64, total: u64) {
        let started = match &self.current {
            Some((n, _, _, at)) if *n == name => *at,
            _ => Instant::now(),
        };
        self.failed = None;
        self.current = Some((name, received, total, started));
    }

    pub fn on_received(&mut self, name: String, bytes: u64) {
        self.received += 1;
        self.bytes += bytes;
        self.current = None;
        self.last = Some(name);
    }

    pub fn on_failed(&mut self, name: String, error: String) {
        self.current = None;
        self.failed = Some((name, error));
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => DialogResult::Cancel,
            _ => DialogResult::None,
        }
    }

    /// The status line, and whether it reports a failure.
    fn status(&self) -> (String, bool) {
        if let Some((name, received, total, started)) = &self.current {
            let pct = if *total == 0 { 100 } else { received * 100 / total };
            let secs = started.elapsed().as_secs_f64();
            let rate = if secs > 0.2 {
                format!("  ·  {}/s", human_size((*received as f64 / secs) as u64))
            } else {
                String::new()
            };
            return (format!("{} {name}  {pct}%{rate}", crate::l10n::trd("Receiving")), false);
        }
        if let Some((name, error)) = &self.failed {
            return (format!("{} {name}: {error}", crate::l10n::trd("Failed:")), true);
        }
        if self.received == 0 {
            return (crate::l10n::trd("Waiting for files…"), false);
        }
        let mut s = format!(
            "{} {}  ·  {}",
            crate::l10n::trd("Files received:"),
            self.received,
            human_size(self.bytes)
        );
        if let Some(last) = &self.last {
            s.push_str(&format!("  ·  {last}"));
        }
        (s, false)
    }

    pub(crate) fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        mut gfx: Option<&mut Gfx>,
    ) {
        // The same sizing as the Send dialog: see `SendFileDialog::render`.
        let ascii_cols = self.qr.padded() as u16;
        let ascii_rows = self.qr.padded().div_ceil(2) as u16;
        // Below the QR: URL, destination, status, the progress bar and a button.
        let text_rows = 5u16;
        let avail_qr_rows = area.height.saturating_sub(text_rows + 2).max(1);
        let have_gfx = gfx.as_deref().map(Gfx::available).unwrap_or(false);
        let (qr_cols, qr_rows) = if have_gfx {
            let rows = ascii_rows.min(13).min(avail_qr_rows);
            (rows * 2, rows)
        } else {
            (ascii_cols, ascii_rows.min(avail_qr_rows))
        };

        let inner_w =
            qr_cols.max(self.url.chars().count() as u16).max(44).min(area.width.saturating_sub(4));
        let box_w = (inner_w + 4).min(area.width);
        let box_h = (qr_rows + text_rows + 2).min(area.height);
        let rect = centered(area, box_w, box_h);

        draw_shadow(f, rect, theme);
        f.render_widget(Clear, rect);
        let block = dialog_block(&crate::l10n::trd("Receive files over LAN"), theme);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(qr_rows), // QR code
                Constraint::Length(1),       // URL
                Constraint::Length(1),       // destination
                Constraint::Length(1),       // status
                Constraint::Length(1),       // progress bar
                Constraint::Min(1),          // OK button
            ])
            .split(inner);

        // A white plate behind the code: QR readers want a quiet light border.
        f.render_widget(
            Block::default().style(Style::default().bg(Color::Rgb(255, 255, 255))),
            rows[0],
        );
        let drawn = gfx
            .as_deref_mut()
            .map(|g| {
                if g.available() {
                    let (pw, ph) = g.px_size(rows[0]);
                    g.draw(f, rows[0], Slot::SendQr, self.qr.to_image_fit(pw.min(ph)));
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if !drawn {
            self.qr.render_ascii(f, rows[0]);
        }

        let width = inner.width as usize;
        let centered_line = |s: String, style: Style| {
            Paragraph::new(Line::from(ellipsize(&s, width)))
                .alignment(ratatui::layout::Alignment::Center)
                .style(style)
        };
        f.render_widget(
            centered_line(
                self.url.clone(),
                base.fg(theme.dialog_title).add_modifier(Modifier::BOLD),
            ),
            rows[1],
        );
        f.render_widget(
            centered_line(format!("{} {}", crate::l10n::trd("Saving into"), self.dir), base),
            rows[2],
        );
        let (status, failed) = self.status();
        let status_fg = crate::ui::theme::readable_on(
            if failed { theme.error_fg } else { theme.exec_fg },
            theme.dialog_bg,
        );
        f.render_widget(centered_line(status, base.fg(status_fg)), rows[3]);

        // A plain cell bar for the file in flight.
        if let Some((_, received, total, _)) = &self.current {
            let bar_w = inner.width.saturating_sub(6) as usize;
            let filled =
                if *total == 0 { bar_w } else { (bar_w as u64 * received / total) as usize };
            let bar = Line::from(vec![
                Span::styled("█".repeat(filled), base.fg(theme.exec_fg)),
                Span::styled(
                    "░".repeat(bar_w - filled.min(bar_w)),
                    base.fg(theme.dialog_border_fg),
                ),
            ]);
            f.render_widget(
                Paragraph::new(bar).alignment(ratatui::layout::Alignment::Center).style(base),
                rows[4],
            );
        }

        let ok = center_button_rect(rows[5], 10);
        if !gfx_button(f, gfx, Slot::Button(0), ok, "OK", true, theme) {
            f.render_widget(
                Paragraph::new(Line::from(button("[ OK ]", true, theme)))
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(base),
                rows[5],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(d: &mut ReceiveDialog, gfx: Option<&mut Gfx>) -> String {
        let theme = crate::ui::theme::Theme::default();
        let mut t = Terminal::new(TestBackend::new(80, 40)).unwrap();
        t.draw(|f| d.render(f, f.area(), &theme, gfx)).unwrap();
        let buf = t.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect()
    }

    #[test]
    fn follows_an_upload_from_waiting_to_received() {
        let mut d = ReceiveDialog::new(
            "http://192.168.1.5:8000/0123456789abcdef/".into(),
            "/home/alex/Pictures".into(),
        )
        .expect("encodes");
        let s = screen(&mut d, None);
        assert!(s.contains("192.168.1.5:8000/0123456789abcdef/"));
        assert!(s.contains("Saving into /home/alex/Pictures"));
        assert!(s.contains("Waiting for files"));

        d.on_progress("IMG_2041.jpg".into(), 512, 1024);
        let s = screen(&mut d, None);
        assert!(s.contains("Receiving IMG_2041.jpg  50%"), "{s}");
        assert!(s.contains('█') && s.contains('░'), "a half-full bar");

        d.on_received("IMG_2041.jpg".into(), 1024);
        let s = screen(&mut d, None);
        assert!(s.contains("Files received: 1  ·  1.0K  ·  IMG_2041.jpg"), "{s}");
        assert!(!s.contains('█'), "no bar between files");

        d.on_failed("big.iso".into(), "the transfer was cut off".into());
        assert!(screen(&mut d, None).contains("Failed: big.iso: the transfer was cut off"));
        let mut gfx = Gfx::test_halfblocks();
        let _ = screen(&mut d, Some(&mut gfx));
    }
}
