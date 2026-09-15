//! Finding GeoJSON in a JSON document — a whole GeoJSON file, or GeoJSON
//! nested anywhere inside something larger, such as the `geometry` of an API
//! response.
//!
//! The document is read as [`json::parse`] events rather than as a tree. What
//! is kept is only what a map needs: each container's place in the document
//! while it is open, the members that make an object GeoJSON (`type`,
//! `coordinates`, `geometry`, `features`, `properties`…), and the positions,
//! gathered straight into flat lists as their numbers go by. A large file of
//! coordinates costs its positions and nothing more.
//!
//! What is found is the *outermost* GeoJSON: a FeatureCollection is one object,
//! not one per feature, and a Feature is not listed again for its geometry.
//! Anything that did not end up inside something bigger — a geometry that is
//! the value of some other key, the elements of an array of features that is
//! not a collection's — is listed on its own, with the path it was found at.

use crate::json::{self, Lit, Options, Sink};
use std::ops::Range;

/// Most properties kept per feature, for the dialog to show.
const MAX_PROPS: usize = 64;

/// A longitude/latitude box. Longitudes may run past 180 for a box that
/// crosses the antimeridian, so `lon0 <= lon1` always holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub lon0: f64,
    pub lat0: f64,
    pub lon1: f64,
    pub lat1: f64,
}

/// One drawable part of a geometry, in (longitude, latitude).
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Point([f64; 2]),
    Line(Vec<[f64; 2]>),
    /// The outer ring first, then any holes — wound in opposite directions, so
    /// a non-zero fill leaves the holes empty.
    Polygon(Vec<Vec<[f64; 2]>>),
}

/// A feature, or a bare geometry standing for one.
#[derive(Debug, Clone)]
pub struct Feature {
    /// Its bytes in the document.
    pub span: Range<usize>,
    /// A name to show for it, from its properties or its id.
    pub name: Option<String>,
    /// Its properties as `key: value` text, in the order they are written.
    pub props: Vec<(String, String)>,
    pub shapes: Vec<Shape>,
    pub bounds: Option<Bounds>,
}

/// One piece of GeoJSON found in the document.
#[derive(Debug, Clone)]
pub struct GeoObject {
    /// Where it is, as a path from the document's root: `$.data.regions[2]`.
    pub path: String,
    pub span: Range<usize>,
    /// Its GeoJSON type: `FeatureCollection`, `Feature`, `Polygon`, …
    pub kind: String,
    pub features: Vec<Feature>,
    pub bounds: Option<Bounds>,
}

/// Everything found in a document.
#[derive(Debug, Clone, Default)]
pub struct GeoDoc {
    pub objects: Vec<GeoObject>,
    /// Positions left out for not being longitude and latitude: out of range,
    /// or not numbers. GeoJSON in a projected coordinate system ends up here.
    pub skipped: usize,
    /// Syntax errors in the document; what could be read around them is used.
    pub errors: usize,
}

impl GeoDoc {
    /// The box around every object.
    pub fn bounds(&self) -> Option<Bounds> {
        union(self.objects.iter().filter_map(|o| o.bounds))
    }
}

/// Find the GeoJSON in `text`.
pub fn extract(text: &str) -> GeoDoc {
    let mut sink = GeoSink::default();
    // Read leniently: comments, trailing commas and several documents in a row
    // (GeoJSON Lines) are all worth drawing, errors or not.
    let opts = Options { comments: true, trailing_commas: true, multiple_roots: true };
    let errors = json::parse(text, opts, &mut sink).len();
    let mut objects = sink.found;
    objects.sort_by_key(|o| o.span.start);
    GeoDoc { objects, skipped: sink.skipped, errors }
}

/// A geometry, before it is known what it belongs to.
#[derive(Debug, Clone)]
struct Geometry {
    kind: String,
    shapes: Vec<Shape>,
    span: Range<usize>,
}

/// A container that turned out to be GeoJSON, waiting for its parent to say
/// whether it is part of something bigger.
#[derive(Debug)]
enum Found {
    Geometry(Geometry, String),
    Feature(Feature, String),
    Collection(GeoObject),
}

