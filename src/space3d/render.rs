//! Presenting the 3D scene into a panel.
//!
//! The scene is always rasterized into an RGBA buffer; only the final step
//! differs. On a graphics terminal the buffer is sized in real pixels and shipped
//! as an image by the root draw layer (which owns `Gfx`). Otherwise the panel
//! draws it itself: as half-block cells on a truecolor terminal, or as an ASCII
//! luminance ramp without one.

use super::{Space3d, Space3dStyle, raster3d};
use crate::ui::graphics::raster;
use crate::ui::theme::Theme;
use image::RgbaImage;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Below this the scene is unreadable, so say so rather than draw mush.
const MIN_W: u16 = 24;
const MIN_H: u16 = 8;

/// Largest raster we will build per frame. A camera lerp rebuilds this at the
/// frame rate, so an uncapped 4K panel would re-encode ~8 MP every frame; the
/// image is scaled back up to the cell area on the way out.
const MAX_PX: u32 = 1280;

/// Draw the 3D view into `area`.
///
/// Returns `Some(area)` when the caller should composite a pixel image there —
/// the same deferred handoff the Details thumbnail uses, because the panel
/// renderer has no access to `Gfx`.
pub fn render(
    f: &mut Frame,
    area: Rect,
    sp: &mut Space3d,
    theme: &Theme,
    graphics: bool,
) -> Option<Rect> {
    if area.width == 0 || area.height == 0 {
        sp.bounds.clear();
        return None;
    }
    if area.width < MIN_W || area.height < MIN_H {
        sp.bounds.clear();
        center_text(f, area, &crate::l10n::trd("Panel too small"), theme);
        return None;
    }

    let pal = super::ScenePalette::from_theme(theme);

    if graphics {
        // The root layer knows the pixel size it will build at, so it sets the
        // viewport there; here we only claim the area.
        // The root layer owns `Gfx`, so it both draws the image and records the
        // hit-test bounds — only it knows the raster size the image will be
        // built at, and having it do both keeps the two in the same coordinate
        // space from the very first frame. We only claim the area here, and mark
        // the cells painted so the gradient post-pass leaves them alone.
        crate::ui::gradient::mark_painted(area);
        return Some(area);
    }

    // Hit-test geometry is refreshed every frame; it is only a handful of
    // projections, unlike the rasterization itself.
    let (w, h) = (area.width as u32, area.height as u32 * 2);
    // Re-frame first: a resized panel changes the shape the scene has to fill,
    // and the bounds recorded below must match what is actually drawn.
    sp.set_viewport(w, h);
    sp.bounds_px = (w, h);
    let boxes = sp.boxes(&pal);
    sp.bounds = raster3d::project_bounds(w, h, &boxes, sp.cam.eye(), sp.cam.target);
    // The names are *not* baked here. Baking exists because cell text drawn over
    // a Kitty/Sixel image is never shown — but in the cell-art modes the cells
    // are ours, and a name downsampled into half-blocks is an illegible smudge
    // where real terminal text is crisp.
    let (img, slots) = scene(sp, &boxes, w, h, theme, false);
    if theme.truecolor {
        crate::util::img::render_halfblocks(f, area, &img, theme.panel_bg);
    } else {
        crate::util::img::render_ascii_ramp(f, area, &img, theme);
    }
    draw_cell_labels(f, area, &slots, theme);
    crate::ui::gradient::mark_painted(area);
    None
}

/// Draw the box names as ordinary terminal text, over the cell art.
///
/// Placement mirrors the baked path — most important first, and a name that
/// would land on one already drawn is dropped — but measured in **characters**
/// against the room the roof offers, since at cell resolution that is the only
/// unit there is.
fn draw_cell_labels(f: &mut Frame, area: Rect, slots: &[raster3d::LabelSlot], theme: &Theme) {
    // The raster is one pixel per column and two per row, in both cell modes.
    let to_cell = |x: f32, y: f32| (area.x as f32 + x, area.y as f32 + y * 0.5);
    let mut taken: Vec<(f32, f32, f32, f32)> = Vec::new();
    for s in slots {
        // Gated on the roof's *width*, not its smaller dimension: a cell label
        // is one row tall whatever the roof looks like, so how squat the roof
        // appears at this angle says nothing about whether a name fits.
        if s.half_w < if s.important { 2.0 } else { 3.0 } {
            continue;
        }
        // Budgeted against that width too, with a small floor so a name is not
        // cut to two or three characters on a merely narrow box. A little
        // overhang reads fine, and a label that would land on one already drawn
        // is dropped anyway.
        let cols = ((s.half_w * 2.0 * 0.95) as i32).max(if s.important { 8 } else { 5 });
        let text = ellipsize_cells(&s.name, cols as usize);
        if text.is_empty() {
            continue;
        }
        let w = unicode_width::UnicodeWidthStr::width(text.as_str()) as f32;
        let (cx, cy) = to_cell(s.cx, s.cy);
        let x = (cx - w * 0.5).round();
        let y = cy.round();
        let rect = (x, y, x + w, y + 1.0);
        if taken.iter().any(|t| raster3d::overlaps(*t, rect)) {
            continue;
        }
        taken.push(rect);
        let x = x.max(area.x as f32) as u16;
        if y < area.y as f32 || y >= (area.y + area.height) as f32 || x >= area.x + area.width {
            continue;
        }
        let width = (w as u16).min(area.x + area.width - x);
        if width == 0 {
            continue;
        }
        // A fading box's name fades with it, by mixing toward the panel
        // background — the same thing the box itself does.
        if s.fade < 0.12 {
            continue;
        }
        let bg = raster::rgb(theme.panel_bg);
        let fg = raster::over(bg, raster::rgb(theme.panel_fg), s.fade.clamp(0.0, 1.0) as f64);
        let mut style = Style::default()
            .fg(ratatui::style::Color::Rgb(fg.0, fg.1, fg.2))
            .bg(theme.panel_bg);
        if s.important {
            style = style.add_modifier(ratatui::style::Modifier::BOLD);
        }
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, style))),
            Rect { x, y: y as u16, width, height: 1 },
        );
    }
}

