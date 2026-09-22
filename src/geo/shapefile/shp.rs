//! The `.shp` itself and the `.shx` index beside it.
//!
//! The format is small: a 100-byte header, then one record per shape, each with
//! a big-endian number and length in front of little-endian content. A file
//! holds exactly one shape type (a Null shape may appear in any of them), which
//! is why a drawing that changes the type cannot simply be written back.
//!
//! **Winding matters and is the opposite of GeoJSON's.** A shapefile writes a
//! polygon's outer ring clockwise and its holes counter-clockwise; GeoJSON
//! (RFC 7946) asks for the reverse. Rings are therefore flipped in both
//! directions — see [`ring_is_clockwise`]. Getting this wrong produces a file
//! that looks right here and draws inside-out everywhere else.

use crate::geo::geojson::Shape;

/// The magic in the first four bytes, big-endian.
const FILE_CODE: i32 = 9994;
/// The only version the format has.
const VERSION: i32 = 1000;
/// Both headers are this long.
pub const HEADER_LEN: usize = 100;

/// What kind of geometry a file holds. Only the 2D types are written; the Z and
/// M variants are read by dropping the extra ordinates, which is what a map
/// that draws longitude and latitude can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeType {
    Null,
    Point,
    PolyLine,
    Polygon,
    MultiPoint,
}

impl ShapeType {
    /// The type code as it is written, for the 2D forms.
    pub fn code(self) -> i32 {
        match self {
            ShapeType::Null => 0,
            ShapeType::Point => 1,
            ShapeType::PolyLine => 3,
            ShapeType::Polygon => 5,
            ShapeType::MultiPoint => 8,
        }
    }

    /// The kind a type code means, with the Z and M variants folded onto their
    /// 2D form. `None` for a code the format does not define.
    pub fn from_code(code: i32) -> Option<ShapeType> {
        Some(match code {
            0 => ShapeType::Null,
            1 | 11 | 21 => ShapeType::Point,
            3 | 13 | 23 => ShapeType::PolyLine,
            5 | 15 | 25 => ShapeType::Polygon,
            8 | 18 | 28 => ShapeType::MultiPoint,
            _ => return None,
        })
    }

    /// Whether a code carries a Z ordinate after the points.
    fn has_z(code: i32) -> bool {
        matches!(code, 11 | 13 | 15 | 18)
    }

    /// Whether a code carries an M (measure) ordinate after the points.
    fn has_m(code: i32) -> bool {
        matches!(code, 21 | 23 | 25 | 28)
    }
}

/// A bounding box in the file's own coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    pub xmin: f64,
    pub ymin: f64,
    pub xmax: f64,
    pub ymax: f64,
}

impl Bbox {
    /// A box that contains nothing, ready to be grown.
    fn empty() -> Bbox {
        Bbox { xmin: f64::MAX, ymin: f64::MAX, xmax: f64::MIN, ymax: f64::MIN }
    }

    fn add(&mut self, p: [f64; 2]) {
        self.xmin = self.xmin.min(p[0]);
        self.ymin = self.ymin.min(p[1]);
        self.xmax = self.xmax.max(p[0]);
        self.ymax = self.ymax.max(p[1]);
    }

    /// Zeroed when nothing was added, which is what an empty file writes.
    fn finish(self) -> Bbox {
        if self.xmin > self.xmax {
            Bbox { xmin: 0.0, ymin: 0.0, xmax: 0.0, ymax: 0.0 }
        } else {
            self
        }
    }
}

/// Whether a ring is wound clockwise, by the sign of twice its signed area.
///
/// This is the shoelace formula. A shapefile's outer rings are clockwise and
/// its holes counter-clockwise; GeoJSON is the other way round.
pub fn ring_is_clockwise(ring: &[[f64; 2]]) -> bool {
    let mut area = 0.0;
    for w in ring.windows(2) {
        area += (w[1][0] - w[0][0]) * (w[1][1] + w[0][1]);
    }
    // Close the ring if the caller did not.
    if let (Some(first), Some(last)) = (ring.first(), ring.last())
        && first != last
    {
        area += (first[0] - last[0]) * (first[1] + last[1]);
    }
    area > 0.0
}

