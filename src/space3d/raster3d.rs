//! Rasterizing the scene into an RGBA buffer.
//!
//! One code path serves both presentations: the caller picks the buffer size,
//! and everything downstream — projection, culling, shading, labels — is
//! identical. In graphics mode the buffer is sized in real terminal pixels and
//! shipped as an image; in text mode it is `width × 2·height` and presented as
//! half-block cells. Because a half-cell is very nearly square, the same focal
//! length is correct in both, so there is no aspect fudge anywhere.

use super::vec3::{self, Basis, V3, v3};
use crate::ui::graphics::raster::{self, Rgb};
use image::RgbaImage;

/// A box's projected silhouette in raster pixels, as `(x0, y0, x1, y1)`.
/// `None` when it fell entirely outside the view (or behind the camera).
pub type Bounds = Option<(f32, f32, f32, f32)>;

/// One directory, as a box standing on the ground plane.
#[derive(Debug, Clone)]
pub struct SceneBox {
    pub name: String,
    /// Human-readable size, baked onto the roof when there is room.
    pub size_label: String,
    /// Axis-aligned extent. `y` runs from the ground up.
    pub min: V3,
    pub max: V3,
    pub color: Rgb,
    pub selected: bool,
    /// The directory the view is about. Always labelled, and labelled first.
    pub focus: bool,
    /// The directory the other panel's cursor is on: lit up, so where you are
    /// about to go is visible before you go there.
    pub cursor: bool,
    /// Drawn only for context — faded back so it cannot be mistaken for the
    /// subject of the view.
    pub dim: bool,
    /// How far this box has faded in: 1 is fully present, 0 invisible. Boxes
    /// appearing and disappearing ride this up and down so a directory change
    /// is a dissolve rather than a jump cut.
    pub fade: f32,
    /// Still being sized — drawn slightly washed out, so a box that is merely
    /// incomplete never reads as a box that is genuinely small.
    pub partial: bool,
}

/// Vertical field of view.
pub(crate) const FOV_Y: f32 = std::f32::consts::PI / 3.0;

/// Face brightness by orientation. Fixed per axis rather than computed from a
/// light vector: it gives the crisp, legible "city block" read, and the roof —
/// which carries the label — is always the brightest surface.
fn face_shade(n: V3) -> f64 {
    if n.y.abs() > 0.5 {
        if n.y > 0.0 { 1.12 } else { 0.35 }
    } else if n.x.abs() > 0.5 {
        0.78
    } else {
        0.58
    }
}

/// The six faces of an axis-aligned box, as corner quads with outward normals.
fn faces(min: V3, max: V3) -> [([V3; 4], V3); 6] {
    let (a, b) = (min, max);
    [
        // roof
        ([v3(a.x, b.y, a.z), v3(b.x, b.y, a.z), v3(b.x, b.y, b.z), v3(a.x, b.y, b.z)], v3(0.0, 1.0, 0.0)),
        // floor
        ([v3(a.x, a.y, a.z), v3(a.x, a.y, b.z), v3(b.x, a.y, b.z), v3(b.x, a.y, a.z)], v3(0.0, -1.0, 0.0)),
        // -Z / +Z walls
        ([v3(a.x, a.y, a.z), v3(b.x, a.y, a.z), v3(b.x, b.y, a.z), v3(a.x, b.y, a.z)], v3(0.0, 0.0, -1.0)),
        ([v3(b.x, a.y, b.z), v3(a.x, a.y, b.z), v3(a.x, b.y, b.z), v3(b.x, b.y, b.z)], v3(0.0, 0.0, 1.0)),
        // -X / +X walls
        ([v3(a.x, a.y, b.z), v3(a.x, a.y, a.z), v3(a.x, b.y, a.z), v3(a.x, b.y, b.z)], v3(-1.0, 0.0, 0.0)),
        ([v3(b.x, a.y, a.z), v3(b.x, a.y, b.z), v3(b.x, b.y, b.z), v3(b.x, b.y, a.z)], v3(1.0, 0.0, 0.0)),
    ]
}