/// Shorten `s` to at most `cols` terminal columns, with an ellipsis.
fn ellipsize_cells(s: &str, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    crate::util::text::ellipsize(s, cols)
}

/// The pixel size the root layer should build at for a given cell area.
pub fn raster_size(px: (u32, u32)) -> (u32, u32) {
    let (w, h) = (px.0.max(1), px.1.max(1));
    let long = w.max(h);
    if long <= MAX_PX {
        return (w, h);
    }
    let k = MAX_PX as f32 / long as f32;
    (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1))
}

/// Rasterize the scene, baking the names into the pixels.
pub fn rasterize(
    sp: &Space3d,
    boxes: &[raster3d::SceneBox],
    w: u32,
    h: u32,
    theme: &Theme,
) -> RgbaImage {
    scene(sp, boxes, w, h, theme, true).0
}

/// Rasterize the scene, returning the label placements alongside it.
fn scene(
    sp: &Space3d,
    boxes: &[raster3d::SceneBox],
    w: u32,
    h: u32,
    theme: &Theme,
    bake: bool,
) -> (RgbaImage, Vec<raster3d::LabelSlot>) {
    let bg = raster::rgb(theme.panel_bg);
    let sky = sky_for(sp, theme);
    // Over a ground plane the links have to read against grass, not against the
    // panel background — so they take the platform's own pale colour rather
    // than a mix of a background that is nowhere on screen.
    let link_c = match sky {
        Some(_) => super::ScenePalette::from_theme(theme).platform,
        None => raster::over(bg, raster::rgb(theme.panel_fg), 0.45),
    };
    let (img, _, slots) = raster3d::render_scene(
        w,
        h,
        boxes,
        &sp.links(),
        sp.cam.eye(),
        sp.cam.target,
        bg,
        raster::rgb(theme.panel_fg),
        // The links are structure, not content: drawn midway between the
        // background and the text so the tree's shape reads without the lines
        // competing with the boxes they connect.
        link_c,
        // The same colour the other panel paints its own cursor row with, so
        // the two read as the same cursor in two places.
        raster::rgb(theme.cursor.bg.unwrap_or(theme.panel_border_active)),
        sky,
        bake,
    );
    (img, slots)
}

/// The backdrop for the current style: an open sky over a ground plane in the
/// fsn style, nothing at all in Cubes.
///
/// Theme-tinted rather than fsn's own fixed blue-over-green. The sky is built
/// from the theme's own accent and the ground from its directory colour, both
/// pushed well down in brightness and saturation — a scene lit like an outdoor
/// one, in whatever palette is loaded.
fn sky_for(sp: &Space3d, theme: &Theme) -> Option<raster3d::Sky> {
    if sp.style != Space3dStyle::Fsn {
        return None;
    }
    let accent = raster::rgb(theme.panel_border_active);
    let bg = raster::rgb(theme.panel_bg);
    // A pale band where the sky meets the ground and a deeper vault overhead:
    // that vertical spread is most of what makes a flat wash read as open air.
    // Mixed from the panel background, so a dark theme gets a night sky rather
    // than a daylit one and the scene still belongs to the theme it sits in.
    let horizon = raster::shade(raster::over(bg, accent, 0.22), 1.80);
    let top = raster::shade(raster::over(bg, accent, 0.62), 0.60);

    // The ground takes the sky's *opposite* hue.
    //
    // What makes fsn's world read as a world is that its ground is green under
    // a blue sky — two colours that cannot be mistaken for one another. A
    // ground mixed from the same theme colour as the sky comes out a shade of
    // it, and the join then looks like a seam between two washes rather than
    // like land meeting air. Rotating the hue keeps the scene tied to the theme
    // while guaranteeing the two halves separate; since UI accents are so often
    // blue, in practice this lands on fsn's own green by itself.
    // Measured off the sky that actually gets drawn rather than off the raw
    // accent, which the mix with the background has already moved.
    let (h, sat, _) = to_hsv(top);
    // A quarter-turn back down the wheel: blue sky, green ground — fsn's own
    // relationship, and the one the eye reads as "outdoors".
    let gh = (h - 95.0 + 360.0) % 360.0;
    let gs = (sat * 1.05).clamp(0.38, 0.82);

    Some(raster3d::Sky {
        top,
        horizon,
        // Hazed toward the horizon with distance: that aerial perspective is
        // what turns a flat wash into a plane receding to the skyline.
        ground_far: raster::over(raster::hsv(gh, gs * 0.80, 0.44), horizon, 0.34),
        ground_near: raster::hsv(gh, gs, 0.20),
    })
}

