//! Cast shadows for the software rasterizer.
//!
//! A pass over the finished picture rather than a step in filling faces: once
//! the geometry is down, the depth buffer already says where every visible
//! surface is, so each pixel is lifted back into world space and asked whether
//! anything stands between it and the sun. Doing it afterwards shades each pixel
//! once, where shading while filling would pay for every face that is later
//! drawn over — and a 400 000-triangle model is mostly that.
//!
//! The "anything in between" is a shadow map: the casters rendered from the
//! sun's side, orthographically, keeping per texel how far toward the sun the
//! nearest one reaches. Its extent is fitted to the receivers actually on screen
//! (clipped to where casters exist), so the resolution goes where the picture
//! is rather than being spread over a whole scene the camera has zoomed into
//! one corner of. A scene seen across a receding ground plane gets more than one
//! map, split by distance, since one fitted to the horizon would leave the
//! foreground in blocks.

use super::raster3d::scan_quad;
use super::vec3::{self, Basis, V3, v3};
use image::RgbaImage;

/// What the surface drawn at one pixel asks of the shadow pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surf {
    /// The most a full shadow may darken this pixel, as a fraction of 255 —
    /// the share of its brightness the sun accounts for. Zero leaves the pixel
    /// alone: lines, outlines and faces turned away from the sun.
    pub take: u8,
    /// Cosine between the surface normal and the sun, as a fraction of 255.
    /// Sets the depth bias: a surface met at a glancing angle changes depth
    /// fast across one texel and needs more slack to not shadow itself.
    pub facing: u8,
}

impl Surf {
    /// A pixel the shadow pass must not touch.
    pub const NONE: Surf = Surf { take: 0, facing: 0 };

    pub fn new(take: f32, facing: f32) -> Surf {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Surf { take: q(take), facing: q(facing.abs()) }
    }
}

/// How the picture is lit.
pub struct Light<'a> {
    /// Unit vector pointing *toward* the sun.
    pub sun: V3,
    /// View depth at which each shadow map gives way to the next, nearest
    /// first. The last is how far shadows reach at all; a finite one is faded
    /// out over its final stretch rather than cut off at a line.
    pub cascades: &'a [f32],
    /// A ground plane at this height that receives shadows wherever nothing
    /// was drawn over it (depth zero), presenting this surface.
    pub ground: Option<(f32, Surf)>,
}

/// Hands every shadow-casting face to the callback it is given, with the face's
/// opacity. Walked more than once, so it must hand out the same faces each time.
pub type Casters<'a> = dyn Fn(&mut dyn FnMut(&[V3; 4], u8)) + 'a;

/// Where the last cascade starts fading, as a fraction of its reach.
const FADE_FROM: f32 = 0.75;

/// Depth bias in texels: a constant part, and a part scaled by the tangent of
/// the angle the surface meets the sun at.
const BIAS_TEXELS: f32 = 1.0;
const BIAS_SLOPE: f32 = 2.0;

