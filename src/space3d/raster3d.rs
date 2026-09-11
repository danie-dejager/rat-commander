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
    /// The solid to draw inside `min..max`. Everything the Cubes style draws is
    /// a [`Shape::Block`]; the fsn style uses the rest to say, by silhouette
    /// alone, what kind of file a solid stands for.
    pub shape: Shape,
}

/// The solid a [`SceneBox`] is drawn as, inside its own bounding box.
///
/// Every one of these keeps its full height, so a file's size still reads from
/// how tall its solid is whatever kind of file it is; what changes is the
/// cross-section and how the top is finished. They are meant to be told apart
/// by outline in a panel-sized raster, where colour alone is a few pixels and
/// text is nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Shape {
    /// The full axis-aligned box. Directories, platforms, and plain files.
    #[default]
    Block,
    /// A thin upright slab: a sheet of paper standing on edge. Documents.
    Sheet,
    /// An octagonal prism — a drum or barrel. Archives.
    Drum,
    /// A box tapering to a smaller top — a truncated pyramid. Images.
    Frustum,
    /// A triangular prism, ridged along Z, like a roof. Audio and video.
    Wedge,
    /// A square base tapering to a point. Programs and libraries.
    Pyramid,
}

/// Sky and ground colours for the fsn style's backdrop.
///
/// The scene stands on a ground plane under an open sky, and both are painted
/// before any geometry, at depth zero — infinitely far away — so every solid,
/// platform and link wins the depth test over them without the rasterizer
/// needing to know they are there.
#[derive(Debug, Clone, Copy)]
pub struct Sky {
    pub top: Rgb,
    pub horizon: Rgb,
    pub ground_near: Rgb,
    pub ground_far: Rgb,
}

/// Vertical field of view.
pub(crate) const FOV_Y: f32 = std::f32::consts::PI / 3.0;

/// Face brightness by orientation. Fixed per axis rather than computed from a
/// light vector: it gives the crisp, legible "city block" read, and the roof —
/// which carries the label — is always the brightest surface.
fn face_shade(n: V3) -> f64 {
    if n.y.abs() > 0.5 {
        if n.y > 0.0 { 1.12 } else { 0.35 }
    } else {
        // Blended rather than stepped, because the fsn shapes have slanted
        // walls — a drum's eight sides and a pyramid's four — and a step would
        // band them. Exactly 0.58 at ±Z and 0.78 at ±X, so an axis-aligned box
        // is shaded precisely as it always was.
        0.58 + 0.20 * n.x.abs() as f64
    }
}

/// A solid's faces, as corner quads with outward normals.
///
/// Triangles come back as quads with a doubled corner: `fill_quad` skips any
/// edge that does not cross the scanline, and a zero-height edge never does, so
/// they rasterize correctly with no second primitive.
fn shape_faces(shape: Shape, min: V3, max: V3) -> Vec<([V3; 4], V3)> {
    match shape {
        Shape::Block => faces(min, max).to_vec(),
        // Thinned along Z about its own centre: full height and width, so the
        // size still reads, but edge-on it is a sheet rather than a slab.
        Shape::Sheet => {
            let cz = (min.z + max.z) * 0.5;
            let t = (max.z - min.z) * 0.16;
            faces(v3(min.x, min.y, cz - t), v3(max.x, max.y, cz + t)).to_vec()
        }
        Shape::Drum => ngon_prism(min, max, 8, 0.0),
        // Sloped sides but a flat top: unmistakably neither a box nor a cone,
        // which is what the other two nearby shapes are.
        Shape::Frustum => frustum(min, max, 0.62),
        Shape::Wedge => wedge(min, max),
        Shape::Pyramid => pyramid(min, max),
    }
}

