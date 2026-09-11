//! Rendering of the [`DiskView`] treemap.

use super::{DiskEntry, DiskView, human_gb};
use crate::ui::graphics::{Gfx, Slot, raster};
use crate::ui::theme::Theme;
use crate::util::text::{ellipsize, pad_right};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

pub fn render(f: &mut Frame, area: Rect, dv: &mut DiskView, theme: &Theme, gfx: Option<&mut Gfx>) {
    // While the crawler is still working the title carries the readout, so the
    // treemap itself stays visible and usable from the very first frame instead
    // of being replaced by a progress bar.
    let title = if dv.scanning {
        format!(
            " {} — {}  ({} · {} {} {}) ",
            crate::l10n::trd("Disk Explorer"),
            dv.cwd.display(),
            human_gb(dv.total()),
            crate::l10n::trd("Scanning…"),
            dv.dirs_seen,
            crate::l10n::trd("directories")
        )
    } else {
        format!(
            " {} — {}  ({}) ",
            crate::l10n::trd("Disk Explorer"),
            dv.cwd.display(),
            human_gb(dv.total())
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(theme.panel_border_active).bg(theme.panel_bg))
        .title(Span::styled(
            crate::util::text::ellipsize(&title, area.width.saturating_sub(2) as usize),
            Style::default()
                .fg(theme.panel_border_active)
                .bg(theme.panel_bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(theme.panel_base());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 || inner.width < 4 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // selected-box readout
            Constraint::Min(1),    // treemap
            Constraint::Length(1), // shortcut bar
        ])
        .split(inner);
    let header = rows[0];
    let body = rows[1];
    render_footer(f, rows[2], theme);

    dv.rects.clear();
    dv.file_rects.clear();
    if dv.entries.is_empty() {
        render_header(f, header, None, None, 0, theme);
        let msg = if dv.scanning {
            crate::l10n::trd("Scanning…")
        } else {
            crate::l10n::trd("(no subdirectories)")
        };
        center_text(f, body, &msg, theme);
        return;
    }
    if dv.selected >= dv.entries.len() {
        dv.selected = dv.entries.len() - 1;
    }
    let cur = Cursor { box_i: dv.selected, file: dv.on_file() };
    let selected = dv.entries.get(cur.box_i);
    render_header(f, header, selected, cur.detail(selected), dv.total(), theme);

    let rects = treemap(&dv.entries, body);
    dv.rects = rects.clone();
    let n = dv.entries.len();
    let mut files = match gfx {
        // Graphics terminal: draw the whole treemap as one image of nested
        // "pillow" boxes, then overlay the text labels on top.
        // A full-screen pillow-shaded raster is built on the main thread, so
        // past a few megapixels (a 4K terminal) the cell boxes are the better
        // trade even where graphics are available.
        Some(g)
            if g.available() && {
                let (iw, ih) = g.px_size(body);
                iw as u64 * ih as u64 <= MAX_TREEMAP_PX
            } =>
        {
            render_treemap_graphics(f, body, &dv.entries, &rects, cur, theme, g, dv.image_epoch)
        }
        // Fallback: classic character-cell boxes.
        _ => {
            let mut rows = Vec::new();
            for (i, (entry, rect)) in dv.entries.iter().zip(rects.iter()).enumerate() {
                draw_box(f, *rect, entry, cur.at(i), i, n, theme, &mut rows);
            }
            rows
        }
    };
    dv.file_rects = std::mem::take(&mut files);
}

/// Largest treemap raster we will build on the main thread, in pixels. Beyond
/// this the character-cell boxes are drawn instead.
const MAX_TREEMAP_PX: u64 = 4_000_000;

/// Where the cursor is, as the renderer needs it: which box, and which of its
/// file rows if the list has the cursor rather than the treemap.
#[derive(Clone, Copy, Default)]
struct Cursor {
    box_i: usize,
    file: Option<usize>,
}

/// What a single box needs to know about the cursor: whether it is *the*
/// selected box, and which file row inside it the cursor is on. Every field is
/// blank for a box that isn't selected, so a box can never mistake itself for the
/// selected one — which is exactly what an earlier "narrowed cursor" shape
/// allowed, leaving every box drawn as though it were selected.
#[derive(Clone, Copy, Default)]
struct BoxCursor {
    selected: bool,
    file: Option<usize>,
}

impl Cursor {
    /// This cursor as box `i` sees it.
    fn at(&self, i: usize) -> BoxCursor {
        if i == self.box_i {
            BoxCursor { selected: true, file: self.file }
        } else {
            BoxCursor::default()
        }
    }

    /// The name and size of the file the cursor is on, for the readout line.
    /// `None` while the treemap rather than the list has the cursor.
    fn detail(&self, entry: Option<&DiskEntry>) -> Option<(String, u64)> {
        let entry = entry?;
        let file = entry.files.get(self.file?)?;
        Some((format!("{}/{}", entry.name, file.rel), file.size))
    }
}

/// Show the selected box's name and size at the top, so the selection is always
/// legible even when its box is too small to render a label.
fn render_header(
    f: &mut Frame,
    area: Rect,
    selected: Option<&DiskEntry>,
    detail: Option<(String, u64)>,
    total: u64,
    theme: &Theme,
) {
    // With the cursor inside a box, the readout names what it is on — a nested
    // subdirectory or a listed file — rather than the box, so there is always
    // something saying what Enter would descend into or Del would remove.
    if let Some((label, size)) = detail {
        let spans = vec![
            Span::styled(" ▶ ", Style::default().fg(theme.panel_border_active).bg(theme.panel_bg)),
            Span::styled(
                ellipsize(&label, area.width.saturating_sub(20) as usize),
                Style::default()
                    .fg(cursor_accent(theme))
                    .bg(theme.panel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("   {}", human_gb(size)),
                Style::default().fg(theme.panel_fg).bg(theme.panel_bg),
            ),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)).style(theme.panel_base()), area);
        return;
    }
    let spans = match selected {
        Some(e) => {
            let pct = if total > 0 { 100.0 * e.size as f32 / total as f32 } else { 0.0 };
            vec![
                Span::styled(
                    " ▶ ",
                    Style::default().fg(theme.panel_border_active).bg(theme.panel_bg),
                ),
                Span::styled(
                    ellipsize(&e.name, area.width.saturating_sub(28) as usize),
                    Style::default()
                        .fg(cursor_accent(theme))
                        .bg(theme.panel_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "   {}   {:.0}% {}",
                        human_gb(e.size),
                        pct,
                        crate::l10n::trd("of total")
                    ),
                    Style::default().fg(theme.panel_fg).bg(theme.panel_bg),
                ),
            ]
        }
        None => vec![Span::styled(
            format!(" {}", crate::l10n::trd("(nothing selected)")),
            Style::default().fg(theme.panel_fg).bg(theme.panel_bg),
        )],
    };
    f.render_widget(Paragraph::new(Line::from(spans)).style(theme.panel_base()), area);
}

fn render_footer(f: &mut Frame, area: Rect, theme: &Theme) {
    let hint =
        "←↑↓→/click move   Enter open   Tab files   Del delete   g go to dir   Bksp up   Esc back";
    // Draw as a highlighted bar (like the F-key row) so it's clearly visible.
    let line = pad_right(&format!(" {}", crate::l10n::trd(hint)), area.width as usize);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(line, theme.fkey_label))).style(theme.fkey_label),
        area,
    );
}