/// Darken `img` wherever a caster stands between a visible surface and the sun.
///
/// `depth` and `surf` are the buffers the picture was drawn with.
pub fn cast(
    img: &mut RgbaImage,
    depth: &[f32],
    surf: &[Surf],
    basis: &Basis,
    focal: f32,
    light: &Light,
    casters: &Casters,
) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 || light.cascades.is_empty() {
        return;
    }
    let sun = light.sun.norm();
    // The sun's own frame: `right` and `up` span the map, `sun` is height.
    let frame = vec3::look_at(v3(0.0, 0.0, 0.0), sun.scale(-1.0), v3(0.0, 1.0, 0.0));
    let view = View { w, h, depth, surf, basis, focal, ground: light.ground };

    // Where anything could cast a shadow at all. The ground runs to the
    // horizon, but a shadow can only fall inside the casters' footprint as the
    // sun sees it.
    let mut cast_by = Rect::EMPTY;
    casters(&mut |q, _| {
        for p in q {
            cast_by.add(p.dot(frame.right), p.dot(frame.up));
        }
    });
    if cast_by.is_empty() {
        return;
    }
    // Where the receivers inside that lie, per cascade, and how near the
    // nearest of them is. Each row's span of them is kept too, so the shading
    // pass can skip the rest: under a model, most of the floor is nowhere near
    // its shadow.
    let mut wanted = vec![(Rect::EMPTY, f32::INFINITY); light.cascades.len()];
    let mut spans = vec![(usize::MAX, 0usize); h];
    view.each(&frame, sun, None, |idx, p, z, _| {
        if cast_by.contains(p.u, p.v)
            && let Some(c) = light.cascades.iter().position(|&lim| z <= lim)
        {
            wanted[c].0.add(p.u, p.v);
            wanted[c].1 = wanted[c].1.min(z);
            let (y, x) = (idx / w, idx % w);
            spans[y] = (spans[y].0.min(x), spans[y].1.max(x + 1));
        }
    });
    // At most about one texel per pixel along the picture's long edge…
    let side = w.max(h).clamp(96, 1600);
    let mut maps: Vec<Option<ShadowMap>> = wanted
        .iter()
        // …and no finer than a pixel is wide at the nearest receiver, which
        // is as fine as any of them can show. A model filling a third of the
        // picture then gets a map a third of the size, not one as large as
        // the picture drawn at a sixth of the pixel.
        .map(|&(r, near)| ShadowMap::fitted(r, side, near / focal))
        .collect();
    if maps.iter().all(Option::is_none) {
        return;
    }
    casters(&mut |q, alpha| {
        for m in maps.iter_mut().flatten() {
            m.draw(&frame, sun, q, alpha);
        }
    });
    maps.iter_mut().flatten().for_each(ShadowMap::finish);

    let reach = *light.cascades.last().unwrap_or(&f32::INFINITY);
    let buf: &mut [u8] = img;
    view.each(&frame, sun, Some(&spans), |idx, p, z, s| {
        let Some(c) = light.cascades.iter().position(|&lim| z <= lim) else {
            return;
        };
        let Some(m) = &maps[c] else {
            return;
        };
        // Scaled by the pixel as well as the texel: the receiver's position is
        // read back from a depth buffer that is only exact at pixel centres
        // inside a face, and off by up to a pixel's worth along its edges.
        let bias = m.bias[s.facing as usize] * (z / focal * m.inv_texel).max(1.0);
        let occ = m.occlusion(p.u, p.v, p.height + bias);
        if occ <= 0.0 {
            return;
        }
        let fade = if reach.is_finite() {
            ((reach - z) / (reach * (1.0 - FADE_FROM))).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let k = 1.0 - s.take as f32 / 255.0 * occ * fade;
        for ch in &mut buf[idx * 4..idx * 4 + 3] {
            *ch = (*ch as f32 * k).round() as u8;
        }
    });
}

/// The drawn picture, read back as receivers in the sun's frame.
struct View<'a> {
    w: usize,
    h: usize,
    depth: &'a [f32],
    surf: &'a [Surf],
    basis: &'a Basis,
    focal: f32,
    ground: Option<(f32, Surf)>,
}

/// A receiver, placed in the sun's frame: across the map (`u`, `v`) and toward
/// the sun (`height`).
#[derive(Clone, Copy)]
struct Spot {
    u: f32,
    v: f32,
    height: f32,
}