impl Found {
    /// Standing on its own, as a listed object.
    fn into_object(self) -> GeoObject {
        match self {
            Found::Geometry(g, path) => {
                let feature = feature_of(g.span.clone(), None, Vec::new(), g.shapes);
                let bounds = feature.bounds;
                GeoObject { path, span: g.span, kind: g.kind, features: vec![feature], bounds }
            }
            Found::Feature(f, path) => {
                let bounds = f.bounds;
                GeoObject {
                    path,
                    span: f.span.clone(),
                    kind: "Feature".into(),
                    features: vec![f],
                    bounds,
                }
            }
            Found::Collection(o) => o,
        }
    }
}

/// What an open container is to its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Plain,
    /// The `properties` object of what may be a feature.
    Properties,
}

#[derive(Debug)]
struct Frame {
    is_object: bool,
    path: String,
    role: Role,
    /// The key whose value comes next (objects), or the next index (arrays).
    key: Option<String>,
    len: usize,
    // The members that make an object GeoJSON.
    ty: Option<String>,
    coords: Option<Coords>,
    geometry: Option<Geometry>,
    geometries: Option<Vec<Geometry>>,
    features: Option<Vec<(Feature, String)>>,
    /// This object's own members, when it is a `properties` object; or the
    /// `properties` it was given, when it may be a feature.
    props: Vec<(String, String)>,
    id: Option<String>,
    /// An array's elements that are GeoJSON.
    items: Vec<Found>,
    /// GeoJSON further down that nothing took.
    found: Vec<GeoObject>,
}

impl Frame {
    fn new(is_object: bool, path: String, role: Role) -> Self {
        Frame {
            is_object,
            path,
            role,
            key: None,
            len: 0,
            ty: None,
            coords: None,
            geometry: None,
            geometries: None,
            features: None,
            props: Vec::new(),
            id: None,
            items: Vec::new(),
            found: Vec::new(),
        }
    }
}

#[derive(Default)]
struct GeoSink {
    stack: Vec<Frame>,
    /// A `coordinates` array being read.
    capture: Option<Capture>,
    roots: usize,
    found: Vec<GeoObject>,
    skipped: usize,
}

impl GeoSink {
    /// The path of the next value in the innermost container.
    fn child_path(&self) -> String {
        match self.stack.last() {
            None if self.roots == 0 => "$".into(),
            None => format!("${}", self.roots),
            Some(f) if f.is_object => {
                let key = f.key.as_deref().unwrap_or("?");
                if !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    format!("{}.{key}", f.path)
                } else {
                    format!("{}[{key:?}]", f.path)
                }
            }
            Some(f) => format!("{}[{}]", f.path, f.len),
        }
    }

    /// A container opening: its role, and — inside a `properties` object — a
    /// note that the member holds one.
    fn open(&mut self, is_object: bool) {
        let path = self.child_path();
        let role = match self.stack.last_mut() {
            Some(p) if p.role == Role::Properties => {
                let key = p.key.clone().unwrap_or_default();
                let shown = if is_object { "{…}" } else { "[…]" };
                push_prop(&mut p.props, key, shown.into());
                Role::Plain
            }
            Some(p) if p.is_object && is_object && p.key.as_deref() == Some("properties") => {
                Role::Properties
            }
            _ => Role::Plain,
        };
        self.stack.push(Frame::new(is_object, path, role));
    }

    /// A scalar value in the innermost container.
    fn scalar(&mut self, text: String, string: bool) {
        let Some(f) = self.stack.last_mut() else {
            self.roots += 1;
            return;
        };
        if f.is_object {
            let key = f.key.take().unwrap_or_default();
            if f.role == Role::Properties {
                push_prop(&mut f.props, key, text);
            } else if key == "type" && string {
                f.ty = Some(text);
            } else if key == "id" {
                f.id = Some(text);
            }
        } else {
            f.len += 1;
        }
    }

    /// The innermost container's value is complete: move on past it.
    fn advance(&mut self) {
        match self.stack.last_mut() {
            Some(f) if f.is_object => f.key = None,
            Some(f) => f.len += 1,
            None => self.roots += 1,
        }
    }

    /// Hand a finished piece of GeoJSON to the container it closed in.
    fn deliver(&mut self, found: Found) {
        let Some(parent) = self.stack.last_mut() else {
            self.found.push(found.into_object());
            return;
        };
        if parent.is_object {
            match (parent.key.as_deref(), found) {
                (Some("geometry"), Found::Geometry(g, _)) => parent.geometry = Some(g),
                (_, other) => parent.found.push(other.into_object()),
            }
        } else {
            parent.items.push(found);
        }
    }
}

