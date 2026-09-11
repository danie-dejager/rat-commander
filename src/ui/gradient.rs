//! Painting the theme's per-element gradients onto a finished frame.
//!
//! A theme may attach a [`GradientSpec`](super::theme::GradientSpec) to any of
//! the elements in [`GradRole`] — panel and dialog backgrounds, frames, cursor
//! bars, menus, inputs and buttons. Ratatui colors a whole `Block` (border and
//! background alike) with a single [`Style`](ratatui::style::Style), so rather
//! than threading a per-cell color through every widget in the program, the
//! ramps are painted **after** the frame is drawn: [`apply`] finds the cells
//! still carrying an element's flat color, groups them into connected regions,
//! and repaints each region with the ramp spread across its own bounds. A
//! dialog, a button and a cursor bar therefore each get a whole gradient rather
//! than a slice of one screen-wide ramp.
//!
//! Identifying an element by the color it painted is what keeps this cheap, and
//! four rules keep it honest:
//!
//! * the bars claim only the rows they were drawn on ([`mark_bar`]), so a
//!   cursor gradient can't spill onto a menu bar that shares its teal;
//! * cells a renderer already ramped itself are claimed ([`mark_painted`]) and
//!   left alone, so a moving shade that happens to land exactly on the
//!   element's flat color is not mistaken for an unpainted one;
//! * a foreground (frame) gradient only repaints box-drawing glyphs, never text
//!   that happens to use the border color;
//! * elements a theme paints in *the same* color are indistinguishable on
//!   screen and so share a gradient — giving them distinct colors separates
//!   them.
//!
//! The bars and the panel cursor bar paint their own gradient directly (they
//! already build a style per cell) and claim the cells they painted, so this
//! pass skips them and covers the same elements only where they are drawn flat.

use super::theme::{GRAD_ROLES, GradPaint, GradRole, GradZone, Theme};
use ratatui::Frame;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::cell::RefCell;

thread_local! {
    /// The bar rows drawn so far this frame (see [`mark_bar`]).
    static BARS: RefCell<Vec<(GradZone, Rect)>> = const { RefCell::new(Vec::new()) };
    /// The areas already ramped by their own renderer (see [`mark_painted`]).
    static PAINTED: RefCell<Vec<Rect>> = const { RefCell::new(Vec::new()) };
}

/// Forget the claims of the previous frame. Called once at the top of the root
/// [`draw`](super::draw).
pub fn reset() {
    BARS.with(|b| b.borrow_mut().clear());
    PAINTED.with(|p| p.borrow_mut().clear());
}

/// Record where a bar was drawn, so [`apply`] can tell that row apart from the
/// body. Called by the menu-bar and F-key-bar renderers, wherever they are used
/// (the panels, the editor, the viewer).
pub fn mark_bar(zone: GradZone, area: Rect) {
    if area.width > 0 && area.height > 0 {
        BARS.with(|b| b.borrow_mut().push((zone, area)));
    }
}

/// Claim `area` as already carrying its element's gradient, painted cell by
/// cell by the renderer itself — the two bars and the panel cursor bar all build
/// a style per cell anyway, so they ramp themselves.
///
/// Skipping those cells is not just saved work. An animated ramp bounces
/// *through* the element's flat color, so a cell whose current shade lands
/// exactly on it reads as unpainted to [`apply`], which then spreads a whole
/// ramp over that one cell — a bright speck flickering across the bar.
pub fn mark_painted(area: Rect) {
    if area.width > 0 && area.height > 0 {
        PAINTED.with(|p| p.borrow_mut().push(area));
    }
}

/// The zone a cell belongs to: a bar it was drawn on, else the body.
fn zone_at(bars: &[(GradZone, Rect)], x: u16, y: u16) -> GradZone {
    bars.iter()
        .find(|(_, r)| r.contains((x, y).into()))
        .map_or(GradZone::Body, |(z, _)| *z)
}

/// Whether `symbol` is a box-drawing glyph — the frames, corners and column
/// separators a foreground (border) gradient may repaint.
fn is_frame_glyph(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    matches!((chars.next(), chars.next()), (Some('\u{2500}'..='\u{257f}'), None))
}