fn center_text(f: &mut Frame, area: Rect, text: &str, theme: &Theme) {
    let row = Rect { y: area.y + area.height / 2, height: 1, ..area };
    f.render_widget(
        Paragraph::new(Line::from(text.to_string()))
            .alignment(Alignment::Center)
            .style(theme.panel_base()),
        row,
    );
}

/// Character-cell rendering of one treemap box (the fallback used when there is
/// no terminal-graphics protocol).
#[allow(clippy::too_many_arguments)]
fn draw_box(
    f: &mut Frame,
    rect: Rect,
    entry: &DiskEntry,
    cur: BoxCursor,
    idx: usize,
    n: usize,
    theme: &Theme,
    rows: &mut Vec<(usize, usize, Rect)>,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let selected = cur.selected;
    let color = if selected {
        theme.panel_border_active
    } else if theme.truecolor {
        theme.gradient_at(idx, n.max(1))
    } else {
        theme.panel_border
    };

    // Tiny boxes: just a colored block (no room for a border or labels).
    if rect.width < 4 || rect.height < 3 {
        let style = Style::default().fg(color).bg(theme.panel_bg);
        let buf = f.buffer_mut();
        for yy in rect.y..rect.y + rect.height {
            buf.set_string(rect.x, yy, "█".repeat(rect.width as usize), style);
        }
        return;
    }

    let mut border = Style::default().fg(color).bg(theme.panel_bg);
    if selected {
        border = border.add_modifier(Modifier::BOLD);
    }
    let block = Block::default()
        .borders(Borders::ALL)
        // A doubled frame on the selected box: with every interior now filled by
        // nested tiles, a single line in a slightly different hue was too easy to
        // lose among them.
        .border_type(if selected { BorderType::Double } else { BorderType::Rounded })
        .border_style(border)
        .style(theme.panel_base());
    let bi = block.inner(rect);
    f.render_widget(block, rect);
    if bi.width == 0 || bi.height == 0 {
        return;
    }

    // The selected box wears a filled title bar in the cursor colours, the same
    // way the file panels mark their cursor row — the one cue that reads at a
    // glance across a screen full of boxes.
    let name_style = if selected {
        theme.cursor
    } else {
        Style::default().fg(theme.panel_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD)
    };
    let size_style = Style::default().fg(color).bg(theme.panel_bg);

    let name = ellipsize(&entry.name, bi.width as usize);
    let size = human_gb(entry.size);
    if bi.height < 2 {
        // One interior row: show the name only.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(name, name_style)))
                .alignment(Alignment::Center)
                .style(name_style),
            bi,
        );
        return;
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(name, name_style)))
            .alignment(Alignment::Center)
            .style(name_style),
        Rect { height: 1, ..bi },
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(ellipsize(&size, bi.width as usize), size_style)))
            .alignment(Alignment::Center),
        Rect { y: bi.y + 1, height: 1, ..bi },
    );
    // Everything below the header is the list of this box's largest files.
    if bi.height < 5 || bi.width < 16 || entry.files.is_empty() {
        return;
    }
    let list = Rect { y: bi.y + 3, height: bi.height - 3, ..bi };
    draw_file_list(f, list, &entry.files, cur.file, color, theme, idx, rows);
}

