//! Laying out and drawing an [`AudioView`]: the picture, the progress row, the
//! time scale and the transport row.
//!
//! The picture is the only part a graphics protocol draws. Text laid over a
//! Kitty or Sixel image is not shown, so everything that moves — the play
//! position, the time, the buttons — lives in cell rows beneath it.
//!
//! The Details view has no [`Gfx`] of its own (the root layer composites its
//! pixels once the panels are laid out), so drawing is split: [`render_cells`]
//! does everything but the pixels, and [`draw_pixels`] the pixels, wherever
//! the caller has the graphics layer to hand. The viewer does both at once
//! through [`render`].

use super::AudioView;
use super::raster::Palette;
use super::view::{AudioHits, Transport};
use crate::ui::graphics::{Gfx, Slot};
use crate::ui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

/// Longest edge a pixel picture is built at. A larger area has it scaled up
/// by the graphics layer, which costs a resize rather than a raster.
const MAX_PX: u32 = 2048;

/// Where each part of the widget goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioLayout {
    pub area: Rect,
    pub image: Rect,
    pub progress: Rect,
    /// Time labels under the progress row (the viewer, when there is room).
    pub ticks: Option<Rect>,
    pub transport: Rect,
}

/// Split `area` into the widget's rows, bottom up: the transport row, the time
/// scale (unless `compact`), the progress row, then — under Sixel, which can
/// place an image a row lower than asked — a spare row, and the picture above.
pub fn layout(area: Rect, compact: bool, spacer: bool) -> AudioLayout {
    let mut lay = AudioLayout { area, ..Default::default() };
    let mut bottom = area.y + area.height;
    let mut take = |n: u16| {
        bottom -= n;
        Rect { y: bottom, height: n, ..area }
    };
    let mut left = area.height;
    if left >= 1 {
        lay.transport = take(1);
        left -= 1;
    }
    if !compact && left >= 4 {
        lay.ticks = Some(take(1));
        left -= 1;
    }
    if left >= 1 {
        lay.progress = take(1);
        left -= 1;
    }
    if spacer && left >= 2 {
        take(1);
        left -= 1;
    }
    lay.image = Rect { y: area.y, height: left, ..area };
    lay
}

/// Draw the viewer's audio view: cells and pixels together.
pub fn render(
    f: &mut Frame,
    area: Rect,
    view: &AudioView,
    theme: &Theme,
    gfx: Option<&mut Gfx>,
    slot: Slot,
) {
    let graphics = gfx.as_ref().is_some_and(|g| g.available());
    let spacer = graphics && gfx.as_ref().is_some_and(|g| g.may_land_low());
    let lay = layout(area, false, spacer);
    render_cells(f, &lay, view, theme, graphics, false);
    if graphics && let Some(g) = gfx {
        draw_pixels(g, f, lay.image, view, theme, slot);
    }
}

/// Draw the pixel picture into `area` through the graphics layer.
pub fn draw_pixels(g: &mut Gfx, f: &mut Frame, area: Rect, view: &AudioView, theme: &Theme, slot: Slot) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (mut w, mut h) = g.px_size(area);
    let long = w.max(h);
    if long > MAX_PX {
        w = (w * MAX_PX / long).max(1);
        h = (h * MAX_PX / long).max(1);
    }
    let pal = Palette::from_theme(theme);
    let sig = view.image_sig(w, h, &pal);
    // Scaled rather than fitted: the picture must fill the rect exactly, or a
    // click would seek to a different moment than the one under the pointer.
    g.draw_cached_scaled(f, area, slot, sig, || view.build_image(w, h, &pal, false));
}

/// Draw everything but the pixel picture, and record where the controls went.
/// Without `graphics` the picture is drawn here too, as half-block cell art
/// (or an ASCII ramp on a terminal without truecolor), with a play-position
/// line in it.
pub fn render_cells(
    f: &mut Frame,
    lay: &AudioLayout,
    view: &AudioView,
    theme: &Theme,
    graphics: bool,
    compact: bool,
) {
    let bg = Style::default().bg(theme.panel_bg);
    f.render_widget(Block::default().style(bg), lay.area);
    let mut hits = AudioHits { area: lay.area, image: lay.image, progress: lay.progress, ..Default::default() };

    if lay.image.width > 0 && lay.image.height > 0 {
        if !graphics {
            let pal = Palette::from_theme(theme);
            let (w, h) = (lay.image.width as u32, lay.image.height as u32 * 2);
            let img = view.build_image(w, h, &pal, true);
            if theme.truecolor {
                crate::util::img::render_halfblocks(f, lay.image, &img, theme.panel_bg);
            } else {
                crate::util::img::render_ascii_ramp(f, lay.image, &img, theme);
            }
        }
        crate::ui::gradient::mark_painted(lay.image);
    }
    if lay.progress.width > 0 {
        render_progress(f, lay.progress, view, theme);
    }
    if let Some(t) = lay.ticks {
        render_ticks(f, t, view, theme);
    }
    if lay.transport.width > 0 {
        render_transport(f, lay.transport, view, theme, compact, &mut hits);
    }
    view.set_hits(hits);
}

