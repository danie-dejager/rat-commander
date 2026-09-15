//! Anti-aliased coverage for filled polygons and stroked lines.
//!
//! The technique is the one font rasterizers use (font-rs): each edge adds, to
//! the pixels it crosses, the signed area it sweeps, and a running sum along
//! every row turns those into how much of each pixel is inside. That is an
//! exact anti-aliased fill under the non-zero winding rule, in one pass per
//! edge — a hole wound against its outer ring cancels out, and thousands of
//! rings (a coastline) accumulate into one buffer before anything is painted.
//!
//! A line is drawn as the thin quadrilateral around each of its segments, all
//! wound the same way, so where segments overlap at a joint the coverage adds
//! up and is capped rather than cancelling.

use crate::ui::graphics::raster::{Rgb, over};
use image::RgbaImage;

/// Coverage accumulated over a `w` × `h` canvas.
pub struct Coverage {
    w: usize,
    h: usize,
    /// Row-major, with two spare columns per row for edges clamped to the
    /// right-hand side.
    acc: Vec<f32>,
    stride: usize,
    touched: bool,
}

impl Coverage {
    pub fn new(w: u32, h: u32) -> Self {
        let (w, h) = (w as usize, h as usize);
        let stride = w + 2;
        Coverage { w, h, acc: vec![0.0; stride * h], stride, touched: false }
    }

    /// Start over, for the next layer.
    pub fn clear(&mut self) {
        if self.touched {
            self.acc.fill(0.0);
            self.touched = false;
        }
    }

    /// Add a closed ring (the last point joins the first).
    pub fn add_ring(&mut self, pts: &[(f32, f32)]) {
        // A ring with a point that is not a number cannot be closed; leaving it
        // half-added would fill everything to the right of its other edges.
        if pts.len() < 3 || pts.iter().any(|p| !(p.0.is_finite() && p.1.is_finite())) {
            return;
        }
        for i in 0..pts.len() {
            self.edge(pts[i], pts[(i + 1) % pts.len()]);
        }
    }