/// Draw `boxes` as seen from `eye` looking at `target`.
///
/// Returns the buffer plus, for each box, the pixel bounding rectangle of its
/// projected silhouette — the caller turns those into cell rects for mouse
/// hit-testing.
#[allow(clippy::too_many_arguments)]
pub fn render_scene(
    w: u32,
    h: u32,
    boxes: &[SceneBox],
    links: &[(V3, V3)],
    eye: V3,
    target: V3,
    bg: Rgb,
    label_fg: Rgb,
    link_c: Rgb,
    cursor_c: Rgb,
    // `bake`: draw the names into the pixels. True on a graphics terminal,
    // where cell text over the image would not be shown at all; false in the
    // cell-art modes, where the caller draws them as real terminal text.
    bake: bool,
) -> (RgbaImage, Vec<Bounds>, Vec<LabelSlot>) {
    let mut img = raster::canvas(w.max(1), h.max(1), bg);
    let mut bounds: Vec<Bounds> = vec![None; boxes.len()];
    if w == 0 || h == 0 || boxes.is_empty() {
        return (img, bounds, Vec::new());
    }
    // `inv_z`, so a larger value is nearer; 0.0 is infinitely far away.
    let mut depth = vec![0.0f32; (w * h) as usize];
    let basis = vec3::look_at(eye, target, v3(0.0, 1.0, 0.0));
    let focal = vec3::focal_for(h as f32, FOV_Y);
    let (fw, fh) = (w as f32, h as f32);

    // Connectors first. Draw order does not actually matter — the depth buffer
    // decides — but doing the lines first means a box's own face wins any tie at
    // the point where its link attaches, so the line reads as going *into* the
    // box rather than lying on top of it.
    for (a, b) in links {
        let (Some(pa), Some(pb)) = (
            vec3::project(vec3::to_view(&basis, *a), fw, fh, focal),
            vec3::project(vec3::to_view(&basis, *b), fw, fh, focal),
        ) else {
            continue;
        };
        // Thickness follows the raster size, so the tree's structure stays
        // visible in a small half-block buffer and does not turn into rope in a
        // large pixel one.
        let thick = if h >= 400 { 2i32 } else { 1i32 };
        for o in 0..thick {
            let d = o as f32;
            line(&mut img, &mut depth, (pa.0 + d, pa.1, pa.2), (pb.0 + d, pb.1, pb.2), link_c);
        }
    }

    for (bi, b) in boxes.iter().enumerate() {
        let mut base = if b.partial { raster::over(bg, b.color, 0.72) } else { b.color };
        if b.dim {
            base = raster::over(bg, base, 0.45);
        }
        // Fading is done by mixing toward the background rather than by real
        // alpha compositing: the depth buffer holds one opaque surface per
        // pixel, and a half-transparent box would need the geometry behind it
        // that was never drawn.
        if b.fade < 0.999 {
            base = raster::over(bg, base, b.fade.clamp(0.0, 1.0) as f64);
        }
        if b.cursor {
            // Lit from within rather than merely outlined: the text-mode
            // fallbacks reduce the scene to brightness alone, so a highlight
            // that lives only in the colour would vanish there. Pushed hard,
            // because in a panel-sized raster a single box is not many pixels
            // and a subtle lift reads as nothing at all.
            base = raster::shade(raster::over(base, cursor_c, 0.75), 1.35);
        }
        let mut bb: Bounds = None;
        for (quad, normal) in faces(b.min, b.max) {
            // Back-face cull: keep only faces turned toward the camera.
            let centre = quad
                .iter()
                .fold(v3(0.0, 0.0, 0.0), |acc, &p| acc.add(p))
                .scale(0.25);
            if normal.dot(eye.sub(centre)) <= 0.0 {
                continue;
            }
            let mut pts = [(0.0f32, 0.0f32, 0.0f32); 4];
            let mut ok = true;
            for (i, &p) in quad.iter().enumerate() {
                match vec3::project(vec3::to_view(&basis, p), fw, fh, focal) {
                    Some(v) => pts[i] = v,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue; // straddles the eye plane; skip rather than distort
            }
            for p in &pts {
                bb = Some(match bb {
                    None => (p.0, p.1, p.0, p.1),
                    Some((x0, y0, x1, y1)) => (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1)),
                });
            }
            let c = raster::shade(base, face_shade(normal));
            fill_quad(&mut img, &mut depth, &pts, c);
            if b.cursor {
                let rim = raster::shade(cursor_c, 1.25);
                outline(&mut img, &mut depth, &pts, rim);
                if h >= 240 {
                    // Thicken on a large raster, where one pixel is a hairline.
                    let wide = pts.map(|(x, y, z)| (x + 1.0, y, z));
                    outline(&mut img, &mut depth, &wide, rim);
                }
            }
            if b.selected {
                outline(&mut img, &mut depth, &pts, raster::shade(base, 1.9));
            }
        }
        bounds[bi] = bb;
    }

    let slots = label_slots(&depth, w, boxes, &basis, focal, fw, fh);
    if bake {
        bake_labels(&mut img, &slots, label_fg, bg);
    }
    (img, bounds, slots)
}