/// The played part of the file as a solid run, with a marker at the position.
fn render_progress(f: &mut Frame, area: Rect, view: &AudioView, theme: &Theme) {
    let w = area.width as usize;
    let head = ((view.frac() * w as f32) as usize).min(w.saturating_sub(1));
    let played = Style::default().fg(theme.media_fg).bg(theme.panel_bg);
    let rest = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    let marker = Style::default().fg(theme.panel_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD);
    let line = Line::from(vec![
        Span::styled("━".repeat(head), played),
        Span::styled("●", marker),
        Span::styled("─".repeat(w.saturating_sub(head + 1)), rest),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Time labels at a round interval along the progress row.
fn render_ticks(f: &mut Frame, area: Rect, view: &AudioView, theme: &Theme) {
    const STEPS: [u64; 14] = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200];
    let dur = view.duration().as_secs_f64();
    let w = area.width as usize;
    if dur <= 0.0 || w == 0 {
        return;
    }
    // Labels at least ten cells apart.
    let step = STEPS.iter().copied().find(|&s| s as f64 / dur * w as f64 >= 10.0).unwrap_or(7200);
    let mut row = vec![' '; w];
    let mut free_from = 0;
    let mut t = 0;
    while (t as f64) < dur {
        let x = (t as f64 / dur * w as f64) as usize;
        let label = super::format_time(std::time::Duration::from_secs(t));
        let len = label.chars().count();
        if x >= free_from && x + len <= w {
            for (i, c) in label.chars().enumerate() {
                row[x + i] = c;
            }
            free_from = x + len + 1;
        }
        t += step;
    }
    let dim = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    f.render_widget(Paragraph::new(Line::from(Span::styled(row.into_iter().collect::<String>(), dim))), area);
}

/// A row of spans laid out left to right, remembering where each went.
struct Row {
    spans: Vec<Span<'static>>,
    x: u16,
    y: u16,
    right: u16,
}

impl Row {
    fn push(&mut self, s: String, style: Style) -> Rect {
        let w = s.width() as u16;
        let r = Rect { x: self.x, y: self.y, width: w.min(self.right.saturating_sub(self.x)), height: 1 };
        self.x = self.x.saturating_add(w);
        self.spans.push(Span::styled(s, style));
        r
    }
}

/// The buttons, the time, what is going on, and the volume.
fn render_transport(
    f: &mut Frame,
    area: Rect,
    view: &AudioView,
    theme: &Theme,
    compact: bool,
    hits: &mut AudioHits,
) {
    let button = Style::default().fg(theme.panel_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD);
    let text = Style::default().fg(theme.panel_fg).bg(theme.panel_bg);
    let dim = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    let accent = Style::default().fg(theme.media_fg).bg(theme.panel_bg);
    let mut row = Row { spans: Vec::new(), x: area.x, y: area.y, right: area.x + area.width };

    let play = if view.playing() { "❚❚" } else { "▶" };
    let mut controls = vec![
        (Transport::Back, "◀◀"),
        (Transport::PlayPause, play),
        (Transport::Stop, "■"),
        (Transport::Forward, "▶▶"),
    ];
    if !compact {
        controls.insert(0, (Transport::Start, "|◀"));
    }
    for (t, glyph) in controls {
        let r = row.push(format!(" {glyph} "), button);
        hits.buttons.push((r, t));
    }
    let (pos, dur) = (super::format_time(view.position()), super::format_time(view.duration()));
    row.push(if compact { format!(" {pos}/{dur}") } else { format!("  {pos} / {dur}") }, text);

    // The volume, right-aligned: a bar in the viewer, just the figure beside
    // the − and + in the Details view.
    let pct = format!("{:>3}%", (view.volume() * 100.0).round() as u32);
    let bar_w: u16 = if compact { 0 } else { 10 };
    let volume_w = 3 + bar_w + 3 + pct.width() as u16;
    let used = row.x - area.x;
    let show_volume = used + 1 + volume_w <= area.width;
    let room = area.width - used - if show_volume { volume_w } else { 0 };

    // What is going on, when anything is — in the viewer; the Details view
    // says it beside the format instead, where there is more room.
    if !compact && let Some(mut s) = status_text(view) {
        if view.unavailable()
            && let Some(e) = view.output_error()
        {
            s = format!("{s}: {e}");
        }
        let style = if view.unavailable() || view.failed() {
            Style::default().fg(theme.error_fg).bg(theme.panel_bg)
        } else {
            dim
        };
        // Cut to the room there is (an error can be long), but not to a stub.
        if room > 12 {
            let s = crate::util::text::ellipsize(&format!("  {s}"), room as usize - 1);
            row.push(s, style);
        }
    }
    if show_volume {
        let gap = (row.right - volume_w).saturating_sub(row.x);
        row.push(" ".repeat(gap as usize), text);
        let r = row.push(" − ".to_string(), button);
        hits.buttons.push((r, Transport::VolumeDown));
        if bar_w > 0 {
            let filled = ((view.volume() * bar_w as f32).round() as u16).min(bar_w);
            let bar_x = row.x;
            row.push("█".repeat(filled as usize), accent);
            row.push("░".repeat((bar_w - filled) as usize), dim);
            hits.volume_bar = Rect { x: bar_x, y: area.y, width: bar_w, height: 1 };
            let r = row.push(" + ".to_string(), button);
            hits.buttons.push((r, Transport::VolumeUp));
            row.push(pct, text);
        } else {
            // Without the bar, the figure sits between the two buttons.
            row.push(pct, text);
            let r = row.push(" + ".to_string(), button);
            hits.buttons.push((r, Transport::VolumeUp));
        }
    }
    f.render_widget(Paragraph::new(Line::from(row.spans)), area);
}

/// The Details view's one-line description of an audio file's state beneath
/// its format: the analysis, or why it will not play.
pub fn status_text(view: &AudioView) -> Option<String> {
    if view.unavailable() {
        Some(crate::l10n::trd("No audio output"))
    } else if view.failed() {
        Some(crate::l10n::trd("Cannot decode audio"))
    } else {
        view.progress().map(|p| format!("{} {:.0}%", crate::l10n::trd("Analyzing…"), p * 100.0))
    }
}