/// RGB to hue/saturation/value, the inverse of [`raster::hsv`]. Hue in degrees.
fn to_hsv(c: raster::Rgb) -> (f64, f64, f64) {
    let (r, g, b) = (c.0 as f64 / 255.0, c.1 as f64 / 255.0, c.2 as f64 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d < 1e-6 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max < 1e-6 { 0.0 } else { d / max };
    ((h + 360.0) % 360.0, s, max)
}

/// A cheap signature of everything the image depends on, so a settled camera
/// over a quiet cache neither rebuilds nor re-encodes it.
pub fn signature(
    sp: &Space3d,
    boxes: &[raster3d::SceneBox],
    w: u32,
    h: u32,
    theme: &Theme,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hsh = std::collections::hash_map::DefaultHasher::new();
    (w, h).hash(&mut hsh);
    // Camera pose, quantised: sub-pixel differences cannot change the image.
    let q = |v: f32, s: f32| (v * s).round() as i64;
    (
        q(sp.cam.target.x, 512.0),
        q(sp.cam.target.y, 512.0),
        q(sp.cam.target.z, 512.0),
        q(sp.cam.dist, 512.0),
        q(sp.cam.yaw, 2048.0),
        q(sp.cam.pitch, 2048.0),
    )
        .hash(&mut hsh);
    sp.selected.hash(&mut hsh);
    raster::rgb(theme.panel_bg).hash(&mut hsh);
    raster::rgb(theme.panel_border_active).hash(&mut hsh);
    // The style changes the backdrop and the shapes, neither of which is
    // visible in the box list alone — without this the cached image would
    // survive a switch between the two looks.
    (sp.style == Space3dStyle::Fsn).hash(&mut hsh);
    // The boxes as they will actually be drawn, quantised: a sub-pixel step of
    // a growing box must not force a full rebuild on every crawl update.
    for b in boxes {
        b.name.hash(&mut hsh);
        b.partial.hash(&mut hsh);
        b.selected.hash(&mut hsh);
        b.cursor.hash(&mut hsh);
        b.dim.hash(&mut hsh);
        b.shape.hash(&mut hsh);
        b.color.hash(&mut hsh);
        // Quantised, so a fade in progress redraws but a settled one does not.
        ((b.fade * 64.0) as i32).hash(&mut hsh);
        for v in [b.min, b.max] {
            (q(v.x, 256.0), q(v.y, 256.0), q(v.z, 256.0)).hash(&mut hsh);
        }
    }
    hsh.finish()
}

fn center_text(f: &mut Frame, area: Rect, text: &str, theme: &Theme) {
    let style = Style::default().fg(theme.panel_fg).bg(theme.panel_bg);
    let y = area.y + area.height / 2;
    let w = text.chars().count().min(area.width as usize) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(text.to_string(), style))),
        Rect { x, y, width: w, height: 1 },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_shortened_to_the_room_the_roof_offers() {
        assert_eq!(ellipsize_cells("docs", 8), "docs", "a name that fits is untouched");
        let cut = ellipsize_cells("a-very-long-directory-name", 8);
        assert!(
            unicode_width::UnicodeWidthStr::width(cut.as_str()) <= 8,
            "{cut:?} is wider than its box"
        );
        assert!(cut.len() < "a-very-long-directory-name".len(), "and was shortened");
        assert!(ellipsize_cells("docs", 0).is_empty(), "no room, no label");
    }

    #[test]
    fn a_wide_character_name_is_measured_in_columns_not_bytes() {
        // Two columns per glyph, so four of them do not fit in five columns.
        let cut = ellipsize_cells("日本語です", 5);
        assert!(
            unicode_width::UnicodeWidthStr::width(cut.as_str()) <= 5,
            "{cut:?} overflows its box"
        );
    }
}