/// Keep a property, up to [`MAX_PROPS`] of them.
fn push_prop(props: &mut Vec<(String, String)>, key: String, value: String) {
    if props.len() < MAX_PROPS {
        props.push((key, value));
    }
}

impl Sink for GeoSink {
    fn begin_object(&mut self, _at: usize) {
        if let Some(c) = self.capture.as_mut() {
            c.foreign += 1;
            return;
        }
        self.open(true);
    }

    fn begin_array(&mut self, _at: usize) {
        if let Some(c) = self.capture.as_mut() {
            c.open();
            return;
        }
        if let Some(p) = self.stack.last()
            && p.is_object
            && p.role == Role::Plain
            && p.key.as_deref() == Some("coordinates")
        {
            let mut c = Capture::default();
            c.open();
            self.capture = Some(c);
            return;
        }
        self.open(false);
    }

    fn key(&mut self, raw: &str, _span: Range<usize>) {
        if self.capture.is_some() {
            return;
        }
        if let Some(f) = self.stack.last_mut() {
            f.key = Some(json::unescape(raw).into_owned());
        }
    }

    fn string(&mut self, raw: &str, _span: Range<usize>) {
        if self.capture.is_some() {
            return;
        }
        self.scalar(json::unescape(raw).into_owned(), true);
    }

    fn number(&mut self, raw: &str, _span: Range<usize>) {
        if let Some(c) = self.capture.as_mut() {
            c.number(json::number(raw));
            return;
        }
        self.scalar(raw.to_string(), false);
    }

    fn literal(&mut self, lit: Lit, _span: Range<usize>) {
        if let Some(c) = self.capture.as_mut() {
            c.number(None);
            return;
        }
        let text = match lit {
            Lit::True => "true",
            Lit::False => "false",
            Lit::Null => "null",
        };
        self.scalar(text.into(), false);
    }

    fn end_array(&mut self, _span: Range<usize>) {
        if let Some(c) = self.capture.as_mut() {
            if c.foreign > 0 {
                return;
            }
            if c.close() {
                let c = self.capture.take().expect("just checked");
                self.skipped += c.skipped;
                if let Some(f) = self.stack.last_mut() {
                    f.coords = Some(c.finish());
                    f.key = None;
                }
            }
            return;
        }
        let Some(frame) = self.stack.pop() else { return };
        let Frame { items, found, .. } = frame;
        let mut leftovers: Vec<GeoObject> = found;
        match self.stack.last_mut() {
            Some(parent) if parent.is_object && parent.key.as_deref() == Some("features") => {
                let mut features = Vec::new();
                for item in items {
                    match item {
                        Found::Feature(f, path) => features.push((f, path)),
                        other => leftovers.push(other.into_object()),
                    }
                }
                parent.features = Some(features);
            }
            Some(parent) if parent.is_object && parent.key.as_deref() == Some("geometries") => {
                let mut geometries = Vec::new();
                for item in items {
                    match item {
                        Found::Geometry(g, _) => geometries.push(g),
                        other => leftovers.push(other.into_object()),
                    }
                }
                parent.geometries = Some(geometries);
            }
            _ => leftovers.extend(items.into_iter().map(Found::into_object)),
        }
        match self.stack.last_mut() {
            Some(parent) => parent.found.extend(leftovers),
            None => self.found.extend(leftovers),
        }
        self.advance();
    }