    /// Add the segment from `a` to `b` as a stroke `width` pixels wide.
    pub fn add_segment(&mut self, a: (f32, f32), b: (f32, f32), width: f32) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        let half = width * 0.5;
        // A segment shorter than the stroke is drawn as a square dot, so the
        // vertices of a line zoomed far out still leave a mark.
        let (ux, uy) = if len < 1e-3 { (1.0, 0.0) } else { (dx / len, dy / len) };
        let (nx, ny) = (-uy * half, ux * half);
        let (a, b) = if len < 1e-3 { ((a.0 - half, a.1), (a.0 + half, a.1)) } else { (a, b) };
        let quad = [
            (a.0 + nx, a.1 + ny),
            (b.0 + nx, b.1 + ny),
            (b.0 - nx, b.1 - ny),
            (a.0 - nx, a.1 - ny),
        ];
        self.add_ring(&quad);
    }

    /// Add a filled disc of radius `r` about `c`.
    pub fn add_dot(&mut self, c: (f32, f32), r: f32) {
        let n = 16;
        let ring: Vec<(f32, f32)> = (0..n)
            .map(|i| {
                let t = i as f32 / n as f32 * std::f32::consts::TAU;
                (c.0 + r * t.cos(), c.1 + r * t.sin())
            })
            .collect();
        self.add_ring(&ring);
    }

    /// How much of each pixel is covered, row by row: `f(x, y, coverage)` for
    /// every pixel with any.
    pub fn for_each(&self, mut f: impl FnMut(usize, usize, f32)) {
        if !self.touched {
            return;
        }
        for y in 0..self.h {
            let row = &self.acc[y * self.stride..y * self.stride + self.w];
            let mut sum = 0.0f32;
            for (x, a) in row.iter().enumerate() {
                sum += a;
                let cov = sum.abs().min(1.0);
                if cov > 1.0 / 255.0 {
                    f(x, y, cov);
                }
            }
        }
    }

    /// Paint `color` over `img` at `alpha` times the coverage.
    pub fn composite(&self, img: &mut RgbaImage, color: Rgb, alpha: f32) {
        self.for_each(|x, y, cov| {
            let p = img.get_pixel_mut(x as u32, y as u32);
            let c = over((p[0], p[1], p[2]), color, f64::from(cov * alpha));
            *p = image::Rgba([c.0, c.1, c.2, 255]);
        });
    }

    /// Accumulate one edge.
    fn edge(&mut self, p0: (f32, f32), p1: (f32, f32)) {
        if (p0.1 - p1.1).abs() < f32::EPSILON {
            return;
        }
        let (dir, p0, p1) = if p0.1 < p1.1 { (1.0, p0, p1) } else { (-1.0, p1, p0) };
        if p1.1 <= 0.0 || p0.1 >= self.h as f32 {
            return;
        }
        self.touched = true;
        let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
        let y_start = p0.1.max(0.0);
        let mut x = p0.0 + (y_start - p0.1) * dxdy;
        let row0 = y_start as usize;
        let row1 = (p1.1.ceil() as usize).min(self.h);
        let right = self.w as f32;
        for y in row0..row1 {
            let top = (y as f32).max(p0.1);
            let bottom = ((y + 1) as f32).min(p1.1);
            let dy = bottom - top;
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            // Clamped to the canvas: an edge off the left still counts for the
            // whole row, one off the right for none of it.
            let (xa, xb) = (x.clamp(0.0, right), xnext.clamp(0.0, right));
            let (x0, x1) = if xa < xb { (xa, xb) } else { (xb, xa) };
            let base = y * self.stride;
            let x0f = x0.floor();
            let x0i = x0f as usize;
            let x1i = x1.ceil() as usize;
            if x1i <= x0i + 1 {
                // Within one pixel: split by where the edge crosses it.
                let xm = 0.5 * (xa + xb) - x0f;
                self.acc[base + x0i] += d * (1.0 - xm);
                self.acc[base + x0i + 1] += d * xm;
            } else {
                let s = 1.0 / (x1 - x0);
                let x0frac = x0 - x0f;
                let a0 = 0.5 * s * (1.0 - x0frac) * (1.0 - x0frac);
                let x1frac = x1 - x1.ceil() + 1.0;
                let am = 0.5 * s * x1frac * x1frac;
                self.acc[base + x0i] += d * a0;
                if x1i == x0i + 2 {
                    self.acc[base + x0i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0frac);
                    self.acc[base + x0i + 1] += d * (a1 - a0);
                    for xi in x0i + 2..x1i - 1 {
                        self.acc[base + xi] += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.acc[base + x1i - 1] += d * (1.0 - a2 - am);
                }
                self.acc[base + x1i] += d * am;
            }
            x = xnext;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(c: &Coverage) -> Vec<Vec<f32>> {
        let mut g = vec![vec![0.0; c.w]; c.h];
        c.for_each(|x, y, v| g[y][x] = v);
        g
    }

    #[test]
    fn a_square_covers_its_inside_and_nothing_outside() {
        let mut c = Coverage::new(10, 10);
        c.add_ring(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0)]);
        let g = grid(&c);
        assert!((g[3][3] - 1.0).abs() < 1e-4 && (g[5][5] - 1.0).abs() < 1e-4);
        assert_eq!(g[1][3], 0.0);
        assert_eq!(g[3][7], 0.0);
        assert_eq!(g[7][3], 0.0);
        // The same square wound the other way covers the same pixels.
        let mut back = Coverage::new(10, 10);
        back.add_ring(&[(2.0, 6.0), (6.0, 6.0), (6.0, 2.0), (2.0, 2.0)]);
        assert_eq!(grid(&back), g);
    }

    #[test]
    fn an_edge_through_a_pixel_covers_part_of_it() {
        let mut c = Coverage::new(10, 10);
        c.add_ring(&[(2.5, 2.0), (6.0, 2.0), (6.0, 6.0), (2.5, 6.0)]);
        let g = grid(&c);
        assert!((g[3][2] - 0.5).abs() < 1e-4, "half of pixel 2 is inside: {}", g[3][2]);
        let mut tri = Coverage::new(10, 10);
        tri.add_ring(&[(0.0, 0.0), (8.0, 0.0), (0.0, 8.0)]);
        let g = grid(&tri);
        // Pixel (4, 3) straddles x + y = 8.
        assert!(g[3][4] > 0.2 && g[3][4] < 0.8, "the diagonal is anti-aliased: {}", g[3][4]);
        assert!((g[3][3] - 1.0).abs() < 1e-4 && g[3][5] == 0.0);
    }

    #[test]
    fn a_hole_wound_against_its_ring_stays_empty() {
        let mut c = Coverage::new(12, 12);
        c.add_ring(&[(1.0, 1.0), (11.0, 1.0), (11.0, 11.0), (1.0, 11.0)]);
        c.add_ring(&[(4.0, 4.0), (4.0, 8.0), (8.0, 8.0), (8.0, 4.0)]);
        let g = grid(&c);
        assert_eq!(g[6][6], 0.0);
        assert!((g[2][6] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_line_doubling_back_on_itself_does_not_cancel() {
        let mut c = Coverage::new(20, 6);
        c.add_segment((2.0, 3.0), (18.0, 3.0), 2.0);
        c.add_segment((18.0, 3.0), (2.0, 3.0), 2.0);
        let g = grid(&c);
        assert!((g[2][10] - 1.0).abs() < 1e-4 && (g[3][10] - 1.0).abs() < 1e-4, "{:?}", g[2]);
        assert_eq!(g[0][10], 0.0);
    }

    #[test]
    fn shapes_far_off_the_canvas_are_clipped_rather_than_crashing() {
        let mut c = Coverage::new(8, 8);
        c.add_ring(&[(-1e7, -1e7), (1e7, -1e7), (1e7, 1e7), (-1e7, 1e7)]);
        let g = grid(&c);
        assert!(g.iter().flatten().all(|&v| (v - 1.0).abs() < 1e-3), "{g:?}");
        let mut off = Coverage::new(8, 8);
        off.add_ring(&[(20.0, 1.0), (30.0, 1.0), (30.0, 5.0), (20.0, 5.0)]);
        off.add_ring(&[(f32::NAN, 0.0), (1.0, 1.0), (2.0, 2.0)]);
        assert!(grid(&off).iter().flatten().all(|&v| v == 0.0));
    }
}
