//! Laying the map out: the world's layers and the GeoJSON over them, onto a
//! canvas of square pixels — the true pixels of a graphics terminal here, or
//! the braille dots of a character cell in [`super::cells`], which share every
//! step up to the last.

use super::cover::Coverage;
use super::geojson::{GeoDoc, Shape};
use super::palette::MapPalette;
use super::view::{MapView, Projection};
use super::world::{Layer, world};
use crate::ui::graphics::raster::{self, Rgb};
use image::RgbaImage;

/// What to draw.
pub struct Scene<'a> {
    pub view: MapView,
    pub doc: &'a GeoDoc,
    /// The object chosen in the list; the others are drawn dimmed. `None` draws
    /// every object alike.
    pub object: Option<usize>,
    /// The feature picked out: (object, feature).
    pub selected: Option<(usize, usize)>,
}

/// Stroke widths, in canvas pixels, for a canvas whose pixels are `scale`
/// times a braille dot (the text tier is 1).
pub struct Widths {
    pub coast: f32,
    pub border: f32,
    pub river: f32,
    pub feature: f32,
    pub selected: f32,
    pub point: f32,
}

impl Widths {
    pub fn for_scale(scale: f32) -> Self {
        Widths {
            coast: 0.9 * scale.max(1.0),
            border: 0.75 * scale.max(1.0),
            river: 0.7 * scale.max(1.0),
            feature: 1.6 * scale,
            selected: 2.6 * scale,
            point: 2.4 * scale,
        }
    }
}

/// Which parts of a feature a pass draws.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Fill,
    Stroke,
    Points,
}

/// Fill a polygon layer's rings into `cov`.
pub fn fill_layer(cov: &mut Coverage, p: &Projection, layer: &Layer) {
    let Some(level) = layer.level(p.deg_per_px()) else { return };
    let (south, north) = p.lat_range();
    let mut pts = Vec::new();
    for s in &level.shapes {
        if f64::from(s.bbox[3]) < south || f64::from(s.bbox[1]) > north {
            continue;
        }
        for off in p.copies(f64::from(s.bbox[0]), f64::from(s.bbox[2])) {
            pts.clear();
            pts.extend(s.pts.iter().map(|q| p.xy(f64::from(q[0]) + off, f64::from(q[1]))));
            cov.add_ring(&pts);
        }
    }
}

/// Stroke a layer's shapes into `cov` — as closed rings for a polygon layer —
/// at the width `width` gives each, leaving out the segments `skip` rejects.
pub fn stroke_layer(
    cov: &mut Coverage,
    p: &Projection,
    layer: &Layer,
    closed: bool,
    width: impl Fn(u8) -> f32,
    skip: impl Fn([f32; 2], [f32; 2]) -> bool,
) {
    let Some(level) = layer.level(p.deg_per_px()) else { return };
    let (south, north) = p.lat_range();
    let (w, h) = (p.w as f32, p.h as f32);
    for s in &level.shapes {
        if f64::from(s.bbox[3]) < south || f64::from(s.bbox[1]) > north {
            continue;
        }
        let width = width(s.rank);
        if width <= 0.0 {
            continue;
        }
        let n = s.pts.len();
        let segments = if closed { n } else { n - 1 };
        for off in p.copies(f64::from(s.bbox[0]), f64::from(s.bbox[2])) {
            let at = |i: usize| s.pts[i % n];
            let mut prev = p.xy(f64::from(at(0)[0]) + off, f64::from(at(0)[1]));
            for i in 0..segments {
                let (a, b) = (at(i), at(i + 1));
                let next = p.xy(f64::from(b[0]) + off, f64::from(b[1]));
                // Only what is on the canvas, and not the seams the source cut
                // the world along.
                let outside = (prev.0 < -width && next.0 < -width)
                    || (prev.0 > w + width && next.0 > w + width)
                    || (prev.1 < -width && next.1 < -width)
                    || (prev.1 > h + width && next.1 > h + width);
                if !outside && !skip(a, b) {
                    cov.add_segment(prev, next, width);
                }
                prev = next;
            }
        }
    }
}

/// Whether a segment of a land ring is one of the cuts Natural Earth made to
/// fit the world on a flat map — along the antimeridian, or along the south
/// edge of Antarctica — rather than a coastline.
pub fn is_seam(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0].abs() >= 179.999 && b[0].abs() >= 179.999) || (a[1] <= -89.99 && b[1] <= -89.99)
}