    fn end_object(&mut self, span: Range<usize>) {
        if let Some(c) = self.capture.as_mut() {
            c.foreign = c.foreign.saturating_sub(1);
            return;
        }
        let Some(mut frame) = self.stack.pop() else { return };
        // A properties object hands its members to the object holding it.
        if frame.role == Role::Properties {
            let props = std::mem::take(&mut frame.props);
            if let Some(parent) = self.stack.last_mut() {
                parent.props = props;
                parent.found.append(&mut frame.found);
            }
            self.advance();
            return;
        }
        let path = frame.path.clone();
        let mut leftovers = std::mem::take(&mut frame.found);
        let found = match frame.ty.as_deref() {
            Some(
                kind @ ("Point" | "MultiPoint" | "LineString" | "MultiLineString" | "Polygon"
                | "MultiPolygon"),
            ) => frame.coords.take().and_then(|c| c.shapes(kind)).map(|shapes| {
                Found::Geometry(Geometry { kind: kind.into(), shapes, span: span.clone() }, path)
            }),
            Some("GeometryCollection") => frame.geometries.take().map(|gs| {
                let shapes = gs.into_iter().flat_map(|g| g.shapes).collect();
                let g = Geometry { kind: "GeometryCollection".into(), shapes, span: span.clone() };
                Found::Geometry(g, path)
            }),
            Some("Feature") => {
                let shapes = frame.geometry.take().map(|g| g.shapes).unwrap_or_default();
                let name = feature_name(&frame.props, frame.id.as_deref());
                let props = std::mem::take(&mut frame.props);
                Some(Found::Feature(feature_of(span.clone(), name, props, shapes), path))
            }
            Some("FeatureCollection") => frame.features.take().map(|features| {
                let features: Vec<Feature> = features.into_iter().map(|(f, _)| f).collect();
                let bounds = union(features.iter().filter_map(|f| f.bounds));
                Found::Collection(GeoObject {
                    path,
                    span: span.clone(),
                    kind: "FeatureCollection".into(),
                    features,
                    bounds,
                })
            }),
            _ => None,
        };
        // Pieces this object held that it turned out not to be the owner of.
        if let Some(g) = frame.geometry.take() {
            let path = format!("{}.geometry", frame.path);
            leftovers.push(Found::Geometry(g, path).into_object());
        }
        if let Some(features) = frame.features.take() {
            leftovers.extend(features.into_iter().map(|(f, p)| Found::Feature(f, p).into_object()));
        }
        match self.stack.last_mut() {
            Some(parent) => parent.found.extend(leftovers),
            None => self.found.extend(leftovers),
        }
        if let Some(found) = found {
            self.deliver(found);
        }
        self.advance();
    }
}

/// A feature's name: the first of the usual naming properties, or its id.
fn feature_name(props: &[(String, String)], id: Option<&str>) -> Option<String> {
    ["name", "Name", "NAME", "title", "Title", "label"]
        .iter()
        .find_map(|k| props.iter().find(|(pk, v)| pk == k && !v.is_empty()).map(|(_, v)| v.clone()))
        .or_else(|| id.map(str::to_string))
}

fn feature_of(
    span: Range<usize>,
    name: Option<String>,
    props: Vec<(String, String)>,
    shapes: Vec<Shape>,
) -> Feature {
    let bounds = shapes_bounds(&shapes);
    Feature { span, name, props, shapes, bounds }
}

/// The box around some shapes, the narrower of the two ways round the world a
/// set of longitudes can be boxed.
fn shapes_bounds(shapes: &[Shape]) -> Option<Bounds> {
    let mut lons = Vec::new();
    let (mut lat0, mut lat1) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut add = |p: &[f64; 2]| {
        lons.push(p[0]);
        lat0 = lat0.min(p[1]);
        lat1 = lat1.max(p[1]);
    };
    for s in shapes {
        match s {
            Shape::Point(p) => add(p),
            Shape::Line(pts) => pts.iter().for_each(&mut add),
            Shape::Polygon(rings) => rings.iter().flatten().for_each(&mut add),
        }
    }
    if lons.is_empty() {
        return None;
    }
    let (a0, a1) =
        lons.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &l| (lo.min(l), hi.max(l)));
    // The same longitudes on 0…360: narrower for anything straddling ±180.
    let (b0, b1) = lons
        .iter()
        .map(|&l| l.rem_euclid(360.0))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), l| (lo.min(l), hi.max(l)));
    let (lon0, lon1) = if b1 - b0 < a1 - a0 { (b0, b1) } else { (a0, a1) };
    Some(Bounds { lon0, lat0, lon1, lat1 })
}

/// The box around several boxes.
pub fn union(boxes: impl Iterator<Item = Bounds>) -> Option<Bounds> {
    boxes.reduce(|a, b| Bounds {
        lon0: a.lon0.min(b.lon0),
        lat0: a.lat0.min(b.lat0),
        lon1: a.lon1.max(b.lon1),
        lat1: a.lat1.max(b.lat1),
    })
}