impl View<'_> {
    /// Call `f` with the buffer index, place in the sun's frame, view depth and
    /// surface of every pixel that can take a shadow — within each row's
    /// `spans` entry (start, end) where those are given.
    ///
    /// Nothing is lifted into world space. A point at depth `z` along a pixel's
    /// ray is `eye + ray·z`, so each of its coordinates in the sun's frame is
    /// the eye's plus `z` times the ray's — and the ray's change by a fixed step
    /// from one pixel to the next. That is a few additions a pixel where the
    /// world position and three dot products would be a dozen multiplications,
    /// on a loop that visits every pixel of the picture twice.
    fn each(
        &self,
        frame: &Basis,
        sun: V3,
        spans: Option<&[(usize, usize)]>,
        mut f: impl FnMut(usize, Spot, f32, Surf),
    ) {
        let b = self.basis;
        let (cx, cy) = (self.w as f32 * 0.5, self.h as f32 * 0.5);
        let at = |d: V3| v3(d.dot(frame.right), d.dot(frame.up), d.dot(sun));
        let eye = at(b.eye);
        // One pixel to the right, in the sun's frame and in world `y`.
        let step = b.right.scale(1.0 / self.focal);
        let (step_l, step_y) = (at(step), step.y);
        for y in 0..self.h {
            let (x0, x1) = spans.map_or((0, self.w), |s| s[y]);
            if x0 >= x1 {
                continue;
            }
            let vy = (cy - (y as f32 + 0.5)) / self.focal;
            // The ray through the first pixel centre visited, scaled to unit
            // view depth — the inverse of `vec3::project`.
            let ray = b.fwd.add(b.up.scale(vy)).add(step.scale(x0 as f32 + 0.5 - cx));
            let (mut ray_l, mut ray_y) = (at(ray), ray.y);
            for x in x0..x1 {
                let idx = y * self.w + x;
                if x > x0 {
                    ray_l = ray_l.add(step_l);
                    ray_y += step_y;
                }
                let d = self.depth[idx];
                let (z, s) = if d > 0.0 {
                    (1.0 / d, self.surf[idx])
                } else if let Some((gy, s)) = self.ground {
                    // Nothing drawn here, so this is the backdrop: ground if
                    // the ray comes down onto the plane in front of the eye.
                    // Seen from underneath there is no ground to shade.
                    if b.eye.y <= gy || ray_y >= 0.0 {
                        continue;
                    }
                    ((gy - b.eye.y) / ray_y, s)
                } else {
                    continue;
                };
                if s.take == 0 {
                    continue;
                }
                let p = eye.add(ray_l.scale(z));
                f(idx, Spot { u: p.x, v: p.y, height: p.z }, z, s);
            }
        }
    }
}

/// An axis-aligned rectangle in the sun's frame.
#[derive(Debug, Clone, Copy)]
struct Rect {
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
}

impl Rect {
    const EMPTY: Rect = Rect { u0: f32::MAX, v0: f32::MAX, u1: f32::MIN, v1: f32::MIN };

    fn add(&mut self, u: f32, v: f32) {
        self.u0 = self.u0.min(u);
        self.v0 = self.v0.min(v);
        self.u1 = self.u1.max(u);
        self.v1 = self.v1.max(v);
    }

    fn contains(self, u: f32, v: f32) -> bool {
        u >= self.u0 && u <= self.u1 && v >= self.v0 && v <= self.v1
    }

    fn is_empty(self) -> bool {
        !(self.u0 <= self.u1 && self.v0 <= self.v1)
    }
}

/// Texels of margin kept around a fitted map, so the filter footprint of a
/// receiver on the very edge still lands inside it.
const MARGIN: f32 = 3.0;

/// One shadow map: for each texel, how far toward the sun the nearest caster
/// over it reaches, and how opaque that caster is.
struct ShadowMap {
    w: usize,
    h: usize,
    u0: f32,
    v0: f32,
    /// Texels per world unit.
    inv_texel: f32,
    /// Depth bias by [`Surf::facing`], in world units.
    bias: [f32; 256],
    top: Vec<f32>,
    alpha: Vec<u8>,
    /// The highest `top` in each [`BLOCK`]² block of texels and the blocks
    /// around it, once [`finish`](ShadowMap::finish) has run: a receiver above
    /// that is in full sun without its filter footprint being read at all.
    /// Most of what is on screen is, and the ground runs far past the casters.
    peak: Vec<f32>,
    pw: usize,
    ph: usize,
}

/// Texels to a side of one [`ShadowMap::peak`] block. Anything from the filter
/// footprint's own width up works; this is about where the lookup stops
/// rejecting enough to pay for itself.
const BLOCK: usize = 8;

impl ShadowMap {
    /// A map covering `r` with texels no smaller than `finest`, at most `side`
    /// texels along its longer edge. `None` when there is nothing to cover.
    fn fitted(r: Rect, side: usize, finest: f32) -> Option<ShadowMap> {
        if r.is_empty() {
            return None;
        }
        let span = (r.u1 - r.u0).max(r.v1 - r.v0).max(1e-5);
        // Square texels, so a shadow is not stretched along one axis.
        let texel = (span / (side as f32 - 2.0 * MARGIN).max(1.0)).max(finest);
        let (u0, v0) = (r.u0 - MARGIN * texel, r.v0 - MARGIN * texel);
        let w = (((r.u1 - r.u0) / texel).ceil() + 2.0 * MARGIN) as usize;
        let h = (((r.v1 - r.v0) / texel).ceil() + 2.0 * MARGIN) as usize;
        let (w, h) = (w.clamp(1, side), h.clamp(1, side));
        let (pw, ph) = (w.div_ceil(BLOCK), h.div_ceil(BLOCK));
        let bias = std::array::from_fn(|f| {
            let cos = (f as f32 / 255.0).max(0.2);
            let tan = (1.0 - cos * cos).sqrt() / cos;
            texel * (BIAS_TEXELS + BIAS_SLOPE * tan)
        });
        Some(ShadowMap {
            w,
            h,
            u0,
            v0,
            inv_texel: 1.0 / texel,
            bias,
            top: vec![f32::NEG_INFINITY; w * h],
            alpha: vec![0; w * h],
            peak: vec![f32::NEG_INFINITY; pw * ph],
            pw,
            ph,
        })
    }

