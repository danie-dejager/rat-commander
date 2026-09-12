//! Drawing the Activity log: one row per event (or folded burst), newest at the
//! top, and a status line with the event rate over the last minute.

use super::{ActivityLog, Entry};
use crate::app::state::watch::FsKind;
use crate::ui::theme::Theme;
use crate::util::text::{ellipsize, pad_right};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Sparkline};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

/// The glyph and colour an event kind is drawn with.
pub fn mark(kind: FsKind, theme: &Theme) -> (&'static str, Color) {
    match kind {
        FsKind::Create => ("+", theme.exec_fg),
        FsKind::Modify => ("~", theme.hotkey_fg),
        FsKind::Written => ("✓", theme.symlink_fg),
        FsKind::Remove => ("-", theme.error_fg),
        FsKind::Rename => ("→", theme.marked_fg),
        FsKind::MovedAway => ("↑", theme.error_fg),
        FsKind::MovedHere => ("↓", theme.exec_fg),
    }
}

/// How long ago, compactly: `now`, `12s`, `4m`, `3h`.
pub fn age(at: Instant, now: Instant) -> String {
    let secs = now.saturating_duration_since(at).as_secs();
    match secs {
        0 => "now".to_string(),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// Draw the rows into `area`, keeping the cursor on screen. Returns the number
/// of rows a page holds.
pub fn render(
    f: &mut Frame,
    area: Rect,
    log: &mut ActivityLog,
    active: bool,
    theme: &Theme,
    now: Instant,
) -> usize {
    let bg = theme.panel_bg;
    let rows = area.height as usize;
    let width = area.width as usize;
    if rows == 0 || width < 8 {
        return rows.max(1);
    }
    let dim = Style::default().fg(theme.panel_border).bg(bg);
    if log.root.is_none() {
        let msg = crate::l10n::trd("The Activity log needs a local directory");
        f.render_widget(Paragraph::new(Line::from(Span::styled(msg, dim))), area);
        return rows;
    }
    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    let mut body = area;
    if log.partial {
        // Said up front, since what is missing would otherwise just look quiet.
        let msg =
            crate::l10n::trd("Too many subdirectories to watch: only this directory is logged");
        lines.push(Line::from(Span::styled(
            ellipsize(&msg, width),
            Style::default().fg(theme.error_fg).bg(bg),
        )));
        body.height = body.height.saturating_sub(1);
    }
    let visible_rows = body.height as usize;
    let total = log.visible_len();
    if total == 0 {
        let msg = if log.filter.is_some() {
            crate::l10n::trd("Nothing matching the filter yet")
        } else {
            crate::l10n::trd("Waiting for something to change…")
        };
        lines.push(Line::from(Span::styled(msg, dim.add_modifier(Modifier::ITALIC))));
        f.render_widget(Paragraph::new(lines).style(Style::default().bg(bg)), area);
        return visible_rows.max(1);
    }
    log.cursor = log.cursor.min(total - 1);
    log.offset = crate::util::scroll::scroll_to_visible(log.offset, log.cursor, visible_rows);
    let cursor_style = if active { theme.cursor } else { theme.cursor_inactive };
    for (i, e) in log.visible().enumerate().skip(log.offset).take(visible_rows) {
        let selected = i == log.cursor;
        lines.push(row(e, width, selected.then_some(cursor_style), theme, now));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(bg)), area);
    visible_rows.max(1)
}

/// One row: `  4s + src/main.rs ×12`, the directory part dimmed.
fn row(
    e: &Entry,
    width: usize,
    cursor: Option<Style>,
    theme: &Theme,
    now: Instant,
) -> Line<'static> {
    let bg = theme.panel_bg;
    let (glyph, color) = mark(e.kind, theme);
    let base = |fg: Color| match cursor {
        Some(c) => c,
        None => Style::default().fg(fg).bg(bg),
    };
    let when = format!("{:>4} ", age(e.at, now));
    let count = if e.count > 1 { format!(" ×{}", e.count) } else { String::new() };
    let (dirs, name) = split_path(e);
    // The name is what matters: it keeps its room, and the directories give
    // theirs up from the left.
    let room = width.saturating_sub(when.width() + 2 + count.width());
    let name = ellipsize(&name, room);
    let dirs = shorten_left(&dirs, room - name.width());
    let mut spans = vec![
        Span::styled(when, base(theme.panel_border)),
        Span::styled(glyph.to_string(), base(color).add_modifier(Modifier::BOLD)),
        Span::styled(" ", base(theme.panel_fg)),
        Span::styled(dirs.to_string(), base(theme.panel_border)),
        Span::styled(name.to_string(), base(theme.panel_fg)),
    ];
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    let pad = width.saturating_sub(used + count.width());
    spans.push(Span::styled(" ".repeat(pad), base(theme.panel_fg)));
    spans.push(Span::styled(count, base(theme.panel_border)));
    Line::from(spans)
}

/// A row's path as dimmed directories and the bright name after them. A rename
/// within one directory reads `dir/old → new`; across directories, both paths.
fn split_path(e: &Entry) -> (String, String) {
    let parent = |p: &std::path::Path| match p.parent().map(|d| d.display().to_string()) {
        Some(d) if !d.is_empty() => format!("{d}/"),
        _ => String::new(),
    };
    let name = |p: &std::path::Path| {
        p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
    };
    match &e.to {
        Some(to) if to.parent() == e.rel.parent() => {
            (parent(&e.rel), format!("{} → {}", name(&e.rel), name(to)))
        }
        Some(to) => (String::new(), format!("{} → {}", e.rel.display(), to.display())),
        None => (parent(&e.rel), format!("{}{}", name(&e.rel), if e.dir { "/" } else { "" })),
    }
}

/// `text` cut to `max` cells by dropping its start: `…/deps/`.
fn shorten_left(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    if max < 2 {
        return String::new();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1; // the ellipsis
    for ch in text.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > max {
            break;
        }
        used += w;
        kept.push(ch);
    }
    let tail: String = kept.into_iter().rev().collect();
    // Start at a whole directory where one fits: `…/deps/`, not `…ps/`.
    match tail.find('/') {
        Some(i) if i + 1 < tail.len() => format!("…/{}", &tail[i + 1..]),
        _ => format!("…{tail}"),
    }
}

/// The status line: events per second now, a sparkline of the last minute, and
/// whether the log is paused.
pub fn render_status(f: &mut Frame, area: Rect, log: &ActivityLog, theme: &Theme) {
    let style = Style::default().fg(theme.panel_border_active).bg(theme.panel_bg);
    let rate = log.rate();
    // The last complete second: the one in progress is still filling.
    let per_sec = rate.iter().rev().nth(1).or(rate.last()).copied().unwrap_or(0);
    let mut text = format!(" {per_sec}/s ");
    if log.paused {
        text.push_str(&format!("[{} +{}] ", crate::l10n::trd("Paused"), log.held()));
    }
    let text_w = (text.width() as u16).min(area.width);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(pad_right(&text, text_w as usize), style))),
        Rect { width: text_w, ..area },
    );
    let spark = Rect { x: area.x + text_w, width: area.width - text_w, ..area };
    if spark.width > 0 {
        // Right-aligned, newest at the right edge.
        let take = spark.width as usize;
        let data: Vec<u64> = rate.iter().rev().take(take).rev().copied().collect();
        let spark = Rect {
            x: spark.x + spark.width - data.len() as u16,
            width: data.len() as u16,
            ..spark
        };
        f.render_widget(
            Sparkline::default()
                .data(&data)
                .style(Style::default().fg(theme.exec_fg).bg(theme.panel_bg)),
            spark,
        );
    }
}
