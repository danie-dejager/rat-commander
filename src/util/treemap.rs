//! Squarified treemap layout (Bruls, Huizing & van Wijk).
//!
//! Shared by the 2D disk explorer, which rounds the result to character cells,
//! and the 3D space view, which uses the floating-point rectangles directly as
//! a floor plan. Keeping one implementation means the two views are recognisably
//! the same picture from two angles.

/// A floating-point rectangle: the layout's native output, before any rounding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Turn directory sizes into layout areas summing to `total_area`.
///
/// A floor is added to every size so that even an empty directory keeps a
/// visible, clickable box instead of collapsing to nothing.
pub fn size_areas(sizes: &[u64], total_area: f64) -> Vec<f64> {
    let n = sizes.len();
    if n == 0 {
        return Vec::new();
    }
    let total: u64 = sizes.iter().sum();
    let base = (total / n as u64 / 12).max(1);
    let weights: Vec<f64> = sizes.iter().map(|s| (s + base) as f64).collect();
    let wsum: f64 = weights.iter().sum();
    if wsum <= 0.0 {
        return vec![0.0; n];
    }
    weights.iter().map(|w| w / wsum * total_area).collect()
}

/// Lay `areas` (already largest-first) out inside the given rectangle.
pub fn squarify(areas: &[f64], x: f64, y: f64, w: f64, h: f64) -> Vec<FRect> {
    let mut out: Vec<FRect> = Vec::with_capacity(areas.len());
    let mut rect = FRect { x, y, w, h };
    let mut row: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < areas.len() {
        let length = rect.w.min(rect.h);
        if length <= 0.0 {
            // No space left; emit zero rects for the remainder.
            for _ in i..areas.len() {
                out.push(FRect { x: rect.x, y: rect.y, w: 0.0, h: 0.0 });
            }
            return out;
        }
        let a = areas[i];
        row.push(a);
        let with = worst(&row, length);
        row.pop();
        let without = if row.is_empty() { f64::MAX } else { worst(&row, length) };
        if row.is_empty() || without >= with {
            row.push(a);
            i += 1;
        } else {
            layout_row(&row, &mut rect, &mut out);
            row.clear();
        }
    }
    if !row.is_empty() {
        layout_row(&row, &mut rect, &mut out);
    }
    out
}

/// Worst (largest) aspect ratio in a row laid along side `length`.
fn worst(row: &[f64], length: f64) -> f64 {
    let sum: f64 = row.iter().sum();
    if sum <= 0.0 || length <= 0.0 {
        return f64::MAX;
    }
    let max = row.iter().cloned().fold(f64::MIN, f64::max);
    let min = row.iter().cloned().fold(f64::MAX, f64::min);
    let l2 = length * length;
    let s2 = sum * sum;
    f64::max(l2 * max / s2, s2 / (l2 * min))
}

fn layout_row(row: &[f64], rect: &mut FRect, out: &mut Vec<FRect>) {
    let sum: f64 = row.iter().sum();
    if sum <= 0.0 {
        for _ in row {
            out.push(FRect { x: rect.x, y: rect.y, w: 0.0, h: 0.0 });
        }
        return;
    }
    if rect.w >= rect.h {
        // Lay the row as a column down the left edge.
        let col_w = sum / rect.h;
        let mut yy = rect.y;
        for &a in row {
            let cell_h = a / col_w;
            out.push(FRect { x: rect.x, y: yy, w: col_w, h: cell_h });
            yy += cell_h;
        }
        rect.x += col_w;
        rect.w -= col_w;
    } else {
        // Lay the row across the top edge.
        let row_h = sum / rect.w;
        let mut xx = rect.x;
        for &a in row {
            let cell_w = a / row_h;
            out.push(FRect { x: xx, y: rect.y, w: cell_w, h: row_h });
            xx += cell_w;
        }
        rect.y += row_h;
        rect.h -= row_h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprints_stay_inside_the_box_and_do_not_overlap() {
        let areas = size_areas(&[900, 400, 250, 100, 40, 1], 1.0);
        let rs = squarify(&areas, 0.0, 0.0, 1.0, 1.0);
        assert_eq!(rs.len(), 6);
        for r in &rs {
            assert!(r.x >= -1e-9 && r.y >= -1e-9, "{r:?} starts inside");
            assert!(r.x + r.w <= 1.0 + 1e-9, "{r:?} fits horizontally");
            assert!(r.y + r.h <= 1.0 + 1e-9, "{r:?} fits vertically");
        }
        // Pairwise disjoint (allowing shared edges).
        for (i, a) in rs.iter().enumerate() {
            for b in rs.iter().skip(i + 1) {
                let apart = a.x + a.w <= b.x + 1e-9
                    || b.x + b.w <= a.x + 1e-9
                    || a.y + a.h <= b.y + 1e-9
                    || b.y + b.h <= a.y + 1e-9;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn footprint_area_tracks_size_and_fills_the_box() {
        let areas = size_areas(&[800, 200], 1.0);
        let rs = squarify(&areas, 0.0, 0.0, 1.0, 1.0);
        let covered: f64 = rs.iter().map(|r| r.w * r.h).sum();
        assert!((covered - 1.0).abs() < 1e-6, "the layout fills its box");
        assert!(rs[0].w * rs[0].h > rs[1].w * rs[1].h, "bigger size, bigger box");
    }

    #[test]
    fn even_an_empty_directory_keeps_a_visible_box() {
        // Without the size floor a 0-byte directory would be unclickable.
        let areas = size_areas(&[1_000_000, 0], 1.0);
        let rs = squarify(&areas, 0.0, 0.0, 1.0, 1.0);
        assert!(rs[1].w * rs[1].h > 0.0, "the empty directory still has area");
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert!(squarify(&[], 0.0, 0.0, 1.0, 1.0).is_empty());
        assert!(size_areas(&[], 1.0).is_empty());
        // Zero-sized box: every entry still gets a (zero) rect, one per input.
        let rs = squarify(&size_areas(&[5, 5], 0.0), 0.0, 0.0, 0.0, 0.0);
        assert_eq!(rs.len(), 2);
    }
}