    fn draw(&mut self, frame: &Basis, sun: V3, q: &[V3; 4], alpha: u8) {
        let inv = self.inv_texel;
        // Orthographic, so height is linear across the face and needs none of
        // the perspective care the camera's depth does.
        let pts = q.map(|p| {
            ((p.dot(frame.right) - self.u0) * inv, (p.dot(frame.up) - self.v0) * inv, p.dot(sun))
        });
        let (w, pw) = (self.w, self.pw);
        let ShadowMap { top, alpha: a, peak, .. } = self;
        scan_quad(w as u32, self.h as u32, &pts, |x, y, z| {
            let t = y as usize * w + x as usize;
            if z > top[t] {
                top[t] = z;
                a[t] = alpha;
                let b = y as usize / BLOCK * pw + x as usize / BLOCK;
                peak[b] = peak[b].max(z);
            }
        });
    }

    /// Spread each block's peak over its neighbours, once everything is drawn.
    ///
    /// A receiver's filter footprint reaches two texels either side of it,
    /// which can cross into the next block, so the value it is tested against
    /// has to cover the blocks around its own as well.
    fn finish(&mut self) {
        let (pw, ph) = (self.pw, self.ph);
        let raw = std::mem::take(&mut self.peak);
        self.peak = (0..pw * ph)
            .map(|b| {
                let (bx, by) = (b % pw, b / pw);
                let mut m = f32::NEG_INFINITY;
                for y in by.saturating_sub(1)..(by + 2).min(ph) {
                    for x in bx.saturating_sub(1)..(bx + 2).min(pw) {
                        m = m.max(raw[y * pw + x]);
                    }
                }
                m
            })
            .collect();
    }