/// A `coordinates` array being read: its positions in one flat list, and where
/// each nested list of them ends.
#[derive(Default)]
struct Capture {
    /// The arrays open inside it, innermost last; each knows its nesting level
    /// once its first element has shown (0 holds numbers).
    arrays: Vec<CaptureArray>,
    positions: Vec<[f64; 2]>,
    /// `ends[l]`: the position count each level-`l + 1` array closed at.
    ends: [Vec<usize>; 3],
    /// The level of the `coordinates` array itself, once closed.
    level: Option<u8>,
    /// Objects nested inside, where there should be none.
    foreign: usize,
    bad: bool,
    skipped: usize,
}

#[derive(Default)]
struct CaptureArray {
    level: Option<u8>,
    nums: [f64; 2],
    count: usize,
    valid: bool,
}

impl Capture {
    fn open(&mut self) {
        self.arrays.push(CaptureArray { valid: true, ..CaptureArray::default() });
    }

    fn number(&mut self, value: Option<f64>) {
        let Some(a) = self.arrays.last_mut() else { return };
        match a.level {
            Some(l) if l > 0 => self.bad = true,
            _ => a.level = Some(0),
        }
        match value {
            Some(v) if a.count < 2 => a.nums[a.count] = v,
            None => a.valid = false,
            _ => {}
        }
        a.count += 1;
    }

    /// An array closes; true when it was the `coordinates` array itself.
    fn close(&mut self) -> bool {
        let Some(a) = self.arrays.pop() else { return true };
        let parent_level = self.arrays.last().and_then(|p| p.level);
        // An empty array takes the level its siblings have.
        let level = a.level.or_else(|| parent_level.and_then(|p| p.checked_sub(1)));
        match level {
            Some(0) => {
                let [lon, lat] = a.nums;
                if a.valid
                    && a.count >= 2
                    && (-180.0..=180.0).contains(&lon)
                    && (-90.0..=90.0).contains(&lat)
                {
                    self.positions.push([lon, lat]);
                } else {
                    self.skipped += 1;
                }
            }
            Some(l) if (l as usize) <= self.ends.len() => {
                self.ends[l as usize - 1].push(self.positions.len());
            }
            Some(_) => self.bad = true,
            None => {}
        }
        match self.arrays.last_mut() {
            Some(p) => {
                if let Some(l) = level {
                    match p.level {
                        None => p.level = Some(l + 1),
                        Some(pl) if pl != l + 1 => self.bad = true,
                        _ => {}
                    }
                }
                false
            }
            None => {
                self.level = level;
                true
            }
        }
    }

    fn finish(self) -> Coords {
        Coords { positions: self.positions, ends: self.ends, level: self.level, bad: self.bad }
    }
}

/// A finished `coordinates` value.
#[derive(Debug, Clone)]
struct Coords {
    positions: Vec<[f64; 2]>,
    ends: [Vec<usize>; 3],
    level: Option<u8>,
    bad: bool,
}

impl Coords {
    /// The shapes these coordinates make for a geometry of type `kind`, when
    /// they have the nesting that type needs.
    fn shapes(self, kind: &str) -> Option<Vec<Shape>> {
        let need = match kind {
            "Point" => 0,
            "MultiPoint" | "LineString" => 1,
            "MultiLineString" | "Polygon" => 2,
            _ => 3,
        };
        if self.bad || self.level != Some(need) {
            return None;
        }
        let pts = &self.positions;
        let runs = |ends: &[usize]| -> Vec<Range<usize>> {
            let mut start = 0;
            ends.iter()
                .map(|&e| {
                    let r = start..e;
                    start = e;
                    r
                })
                .collect()
        };
        let shapes = match kind {
            "Point" => pts.first().map(|&p| vec![Shape::Point(p)]).unwrap_or_default(),
            "MultiPoint" => pts.iter().map(|&p| Shape::Point(p)).collect(),
            "LineString" => vec![Shape::Line(unwrap_dateline(pts))],
            "MultiLineString" => runs(&self.ends[0])
                .into_iter()
                .map(|r| Shape::Line(unwrap_dateline(&pts[r])))
                .collect(),
            "Polygon" => vec![polygon(runs(&self.ends[0]).into_iter().map(|r| &pts[r]))],
            _ => {
                let rings = runs(&self.ends[0]);
                let mut shapes = Vec::new();
                let mut next = 0;
                for end in &self.ends[1] {
                    let mine: Vec<&[[f64; 2]]> = rings[next..]
                        .iter()
                        .take_while(|r| r.end <= *end)
                        .map(|r| &pts[r.clone()])
                        .collect();
                    next += mine.len();
                    shapes.push(polygon(mine.into_iter()));
                }
                shapes
            }
        };
        Some(shapes.into_iter().filter(|s| !matches!(s, Shape::Line(l) if l.is_empty())).collect())
    }
}