/// A prism with `sides` equal faces, inscribed in the box's footprint.
fn ngon_prism(min: V3, max: V3, sides: usize, phase: f32) -> Vec<([V3; 4], V3)> {
    let (cx, cz) = ((min.x + max.x) * 0.5, (min.z + max.z) * 0.5);
    let (rx, rz) = ((max.x - min.x) * 0.5, (max.z - min.z) * 0.5);
    let pt = |i: usize| {
        let a = phase + std::f32::consts::TAU * i as f32 / sides as f32;
        (cx + a.cos() * rx, cz + a.sin() * rz)
    };
    let mut out = Vec::with_capacity(sides * 3);
    for i in 0..sides {
        let (x0, z0) = pt(i);
        let (x1, z1) = pt((i + 1) % sides);
        // Outward normal of the wall, in the ground plane.
        let n = v3(z1 - z0, 0.0, -(x1 - x0)).norm();
        out.push(([v3(x0, min.y, z0), v3(x1, min.y, z1), v3(x1, max.y, z1), v3(x0, max.y, z0)], n));
    }
    // Caps, as triangle fans from the centre.
    for i in 0..sides {
        let (x0, z0) = pt(i);
        let (x1, z1) = pt((i + 1) % sides);
        out.push((
            [v3(cx, max.y, cz), v3(x0, max.y, z0), v3(x1, max.y, z1), v3(cx, max.y, cz)],
            v3(0.0, 1.0, 0.0),
        ));
        out.push((
            [v3(cx, min.y, cz), v3(x1, min.y, z1), v3(x0, min.y, z0), v3(cx, min.y, cz)],
            v3(0.0, -1.0, 0.0),
        ));
    }
    out
}

/// A box whose top face is shrunk to `top_scale` of its base, so its four walls
/// lean inward.
fn frustum(min: V3, max: V3, top_scale: f32) -> Vec<([V3; 4], V3)> {
    let (a, b) = (min, max);
    let (cx, cz) = ((a.x + b.x) * 0.5, (a.z + b.z) * 0.5);
    let (hx, hz) = ((b.x - a.x) * 0.5 * top_scale, (b.z - a.z) * 0.5 * top_scale);
    let base = [v3(a.x, a.y, a.z), v3(b.x, a.y, a.z), v3(b.x, a.y, b.z), v3(a.x, a.y, b.z)];
    let top = [
        v3(cx - hx, b.y, cz - hz),
        v3(cx + hx, b.y, cz - hz),
        v3(cx + hx, b.y, cz + hz),
        v3(cx - hx, b.y, cz + hz),
    ];
    let mut out = vec![
        ([base[0], base[3], base[2], base[1]], v3(0.0, -1.0, 0.0)),
        ([top[0], top[1], top[2], top[3]], v3(0.0, 1.0, 0.0)),
    ];
    for i in 0..4 {
        let (p0, p1) = (base[i], base[(i + 1) % 4]);
        let (q1, q0) = (top[(i + 1) % 4], top[i]);
        // `up × along`, not the other way about: the base winds clockwise seen
        // from above, so the reverse order points every wall *into* the solid
        // and back-face culling then keeps the far side and drops the near one
        // — the shape stays the right silhouette but you see through it.
        let n = q0.sub(p0).cross(p1.sub(p0)).norm();
        out.push(([p0, p1, q1, q0], n));
    }
    out
}

/// A triangular prism — a pitched roof — ridged along **Z**.
///
/// Along Z rather than X on purpose: the camera starts out looking down the Z
/// axis, so the end of the prism faces it and the shape reads as the triangle
/// it is. Ridged the other way it presents its rectangular slope to the camera
/// and is indistinguishable from a plain box at the size these are drawn.
fn wedge(min: V3, max: V3) -> Vec<([V3; 4], V3)> {
    let (a, b) = (min, max);
    let cx = (a.x + b.x) * 0.5;
    let (rise, run) = (b.y - a.y, (b.x - a.x) * 0.5);
    let n_px = v3(rise, run, 0.0).norm();
    let n_nx = v3(-rise, run, 0.0).norm();
    vec![
        // Base.
        (
            [v3(a.x, a.y, a.z), v3(a.x, a.y, b.z), v3(b.x, a.y, b.z), v3(b.x, a.y, a.z)],
            v3(0.0, -1.0, 0.0),
        ),
        // The two slopes, meeting along the ridge at x = cx.
        ([v3(b.x, a.y, a.z), v3(b.x, a.y, b.z), v3(cx, b.y, b.z), v3(cx, b.y, a.z)], n_px),
        ([v3(a.x, a.y, b.z), v3(a.x, a.y, a.z), v3(cx, b.y, a.z), v3(cx, b.y, b.z)], n_nx),
        // Triangular ends.
        (
            [v3(a.x, a.y, a.z), v3(b.x, a.y, a.z), v3(cx, b.y, a.z), v3(cx, b.y, a.z)],
            v3(0.0, 0.0, -1.0),
        ),
        (
            [v3(b.x, a.y, b.z), v3(a.x, a.y, b.z), v3(cx, b.y, b.z), v3(cx, b.y, b.z)],
            v3(0.0, 0.0, 1.0),
        ),
    ]
}