/// Read every shape in `bytes`, and the type the file declares.
///
/// Shapes come back in the file's own coordinates: reprojection, if the `.prj`
/// calls for it, happens above this.
pub fn read(bytes: &[u8]) -> Option<(ShapeType, Vec<Shape>)> {
    if bytes.len() < HEADER_LEN {
        return None;
    }
    if i32::from_be_bytes(bytes[0..4].try_into().ok()?) != FILE_CODE {
        return None;
    }
    let declared = i32::from_le_bytes(bytes[32..36].try_into().ok()?);
    let kind = ShapeType::from_code(declared)?;

    let mut shapes = Vec::new();
    let mut at = HEADER_LEN;
    while at + 8 <= bytes.len() {
        let len_words = i32::from_be_bytes(bytes[at + 4..at + 8].try_into().ok()?);
        if len_words < 0 {
            break;
        }
        let content = at + 8;
        let end = content + len_words as usize * 2;
        let Some(rec) = bytes.get(content..end.min(bytes.len())) else {
            break;
        };
        if let Some(s) = read_record(rec) {
            shapes.push(s);
        }
        at = end;
    }
    Some((kind, shapes))
}

/// One record's content, after its number and length.
fn read_record(rec: &[u8]) -> Option<Shape> {
    if rec.len() < 4 {
        return None;
    }
    let code = i32::from_le_bytes(rec[0..4].try_into().ok()?);
    let f64at = |b: &[u8], at: usize| -> Option<f64> {
        Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
    };
    match ShapeType::from_code(code)? {
        // A Null shape is a hole in the sequence; it has no geometry at all.
        ShapeType::Null => None,
        ShapeType::Point => Some(Shape::Point([f64at(rec, 4)?, f64at(rec, 12)?])),
        ShapeType::MultiPoint => {
            let n = i32::from_le_bytes(rec[36..40].try_into().ok()?) as usize;
            let pts = read_points(rec, 40, n)?;
            // A map draws a multipoint as its points; the first stands for it
            // when only one shape can be kept.
            Some(Shape::Line(pts))
        }
        kind @ (ShapeType::PolyLine | ShapeType::Polygon) => {
            let nparts = i32::from_le_bytes(rec[36..40].try_into().ok()?) as usize;
            let npoints = i32::from_le_bytes(rec[40..44].try_into().ok()?) as usize;
            let parts_at = 44;
            let points_at = parts_at + nparts * 4;
            let mut starts = Vec::with_capacity(nparts);
            for i in 0..nparts {
                let at = parts_at + i * 4;
                starts.push(i32::from_le_bytes(rec.get(at..at + 4)?.try_into().ok()?) as usize);
            }
            let pts = read_points(rec, points_at, npoints)?;
            // Z and M ordinates follow the points; they are read past, not used.
            let _ = (ShapeType::has_z(code), ShapeType::has_m(code));

            let mut rings: Vec<Vec<[f64; 2]>> = Vec::with_capacity(nparts);
            for (i, start) in starts.iter().enumerate() {
                let end = starts.get(i + 1).copied().unwrap_or(npoints);
                if *start <= end && end <= pts.len() {
                    rings.push(pts[*start..end].to_vec());
                }
            }
            if rings.is_empty() {
                return None;
            }
            if kind == ShapeType::Polygon {
                // Into GeoJSON's winding: outer counter-clockwise, holes
                // clockwise — the opposite of what the file holds.
                for ring in &mut rings {
                    ring.reverse();
                }
                Some(Shape::Polygon(rings))
            } else if rings.len() == 1 {
                Some(Shape::Line(rings.remove(0)))
            } else {
                // Several parts: the map draws each, so keep them as a polygon
                // -shaped list of lines is wrong; give back the longest run and
                // let the caller split. Callers use `read_parts` for the rest.
                Some(Shape::Line(rings.concat()))
            }
        }
    }
}

/// `n` consecutive (x, y) pairs starting at `at`.
fn read_points(rec: &[u8], at: usize, n: usize) -> Option<Vec<[f64; 2]>> {
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let o = at + i * 16;
        let x = f64::from_le_bytes(rec.get(o..o + 8)?.try_into().ok()?);
        let y = f64::from_le_bytes(rec.get(o + 8..o + 16)?.try_into().ok()?);
        pts.push([x, y]);
    }
    Some(pts)
}

/// Write `shapes` as a `.shp` and its `.shx`, both headers included.
///
/// Every shape must suit `kind`; one that does not is written as a Null shape
/// rather than silently changing the file's type, which the format forbids.
pub fn write(kind: ShapeType, shapes: &[Shape]) -> (Vec<u8>, Vec<u8>) {
    let mut body: Vec<u8> = Vec::new();
    let mut index: Vec<(u32, u32)> = Vec::new();
    let mut bbox = Bbox::empty();

    for (i, shape) in shapes.iter().enumerate() {
        let content = encode(kind, shape, &mut bbox);
        let offset_words = ((HEADER_LEN + body.len()) / 2) as u32;
        let len_words = (content.len() / 2) as u32;
        body.extend_from_slice(&((i + 1) as i32).to_be_bytes());
        body.extend_from_slice(&(len_words as i32).to_be_bytes());
        body.extend_from_slice(&content);
        index.push((offset_words, len_words));
    }
    let bbox = bbox.finish();

    let shp_words = (HEADER_LEN + body.len()) / 2;
    let mut shp = header(kind, shp_words, bbox);
    shp.extend_from_slice(&body);

    let shx_words = (HEADER_LEN + index.len() * 8) / 2;
    let mut shx = header(kind, shx_words, bbox);
    for (off, len) in index {
        shx.extend_from_slice(&(off as i32).to_be_bytes());
        shx.extend_from_slice(&(len as i32).to_be_bytes());
    }
    (shp, shx)
}