/// List the biggest files inside a box: each row is `relative/path … SIZE`,
/// the path left-aligned (dim) and the size right-aligned in the box color. The
/// row the cursor stepped onto (`file_sel`) is drawn in the cursor colors, and
/// every drawn row is recorded in `frects` so the mouse can hit it too.
#[allow(clippy::too_many_arguments)]
fn draw_file_list(
    f: &mut Frame,
    area: Rect,
    files: &[super::FileEntry],
    file_sel: Option<usize>,
    color: ratatui::style::Color,
    theme: &Theme,
    idx: usize,
    frects: &mut Vec<(usize, usize, Rect)>,
) {
    let w = area.width as usize;
    let rows = area.height as usize;
    for (k, file) in files.iter().take(rows).enumerate() {
        let row = Rect { y: area.y + k as u16, height: 1, ..area };
        frects.push((idx, k, row));
        let on = file_sel == Some(k);
        // In truecolor, fade each successive row's background a little darker so
        // the list reads as a gradient down the box.
        let bg = if on {
            theme.cursor.bg.unwrap_or(theme.panel_bg)
        } else if theme.truecolor {
            darken(theme.panel_bg, (k as f32 * 0.08).min(0.6))
        } else {
            theme.panel_bg
        };
        let path_style = if on {
            theme.cursor.add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.panel_fg).bg(bg)
        };
        let size_style = if on { theme.cursor } else { Style::default().fg(color).bg(bg) };

        let size = human_gb(file.size);
        // Reserve "<space>SIZE" on the right; the path fills the rest.
        let path_w = w.saturating_sub(size.chars().count() + 1).max(1);
        let path = ellipsize(&file.rel, path_w);
        let line = Line::from(vec![
            Span::styled(pad_right(&path, path_w), path_style),
            Span::styled(format!(" {size}"), size_style),
        ]);
        f.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), row);
    }
}

/// Render the whole treemap as a single graphics image of nested "pillow" boxes
/// — every directory a cushion-shaded box (each a distinct hue) subdivided into
/// recessed, semi-transparent sub-boxes for its largest files, with the names
/// baked into the pixels. Used whenever the terminal has a graphics protocol;
/// [`draw_box`] is the cell fallback.
#[allow(clippy::too_many_arguments)]
fn render_treemap_graphics(
    f: &mut Frame,
    body: Rect,
    entries: &[DiskEntry],
    rects: &[Rect],
    cur: Cursor,
    theme: &Theme,
    g: &mut Gfx,
    epoch: u64,
) -> Vec<(usize, usize, Rect)> {
    let (cw, ch) = g.cell();
    let (iw, ih) = g.px_size(body);
    let accent = raster::rgb(theme.panel_border_active);

    // The treemap image is expensive to build (per-pixel pillow shading + baked
    // labels) but stays identical across frames unless its inputs change. Compute
    // a cheap signature of those inputs so `draw_cached` rebuilds only on change —
    // otherwise a burst of redraws (e.g. after the terminal regains focus) would
    // rebuild the full-screen image on the main thread and peg a core for seconds.
    //
    // While a crawl is running the sizes change continuously, so the box sizes
    // are deliberately *not* part of the signature: `epoch` stands in for them
    // and only advances a few times a second. Without that the image would be
    // rebuilt on every progress update, which is exactly the core-pegging this
    // cache exists to prevent.
    let sig = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (iw, ih, cw, ch, cur.box_i, cur.file, epoch).hash(&mut h);
        raster::rgb(theme.panel_bg).hash(&mut h);
        accent.hash(&mut h);
        raster::rgb(cursor_accent(theme)).hash(&mut h);
        for (entry, rect) in entries.iter().zip(rects) {
            entry.name.hash(&mut h);
            (rect.x, rect.y, rect.width, rect.height).hash(&mut h);
        }
        h.finish()
    };

    let build = move || build_treemap_image(body, cw, ch, entries, rects, cur, theme, accent);
    g.draw_cached(f, body, Slot::Treemap(0), sig, build);
    // The image is cached, but the cursor still needs to know where each file
    // row landed, so the (cheap) layout is repeated in cell coordinates.
    file_cell_rows(cw, ch, entries, rects)
}

/// The most file rows any one box will bake, bounding both the layout work and
/// the cache signature. Taller boxes simply stop listing past this.
const TOP_ROWS: usize = 32;

/// Where each box's file rows land, in terminal cells — the mouse and the arrow
/// cursor work in cells, while the image is laid out in pixels. Mirrors the
/// layout [`build_treemap_image`] bakes.
fn file_cell_rows(
    cw: u32,
    ch: u32,
    entries: &[DiskEntry],
    rects: &[Rect],
) -> Vec<(usize, usize, Rect)> {
    let mut out = Vec::new();
    for (i, (entry, rect)) in entries.iter().zip(rects).enumerate() {
        let (bw, bh) = (rect.width as u32 * cw, rect.height as u32 * ch);
        if bw < 3 || bh < 3 {
            continue;
        }
        let lay = box_layout(entry, bw, bh, ch);
        // Rows are a whole cell tall by construction, so each maps to exactly
        // one terminal row.
        for k in 0..lay.rows {
            let y = rect.y + ((lay.files_y as u32 + 1) / ch) as u16 + k as u16;
            let w = (bw / cw) as u16;
            if w == 0 || y >= rect.y + rect.height {
                continue;
            }
            out.push((i, k, Rect { x: rect.x, y, width: w, height: 1 }));
        }
    }
    out
}