    /// How shadowed a point at map position `u, v` and height `height` is,
    /// from 0 (in full sun) to 1.
    ///
    /// Filtered over a 4×4 texel footprint with tent weights — three bilinear
    /// taps a texel apart along each axis — so a shadow's edge is a soft ramp
    /// rather than the staircase of the texels it was drawn into.
    fn occlusion(&self, u: f32, v: f32, height: f32) -> f32 {
        let x = (u - self.u0) * self.inv_texel - 0.5;
        let y = (v - self.v0) * self.inv_texel - 0.5;
        // Stated positively, so a NaN position is turned away here too.
        if !(x > -3.0 && y > -3.0 && x < self.w as f32 + 2.0 && y < self.h as f32 + 2.0) {
            return 0.0;
        }
        let bx = (x.max(0.0) as usize / BLOCK).min(self.pw - 1);
        let by = (y.max(0.0) as usize / BLOCK).min(self.ph - 1);
        if self.peak[by * self.pw + bx] <= height {
            return 0.0;
        }
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let (ix, iy) = (x.floor() as isize - 1, y.floor() as isize - 1);
        let wx = [1.0 - fx, 1.0, 1.0, fx];
        let wy = [1.0 - fy, 1.0, 1.0, fy];
        let mut sum = 0.0;
        for (j, wy) in wy.iter().enumerate() {
            let ty = iy + j as isize;
            if ty < 0 || ty >= self.h as isize {
                continue;
            }
            let row = ty as usize * self.w;
            for (i, wx) in wx.iter().enumerate() {
                let tx = ix + i as isize;
                if tx < 0 || tx >= self.w as isize {
                    continue;
                }
                let t = row + tx as usize;
                if self.top[t] > height {
                    sum += wx * wy * self.alpha[t] as f32;
                }
            }
        }
        sum / (9.0 * 255.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sun_frame(sun: V3) -> Basis {
        vec3::look_at(v3(0.0, 0.0, 0.0), sun.scale(-1.0), v3(0.0, 1.0, 0.0))
    }

    /// A horizontal square at height `y`, as a caster face.
    fn square(y: f32, half: f32) -> [V3; 4] {
        [v3(-half, y, -half), v3(half, y, -half), v3(half, y, half), v3(-half, y, half)]
    }

    fn map_over(sun: V3, faces: &[[V3; 4]]) -> ShadowMap {
        let f = sun_frame(sun);
        let mut r = Rect::EMPTY;
        for q in faces {
            for p in q {
                r.add(p.dot(f.right), p.dot(f.up));
            }
        }
        let mut m = ShadowMap::fitted(r, 128, 0.0).expect("a map");
        for q in faces {
            m.draw(&f, sun, q, 255);
        }
        m.finish();
        m
    }

    #[test]
    fn a_point_under_a_caster_is_shadowed_and_one_above_it_is_not() {
        let sun = v3(0.0, 1.0, 0.0);
        let f = sun_frame(sun);
        let m = map_over(sun, &[square(1.0, 1.0)]);
        let at = |p: V3| m.occlusion(p.dot(f.right), p.dot(f.up), p.dot(sun));
        assert!(at(v3(0.0, 0.0, 0.0)) > 0.99, "beneath the square");
        assert_eq!(at(v3(0.0, 2.0, 0.0)), 0.0, "above it, nothing is in the way");
        assert_eq!(at(v3(5.0, 0.0, 0.0)), 0.0, "off to the side, in the sun");
    }

    #[test]
    fn a_slanted_sun_moves_the_shadow_away_from_it() {
        // Sun up and toward +X: the shadow of a square held up at y = 1 lands
        // on the ground displaced toward −X.
        let sun = v3(1.0, 1.0, 0.0).norm();
        let f = sun_frame(sun);
        let m = map_over(sun, &[square(1.0, 0.5)]);
        let at = |p: V3| m.occlusion(p.dot(f.right), p.dot(f.up), p.dot(sun));
        assert!(at(v3(-1.0, 0.0, 0.0)) > 0.99, "the shadow falls away from the sun");
        assert_eq!(at(v3(1.0, 0.0, 0.0)), 0.0, "and not toward it");
    }

    #[test]
    fn a_translucent_caster_casts_a_lighter_shadow() {
        let sun = v3(0.0, 1.0, 0.0);
        let f = sun_frame(sun);
        let mut r = Rect::EMPTY;
        for p in square(1.0, 1.0) {
            r.add(p.dot(f.right), p.dot(f.up));
        }
        let mut m = ShadowMap::fitted(r, 64, 0.0).unwrap();
        m.draw(&f, sun, &square(1.0, 1.0), 128);
        m.finish();
        let occ = m.occlusion(0.0, 0.0, 0.0);
        assert!((occ - 128.0 / 255.0).abs() < 0.02, "half opacity, half shadow: {occ}");
    }

    #[test]
    fn the_shadow_edge_is_a_ramp_not_a_step() {
        let sun = v3(0.0, 1.0, 0.0);
        let f = sun_frame(sun);
        let m = map_over(sun, &[square(1.0, 1.0)]);
        let steps: Vec<f32> = (0..40)
            .map(|i| {
                let p = v3(0.8 + i as f32 * 0.01, 0.0, 0.0);
                m.occlusion(p.dot(f.right), p.dot(f.up), 0.0)
            })
            .collect();
        assert!(steps.iter().any(|&o| o > 0.05 && o < 0.95), "some partial shadow at the edge");
    }

    #[test]
    fn nothing_to_cover_builds_no_map() {
        assert!(ShadowMap::fitted(Rect::EMPTY, 128, 0.0).is_none());
    }

    #[test]
    fn a_map_is_no_finer_than_the_receivers_can_show() {
        let r = Rect { u0: 0.0, v0: 0.0, u1: 10.0, v1: 5.0 };
        let fine = ShadowMap::fitted(r, 1000, 0.0).unwrap();
        let coarse = ShadowMap::fitted(r, 1000, 0.1).unwrap();
        assert_eq!(fine.w, 1000, "unbounded, the long edge takes the whole budget");
        assert!(coarse.w <= 110, "a pixel 0.1 wide wants ~100 texels, got {}", coarse.w);
        assert!(coarse.h < coarse.w, "and the texels stay square");
    }
}