/// The 100-byte header both files share. `words` is the whole file's length in
/// 16-bit words, which is how the format counts.
fn header(kind: ShapeType, words: usize, b: Bbox) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend_from_slice(&FILE_CODE.to_be_bytes());
    h.extend_from_slice(&[0u8; 20]);
    h.extend_from_slice(&(words as i32).to_be_bytes());
    h.extend_from_slice(&VERSION.to_le_bytes());
    h.extend_from_slice(&kind.code().to_le_bytes());
    for v in [b.xmin, b.ymin, b.xmax, b.ymax] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    // Z and M ranges: zero, since only the 2D forms are written.
    h.extend_from_slice(&[0u8; 32]);
    debug_assert_eq!(h.len(), HEADER_LEN);
    h
}

/// One shape's record content, growing `bbox` by every position written.
fn encode(kind: ShapeType, shape: &Shape, bbox: &mut Bbox) -> Vec<u8> {
    let mut out = Vec::new();
    let null = |out: &mut Vec<u8>| out.extend_from_slice(&0i32.to_le_bytes());

    match (kind, shape) {
        (ShapeType::Point, Shape::Point(p)) => {
            bbox.add(*p);
            out.extend_from_slice(&kind.code().to_le_bytes());
            out.extend_from_slice(&p[0].to_le_bytes());
            out.extend_from_slice(&p[1].to_le_bytes());
        }
        (ShapeType::MultiPoint, Shape::Line(pts)) => {
            let mut own = Bbox::empty();
            pts.iter().for_each(|p| {
                own.add(*p);
                bbox.add(*p);
            });
            out.extend_from_slice(&kind.code().to_le_bytes());
            write_bbox(&mut out, own.finish());
            out.extend_from_slice(&(pts.len() as i32).to_le_bytes());
            write_points(&mut out, pts);
        }
        (ShapeType::PolyLine, Shape::Line(pts)) => {
            write_parts(&mut out, kind, std::slice::from_ref(pts), bbox);
        }
        (ShapeType::Polygon, Shape::Polygon(rings)) => {
            // Back to the file's winding: outer clockwise, holes counter-
            // clockwise, which is the reverse of what GeoJSON holds.
            let mut flipped: Vec<Vec<[f64; 2]>> = Vec::with_capacity(rings.len());
            for (i, ring) in rings.iter().enumerate() {
                let mut r = ring.clone();
                // A shapefile ring is explicitly closed.
                if r.first() != r.last()
                    && let Some(first) = r.first().copied()
                {
                    r.push(first);
                }
                let outer = i == 0;
                let want_clockwise = outer;
                if ring_is_clockwise(&r) != want_clockwise {
                    r.reverse();
                }
                flipped.push(r);
            }
            write_parts(&mut out, kind, &flipped, bbox);
        }
        // A shape that does not suit the file's type cannot be written into it.
        _ => null(&mut out),
    }
    out
}

/// A multi-part record: the box, the part offsets, then every point.
fn write_parts(out: &mut Vec<u8>, kind: ShapeType, parts: &[Vec<[f64; 2]>], bbox: &mut Bbox) {
    let mut own = Bbox::empty();
    for p in parts.iter().flatten() {
        own.add(*p);
        bbox.add(*p);
    }
    let total: usize = parts.iter().map(Vec::len).sum();
    out.extend_from_slice(&kind.code().to_le_bytes());
    write_bbox(out, own.finish());
    out.extend_from_slice(&(parts.len() as i32).to_le_bytes());
    out.extend_from_slice(&(total as i32).to_le_bytes());
    let mut start = 0i32;
    for p in parts {
        out.extend_from_slice(&start.to_le_bytes());
        start += p.len() as i32;
    }
    for p in parts {
        write_points(out, p);
    }
}

fn write_bbox(out: &mut Vec<u8>, b: Bbox) {
    for v in [b.xmin, b.ymin, b.xmax, b.ymax] {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn write_points(out: &mut Vec<u8>, pts: &[[f64; 2]]) {
    for p in pts {
        out.extend_from_slice(&p[0].to_le_bytes());
        out.extend_from_slice(&p[1].to_le_bytes());
    }
}