/// How one box's interior divides, in box-local pixels: where its file list
/// starts and how many rows of it fit. Shared by the image builder and the cell
/// mirror above so the two can never disagree about where a row was drawn.
struct BoxLayout {
    /// Top of the file list.
    files_y: f64,
    /// How many file rows fit.
    rows: usize,
    /// Height of one row: exactly one terminal cell, so the baked rows line up
    /// with the cell rectangles [`file_cell_rows`] derives for the mouse.
    row_h: f64,
    /// Font size for the file rows.
    row_px: f32,
}

/// Lay out one box: the name/size header, then its largest files as one-row-tall
/// list rows filling the rest of the interior.
fn box_layout(entry: &DiskEntry, bw: u32, bh: u32, ch: u32) -> BoxLayout {
    let empty = BoxLayout { files_y: 0.0, rows: 0, row_h: 0.0, row_px: 0.0 };
    let (inner_w, inner_h) = (bw.saturating_sub(2) as f64, bh.saturating_sub(2) as f64);
    let (name_px, size_px) = label_px(bw, bh);
    let header_px = header_layout(bh, name_px, size_px).0 as f64;
    let region_h = inner_h - header_px;
    if inner_w <= 10.0 || region_h <= 10.0 || ch == 0 {
        return empty;
    }
    // Work in whole rows so the baked list lines up with the cell grid.
    let rows = (region_h / ch as f64).floor() as usize;
    BoxLayout {
        files_y: header_px,
        rows: rows.min(entry.files.len()).min(TOP_ROWS),
        row_h: ch as f64,
        row_px: row_px(ch),
    }
}

/// Font size for one baked file row: the largest that still fits inside a
/// terminal row, so the list lines up with the cell grid the cursor works in.
fn row_px(ch: u32) -> f32 {
    [17.0f32, 15.0, 13.0, 11.0, 9.0]
        .into_iter()
        .find(|px| raster::text_height(*px) <= ch)
        .unwrap_or(9.0)
}

/// Build the full treemap image: one nested "pillow" box per entry with baked
/// labels. Split out so [`render_treemap_graphics`] can skip it entirely when the
/// cached signature is unchanged.
#[allow(clippy::too_many_arguments)]
fn build_treemap_image(
    body: Rect,
    cw: u32,
    ch: u32,
    entries: &[DiskEntry],
    rects: &[Rect],
    cur: Cursor,
    theme: &Theme,
    accent: raster::Rgb,
) -> image::RgbaImage {
    let (iw, ih) = (body.width as u32 * cw, body.height as u32 * ch);
    let mut img = raster::canvas(iw, ih, raster::rgb(theme.panel_bg));

    for (i, (entry, rect)) in entries.iter().zip(rects).enumerate() {
        let ox = (rect.x.saturating_sub(body.x)) as u32 * cw;
        let oy = (rect.y.saturating_sub(body.y)) as u32 * ch;
        let (bw, bh) = (rect.width as u32 * cw, rect.height as u32 * ch);
        if bw < 3 || bh < 3 {
            continue;
        }
        let here = cur.at(i);
        let selected = here.selected;
        // A distinct hue per box (golden-angle spread); the selected box keeps the
        // accent color and gets a bright border. Both stay stable frame-to-frame
        // so the encoded image is cached rather than re-transmitted.
        let fill = if selected { accent } else { raster::hsv(i as f64 * 137.508, 0.55, 0.72) };
        let lay = box_layout(entry, bw, bh, ch);
        let border = selected.then(|| raster::rgb(cursor_accent(theme)));
        raster::pillow_into(&mut img, ox, oy, bw, bh, fill, border);
        // Bake the labels into the pixels so they survive every graphics protocol
        // (cell text drawn over an image is painted over by Kitty/Sixel).
        bake_labels(&mut img, (ox, oy, bw, bh), entry, fill, &lay, here, theme);
    }
    img
}

/// Font pixel sizes (name, size) for a box's labels, chosen from its pixel size —
/// bigger boxes get bigger, more legible anti-aliased text.
fn label_px(bw: u32, bh: u32) -> (f32, f32) {
    let name = if bw >= 300 && bh >= 170 {
        30.0
    } else if bw >= 72 && bh >= 40 {
        20.0
    } else {
        14.0
    };
    let size = if bw >= 110 && bh >= 58 { 17.0 } else { 12.0 };
    (name, size)
}

/// The header height (pixels reserved above the sub-boxes) for a box, and whether
/// the size line fits under the name. Keeps the sub-treemap and [`bake_labels`]
/// in agreement about where the header ends.
fn header_layout(bh: u32, name_px: f32, size_px: f32) -> (u32, bool) {
    let name_h = raster::text_height(name_px);
    if bh < name_h + 12 {
        return (0, false);
    }
    let size_h = raster::text_height(size_px);
    let with_size = bh >= name_h + size_h + 24;
    let px = 3 + name_h + if with_size { 3 + size_h } else { 0 } + 4;
    (px, with_size)
}