/// Draw one part of the GeoJSON features into `cov`: those `include` accepts,
/// by object and feature index. Each is drawn for every copy of the world it
/// could be in view on — the box is widened by a turn each way, since a set of
/// points either side of the date line is boxed across it.
pub fn features(
    cov: &mut Coverage,
    p: &Projection,
    doc: &GeoDoc,
    part: Part,
    width: f32,
    mut include: impl FnMut(usize, usize) -> bool,
) {
    let mut pts = Vec::new();
    for (oi, o) in doc.objects.iter().enumerate() {
        for (fi, f) in o.features.iter().enumerate() {
            let Some(b) = f.bounds else { continue };
            if !include(oi, fi) {
                continue;
            }
            let (south, north) = p.lat_range();
            if b.lat1 < south || b.lat0 > north {
                continue;
            }
            for off in p.copies(b.lon0 - 360.0, b.lon1 + 360.0) {
                for shape in &f.shapes {
                    match (shape, part) {
                        (Shape::Polygon(rings), Part::Fill) => {
                            for ring in rings {
                                pts.clear();
                                pts.extend(ring.iter().map(|q| p.xy(q[0] + off, q[1])));
                                cov.add_ring(&pts);
                            }
                        }
                        (Shape::Polygon(rings), Part::Stroke) => {
                            for ring in rings {
                                stroke(cov, p, ring, off, true, width);
                            }
                        }
                        (Shape::Line(line), Part::Stroke) => {
                            stroke(cov, p, line, off, false, width)
                        }
                        (Shape::Point(q), Part::Points) => {
                            cov.add_dot(p.xy(q[0] + off, q[1]), width)
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn stroke(
    cov: &mut Coverage,
    p: &Projection,
    pts: &[[f64; 2]],
    off: f64,
    closed: bool,
    width: f32,
) {
    let n = pts.len();
    if n == 0 {
        return;
    }
    let segments = if closed { n } else { n - 1 };
    let mut prev = p.xy(pts[0][0] + off, pts[0][1]);
    if n == 1 {
        cov.add_segment(prev, prev, width);
    }
    for i in 0..segments {
        let q = pts[(i + 1) % n];
        let next = p.xy(q[0] + off, q[1]);
        cov.add_segment(prev, next, width);
        prev = next;
    }
}

/// The cities to label: biggest first, those in view, as many as there is
/// room for — `budget` — and each as its canvas position, radius and name.
pub fn cities(p: &Projection, budget: usize) -> Vec<((f32, f32), &'static str, u32, bool)> {
    let (west, east) = p.lon_range();
    let (south, north) = p.lat_range();
    let mut out = Vec::new();
    for c in &world().cities {
        if out.len() >= budget {
            break;
        }
        let (lon, lat) = (f64::from(c.lon), f64::from(c.lat));
        if lat < south || lat > north {
            continue;
        }
        let Some(off) = p.copies(lon, lon).next() else { continue };
        if lon + off < west || lon + off > east {
            continue;
        }
        out.push((p.xy(lon + off, lat), &*c.name, c.pop, c.capital));
    }
    out
}

/// The map as a `w` × `h` image.
pub fn raster(w: u32, h: u32, scene: &Scene, pal: &MapPalette, label_px: f32) -> RgbaImage {
    let mut img = raster::canvas(w, h, pal.ocean);
    let p = scene.view.project(w, h);
    let world = world();
    // Stroke widths grow a little with a large canvas, where one pixel is fine.
    let scale = (w.max(h) as f32 / 900.0).clamp(1.0, 2.0);
    let widths = Widths::for_scale(scale);
    let mut cov = Coverage::new(w, h);
    let mut layer =
        |img: &mut RgbaImage, color: Rgb, alpha: f32, draw: &mut dyn FnMut(&mut Coverage)| {
            cov.clear();
            draw(&mut cov);
            cov.composite(img, color, alpha);
        };

    layer(&mut img, pal.land, 1.0, &mut |c| fill_layer(c, &p, &world.land));
    layer(&mut img, pal.ocean, 1.0, &mut |c| fill_layer(c, &p, &world.lakes));
    layer(&mut img, pal.river, 1.0, &mut |c| {
        stroke_layer(
            c,
            &p,
            &world.rivers,
            false,
            |rank| widths.river * (0.6 + f32::from(rank) / 12.0),
            |_, _| false,
        )
    });
    layer(&mut img, pal.border, 1.0, &mut |c| {
        stroke_layer(c, &p, &world.borders, false, |_| widths.border, |_, _| false)
    });
    layer(&mut img, pal.coast, 1.0, &mut |c| {
        stroke_layer(c, &p, &world.land, true, |_| widths.coast, is_seam);
        stroke_layer(c, &p, &world.lakes, true, |_| widths.coast * 0.7, |_, _| false);
    });

    // The GeoJSON: areas, then lines, then points, dimmed where not chosen.
    let doc = scene.doc;
    let chosen = |oi: usize| scene.object.is_none_or(|o| o == oi);
    let picked = |oi: usize, fi: usize| scene.selected == Some((oi, fi));
    for (color, mine) in [(pal.feature_dim, false), (pal.feature, true)] {
        let include = |oi: usize, fi: usize| chosen(oi) == mine && !picked(oi, fi);
        layer(&mut img, color, 0.3, &mut |c| features(c, &p, doc, Part::Fill, 0.0, include));
        layer(&mut img, color, 1.0, &mut |c| {
            features(c, &p, doc, Part::Stroke, widths.feature, include)
        });
        layer(&mut img, pal.ocean, 1.0, &mut |c| {
            features(c, &p, doc, Part::Points, widths.point + scale, include)
        });
        layer(&mut img, color, 1.0, &mut |c| {
            features(c, &p, doc, Part::Points, widths.point, include)
        });
    }
    if scene.selected.is_some() {
        let include = |oi: usize, fi: usize| picked(oi, fi);
        layer(&mut img, pal.selected, 0.35, &mut |c| {
            features(c, &p, doc, Part::Fill, 0.0, include)
        });
        layer(&mut img, pal.selected, 1.0, &mut |c| {
            features(c, &p, doc, Part::Stroke, widths.selected, include)
        });
        layer(&mut img, pal.ocean, 1.0, &mut |c| {
            features(c, &p, doc, Part::Points, widths.point * 1.6 + scale, include)
        });
        layer(&mut img, pal.selected, 1.0, &mut |c| {
            features(c, &p, doc, Part::Points, widths.point * 1.6, include)
        });
    }

    // The cities that fit, with their names where those fit too.
    let budget = ((w * h) as f32 / (label_px * label_px * 90.0)).clamp(3.0, 60.0) as usize;
    let mut placed: Vec<[i32; 4]> = Vec::new();
    let mut dots = Coverage::new(w, h);
    for ((x, y), name, pop, capital) in cities(&p, budget) {
        let big = 0.5 * ((pop.max(1) as f32).log10() - 4.0).clamp(0.0, 3.0);
        let r = scale * (1.2 + big + if capital { 0.8 } else { 0.0 });
        dots.add_dot((x, y), r);
        let tw = raster::text_width(name, label_px) as i32;
        let th = raster::text_height(label_px) as i32;
        let (lx, ly) = ((x + r + 3.0) as i32, (y as i32) - th / 2);
        let rect = [lx - 2, ly - 1, lx + tw + 2, ly + th + 1];
        let fits = lx >= 0 && ly >= 0 && rect[2] < w as i32 && rect[3] < h as i32;
        if fits
            && !placed
                .iter()
                .any(|q| q[0] < rect[2] && rect[0] < q[2] && q[1] < rect[3] && rect[1] < q[3])
        {
            placed.push(rect);
            raster::draw_text(&mut img, lx, ly, name, pal.label, Some(pal.ocean), label_px);
        }
    }
    dots.composite(&mut img, pal.city, 1.0);
    img
}

/// The feature nearest canvas position (`x`, `y`) within `reach` pixels: a
/// point by its distance, a line by its nearest segment, an area by being
/// inside it (or near its edge). Chosen objects win over dimmed ones.
pub fn hit(scene: &Scene, w: u32, h: u32, x: f32, y: f32, reach: f32) -> Option<(usize, usize)> {
    let p = scene.view.project(w, h);
    let mut best: Option<(f32, (usize, usize))> = None;
    for (oi, o) in scene.doc.objects.iter().enumerate() {
        let penalty = if scene.object.is_none_or(|c| c == oi) { 0.0 } else { reach };
        for (fi, f) in o.features.iter().enumerate() {
            let Some(b) = f.bounds else { continue };
            for off in p.copies(b.lon0 - 360.0, b.lon1 + 360.0) {
                let xy = |q: &[f64; 2]| p.xy(q[0] + off, q[1]);
                let mut d = f32::INFINITY;
                for s in &f.shapes {
                    match s {
                        Shape::Point(q) => d = d.min(dist(xy(q), (x, y))),
                        Shape::Line(line) => {
                            d = d.min(polyline_dist(line.iter().map(xy), (x, y), false))
                        }
                        Shape::Polygon(rings) => {
                            let inside =
                                rings.iter().filter(|r| winds(r.iter().map(xy), (x, y))).count()
                                    % 2
                                    == 1;
                            // An area counts from anywhere inside, a little
                            // behind anything drawn on top of it.
                            let edge = rings
                                .iter()
                                .map(|r| polyline_dist(r.iter().map(xy), (x, y), true))
                                .fold(f32::INFINITY, f32::min);
                            d = d.min(if inside { reach * 0.5 } else { edge });
                        }
                    }
                }
                let d = d + penalty;
                if d <= reach + penalty && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, (oi, fi)));
                }
            }
        }
    }
    best.map(|(_, hit)| hit)
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn polyline_dist(pts: impl Iterator<Item = (f32, f32)>, q: (f32, f32), closed: bool) -> f32 {
    let pts: Vec<(f32, f32)> = pts.collect();
    let n = pts.len();
    if n == 1 {
        return dist(pts[0], q);
    }
    let segments = if closed { n } else { n.saturating_sub(1) };
    (0..segments)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 {
                (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            dist((a.0 + t * dx, a.1 + t * dy), q)
        })
        .fold(f32::INFINITY, f32::min)
}

/// Whether a ring winds around `q` (the even-odd crossing test).
fn winds(pts: impl Iterator<Item = (f32, f32)>, q: (f32, f32)) -> bool {
    let pts: Vec<(f32, f32)> = pts.collect();
    let n = pts.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        if (a.1 > q.1) != (b.1 > q.1) && q.0 < a.0 + (q.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::geojson::extract;

    fn doc() -> GeoDoc {
        extract(
            r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"name":"box"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]]]}},
            {"type":"Feature","properties":{"name":"dot"},"geometry":{"type":"Point","coordinates":[30,5]}}]}"#,
        )
    }

    #[test]
    fn the_map_draws_land_sea_and_the_features_where_they_are() {
        let d = doc();
        let view = MapView { clon: 15.0, clat: 5.0, width: 60.0 };
        let scene = Scene { view, doc: &d, object: None, selected: None };
        let pal = MapPalette {
            ocean: (0, 0, 80),
            land: (60, 60, 60),
            coast: (200, 200, 200),
            border: (120, 120, 120),
            river: (0, 120, 200),
            city: (220, 220, 220),
            label: (255, 255, 255),
            feature: (255, 255, 0),
            feature_dim: (120, 120, 0),
            selected: (255, 0, 0),
        };
        let (w, h) = (600, 300);
        let img = raster(w, h, &scene, &pal, 11.0);
        let p = view.project(w, h);
        let px = |lon: f64, lat: f64| {
            let (x, y) = p.xy(lon, lat);
            let q = img.get_pixel(x as u32, y as u32).0;
            (q[0], q[1], q[2])
        };
        // Inside the box, the fill tints the sea (the Gulf of Guinea) yellow.
        let inside = px(2.0, 2.0);
        assert!(inside.0 > 40 && inside.2 < 80, "{inside:?}");
        // The point is drawn solid; the Sahara is land.
        assert_eq!(px(30.0, 5.0), pal.feature);
        assert_eq!(px(18.0, 17.0), pal.land);
        // Picking at the point finds the point, inside the box finds the box.
        let (x, y) = p.xy(30.0, 5.0);
        assert_eq!(hit(&scene, w, h, x + 2.0, y, 8.0), Some((0, 1)));
        let (x, y) = p.xy(2.0, 2.0);
        assert_eq!(hit(&scene, w, h, x, y, 8.0), Some((0, 0)));
        let (x, y) = p.xy(-20.0, 40.0);
        assert_eq!(hit(&scene, w, h, x, y, 8.0), None);
    }

    #[test]
    fn land_seams_are_not_coast() {
        assert!(is_seam([180.0, 10.0], [180.0, 20.0]));
        assert!(is_seam([-180.0, -89.999], [120.0, -89.999]));
        assert!(!is_seam([179.0, 10.0], [180.0, 10.0]));
    }
}