/// A square base tapering to a point at the top of the box.
fn pyramid(min: V3, max: V3) -> Vec<([V3; 4], V3)> {
    let (a, b) = (min, max);
    let apex = v3((a.x + b.x) * 0.5, b.y, (a.z + b.z) * 0.5);
    let mut out = vec![(
        [v3(a.x, a.y, a.z), v3(a.x, a.y, b.z), v3(b.x, a.y, b.z), v3(b.x, a.y, a.z)],
        v3(0.0, -1.0, 0.0),
    )];
    let base = [v3(a.x, a.y, a.z), v3(b.x, a.y, a.z), v3(b.x, a.y, b.z), v3(a.x, a.y, b.z)];
    for i in 0..4 {
        let (p0, p1) = (base[i], base[(i + 1) % 4]);
        // Outward normal of the triangle p0 → p1 → apex. See `frustum` for why
        // the operands are this way round.
        let n = apex.sub(p0).cross(p1.sub(p0)).norm();
        out.push(([p0, p1, apex, apex], n));
    }
    out
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
    // `Some` in the fsn style: paint a ground plane under an open sky instead
    // of a flat background.
    sky: Option<Sky>,
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

    if let Some(sky) = sky {
        paint_backdrop(&mut img, sky, &basis, focal, fw, fh);
    }

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

    // How far away a solid has to be before the air starts to take its colour.
    // Only meaningful with a sky: it is the horizon it fades toward.
    let haze_ref = sky.map(|_| (eye.sub(target).len() * 1.15).max(0.6));

    for (bi, b) in boxes.iter().enumerate() {
        let mut base = if b.partial { raster::over(bg, b.color, 0.72) } else { b.color };
        // Distance haze: a far platform is seen through more air than a near
        // one, which is what makes the ground read as stretching away rather
        // than as a flat diagram. Per solid rather than per pixel — the scene is
        // rebuilt often enough that per-pixel would not pay for itself.
        if let (Some(sky), Some(href)) = (sky, haze_ref) {
            let centre = b.min.add(b.max).scale(0.5);
            let d = eye.sub(centre).len();
            let t = ((d / href - 0.55) * 0.55).clamp(0.0, 0.55);
            if t > 0.001 {
                base = raster::over(base, sky.horizon, t as f64);
            }
        }
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
        for (quad, normal) in shape_faces(b.shape, b.min, b.max) {
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

/// Where the horizon lies, as a raster row, for the current camera.
///
/// `look_at` is always handed world-up `+Y` and the camera never rolls, so the
/// horizon is a *horizontal* line: every direction along the ground is some mix
/// of the view's `right` (which is horizontal by construction) and the
/// horizontal part of `fwd`, and both project to the same row. That means one
/// number describes it, and the backdrop is two vertical gradients rather than
/// a rasterized plane.
///
/// The row is exactly where [`vec3::project`] would put a ground point
/// infinitely far away, so the ground painted below it and the geometry
/// standing on it agree at the join.
pub fn horizon_row(basis: &vec3::Basis, focal: f32, h: f32) -> f32 {
    let flat = v3(basis.fwd.x, 0.0, basis.fwd.z);
    let dir = if flat.len() < 1e-6 { basis.right } else { flat.norm() };
    let vz = dir.dot(basis.fwd);
    if vz <= 1e-4 {
        // Looking straight down: the horizon is off the top of the frame.
        return f32::NEG_INFINITY;
    }
    h * 0.5 - dir.dot(basis.up) * focal / vz
}

/// Paint the sky and the ground, before any geometry.
///
/// Both are left at depth zero, so anything with real depth draws over them.
/// Nothing is ever *behind* the ground — the camera's pitch is clamped above
/// zero, so the eye cannot drop below the plane the platforms stand on.
fn paint_backdrop(img: &mut RgbaImage, sky: Sky, basis: &vec3::Basis, focal: f32, fw: f32, fh: f32) {
    let hy = horizon_row(basis, focal, fh);
    for y in 0..img.height() {
        let yc = y as f32 + 0.5;
        let c = if yc < hy {
            // Sky: deepest overhead, palest where it meets the ground. Measured
            // against the frame rather than the horizon so the ramp does not
            // stretch and snap as the camera pitches.
            let t = (yc / hy.max(1.0)).clamp(0.0, 1.0);
            raster::over(sky.top, sky.horizon, t as f64)
        } else {
            // Ground: hazed at the horizon, its own colour underfoot.
            let span = (fh - hy).max(1.0);
            let t = ((yc - hy) / span).clamp(0.0, 1.0);
            raster::over(sky.ground_far, sky.ground_near, t as f64)
        };
        for x in 0..img.width() {
            raster::put(img, x, y, c);
        }
    }
    let _ = fw;
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

/// Where a name sits on its box: the middle of the roof, except on a slab so
/// flat that things stand on top of it — an fsn platform — where it moves out to
/// the edge facing the camera.
///
/// Two reasons, and the first is fatal on its own: the roof centre of a
/// platform is exactly where its file grid stands, so the visibility test in
/// [`label_slots`] finds a solid in front of the label point and drops the name
/// from every platform that has any files on it. The second is that it is
/// simply where fsn writes them — on the ground in front of the pedestal, clear
/// of its contents.
fn label_anchor(b: &SceneBox, eye: V3) -> V3 {
    let c = b.min.add(b.max).scale(0.5);
    let (hw, hd) = ((b.max.x - b.min.x) * 0.5, (b.max.z - b.min.z) * 0.5);
    let hh = (b.max.y - b.min.y) * 0.5;
    let flat = hh < hw.min(hd) * 0.5;
    let to_eye = v3(eye.x - c.x, 0.0, eye.z - c.z);
    if !flat || to_eye.len() < 1e-4 {
        return v3(c.x, b.max.y, c.z);
    }
    // Not quite at the rim, so a name a little wider than its platform still
    // looks anchored to it rather than adrift on the ground.
    let d = to_eye.norm();
    v3(c.x + d.x * hw * 0.74, b.max.y, c.z + d.z * hd * 0.74)
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
        let anchor = label_anchor(b, basis.eye);
        let Some((cx, cy, cz)) = vec3::project(vec3::to_view(basis, anchor), fw, fh, focal) else {
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
            shape: Shape::Block,
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
            (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true,
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
            shape: Shape::Block,
        };
        let behind = SceneBox {
            min: v3(-0.2, 0.0, 1.0), max: v3(0.2, 0.4, 1.4),
            color: (0, 255, 0), name: String::new(), size_label: String::new(),
            selected: false, focus: false, cursor: false, partial: false, dim: false, fade: 1.0,
            shape: Shape::Block,
        };
        let (img, _, _) = render_scene(
            160, 100, &[front, behind], &[], v3(0.0, 1.0, -6.0), v3(0.0, 0.6, 0.0), bg,
            (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true,
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
            shape: Shape::Block,
        };
        let far = SceneBox {
            min: v3(-0.6, 0.0, 1.0), max: v3(0.6, 1.2, 1.6),
            color: (0, 0, 255), name: String::new(), size_label: String::new(),
            selected: false, focus: false, cursor: false, partial: false, dim: false, fade: 1.0,
            shape: Shape::Block,
        };
        // Draw the near one first: a painter's-algorithm renderer would let the
        // far box overwrite it. The depth buffer must not.
        let (img, _, _) = render_scene(
            160, 100, &[near, far], &[], v3(0.0, 1.2, -6.0), v3(0.0, 0.6, 0.0), bg,
            (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true,
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
                200, 120, &[b], &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true,
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
                200, 120, &[b], &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true,
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
                (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true,
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
            (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true,
        );
    }

    #[test]
    fn selection_brightens_the_silhouette() {
        let bg = (10, 10, 12);
        let eye = v3(2.5, 2.0, -2.5);
        let at = v3(0.0, 0.3, 0.0);
        let (plain, ..) =
            render_scene(200, 120, &[unit_box(false)], &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true);
        let (sel, ..) =
            render_scene(200, 120, &[unit_box(true)], &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true);
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
            render_scene(260, 160, bs, &[], eye, at, bg, (255, 255, 255), (128, 128, 128), (255, 210, 0), None, true).0
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
            shape: Shape::Block,
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
            (10, 10, 12), (255, 255, 255), (128, 128, 128), (255, 210, 0), None, false,
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
            shape: Shape::Block,
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
            shape: Shape::Block,
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
    /// Every shape has to be a closed solid with its faces turned *outward*.
    ///
    /// An inward-facing normal is not a visible crash: back-face culling simply
    /// keeps the far side of the solid and drops the near one, so it still fills
    /// the right silhouette but is lit wrongly and things behind it show
    /// through. That is exactly the bug this catches.
    #[test]
    fn every_shape_turns_its_faces_outward() {
        let (min, max) = (v3(-0.5, 0.0, -0.5), v3(0.5, 1.0, 0.5));
        let centre = min.add(max).scale(0.5);
        for shape in [
            Shape::Block,
            Shape::Sheet,
            Shape::Drum,
            Shape::Frustum,
            Shape::Wedge,
            Shape::Pyramid,
        ] {
            let faces = shape_faces(shape, min, max);
            assert!(
                faces.len() >= 4,
                "{shape:?} is not a solid: {} faces",
                faces.len()
            );
            for (quad, n) in &faces {
                assert!(
                    (n.len() - 1.0).abs() < 1e-3,
                    "{shape:?} has an unnormalised normal"
                );
                // A face's own centre, measured from the middle of the solid,
                // must lie along its normal rather than against it.
                let fc = quad
                    .iter()
                    .fold(v3(0.0, 0.0, 0.0), |a, &p| a.add(p))
                    .scale(0.25);
                let out = fc.sub(centre);
                assert!(
                    n.dot(out) > -1e-4,
                    "{shape:?} has a face pointing into the solid (dot {})",
                    n.dot(out)
                );
            }
        }
    }

    #[test]
    fn every_shape_stays_inside_the_box_it_was_given() {
        let (min, max) = (v3(-0.5, 0.0, -0.5), v3(0.5, 1.0, 0.5));
        for shape in [
            Shape::Sheet,
            Shape::Drum,
            Shape::Frustum,
            Shape::Wedge,
            Shape::Pyramid,
        ] {
            for (quad, _) in shape_faces(shape, min, max) {
                for p in quad {
                    assert!(
                        p.x >= min.x - 1e-4
                            && p.x <= max.x + 1e-4
                            && p.y >= min.y - 1e-4
                            && p.y <= max.y + 1e-4
                            && p.z >= min.z - 1e-4
                            && p.z <= max.z + 1e-4,
                        "{shape:?} pokes out of its own bounds"
                    );
                }
            }
        }
    }

    /// Height is what carries a file's size, so no shape may give it away.
    #[test]
    fn every_shape_reaches_the_full_height_of_its_box() {
        let (min, max) = (v3(-0.5, 0.0, -0.5), v3(0.5, 1.0, 0.5));
        for shape in [
            Shape::Block,
            Shape::Sheet,
            Shape::Drum,
            Shape::Frustum,
            Shape::Wedge,
            Shape::Pyramid,
        ] {
            let top = shape_faces(shape, min, max)
                .iter()
                .flat_map(|(q, _)| q.iter().map(|p| p.y))
                .fold(f32::MIN, f32::max);
            assert!((top - max.y).abs() < 1e-4, "{shape:?} stops short at {top}");
        }
    }

    #[test]
    fn the_shapes_are_told_apart_by_silhouette_not_only_by_colour() {
        // The cell fallbacks reduce the scene to brightness alone, so the
        // outline has to do the work on its own. Same box, same colour, same
        // camera: only the pixel count differs.
        let bg = (10, 10, 12);
        let drawn = |shape: Shape| {
            let b = SceneBox {
                name: String::new(),
                size_label: String::new(),
                min: v3(-0.5, 0.0, -0.5),
                max: v3(0.5, 1.0, 0.5),
                color: (220, 220, 220),
                selected: false,
                focus: false,
                cursor: false,
                partial: false,
                dim: false,
                fade: 1.0,
                shape,
            };
            let (img, _, _) = render_scene(
                160,
                160,
                std::slice::from_ref(&b),
                &[],
                v3(0.9, 1.1, -2.6),
                v3(0.0, 0.5, 0.0),
                bg,
                (255, 255, 255),
                (128, 128, 128),
                (255, 210, 0),
                None,
                false,
            );
            count_non_bg(&img, bg)
        };
        let block = drawn(Shape::Block);
        for shape in [
            Shape::Sheet,
            Shape::Drum,
            Shape::Frustum,
            Shape::Wedge,
            Shape::Pyramid,
        ] {
            let n = drawn(shape);
            assert!(n > 0, "{shape:?} drew nothing at all");
            let diff = (n as f32 - block as f32).abs() / block as f32;
            assert!(
                diff > 0.05,
                "{shape:?} covers all but {diff:.3} of what a block does"
            );
        }
    }

    #[test]
    fn the_horizon_is_level_whichever_way_along_the_ground_you_look() {
        // The backdrop is two vertical gradients meeting on one row, which is
        // only right because the camera never rolls.
        let focal = vec3::focal_for(200.0, FOV_Y);
        let target = v3(0.0, 0.0, 0.0);
        let mut rows = Vec::new();
        for i in 0..12 {
            let yaw = i as f32 * std::f32::consts::TAU / 12.0;
            let eye = v3(yaw.cos() * 3.0, 1.0, yaw.sin() * 3.0);
            let basis = vec3::look_at(eye, target, v3(0.0, 1.0, 0.0));
            rows.push(horizon_row(&basis, focal, 200.0));
        }
        let lo = rows.iter().copied().fold(f32::MAX, f32::min);
        let hi = rows.iter().copied().fold(f32::MIN, f32::max);
        assert!(
            hi - lo < 0.05,
            "the horizon wanders as the camera turns: {lo}..{hi}"
        );
    }

    #[test]
    fn looking_down_more_steeply_pushes_the_horizon_up_the_frame() {
        let focal = vec3::focal_for(200.0, FOV_Y);
        let target = v3(0.0, 0.0, 0.0);
        let row_at = |pitch: f32| {
            let eye = v3(0.0, pitch.sin() * 3.0, -pitch.cos() * 3.0);
            horizon_row(&vec3::look_at(eye, target, v3(0.0, 1.0, 0.0)), focal, 200.0)
        };
        assert!(
            row_at(0.6) < row_at(0.2),
            "a higher camera sees more ground"
        );
    }

    #[test]
    fn the_backdrop_paints_sky_over_ground_and_geometry_draws_on_top_of_both() {
        let sky = Sky {
            top: (20, 40, 160),
            horizon: (200, 225, 250),
            ground_far: (90, 150, 90),
            ground_near: (20, 60, 25),
        };
        let b = SceneBox {
            name: String::new(),
            size_label: String::new(),
            min: v3(-0.6, 0.0, -0.4),
            max: v3(0.6, 0.9, 0.4),
            color: (255, 0, 0),
            selected: false,
            focus: false,
            cursor: false,
            partial: false,
            dim: false,
            fade: 1.0,
            shape: Shape::Block,
        };
        let (img, _, _) = render_scene(
            200,
            200,
            std::slice::from_ref(&b),
            &[],
            v3(0.0, 1.1, -3.0),
            v3(0.0, 0.4, 0.0),
            (10, 10, 12),
            (255, 255, 255),
            (128, 128, 128),
            (255, 210, 0),
            Some(sky),
            false,
        );
        // Nothing is left showing the flat background: the backdrop covers every
        // pixel the geometry does not.
        let flat = img
            .pixels()
            .filter(|p| (p[0], p[1], p[2]) == (10, 10, 12))
            .count();
        assert_eq!(
            flat, 0,
            "the plain background must not show through the backdrop"
        );
        // Bluer up top than down below — that is the sky/ground split.
        let blueness = |y: u32| {
            let p = img.get_pixel(100, y);
            p[2] as i32 - p[1] as i32
        };
        assert!(
            blueness(4) > blueness(196),
            "the sky is above and the ground below"
        );
        // And the box still wins over both.
        assert!(
            img.pixels().any(|p| p[0] > 180 && p[1] < 80),
            "the box is drawn over the backdrop"
        );
    }
}