/// Bake a box's labels into the treemap image: the directory name + size near
/// the top, and the file list underneath. Text is baked as pixels (not cells) so
/// it survives every graphics protocol; the font scale grows with the box so
/// labels stay readable.
#[allow(clippy::too_many_arguments)]
fn bake_labels(
    img: &mut image::RgbaImage,
    (ox, oy, bw, bh): (u32, u32, u32, u32),
    entry: &DiskEntry,
    fill: raster::Rgb,
    lay: &BoxLayout,
    cur: BoxCursor,
    theme: &Theme,
) {
    let selected = cur.selected;
    let accent = raster::rgb(cursor_accent(theme));
    let (name_px, size_px) = label_px(bw, bh);
    let (header_px, with_size) = header_layout(bh, name_px, size_px);
    // The selected box wears a filled title bar in the cursor colour, the same
    // way the file panels mark their cursor row. A rim alone was too easy to lose
    // now that every box's interior is filled with nested tiles.
    let plate = if selected {
        raster::fill_rect(img, ox + 1, oy + 1, bw.saturating_sub(2), header_px, accent);
        accent
    } else {
        raster::over(fill, (0, 0, 0), 0.6)
    };
    let name_fg = if selected {
        raster::rgb(theme.cursor.fg.unwrap_or(theme.panel_bg))
    } else {
        (250, 250, 250)
    };
    // How many characters of `text` fit in `width_px` at font size `px`.
    let fit = |width: u32, px: f32| {
        (width.saturating_sub(4) as f32 / raster::char_advance(px)).max(1.0) as usize
    };

    // Directory name, centered near the top.
    let name = ellipsize(&entry.name, fit(bw, name_px));
    let nx = ox as i32 + (bw as i32 - raster::text_width(&name, name_px) as i32) / 2;
    raster::draw_text(img, nx, oy as i32 + 3, &name, name_fg, Some(plate), name_px);
    // Size, centered under the name.
    if with_size {
        let size = ellipsize(&human_gb(entry.size), fit(bw, size_px));
        let sx = ox as i32 + (bw as i32 - raster::text_width(&size, size_px) as i32) / 2;
        let sy = oy as i32 + 3 + raster::text_height(name_px) as i32 + 3;
        let fg = if selected { name_fg } else { (230, 230, 230) };
        raster::draw_text(img, sx, sy, &size, fg, Some(plate), size_px);
    }

    bake_file_rows(img, (ox, oy, bw, bh), entry, fill, lay, cur, theme);
}

/// Bake the box's file list: one row per file, `relative/path` on the left and
/// its size right-aligned, over a plate dark enough to read against the pillow.
/// The row the cursor is on is filled with the cursor accent instead, which is
/// what tells you which file `Del` would remove.
fn bake_file_rows(
    img: &mut image::RgbaImage,
    (ox, oy, bw, bh): (u32, u32, u32, u32),
    entry: &DiskEntry,
    fill: raster::Rgb,
    lay: &BoxLayout,
    cur: BoxCursor,
    theme: &Theme,
) {
    if lay.rows == 0 {
        return;
    }
    let px = lay.row_px;
    let row_h = lay.row_h.max(1.0);
    let plate = raster::over(fill, (0, 0, 0), 0.62);
    let accent = raster::rgb(cursor_accent(theme));
    let advance = raster::char_advance(px).max(1.0);
    let width = bw.saturating_sub(6);
    let cols = (width as f32 / advance).max(1.0) as usize;

    for k in 0..lay.rows {
        let Some(file) = entry.files.get(k) else { break };
        let top = oy as f64 + 1.0 + lay.files_y + k as f64 * row_h;
        if top + row_h > (oy + bh) as f64 {
            break;
        }
        let on = cur.file == Some(k);
        // Fill the row so the list reads as a panel rather than loose text, and
        // so the cursor's row is unmistakable.
        let bg = if on { accent } else { plate };
        for y in top as u32..(top + row_h) as u32 {
            for x in (ox + 1)..(ox + bw).saturating_sub(1) {
                let p = img.get_pixel(x.min(img.width() - 1), y.min(img.height() - 1)).0;
                let c = raster::over((p[0], p[1], p[2]), bg, if on { 0.92 } else { 0.72 });
                if x < img.width() && y < img.height() {
                    img.put_pixel(x, y, image::Rgba([c.0, c.1, c.2, 255]));
                }
            }
        }
        // `path … SIZE`: reserve the size on the right, the path takes the rest.
        let size = human_gb(file.size);
        let path_cols = cols.saturating_sub(size.chars().count() + 1).max(1);
        let path = ellipsize(&file.rel, path_cols);
        let fg = if on { (255, 255, 255) } else { (232, 232, 232) };
        let ty = (top + (row_h - raster::text_height(px) as f64).max(0.0) / 2.0) as i32;
        raster::draw_text(img, ox as i32 + 3, ty, &path, fg, None, px);
        let sw = raster::text_width(&size, px) as i32;
        raster::draw_text(img, (ox + bw) as i32 - 3 - sw, ty, &size, fg, None, px);
    }
}