/// A line made continuous across the antimeridian: a step of more than 180°
/// is the short way round, so the rest of the line is carried on past ±180
/// rather than jumping back across the whole map.
fn unwrap_dateline(pts: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(pts.len());
    let mut shift = 0.0;
    let mut prev: Option<f64> = None;
    for &[lon, lat] in pts {
        if let Some(p) = prev {
            let step = lon + shift - p;
            if step > 180.0 {
                shift -= 360.0;
            } else if step < -180.0 {
                shift += 360.0;
            }
        }
        out.push([lon + shift, lat]);
        prev = Some(lon + shift);
    }
    out
}

/// A polygon from its rings: continuous across the antimeridian, the outer
/// ring wound one way and the holes the other.
fn polygon<'a>(rings: impl Iterator<Item = &'a [[f64; 2]]>) -> Shape {
    let mut out: Vec<Vec<[f64; 2]>> = Vec::new();
    for (i, ring) in rings.enumerate() {
        let mut ring = unwrap_dateline(ring);
        if ring.len() < 3 {
            continue;
        }
        let outer = i == 0;
        if (signed_area(&ring) > 0.0) != outer {
            ring.reverse();
        }
        out.push(ring);
    }
    Shape::Polygon(out)
}

/// Twice the shoelace area: positive for a ring wound counter-clockwise.
fn signed_area(ring: &[[f64; 2]]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feature_collection_is_one_object_with_its_features() {
        let text = r#"{"type": "FeatureCollection", "features": [
            {"type": "Feature", "properties": {"name": "Home", "pop": 3, "tags": ["a"]},
             "geometry": {"type": "Point", "coordinates": [16.37, 48.21]}},
            {"type": "Feature", "id": "road-7", "properties": null,
             "geometry": {"type": "LineString", "coordinates": [[0, 0], [1, 1], [2, 0]]}}
        ]}"#;
        let doc = extract(text);
        assert_eq!(doc.objects.len(), 1);
        let o = &doc.objects[0];
        assert_eq!((o.path.as_str(), o.kind.as_str()), ("$", "FeatureCollection"));
        assert_eq!(o.features.len(), 2);
        let home = &o.features[0];
        assert_eq!(home.name.as_deref(), Some("Home"));
        assert_eq!(
            home.props,
            [
                ("name".into(), "Home".into()),
                ("pop".into(), "3".into()),
                ("tags".into(), "[…]".into())
            ],
            "properties keep their order, nested values shown as such"
        );
        assert_eq!(home.shapes, [Shape::Point([16.37, 48.21])]);
        assert_eq!(o.features[1].name.as_deref(), Some("road-7"), "the id when there is no name");
        assert!(text[o.features[1].span.clone()].starts_with("{\"type\": \"Feature\", \"id\""));
        let b = o.bounds.unwrap();
        assert_eq!((b.lon0, b.lat0, b.lon1, b.lat1), (0.0, 0.0, 16.37, 48.21));
    }

    #[test]
    fn geojson_nested_in_other_json_is_found_where_it_is() {
        let text = r#"{"status": "ok", "data": {"regions": [
            {"id": 1, "shape": {"type": "Polygon", "coordinates": [[[0,0],[4,0],[4,4],[0,0]]]}},
            {"id": 2, "outline": {"type": "Polygon", "coordinates": [[[5,5],[6,5],[6,6],[5,5]]]}}
        ], "note": {"type": "NotGeoJSON", "coordinates": [1, 2]}}}"#;
        let doc = extract(text);
        let paths: Vec<&str> = doc.objects.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(paths, ["$.data.regions[0].shape", "$.data.regions[1].outline"]);
        assert_eq!(doc.objects[0].kind, "Polygon");
        assert!(text[doc.objects[1].span.clone()].starts_with("{\"type\": \"Polygon\""));
    }

    #[test]
    fn features_outside_a_collection_are_each_listed() {
        let doc = extract(
            r#"[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}},
                {"type":"Feature","geometry":null,"properties":{"title":"Nowhere"}}]"#,
        );
        assert_eq!(doc.objects.len(), 2);
        assert_eq!(doc.objects[0].path, "$[0]");
        assert_eq!(doc.objects[1].features[0].name.as_deref(), Some("Nowhere"));
        assert!(doc.objects[1].features[0].shapes.is_empty(), "a feature may have no geometry");
    }

    #[test]
    fn every_geometry_type_makes_its_shapes() {
        let geom = |t: &str, c: &str| {
            let doc = extract(&format!(r#"{{"type":"{t}","coordinates":{c}}}"#));
            doc.objects.first().map(|o| o.features[0].shapes.clone()).unwrap_or_default()
        };
        assert_eq!(geom("MultiPoint", "[[1,2],[3,4]]").len(), 2);
        assert_eq!(geom("MultiLineString", "[[[0,0],[1,1]],[[2,2],[3,3],[4,4]]]").len(), 2);
        let poly =
            geom("Polygon", "[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[2,2],[2,4],[4,4],[2,2]]]");
        let Shape::Polygon(rings) = &poly[0] else { panic!("{poly:?}") };
        assert_eq!(rings.len(), 2);
        assert!(
            signed_area(&rings[0]) > 0.0 && signed_area(&rings[1]) < 0.0,
            "wound opposite ways"
        );
        let multi = geom(
            "MultiPolygon",
            "[[[[0,0],[1,0],[1,1],[0,0]]],[[[5,5],[6,5],[6,6],[5,5]],[[5.2,5.2],[5.4,5.2],[5.4,5.4],[5.2,5.2]]]]",
        );
        assert_eq!(multi.len(), 2);
        assert!(matches!(&multi[1], Shape::Polygon(r) if r.len() == 2));
        // The wrong nesting for the type is no geometry at all.
        assert!(geom("Polygon", "[[0,0],[1,1]]").is_empty());
        let collection = extract(
            r#"{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,1]},{"type":"LineString","coordinates":[[0,0],[2,2]]}]}"#,
        );
        assert_eq!(collection.objects.len(), 1);
        assert_eq!(collection.objects[0].features[0].shapes.len(), 2);
    }

    #[test]
    fn positions_that_are_not_longitude_and_latitude_are_left_out_and_counted() {
        let doc = extract(
            r#"{"type":"LineString","coordinates":[[0,0],[500000,4000000],[1,1],["x",2]]}"#,
        );
        assert_eq!(doc.skipped, 2);
        assert_eq!(doc.objects[0].features[0].shapes, [Shape::Line(vec![[0.0, 0.0], [1.0, 1.0]])]);
    }

    #[test]
    fn a_shape_across_the_antimeridian_stays_whole_and_boxed_narrowly() {
        let doc = extract(r#"{"type":"LineString","coordinates":[[179,0],[-179,1]]}"#);
        let Shape::Line(line) = &doc.objects[0].features[0].shapes[0] else { panic!() };
        assert_eq!(line[1][0], 181.0, "carried on past 180 rather than jumping back");
        let b = doc.objects[0].bounds.unwrap();
        assert!(b.lon1 - b.lon0 <= 2.0 + 1e-9, "two degrees wide, not 358: {b:?}");
    }

    #[test]
    fn what_parses_before_and_after_a_syntax_error_is_still_found() {
        let doc = extract(
            r#"[{"type":"Point","coordinates":[1,2]} {"type":"Point","coordinates":[3,4],}]"#,
        );
        assert_eq!(doc.errors, 1);
        assert_eq!(doc.objects.len(), 2);
    }

    #[test]
    fn a_feature_s_own_geometry_is_not_listed_again() {
        let doc = extract(r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]}}"#);
        assert_eq!(doc.objects.len(), 1);
        assert_eq!(doc.objects[0].kind, "Feature");
        // A geometry under a key that is not a feature's is listed on its own.
        let doc = extract(r#"{"geometry":{"type":"Point","coordinates":[1,2]}}"#);
        assert_eq!(doc.objects.len(), 1);
        assert_eq!(doc.objects[0].path, "$.geometry");
    }
}