/// The tag array below indexes roles as `u8`, keeping `u8::MAX` for "no
/// element".
const _: () = assert!(GRAD_ROLES < u8::MAX as usize);

/// One element taking part in this frame's repaint.
struct Target {
    role: GradRole,
    base: Color,
    paint: GradPaint,
    zone: GradZone,
}

/// Repaint `area` with the theme's per-element gradients. A no-op for a theme
/// that defines none (every stock theme but `Rat Commander Neon`), so the cost
/// only lands on the frames that actually need it.
pub fn apply(f: &mut Frame, area: Rect, theme: &Theme) {
    let (w, h) = (area.width as usize, area.height as usize);
    if w == 0 || h == 0 || !theme.has_gradients() {
        return;
    }
    let targets: Vec<Target> = GradRole::ALL
        .iter()
        .filter_map(|role| {
            theme.grad(*role).map(|g| Target {
                role: *role,
                base: g.base,
                paint: role.paint(),
                zone: role.zone(),
            })
        })
        .collect();
    let bars = BARS.with(|b| b.borrow().clone());
    let painted = PAINTED.with(|p| p.borrow().clone());

    // 1. Tag every cell with the element whose flat color it still carries.
    //    `NONE` means the cell belongs to no gradient and is left alone.
    const NONE: u8 = u8::MAX;
    let mut tag = vec![NONE; w * h];
    {
        let buf = f.buffer_mut();
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (area.x + x as u16, area.y + y as u16);
                // Cells under a terminal-graphics image are not drawn at all.
                let Some(cell) = buf
                    .cell((px, py))
                    .filter(|c| c.diff_option != CellDiffOption::Skip)
                else {
                    continue;
                };
                // Cells their own renderer already ramped are finished.
                if painted.iter().any(|r| r.contains((px, py).into())) {
                    continue;
                }
                let zone = zone_at(&bars, px, py);
                let frame_glyph = is_frame_glyph(cell.symbol());
                for (i, t) in targets.iter().enumerate() {
                    let hit = t.zone == zone
                        && match t.paint {
                            GradPaint::Bg => cell.bg == t.base,
                            GradPaint::Fg => frame_glyph && cell.fg == t.base,
                        };
                    if hit {
                        tag[y * w + x] = i as u8;
                        break;
                    }
                }
            }
        }
    }

    // 2. Repaint each connected region with the ramp across its own bounds, so
    //    every dialog, button and bar carries a full gradient of its own.
    let mut seen = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    let mut region: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if seen[start] || tag[start] == NONE {
            continue;
        }
        let tagged = tag[start];
        region.clear();
        stack.clear();
        stack.push(start);
        seen[start] = true;
        let (mut x0, mut y0, mut x1, mut y1) = (w - 1, h - 1, 0usize, 0usize);
        while let Some(i) = stack.pop() {
            region.push(i);
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            let mut visit = |j: usize| {
                if !seen[j] && tag[j] == tagged {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        let bounds = Rect::new(
            x0 as u16,
            y0 as u16,
            (x1 - x0 + 1) as u16,
            (y1 - y0 + 1) as u16,
        );
        let target = &targets[tagged as usize];
        let buf = f.buffer_mut();
        for &i in &region {
            let (x, y) = ((i % w) as u16, (i / w) as u16);
            let Some(color) = theme.grad_color_in(target.role, x, y, bounds) else {
                continue;
            };
            let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) else {
                continue;
            };
            match target.paint {
                GradPaint::Bg => cell.bg = color,
                GradPaint::Fg => cell.fg = color,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{GradientDir, GradientSpec, ThemeSpec};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Style;

    /// A theme whose panel background ramps black → white across its region.
    fn ramp_theme() -> Theme {
        let mut spec = ramp_spec();
        spec.panel_bg = Color::Rgb(0, 0, 0);
        spec.gradients.panel_bg = Some(GradientSpec::new(Color::Rgb(255, 255, 255)));
        Theme::from_spec(&spec, true)
    }

    /// A preset with its gradients stripped, so each test adds only its own.
    fn ramp_spec() -> ThemeSpec {
        let spec = crate::ui::theme::active_specs().into_iter().next().expect("a built-in theme");
        ThemeSpec { gradients: Default::default(), ..spec }
    }

    /// Draw `paint` into a 20×6 test terminal, run the gradient pass, and hand
    /// back the finished buffer.
    fn painted(theme: &Theme, paint: impl FnOnce(&mut Frame)) -> ratatui::buffer::Buffer {
        painted_in(20, 6, theme, paint)
    }

    /// [`painted`], on a terminal of the given size.
    fn painted_in(w: u16, h: u16, theme: &Theme, paint: impl FnOnce(&mut Frame)) -> ratatui::buffer::Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            reset();
            paint(f);
            let area = f.area();
            apply(f, area, theme);
        })
        .unwrap();
        t.backend().buffer().clone()
    }

    /// Fill `r` with `style`.
    fn fill(f: &mut Frame, r: Rect, style: Style) {
        let row = " ".repeat(r.width as usize);
        for y in r.top()..r.bottom() {
            f.buffer_mut().set_string(r.x, y, &row, style);
        }
    }

    fn bg_at(buf: &ratatui::buffer::Buffer, x: u16, y: u16) -> Color {
        buf.cell((x, y)).unwrap().bg
    }

    #[test]
    fn a_background_ramps_across_the_region_it_was_painted_on() {
        let theme = ramp_theme();
        let base = theme.grad(GradRole::PanelBg).unwrap().base;
        let region = Rect::new(2, 1, 10, 3);
        let buf = painted(&theme, |f| fill(f, region, Style::default().bg(base)));

        assert_eq!(bg_at(&buf, 2, 1), Color::Rgb(0, 0, 0), "the ramp starts at the element color");
        assert_eq!(bg_at(&buf, 11, 1), Color::Rgb(255, 255, 255), "and reaches the far endpoint");
        assert_ne!(bg_at(&buf, 6, 1), bg_at(&buf, 2, 1), "with the shades in between");
        // A horizontal ramp is the same on every row of the region…
        assert_eq!(bg_at(&buf, 6, 1), bg_at(&buf, 6, 3));
        // …and nothing outside it was touched.
        assert_eq!(bg_at(&buf, 0, 0), Color::Reset);
    }

    #[test]
    fn each_region_gets_a_whole_ramp_of_its_own() {
        // Two separate boxes of the same element (two buttons, two panels): each
        // runs the full ramp rather than showing a slice of one screen-wide one.
        let theme = ramp_theme();
        let base = theme.grad(GradRole::PanelBg).unwrap().base;
        let (left, right) = (Rect::new(1, 1, 5, 2), Rect::new(12, 3, 5, 2));
        let buf = painted(&theme, |f| {
            fill(f, left, Style::default().bg(base));
            fill(f, right, Style::default().bg(base));
        });
        for r in [left, right] {
            assert_eq!(bg_at(&buf, r.x, r.y), Color::Rgb(0, 0, 0), "{r:?} starts the ramp");
            assert_eq!(bg_at(&buf, r.right() - 1, r.y), Color::Rgb(255, 255, 255), "{r:?} finishes it");
        }
    }

    #[test]
    fn a_frame_gradient_repaints_borders_and_leaves_text_alone() {
        let mut spec = ramp_spec();
        spec.panel_border = Color::Rgb(0, 0, 0);
        spec.gradients.panel_border = Some(GradientSpec::new(Color::Rgb(255, 255, 255)));
        let theme = Theme::from_spec(&spec, true);
        let border = Style::default().fg(Color::Rgb(0, 0, 0));
        let buf = painted(&theme, |f| {
            // A run of frame glyphs, and text in the very same color below it.
            f.buffer_mut().set_string(2, 1, "──────────", border);
            f.buffer_mut().set_string(2, 2, "Documents ", border);
        });
        let fg = |x, y| buf.cell((x, y)).unwrap().fg;
        assert_ne!(fg(11, 1), Color::Rgb(0, 0, 0), "the frame took the ramp");
        for x in 2..12 {
            assert_eq!(fg(x, 2), Color::Rgb(0, 0, 0), "text in the border color is left alone");
        }
    }

    #[test]
    fn a_bar_row_is_not_repainted_by_a_body_gradient() {
        // The stock themes give the cursor, the menu bar and the F-key bar one
        // and the same teal, so the cursor's ramp must stop at the bars.
        let mut spec = ramp_spec();
        spec.cursor_bg = Color::Rgb(0, 160, 160);
        spec.menubar_bg = Color::Rgb(0, 160, 160);
        spec.gradients.cursor_bg = Some(GradientSpec::new(Color::Rgb(255, 255, 255)));
        let theme = Theme::from_spec(&spec, true);
        let teal = Style::default().bg(Color::Rgb(0, 160, 160));
        let bar = Rect::new(0, 0, 20, 1);
        let buf = painted(&theme, |f| {
            mark_bar(GradZone::Menubar, bar);
            fill(f, bar, teal);
            fill(f, Rect::new(0, 3, 20, 1), teal); // a cursor bar in the body
        });
        assert_eq!(bg_at(&buf, 19, 0), Color::Rgb(0, 160, 160), "the menu bar keeps its color");
        assert_eq!(bg_at(&buf, 19, 3), Color::Rgb(255, 255, 255), "the cursor bar ramps");
    }

    /// A bar 21 cells wide, on animation phase 10: `t * 1.5 + 10 * 0.04` lands
    /// on a whole turn of the wave at column 8, so that cell is painted in
    /// exactly the element's flat color. Everything the bars and the panel
    /// cursor bar draw themselves is keyed to this — the shades they drew must
    /// survive the pass.
    const WAVE_BAR: Rect = Rect { x: 0, y: 0, width: 21, height: 1 };
    const WAVE_PHASE: usize = 10;
    const WAVE_HIT: u16 = 8;

    /// A theme whose `role` ramps `base` → `to`, animated, with the animation on.
    fn moving(role: GradRole, base: Color, to: Color) -> Theme {
        let mut spec = ramp_spec();
        *spec.gradients.slot(role) = Some(GradientSpec { animated: true, ..GradientSpec::new(to) });
        match role {
            GradRole::MenubarBg => spec.menubar_bg = base,
            GradRole::CursorBg => spec.cursor_bg = base,
            _ => unreachable!("only the self-painting roles are exercised here"),
        }
        let mut theme = Theme::from_spec(&spec, true);
        theme.animated = true;
        theme.anim = WAVE_PHASE;
        theme
    }

    /// Paint `bar` cell by cell with `role`'s own ramp, the way the menu bar,
    /// the F-key bar and the panel cursor bar all do, and claim it.
    fn self_paint(f: &mut Frame, bar: Rect, role: GradRole, theme: &Theme) -> Vec<Color> {
        let width = bar.width as usize;
        (0..bar.width)
            .map(|x| {
                let bg = theme.bar_bg(role, x as usize, width).expect("a truecolor ramp");
                f.buffer_mut().set_string(bar.x + x, bar.y, " ", Style::default().bg(bg));
                bg
            })
            .collect()
    }

    #[test]
    fn a_self_painted_bar_keeps_the_shades_it_drew() {
        // An animated ramp bounces *through* the element's flat color, so every
        // so often one cell of a moving bar carries exactly that color. Such a
        // cell read as unpainted, and got a whole ramp spread over its own
        // single cell — a bright speck flickering across the bar.
        let theme = moving(GradRole::MenubarBg, Color::Rgb(122, 31, 255), Color::Rgb(34, 224, 255));
        let base = theme.grad(GradRole::MenubarBg).unwrap().base;
        let mut drawn = Vec::new();
        let buf = painted_in(WAVE_BAR.width, 2, &theme, |f| {
            mark_bar(GradZone::Menubar, WAVE_BAR);
            mark_painted(WAVE_BAR);
            drawn = self_paint(f, WAVE_BAR, GradRole::MenubarBg, &theme);
        });
        assert_eq!(drawn[WAVE_HIT as usize], base, "the case guarded here: a moving shade on the flat color");
        for x in 0..WAVE_BAR.width {
            assert_eq!(bg_at(&buf, x, 0), drawn[x as usize], "column {x} keeps the shade the bar drew");
        }
    }

    #[test]
    fn a_claimed_row_is_not_claimed_by_another_element_of_the_same_color() {
        // The cursor bar ramps itself in the *body* zone, where a theme may well
        // give another element the very same color (Neon paints the cursor, the
        // bars and the focused button one purple). A cell landing on that shared
        // color must not be picked up as that other element either — hence a
        // claimed cell is skipped outright rather than just for its own role.
        let purple = Color::Rgb(122, 31, 255);
        let mut spec = ramp_spec();
        spec.cursor_bg = purple;
        spec.button_focused_bg = purple;
        spec.gradients.cursor_bg =
            Some(GradientSpec { animated: true, ..GradientSpec::new(Color::Rgb(34, 224, 255)) });
        spec.gradients.button_focused_bg = Some(GradientSpec {
            direction: GradientDir::Radial,
            animated: true,
            ..GradientSpec::new(Color::Rgb(255, 60, 170))
        });
        let mut theme = Theme::from_spec(&spec, true);
        theme.animated = true;
        theme.anim = WAVE_PHASE;

        let mut drawn = Vec::new();
        let buf = painted_in(WAVE_BAR.width, 2, &theme, |f| {
            mark_painted(WAVE_BAR);
            drawn = self_paint(f, WAVE_BAR, GradRole::CursorBg, &theme);
        });
        assert_eq!(drawn[WAVE_HIT as usize], purple, "the case guarded here: a moving shade on the flat color");
        for x in 0..WAVE_BAR.width {
            assert_eq!(bg_at(&buf, x, 0), drawn[x as usize], "column {x} keeps the shade the cursor bar drew");
        }
    }

    #[test]
    fn an_unclaimed_cell_of_the_flat_color_still_gets_its_ramp() {
        // The other side of the coin: a cell the renderer left flat (a dialog, a
        // button, the inactive panel's cursor) is still found and repainted.
        let theme = moving(GradRole::CursorBg, Color::Rgb(122, 31, 255), Color::Rgb(34, 224, 255));
        let row = Rect::new(0, 0, 20, 1);
        let buf = painted(&theme, |f| fill(f, row, Style::default().bg(Color::Rgb(122, 31, 255))));
        assert_ne!(bg_at(&buf, 19, 0), Color::Rgb(122, 31, 255), "a flat cursor bar takes the ramp");
    }

    #[test]
    fn a_theme_without_gradients_leaves_the_frame_untouched() {
        let theme = Theme::from_spec(&ramp_spec(), true);
        assert!(!theme.has_gradients());
        let style = Style::default().bg(theme.panel_bg).fg(theme.panel_fg);
        let buf = painted(&theme, |f| fill(f, Rect::new(0, 0, 20, 6), style));
        for x in [0u16, 9, 19] {
            assert_eq!(bg_at(&buf, x, 0), theme.panel_bg, "flat theme, flat background");
        }
    }

    #[test]
    fn a_vertical_ramp_runs_down_the_region() {
        let mut spec = ramp_spec();
        spec.panel_bg = Color::Rgb(0, 0, 0);
        spec.gradients.panel_bg = Some(GradientSpec {
            direction: GradientDir::Vertical,
            ..GradientSpec::new(Color::Rgb(255, 255, 255))
        });
        let theme = Theme::from_spec(&spec, true);
        let region = Rect::new(0, 0, 20, 6);
        let buf = painted(&theme, |f| fill(f, region, Style::default().bg(Color::Rgb(0, 0, 0))));
        assert_eq!(bg_at(&buf, 4, 0), Color::Rgb(0, 0, 0));
        assert_eq!(bg_at(&buf, 4, 5), Color::Rgb(255, 255, 255));
        assert_eq!(bg_at(&buf, 4, 2), bg_at(&buf, 17, 2), "one shade per row");
    }
}