/// The color marking what the cursor is on in the treemap.
///
/// Not `theme.cursor_fg`: that is the cursor row's *text* color, picked to be
/// legible on `cursor_bg` (black on the default theme, and on derived themes
/// literally `panel_bg`), so drawing it on the panel background rendered the
/// highlight near-invisible. The cursor's background color is the accent the
/// rest of the app trains the eye on, and it is bright by construction.
fn cursor_accent(theme: &Theme) -> ratatui::style::Color {
    theme.cursor.bg.unwrap_or(theme.panel_border_active)
}

/// Scale an RGB color toward black by `t` (0 = unchanged, 1 = black).
fn darken(c: ratatui::style::Color, t: f32) -> ratatui::style::Color {
    use ratatui::style::Color;
    if let Color::Rgb(r, g, b) = c {
        let f = |x: u8| (x as f32 * (1.0 - t)).round().clamp(0.0, 255.0) as u8;
        Color::Rgb(f(r), f(g), f(b))
    } else {
        c
    }
}

// ---------------------------------------------------------------------------
// Treemap layout
// ---------------------------------------------------------------------------

/// Lay out `entries` (already sorted largest-first) as a treemap filling `area`,
/// returning one integer rect per entry in the same order. The squarified layout
/// itself lives in [`crate::util::treemap`], shared with the 3D space view so
/// both draw the same floor plan.
fn treemap(entries: &[DiskEntry], area: Rect) -> Vec<Rect> {
    let n = entries.len();
    if n == 0 || area.width == 0 || area.height == 0 {
        return vec![Rect { width: 0, height: 0, ..area }; n];
    }
    let sizes: Vec<u64> = entries.iter().map(|e| e.size).collect();
    let areas = crate::util::treemap::size_areas(&sizes, area.width as f64 * area.height as f64);
    let frects = crate::util::treemap::squarify(
        &areas,
        area.x as f64,
        area.y as f64,
        area.width as f64,
        area.height as f64,
    );
    let x_max = (area.x + area.width) as f64;
    let y_max = (area.y + area.height) as f64;
    frects
        .iter()
        .map(|r| {
            let x0 = r.x.round().clamp(area.x as f64, x_max);
            let y0 = r.y.round().clamp(area.y as f64, y_max);
            let x1 = (r.x + r.w).round().clamp(x0, x_max);
            let y1 = (r.y + r.h).round().clamp(y0, y_max);
            Rect { x: x0 as u16, y: y0 as u16, width: (x1 - x0) as u16, height: (y1 - y0) as u16 }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, size: u64) -> DiskEntry {
        DiskEntry { name: name.into(), size, files: vec![] }
    }

    /// The treemap marks the cursor with a color drawn *on the panel background*,
    /// so it has to contrast with it. It used to use `cursor_fg`, which is the
    /// cursor row's text color — black on the default theme, and on ANSI-derived
    /// themes literally `panel_bg` — so the highlight was all but invisible.
    #[test]
    fn cursor_accent_contrasts_with_the_panel_background() {
        let theme = crate::ui::theme::Theme::mc();
        let accent = cursor_accent(&theme);
        assert_ne!(accent, theme.panel_bg, "the accent would vanish into the panel");
        assert_ne!(accent, theme.cursor_fg, "cursor_fg is cursor *text*, not an accent color");
    }

    /// A scan in progress no longer hides the treemap behind a progress bar:
    /// whatever has been sized so far is drawn and can be navigated, and the
    /// title carries the readout instead.
    #[test]
    fn scanning_still_draws_its_boxes() {
        use crate::disk::{DiskEntry, DiskView};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = true;
        dv.entries = vec![
            DiskEntry { name: "alpha".into(), size: 8_000_000, files: Vec::new() },
            DiskEntry { name: "beta".into(), size: 2_000_000, files: Vec::new() },
        ];
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        t.draw(|f| render(f, f.area(), &mut dv, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
        }
        assert!(s.contains("alpha"), "partial results are drawn while scanning");
        assert!(s.contains("beta"), "every sized box is drawn");
        assert!(!dv.rects.is_empty(), "and stays navigable (rects recorded)");
        assert!(s.contains("Scanning"), "the title says a scan is still running");
    }

    /// With nothing sized yet there is still no progress bar — just a label, so
    /// the layout doesn't jump when the first boxes arrive.
    #[test]
    fn an_empty_scan_shows_a_label_not_a_bar() {
        use crate::disk::DiskView;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = true;
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        t.draw(|f| render(f, f.area(), &mut dv, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
        }
        assert!(!s.contains('░'), "no progress bar track is drawn any more");
    }

    /// The renderer records where every file row landed (so the mouse and the
    /// ↑/↓ cursor can reach it) and highlights the one the cursor is on.
    #[test]
    fn file_rows_are_recorded_and_the_selected_one_is_highlighted() {
        use crate::disk::{DiskView, FileEntry, Focus};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = false;
        dv.entries = vec![DiskEntry {
            name: "project".into(),
            size: 9_000_000,
            files: vec![
                FileEntry { rel: "target/huge.bin".into(), size: 5_000_000 },
                FileEntry { rel: "assets/movie.mp4".into(), size: 3_000_000 },
            ],
        }];
        dv.focus = Focus::Files;
        dv.file_sel = 1;
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| render(f, f.area(), &mut dv, &theme, None)).unwrap();

        assert!(dv.file_rects.len() >= 2, "both file rows were recorded");
        assert_eq!(dv.file_rects[0].0, 0, "recorded against the box that drew them");
        assert_eq!(dv.file_rects[1].1, 1, "in file order");
        // The selected row is painted in the cursor colors.
        let row = dv.file_rects[1].2;
        let b = t.backend().buffer();
        assert_eq!(b[(row.x, row.y)].style().bg, theme.cursor.bg, "cursor row highlighted");
        let other = dv.file_rects[0].2;
        assert_ne!(b[(other.x, other.y)].style().bg, theme.cursor.bg, "the other row is not");

        // The header names the selected file rather than the directory.
        let mut header = String::new();
        for x in 0..b.area.width {
            header.push_str(b[(x, 1)].symbol());
        }
        assert!(header.contains("project/assets/movie.mp4"), "header: {header:?}");
    }

    #[test]
    fn big_box_lists_its_largest_files() {
        use crate::disk::{DiskView, FileEntry};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = false;
        // A single entry fills the whole treemap → its box is large.
        dv.entries = vec![DiskEntry {
            name: "project".into(),
            size: 9_000_000,
            files: vec![
                FileEntry { rel: "target/huge.bin".into(), size: 5_000_000 },
                FileEntry { rel: "assets/movie.mp4".into(), size: 3_000_000 },
            ],
        }];
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| render(f, f.area(), &mut dv, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
        }
        assert!(s.contains("target/huge.bin"), "largest file path shown");
        assert!(s.contains("assets/movie.mp4"), "second file path shown");
        assert!(s.contains("4.8 MB"), "file size shown");
    }

    #[test]
    fn big_box_pillow_graphics_path_renders_without_panic() {
        use crate::disk::{DiskView, FileEntry};
        use crate::ui::graphics::Gfx;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = false;
        dv.entries = vec![DiskEntry {
            name: "project".into(),
            size: 9_000_000,
            files: vec![
                FileEntry { rel: "target/huge.bin".into(), size: 5_000_000 },
                FileEntry { rel: "assets/movie.mp4".into(), size: 3_000_000 },
                FileEntry { rel: "docs/manual.pdf".into(), size: 1_000_000 },
            ],
        }];
        let theme = crate::ui::theme::Theme::mc();
        let mut gfx = Gfx::test_halfblocks();
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        // Exercises draw_box_pillow: squarify sub-layout + pillow_box + g.draw.
        t.draw(|f| render(f, f.area(), &mut dv, &theme, Some(&mut gfx))).unwrap();
        // The whole treemap is one graphics image (labels are baked into pixels,
        // so they render as image cells, not readable text). Assert it painted.
        let b = t.backend().buffer();
        let image_cells = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| matches!(b[(x, y)].symbol(), "\u{2580}" | "\u{2584}"))
            .count();
        assert!(image_cells > 100, "the graphical pillow treemap should paint many image cells");
        // The file sub-boxes are baked into the image, so the cursor needs the
        // same layout mirrored into cells to know where they are.
        assert!(!dv.file_rects.is_empty(), "file sub-boxes recorded in cell coordinates");
        assert!(
            dv.file_rects.iter().all(|(e, _, r)| *e == 0
                && r.x >= dv.rects[0].x
                && r.x + r.width <= dv.rects[0].x + dv.rects[0].width),
            "every file rect sits inside its box: {:?}",
            dv.file_rects
        );
    }

    #[test]
    fn renders_treemap_with_boxes_and_footer() {
        use crate::disk::DiskView;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = false;
        dv.entries = vec![e("big", 9_000_000), e("small", 100_000)];
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| render(f, f.area(), &mut dv, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
        }
        assert!(s.contains("Disk Explorer"), "title");
        assert!(s.contains("big"), "largest box labeled");
        assert!(s.contains("click") && s.contains("Tab files"), "footer guidance (incl. mouse)");
        assert!(s.contains("▶") && s.contains("of total"), "selected-box header");
        assert_eq!(dv.rects.len(), 2, "geometry recorded for navigation");
    }

    /// A box lists its largest files and records a rectangle per row, so the
    /// cursor and the mouse can reach them.
    #[test]
    fn text_mode_lists_files_and_records_their_rows() {
        use crate::disk::{DiskView, FileEntry};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = false;
        dv.entries = vec![DiskEntry {
            name: "project".into(),
            size: 9_000_000,
            files: vec![
                FileEntry { rel: "target/huge.bin".into(), size: 5_000_000 },
                FileEntry { rel: "assets/movie.mp4".into(), size: 3_000_000 },
            ],
        }];
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| render(f, f.area(), &mut dv, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
        }
        assert!(s.contains("target/huge.bin"), "the largest file is listed");
        assert_eq!(dv.file_rects.len(), 2, "one rectangle per drawn row");
    }

    /// The cell rectangles the cursor and mouse work in must agree with the
    /// pixels the image bakes: every row lands inside its own box and is exactly
    /// one terminal row tall, so a click hits exactly one file.
    #[test]
    fn cell_rows_mirror_the_baked_layout() {
        use crate::disk::FileEntry;
        let entries = vec![DiskEntry {
            name: "project".into(),
            size: 9_000_000,
            files: (0..6)
                .map(|i| FileEntry { rel: format!("f{i}.bin"), size: 1_000_000 - i * 1000 })
                .collect(),
        }];
        let area = Rect { x: 2, y: 1, width: 60, height: 26 };
        let rects = treemap(&entries, area);
        let rows = file_cell_rows(8, 16, &entries, &rects);

        assert!(!rows.is_empty(), "file rows were laid out");
        for (i, _, r) in &rows {
            let b = rects[*i];
            assert!(r.x >= b.x && r.x + r.width <= b.x + b.width, "{r:?} inside {b:?}");
            assert!(r.y >= b.y && r.y + r.height <= b.y + b.height, "{r:?} inside {b:?}");
            assert_eq!(r.height, 1, "a row is one terminal row, so a click hits one file");
        }
    }

    /// Every arrow hop on a real squarified layout has to reverse: step one way,
    /// step back, and you are where you started. Centre-to-centre scoring failed
    /// this — the size of the box you happened to be standing on skewed the
    /// score — so a run of → and a run of ← walked different boxes.
    #[test]
    fn every_arrow_hop_reverses() {
        use crate::disk::DiskView;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = false;
        // Sizes spread widely enough that squarify produces boxes of very
        // different shapes, which is where the asymmetry showed up.
        dv.entries = [12_400, 6_000, 3_100, 2_000, 1_200, 800, 640, 300]
            .iter()
            .enumerate()
            .map(|(i, s)| e(&format!("d{i}"), *s))
            .collect();
        dv.rects = treemap(&dv.entries, Rect { x: 0, y: 0, width: 94, height: 22 });

        let opposite = |k| match k {
            KeyCode::Left => KeyCode::Right,
            KeyCode::Right => KeyCode::Left,
            KeyCode::Up => KeyCode::Down,
            _ => KeyCode::Up,
        };
        for start in 0..dv.entries.len() {
            for dir in [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::Down] {
                dv.reset_cursor();
                dv.selected = start;
                dv.handle_key(KeyEvent::new(dir, KeyModifiers::NONE));
                let moved = dv.selected;
                if moved == start {
                    continue; // nothing that way; nothing to reverse
                }
                dv.handle_key(KeyEvent::new(opposite(dir), KeyModifiers::NONE));
                assert_eq!(
                    dv.selected,
                    start,
                    "{start} --{dir:?}--> {moved} did not come back with {:?}",
                    opposite(dir)
                );
            }
        }
    }

    /// Only the selected box may report itself as selected. A narrowing bug once
    /// handed every box a cursor claiming to be on it, so they all took the
    /// accent fill and the selection rim and nothing stood out at all.
    #[test]
    fn cursor_marks_only_the_selected_box() {
        let cur = Cursor { box_i: 2, file: Some(1) };
        let on = cur.at(2);
        assert!(on.selected, "the selected box knows it is selected");
        assert_eq!(on.file, Some(1), "and carries the file cursor");
        for i in [0, 1, 3] {
            let off = cur.at(i);
            assert!(!off.selected, "box {i} is not the selected one");
            assert_eq!(off.file, None, "box {i} carries no file cursor");
        }
    }

    /// End to end: exactly one box on screen wears the cursor's title bar.
    #[test]
    fn only_one_box_is_drawn_highlighted() {
        use crate::disk::DiskView;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut dv = DiskView::new(std::path::PathBuf::from("/tmp"));
        dv.scanning = false;
        dv.entries = vec![e("big", 9_000_000), e("mid", 4_000_000), e("small", 1_000_000)];
        dv.selected = 1;
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(90, 24)).unwrap();
        t.draw(|f| render(f, f.area(), &mut dv, &theme, None)).unwrap();
        let b = t.backend().buffer();
        // A box's title row is the first interior row; the selected one is
        // painted in the cursor colours across its whole width.
        let lit: Vec<usize> = dv
            .rects
            .iter()
            .enumerate()
            .filter(|(_, r)| r.width > 2 && r.height > 2)
            .filter(|(_, r)| b[(r.x + 1, r.y + 1)].style().bg == theme.cursor.bg)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(lit, vec![1], "only the selected box wears the cursor title bar");
    }

    #[test]
    fn treemap_covers_area_and_orders_by_input() {
        let entries = vec![e("a", 800), e("b", 150), e("c", 50)];
        let area = Rect { x: 0, y: 0, width: 40, height: 20 };
        let rects = treemap(&entries, area);
        assert_eq!(rects.len(), 3);
        // Largest entry gets the largest box.
        let areas: Vec<u32> = rects.iter().map(|r| r.width as u32 * r.height as u32).collect();
        assert!(areas[0] >= areas[1] && areas[1] >= areas[2], "areas: {areas:?}");
        // Every box lies within the area.
        for r in &rects {
            assert!(r.x + r.width <= area.x + area.width);
            assert!(r.y + r.height <= area.y + area.height);
        }
    }
}