/// Project just the silhouette bounds of each box — everything the cursor and
/// the mouse need, without touching a pixel. The panel renderer calls this every
/// frame; the expensive rasterization only happens when the image actually has
/// to be rebuilt.
pub fn project_bounds(
    w: u32,
    h: u32,
    boxes: &[SceneBox],
    eye: V3,
    target: V3,
) -> Vec<Bounds> {
    if w == 0 || h == 0 {
        return vec![None; boxes.len()];
    }
    let basis = vec3::look_at(eye, target, v3(0.0, 1.0, 0.0));
    let focal = vec3::focal_for(h as f32, FOV_Y);
    let (fw, fh) = (w as f32, h as f32);
    boxes
        .iter()
        .map(|b| {
            let (a, c) = (b.min, b.max);
            let mut bb: Bounds = None;
            for corner in [
                v3(a.x, a.y, a.z), v3(c.x, a.y, a.z), v3(a.x, a.y, c.z), v3(c.x, a.y, c.z),
                v3(a.x, c.y, a.z), v3(c.x, c.y, a.z), v3(a.x, c.y, c.z), v3(c.x, c.y, c.z),
            ] {
                let Some((x, y, _)) = vec3::project(vec3::to_view(&basis, corner), fw, fh, focal)
                else {
                    continue;
                };
                bb = Some(match bb {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
            bb
        })
        .collect()
}

/// Scanline-fill a convex quad, depth-testing every pixel.
fn fill_quad(img: &mut RgbaImage, depth: &mut [f32], q: &[(f32, f32, f32); 4], c: Rgb) {
    let w = img.width() as i32;
    let h = img.height() as i32;
    let ymin = q.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor().max(0.0) as i32;
    let ymax = (q.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil()).min(h as f32 - 1.0) as i32;
    for y in ymin..=ymax.max(ymin - 1) {
        let yc = y as f32 + 0.5;
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        let (mut lo_z, mut hi_z) = (0.0f32, 0.0f32);
        for i in 0..4 {
            let (x0, y0, z0) = q[i];
            let (x1, y1, z1) = q[(i + 1) % 4];
            if (y0 <= yc) == (y1 <= yc) {
                continue; // this edge does not cross the scanline
            }
            let t = (yc - y0) / (y1 - y0);
            let x = x0 + (x1 - x0) * t;
            // `z` here is inv_z, which *is* linear in screen space — this is
            // why the projection returns 1/z rather than z.
            let z = z0 + (z1 - z0) * t;
            if x < lo {
                lo = x;
                lo_z = z;
            }
            if x > hi {
                hi = x;
                hi_z = z;
            }
        }
        if lo > hi {
            continue;
        }
        let x0 = lo.floor().max(0.0) as i32;
        let x1 = hi.ceil().min(w as f32 - 1.0) as i32;
        let span = (hi - lo).max(1e-6);
        for x in x0..=x1.max(x0 - 1) {
            let t = (((x as f32 + 0.5) - lo) / span).clamp(0.0, 1.0);
            let z = lo_z + (hi_z - lo_z) * t;
            let idx = y as usize * w as usize + x as usize;
            if z > depth[idx] {
                depth[idx] = z;
                raster::put(img, x as u32, y as u32, c);
            }
        }
    }
}

/// Trace a quad's edges, so the selected box reads as outlined at any angle.
fn outline(img: &mut RgbaImage, depth: &mut [f32], q: &[(f32, f32, f32); 4], c: Rgb) {
    for i in 0..4 {
        line(img, depth, q[i], q[(i + 1) % 4], c);
    }
}

/// Depth-tested line with interpolated `inv_z`.
///
/// The test is a hair permissive so a box's selection outline wins against the
/// very face it sits on, which would otherwise z-fight with it.
fn line(img: &mut RgbaImage, depth: &mut [f32], a: (f32, f32, f32), b: (f32, f32, f32), c: Rgb) {
    let w = img.width() as i32;
    let h = img.height() as i32;
    let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil() as i32).clamp(1, 4096);
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let x = (a.0 + (b.0 - a.0) * t).round() as i32;
        let y = (a.1 + (b.1 - a.1) * t).round() as i32;
        if x < 0 || y < 0 || x >= w || y >= h {
            continue;
        }
        let z = a.2 + (b.2 - a.2) * t;
        let idx = y as usize * w as usize + x as usize;
        // A hair in front, so the outline wins against its own face.
        if z >= depth[idx] * 0.999 {
            depth[idx] = z;
            raster::put(img, x as u32, y as u32, c);
        }
    }
}

/// Where a box's name wants to go, and how much room it has.
///
/// Computed once and then either baked into the raster (on a graphics terminal,
/// where cell text drawn over the image would not show) or handed back so the
/// caller can draw it as ordinary terminal text — which, in the cell-art modes,
/// is far more legible than a name downsampled into half-blocks.
#[derive(Debug, Clone)]
pub struct LabelSlot {
    pub name: String,
    pub size_label: String,
    /// Anchor, in raster pixels: the centre of the box's roof.
    pub cx: f32,
    pub cy: f32,
    /// Half the smaller of the roof's projected dimensions, in raster pixels.
    /// This is the "is there room at all" measure, and sets the baked font size.
    pub half: f32,
    /// Half the roof's projected **width**, in raster pixels — how much room a
    /// name actually has. Seen at an angle a roof is far wider than it is tall,
    /// so budgeting text against `half` cuts names long before it needs to.
    pub half_w: f32,
    pub fade: f32,
    /// The focus, the box under the other panel's cursor, or the selection:
    /// named even when small, because these are the names worth reading.
    pub important: bool,
}

/// Work out which boxes can carry a name, and in what order of importance.
///
/// Ordered focus first, then the box under the other panel's cursor, then the
/// selection, then by how much room the roof offers. Callers place them in that
/// order and drop any that would collide, so with a hundred boxes on screen the
/// names that survive are the ones worth reading.
#[allow(clippy::too_many_arguments)]
fn label_slots(
    depth: &[f32],
    iw: u32,
    boxes: &[SceneBox],
    basis: &Basis,
    focal: f32,
    fw: f32,
    fh: f32,
) -> Vec<LabelSlot> {
    let mut spots: Vec<(LabelSlot, usize)> = Vec::with_capacity(boxes.len());
    for (i, b) in boxes.iter().enumerate() {
        if b.name.is_empty() || b.fade < 0.05 {
            continue;
        }
        let roof_y = b.max.y;
        let centre = v3((b.min.x + b.max.x) * 0.5, roof_y, (b.min.z + b.max.z) * 0.5);
        let Some((cx, cy, cz)) = vec3::project(vec3::to_view(basis, centre), fw, fh, focal) else {
            continue;
        };
        if cx < 0.0 || cy < 0.0 || cx >= fw || cy >= fh {
            continue;
        }
        // Is the roof's own centre still visible, or is something in front of
        // it? The tolerance is loose on purpose: the depth buffer is sampled at
        // a pixel centre while `cz` is the exact centroid, and that half-pixel
        // offset alone moves the value by a fraction of a percent. A box
        // genuinely in front differs by far more than this, whereas a tight
        // tolerance throws away labels from boxes in plain view.
        let idx = cy as usize * iw as usize + cx as usize;
        if depth.get(idx).is_some_and(|&d| d > cz * 1.02) {
            continue;
        }
        // How much room the roof actually offers, from all four of its corners.
        // Measuring one corner and taking the smaller axis delta looks
        // equivalent but is not: at many camera angles one of those deltas
        // collapses to nothing, and the label is then dropped from a box with
        // plenty of space on it.
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        let mut ok = true;
        for c in [
            v3(b.min.x, roof_y, b.min.z),
            v3(b.max.x, roof_y, b.min.z),
            v3(b.max.x, roof_y, b.max.z),
            v3(b.min.x, roof_y, b.max.z),
        ] {
            match vec3::project(vec3::to_view(basis, c), fw, fh, focal) {
                Some((x, y, _)) => {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
                None => ok = false,
            }
        }
        if !ok {
            continue;
        }
        let half = ((x1 - x0).min(y1 - y0) * 0.5).max(1.0);
        let half_w = ((x1 - x0) * 0.5).max(half);
        spots.push((
            LabelSlot {
                name: b.name.clone(),
                size_label: b.size_label.clone(),
                cx,
                cy,
                half,
                half_w,
                fade: b.fade,
                important: b.focus || b.cursor || b.selected,
            },
            i,
        ));
    }
    spots.sort_by(|(a, ai), (b, bi)| {
        let rank = |i: &usize| (!boxes[*i].focus, !boxes[*i].cursor, !boxes[*i].selected);
        rank(ai).cmp(&rank(bi)).then(b.half.total_cmp(&a.half))
    });
    spots.into_iter().map(|(s, _)| s).collect()
}

/// Bake the names into the raster, for terminals where cell text drawn over the
/// image would not be shown at all.
fn bake_labels(img: &mut RgbaImage, slots: &[LabelSlot], fg: Rgb, plate: Rgb) {
    let mut taken: Vec<(f32, f32, f32, f32)> = Vec::new();
    for s in slots {
        if s.half < if s.important { 3.5 } else { 6.0 } {
            continue;
        }
        // Font size follows the smaller dimension, so the text fits vertically
        // on the roof; the width budget follows the roof's actual width.
        let px = (s.half * 0.62).clamp(if s.important { 6.0 } else { 7.0 }, 22.0);
        let avail = (s.half_w * 2.0 * 0.92).max(px * 4.0) as u32;
        // The bundled font has no CJK or Arabic coverage, so baking such a name
        // would produce a row of tofu boxes. Better to draw none.
        if !raster::font_can_render(&s.name) {
            continue;
        }
        let name = fit(&s.name, px, avail);
        if name.is_empty() {
            continue;
        }
        let fg = raster::over(plate, fg, s.fade.clamp(0.0, 1.0) as f64);
        let tw = raster::text_width(&name, px);
        let th = raster::text_height(px);
        let two_line = s.half * 2.0 > th as f32 * 2.4 && !s.size_label.is_empty();
        let top = if two_line { s.cy - th as f32 } else { s.cy - th as f32 * 0.5 };
        let h = if two_line { th as f32 * 2.1 } else { th as f32 };
        let rect = (s.cx - tw as f32 * 0.5, top, s.cx + tw as f32 * 0.5, top + h);
        if taken.iter().any(|t| overlaps(*t, rect)) {
            continue;
        }
        taken.push(rect);
        raster::draw_text(img, rect.0 as i32, top as i32, &name, fg, Some(plate), px);
        if two_line {
            let sp = px * 0.85;
            let sw = raster::text_width(&s.size_label, sp);
            raster::draw_text(
                img,
                (s.cx - sw as f32 * 0.5) as i32,
                (top + th as f32 * 1.05) as i32,
                &s.size_label,
                fg,
                Some(plate),
                sp,
            );
        }
    }
}

pub fn overlaps(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

/// Shorten `s` with an ellipsis until it fits `avail` pixels at size `px`.
fn fit(s: &str, px: f32, avail: u32) -> String {
    if raster::text_width(s, px) <= avail {
        return s.to_string();
    }
    // Measured, not estimated from an average advance width: a name of narrow
    // characters overshoots that estimate, and an overshooting label used to be
    // discarded rather than shortened.
    let chars: Vec<char> = s.chars().collect();
    for n in (1..chars.len()).rev() {
        let mut t: String = chars[..n].iter().collect();
        t.push('…');
        if raster::text_width(&t, px) <= avail {
            return t;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_box(sel: bool) -> SceneBox {
        SceneBox {
            name: "alpha".into(),
            size_label: "1.0 MB".into(),
            min: v3(-0.5, 0.0, -0.5),
            max: v3(0.5, 1.0, 0.5),
            color: (200, 80, 80),
            selected: sel,
            focus: false,
            cursor: false,
            partial: false,
            dim: false,
            fade: 1.0,
        }
    }

    fn count_non_bg(img: &RgbaImage, bg: Rgb) -> usize {
        img.pixels()
            .filter(|p| (p[0], p[1], p[2]) != bg)
            .count()
    }

    #[test]
    fn a_box_is_drawn_and_the_background_survives_around_it() {
        let bg = (10, 10, 12);
        let (img, bounds, _) = render_scene(
            200, 120, &[unit_box(false)], &[], v3(2.5, 2.0, -2.5), v3(0.0, 0.3, 0.0), bg,
            (255, 255, 255), (128, 128, 128), (255, 210, 0), true,
        );
        let drawn = count_non_bg(&img, bg);
        assert!(drawn > 200, "the box covers a real area, got {drawn} px");
        assert!(drawn < (200 * 120) as usize, "and does not fill the whole frame");
        assert!(bounds[0].is_some(), "its silhouette bounds were recorded");
    }

    #[test]
    fn the_roof_is_brighter_than_the_walls() {
        // Face shading is what makes the boxes read as solid rather than flat.
        let roof = face_shade(v3(0.0, 1.0, 0.0));
        let wall_x = face_shade(v3(1.0, 0.0, 0.0));
        let wall_z = face_shade(v3(0.0, 0.0, 1.0));
        assert!(roof > wall_x && wall_x > wall_z, "roof > side > front");
    }

    #[test]
    fn a_fully_occluded_box_contributes_no_pixels() {
        // The z-buffer, not draw order, is what guarantees this: the hidden box
        // is listed *after* the one in front of it.
        let bg = (10, 10, 12);
        let front = SceneBox {
            min: v3(-2.0, 0.0, -1.2), max: v3(2.0, 2.0, -0.8),
            color: (255, 0, 0), name: String::new(), size_label: String::new(),
            selected: false, focus: false, cursor: false, partial: false, dim: false, fade: 1.0,
        };
        let behind = SceneBox {
            min: v3(-0.2, 0.0, 1.0), max: v3(0.2, 0.4, 1.4),
            color: (0, 255, 0), name: String::new(), size_label: String::new(),
            selected: false, focus: false, cursor: false, partial: false, dim: false, fade: 1.0,
        };
        let (img, _, _) = render_scene(
            160, 100, &[front, behind], &[], v3(0.0, 1.0, -6.0), v3(0.0, 0.6, 0.0), bg,
            (255, 255, 255), (128, 128, 128), (255, 210, 0), true,
        );
        let green = img.pixels().filter(|p| p[1] > 120 && p[0] < 80).count();
        assert_eq!(green, 0, "the occluded box must not show through");
    }

    #[test]
    fn a_nearer_box_wins_regardless_of_draw_order() {
        let bg = (0, 0, 0);
        let near = SceneBox {
            min: v3(-0.6, 0.0, -1.6), max: v3(0.6, 1.2, -1.0),
            color: (255, 0, 0), name: String::new(), size_label: String::new(),
            selected: false, focus: false, cursor: false, partial: false, dim: false, fade: 1.0,
        };
        let far = SceneBox {
            min: v3(-0.6, 0.0, 1.0), max: v3(0.6, 1.2, 1.6),
            color: (0, 0, 255), name: String::new(), size_label: String::new(),
            selected: false, focus: false, cursor: false, partial: false, dim: false, fade: 1.0,
        };
        // Draw the near one first: a painter's-algorithm renderer would let the
        // far box overwrite it. The depth buffer must not.
        let (img, _, _) = render_scene(
            160, 100, &[near, far], &[], v3(0.0, 1.2, -6.0), v3(0.0, 0.6, 0.0), bg,
            (255, 255, 255), (128, 128, 128), (255, 210, 0), true,
        );
        let centre = img.get_pixel(80, 55);
        assert!(centre[0] > centre[2], "the nearer (red) box owns the centre");
    }

    /// The cursor highlight has to survive the text-mode fallbacks, which
    /// reduce the scene to brightness alone — so it lights the box up rather
    /// than only recolouring it.
    #[test]
    fn the_cursor_box_is_brighter_not_merely_a_different_colour() {
        let bg = (10, 10, 12);
        let eye = v3(2.5, 2.0, -2.5);
        let at = v3(0.0, 0.3, 0.0);
        let lum = |cursor: bool| -> u64 {
            let mut b = unit_box(false);
            b.name = String::new();
            b.cursor = cursor;
            let (img, _, _) = render_scene(
                200, 120, &[b], &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), true,
            );
            img.pixels()
                .map(|p| p[0] as u64 * 2 + p[1] as u64 * 5 + p[2] as u64)
                .sum()
        };
        assert!(
            lum(true) > lum(false),
            "the highlighted box reads brighter even with colour thrown away"
        );
    }

    /// Within its own silhouette the highlight has to be unmistakable, not a
    /// tint that survives only on a huge raster.
    #[test]
    fn the_cursor_highlight_repaints_most_of_its_own_box() {
        let bg = (10, 10, 12);
        let eye = v3(2.5, 2.0, -2.5);
        let at = v3(0.0, 0.3, 0.0);
        let render = |cursor: bool| {
            let mut b = unit_box(false);
            b.name = String::new();
            b.cursor = cursor;
            render_scene(
                200, 120, &[b], &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), true,
            )
        };
        let (plain, bounds, _) = render(false);
        let (lit, ..) = render(true);
        let (x0, y0, x1, y1) = bounds[0].expect("a silhouette");
        let (mut inside, mut changed) = (0u32, 0u32);
        for y in y0.max(0.0) as u32..(y1.min(119.0) as u32) {
            for x in x0.max(0.0) as u32..(x1.min(199.0) as u32) {
                let p = plain.get_pixel(x, y).0;
                if (p[0], p[1], p[2]) == bg {
                    continue; // silhouette bounds are a box; skip the corners
                }
                inside += 1;
                if lit.get_pixel(x, y).0 != p {
                    changed += 1;
                }
            }
        }
        assert!(inside > 100, "found the box ({inside} px)");
        let frac = changed as f32 / inside as f32;
        assert!(frac > 0.9, "only {:.0}% of the box changed", frac * 100.0);
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        let bg = (0, 0, 0);
        for (w, h) in [(0u32, 0u32), (1, 1), (1, 40), (40, 1)] {
            let _ = render_scene(
                w, h, &[unit_box(true)], &[], v3(2.0, 2.0, -2.0), v3(0.0, 0.0, 0.0), bg,
                (255, 255, 255), (128, 128, 128), (255, 210, 0), true,
            );
        }
    }

    #[test]
    fn a_camera_inside_the_scene_does_not_panic() {
        // Points behind the eye plane must be dropped, not projected to
        // wild coordinates.
        let bg = (0, 0, 0);
        let _ = render_scene(
            80, 60, &[unit_box(false)], &[], v3(0.0, 0.5, 0.0), v3(1.0, 0.5, 0.0), bg,
            (255, 255, 255), (128, 128, 128), (255, 210, 0), true,
        );
    }

    #[test]
    fn selection_brightens_the_silhouette() {
        let bg = (10, 10, 12);
        let eye = v3(2.5, 2.0, -2.5);
        let at = v3(0.0, 0.3, 0.0);
        let (plain, ..) =
            render_scene(200, 120, &[unit_box(false)], &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), true);
        let (sel, ..) =
            render_scene(200, 120, &[unit_box(true)], &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), true);
        let lum = |i: &RgbaImage| -> u64 {
            i.pixels().map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64).sum()
        };
        assert!(lum(&sel) > lum(&plain), "the selected box is outlined brighter");
    }

    /// Ink drawn by a scene: how much it differs from the same scene with the
    /// names stripped. At these sizes every glyph pixel is partial coverage, so
    /// counting "white" pixels would prove nothing.
    fn label_ink(boxes: &[SceneBox], eye: V3, at: V3) -> u64 {
        let bg = (10, 10, 12);
        let render = |bs: &[SceneBox]| {
            render_scene(260, 160, bs, &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), true).0
        };
        let bare: Vec<SceneBox> = boxes
            .iter()
            .cloned()
            .map(|mut b| {
                b.name = String::new();
                b.size_label = String::new();
                b
            })
            .collect();
        let (a, b) = (render(boxes), render(&bare));
        a.pixels()
            .zip(b.pixels())
            .map(|(p, q)| {
                (p[0] as i32 - q[0] as i32).unsigned_abs() as u64
                    + (p[1] as i32 - q[1] as i32).unsigned_abs() as u64
                    + (p[2] as i32 - q[2] as i32).unsigned_abs() as u64
            })
            .sum()
    }

    fn labelled_box(name: &str, half: f32, focus: bool) -> SceneBox {
        SceneBox {
            name: name.into(),
            size_label: String::new(),
            min: v3(-half, 0.0, -half),
            max: v3(half, half * 2.0, half),
            color: (90, 90, 100),
            selected: false,
            focus,
            cursor: false,
            partial: false,
            dim: false,
            fade: 1.0,
        }
    }

    /// A box the view is *about* is named even when it is too small to have
    /// earned the room — it is the one label the user actually needs.
    #[test]
    fn the_focused_box_is_labelled_even_when_it_is_small() {
        let (eye, at) = (v3(3.0, 2.6, -3.0), v3(0.0, 0.2, 0.0));
        let small = 0.22;
        assert!(
            label_ink(&[labelled_box("midi", small, true)], eye, at) > 0,
            "the focus gets its name"
        );
        assert_eq!(
            label_ink(&[labelled_box("midi", small, false)], eye, at),
            0,
            "an ordinary box that small does not"
        );
    }

    /// The box under the other panel's cursor is named even when it is small —
    /// the one highlight that reads at any resolution.
    #[test]
    fn the_cursor_box_is_named_even_when_it_is_small() {
        let (eye, at) = (v3(3.0, 2.6, -3.0), v3(0.0, 0.2, 0.0));
        let small = 0.22;
        let mut b = labelled_box("photos", small, false);
        b.cursor = true;
        assert!(label_ink(std::slice::from_ref(&b), eye, at) > 0, "the cursor box is named");
        b.cursor = false;
        assert_eq!(
            label_ink(std::slice::from_ref(&b), eye, at),
            0,
            "an ordinary box that small is not"
        );
    }

    /// A roof seen at an angle is far wider than it is tall, so a name budgeted
    /// against the smaller dimension gets cut long before it needs to be.
    #[test]
    fn a_label_is_budgeted_against_the_roof_width_not_its_squashed_height() {
        let b = labelled_box("alpha", 0.5, false);
        let (_, _, slots) = render_scene(
            260, 160, std::slice::from_ref(&b), &[], v3(2.5, 2.0, -2.5), v3(0.0, 0.4, 0.0),
            (10, 10, 12), (255, 255, 255), (128, 128, 128), (255, 210, 0), false,
        );
        let s = slots.first().expect("a slot");
        assert!(s.half_w >= s.half, "width is never the smaller of the two");
        assert!(
            s.half_w > s.half * 1.5,
            "at this angle the roof is much wider than tall: {} vs {}",
            s.half_w,
            s.half
        );
    }

    /// A roomy box is named whatever angle it is seen from. Measuring the roof
    /// from a single corner used to collapse to nothing at some angles and drop
    /// the label from a box with plenty of space on it.
    #[test]
    fn a_roomy_box_is_labelled_from_every_angle() {
        let b = labelled_box("alpha", 0.5, false);
        for k in 0..8 {
            let a = k as f32 / 8.0 * std::f32::consts::TAU;
            let eye = v3(a.cos() * 3.2, 2.4, a.sin() * 3.2);
            assert!(
                label_ink(std::slice::from_ref(&b), eye, v3(0.0, 0.4, 0.0)) > 0,
                "no label at yaw {k}/8"
            );
        }
    }

    /// Loosening the anchor's depth tolerance must not let a name show through
    /// the box standing in front of it.
    #[test]
    fn a_hidden_box_keeps_its_name_hidden() {
        let (eye, at) = (v3(0.0, 5.0, -5.0), v3(0.0, 0.5, 0.0));
        let behind = SceneBox {
            name: "hidden".into(),
            size_label: String::new(),
            min: v3(-1.0, 0.0, 1.0),
            max: v3(1.0, 1.2, 3.0),
            color: (90, 90, 100),
            selected: false,
            focus: false,
            cursor: false,
            partial: false,
            dim: false,
            fade: 1.0,
        };
        // On its own it is named…
        assert!(label_ink(std::slice::from_ref(&behind), eye, at) > 0, "named when visible");
        // …but not once a wall is put between it and the camera.
        let wall = SceneBox {
            name: String::new(),
            size_label: String::new(),
            min: v3(-4.0, 0.0, -1.0),
            max: v3(4.0, 4.0, -0.6),
            color: (200, 60, 60),
            selected: false,
            focus: false,
            cursor: false,
            partial: false,
            dim: false,
            fade: 1.0,
        };
        assert_eq!(
            label_ink(&[wall, behind], eye, at),
            0,
            "a name behind a nearer box must not show through it"
        );
    }

    /// With many boxes on screen, overlapping names are worse than no name, so a
    /// label that would land on one already drawn is dropped.
    #[test]
    fn labels_do_not_pile_up_on_top_of_each_other() {
        assert!(overlaps((0.0, 0.0, 10.0, 10.0), (5.0, 5.0, 15.0, 15.0)));
        assert!(!overlaps((0.0, 0.0, 10.0, 10.0), (10.5, 0.0, 20.0, 10.0)));
        let (eye, at) = (v3(2.5, 2.0, -2.5), v3(0.0, 0.4, 0.0));
        let one = labelled_box("alpha", 0.5, false);
        // A second box in exactly the same place: its name must be suppressed
        // rather than stacked on the first.
        let two = [labelled_box("alpha", 0.5, false), labelled_box("bravo", 0.5, false)];
        let a = label_ink(std::slice::from_ref(&one), eye, at);
        assert!(a > 0, "the single box is labelled");
        assert!(label_ink(&two, eye, at) <= a, "a second overlapping label was dropped");
    }

    #[test]
    fn long_names_are_ellipsized_to_fit() {
        let short = fit("abc", 12.0, 1000);
        assert_eq!(short, "abc", "a name that fits is untouched");
        let cut = fit("a-very-long-directory-name-indeed", 12.0, 40);
        assert!(cut.chars().count() < 33 && cut.ends_with('…'), "got {cut:?}");
    }
}



