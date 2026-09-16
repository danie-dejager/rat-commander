//! Editing GeoJSON where it is written.
//!
//! The text stays the only copy of the data. An edit is one replacement of a
//! span of it — a geometry's `coordinates` array written out again, a feature
//! added to or taken out of a collection's `features` array — which the editor
//! makes as a single undo step, and which [`apply`] follows in the [`GeoDoc`]
//! read from the text, without reading the whole document again.
//!
//! What is written matches the layout around it ([`Style`]): on one line or
//! indented, with or without spaces, a position or a number to a line. The
//! numbers of positions that were not moved are kept exactly as written.

use super::geojson::{self, GeoDoc, NAME_KEYS, Shape};
use crate::json::{self, Lit, Options, Sink};
use ropey::Rope;
use std::ops::Range;

/// Most text a layout is guessed from.
const SAMPLE: usize = 4096;

/// Why an edit cannot be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The feature has no geometry.
    NoGeometry,
    /// Its coordinates are not longitudes and latitudes — a projected
    /// coordinate system — or not plain numbers.
    NotLonLat,
    /// A line would be left with fewer than two positions.
    LineTooShort,
    /// A polygon would be left with fewer than three positions.
    PolygonTooSmall,
    /// Only a Feature has properties.
    NotAFeature,
    /// Nothing but its text can take it out: GeoJSON nested in other JSON.
    Nested,
    /// A new FeatureCollection would land inside the GeoJSON the editor's
    /// cursor is in.
    CursorInside,
}

/// A geometry type that has positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Point,
    MultiPoint,
    LineString,
    MultiLineString,
    Polygon,
    MultiPolygon,
}

/// What a geometry's paths are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// A single position.
    Point,
    Line,
    /// A closed ring, kept without the position that closes it.
    Ring,
}

impl Kind {
    pub fn parse(name: &str) -> Option<Kind> {
        Some(match name {
            "Point" => Kind::Point,
            "MultiPoint" => Kind::MultiPoint,
            "LineString" => Kind::LineString,
            "MultiLineString" => Kind::MultiLineString,
            "Polygon" => Kind::Polygon,
            "MultiPolygon" => Kind::MultiPolygon,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Point => "Point",
            Kind::MultiPoint => "MultiPoint",
            Kind::LineString => "LineString",
            Kind::MultiLineString => "MultiLineString",
            Kind::Polygon => "Polygon",
            Kind::MultiPolygon => "MultiPolygon",
        }
    }

    pub fn path(self) -> PathKind {
        match self {
            Kind::Point | Kind::MultiPoint => PathKind::Point,
            Kind::LineString | Kind::MultiLineString => PathKind::Line,
            Kind::Polygon | Kind::MultiPolygon => PathKind::Ring,
        }
    }
}

/// A position: its longitude and latitude, and the text of every number in it
/// — an altitude after them is carried along as it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Pos {
    pub lon: f64,
    pub lat: f64,
    nums: Vec<String>,
}

impl Pos {
    /// A new position, its numbers rounded to `decimals` places.
    pub fn new(lon: f64, lat: f64, decimals: usize) -> Pos {
        let mut p = Pos { lon: 0.0, lat: 0.0, nums: vec![String::new(), String::new()] };
        p.set(lon, lat, decimals);
        p
    }

    /// Move it, keeping any altitude.
    pub fn set(&mut self, lon: f64, lat: f64, decimals: usize) {
        let lon = super::view::wrap180(lon);
        let lat = lat.clamp(-90.0, 90.0);
        self.nums[0] = number(lon, decimals);
        self.nums[1] = number(lat, decimals);
        // What is kept is what the text will read back as.
        self.lon = json::number(&self.nums[0]).unwrap_or(lon);
        self.lat = json::number(&self.nums[1]).unwrap_or(lat);
    }

    pub fn xy(&self) -> [f64; 2] {
        [self.lon, self.lat]
    }
}

/// Places after the decimal point worth keeping for a position placed by
/// pointing at a map with `deg_per_px` degrees to a pixel: one past the
/// pixel's size, and never finer than a centimetre or so.
pub fn decimals(deg_per_px: f64) -> usize {
    if deg_per_px.is_nan() || deg_per_px <= 0.0 {
        return 6;
    }
    ((-deg_per_px.log10()).ceil() + 1.0).clamp(1.0, 7.0) as usize
}

/// `v` to `decimals` places, without the zeros a person would not write.
fn number(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { &s };
    if s == "-0" { "0".into() } else { s.into() }
}

/// One of a feature's positions: which geometry, which part of it (the
/// members of a Multi… type), which path of that (a polygon's rings) and which
/// position along it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vertex {
    pub geom: usize,
    pub part: usize,
    pub path: usize,
    pub index: usize,
}

/// What taking out a position took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// Just the position; this one is selected next.
    Position(Vertex),
    /// The whole path or part it was on.
    Path,
    /// Nothing: a single Point cannot lose its position, and the feature
    /// itself is what should go.
    Feature,
}

/// A geometry's positions, in the one layout every type fits: parts, each a
/// list of paths, each a list of positions.
#[derive(Debug, Clone, PartialEq)]
pub struct Geom {
    pub kind: Kind,
    pub parts: Vec<Vec<Vec<Pos>>>,
}

impl Geom {
    /// Read a geometry of type `kind` from the text of its `coordinates`.
    pub fn parse(kind: Kind, coords: &str) -> Result<Geom, Refusal> {
        let root = parse_numbers(coords).ok_or(Refusal::NotLonLat)?;
        let rings = |n: &Node| -> Result<Vec<Vec<Pos>>, Refusal> {
            items(n)?.iter().map(|r| positions(r).map(unclose)).collect()
        };
        let parts = match kind {
            Kind::Point => vec![vec![vec![position(&root)?]]],
            Kind::MultiPoint => positions(&root)?.into_iter().map(|p| vec![vec![p]]).collect(),
            Kind::LineString => vec![vec![positions(&root)?]],
            Kind::MultiLineString => {
                items(&root)?.iter().map(|l| Ok(vec![positions(l)?])).collect::<Result<_, _>>()?
            }
            Kind::Polygon => vec![rings(&root)?],
            Kind::MultiPolygon => items(&root)?.iter().map(rings).collect::<Result<_, _>>()?,
        };
        Ok(Geom { kind, parts })
    }

    /// A new geometry of one path through `pts`.
    pub fn from_path(kind: Kind, pts: &[[f64; 2]], decimals: usize) -> Geom {
        let path: Vec<Pos> = pts.iter().map(|p| Pos::new(p[0], p[1], decimals)).collect();
        Geom { kind, parts: vec![vec![path]] }
    }

    /// Its coordinates, as JSON to write.
    fn tree(&self) -> Node {
        let pos = |p: &Pos| Node::List(p.nums.iter().cloned().map(Node::Num).collect());
        let line = |pts: &[Pos]| Node::List(pts.iter().map(pos).collect());
        let ring = |pts: &[Pos]| Node::List(pts.iter().chain(pts.first()).map(pos).collect());
        let first = |part: &[Vec<Pos>]| part.first().cloned().unwrap_or_default();
        match self.kind {
            Kind::Point => match self.parts.first().and_then(|p| p.first()?.first()) {
                Some(p) => pos(p),
                None => Node::List(Vec::new()),
            },
            Kind::MultiPoint => {
                Node::List(self.parts.iter().filter_map(|p| p.first()?.first()).map(pos).collect())
            }
            Kind::LineString => line(&self.parts.first().map(|p| first(p)).unwrap_or_default()),
            Kind::MultiLineString => {
                Node::List(self.parts.iter().map(|p| line(&first(p))).collect())
            }
            Kind::Polygon => Node::List(
                self.parts.first().map(|p| p.iter().map(|r| ring(r)).collect()).unwrap_or_default(),
            ),
            Kind::MultiPolygon => Node::List(
                self.parts
                    .iter()
                    .map(|p| Node::List(p.iter().map(|r| ring(r)).collect()))
                    .collect(),
            ),
        }
    }

    /// Its shapes to draw, made as reading its text would make them.
    pub fn shapes(&self) -> Vec<Shape> {
        let xy = |pts: &[Pos]| pts.iter().map(Pos::xy).collect::<Vec<_>>();
        let mut out = Vec::new();
        for part in &self.parts {
            match self.kind.path() {
                PathKind::Point => {
                    out.extend(part.iter().flatten().map(|p| Shape::Point(p.xy())));
                }
                PathKind::Line => {
                    out.extend(part.iter().map(|l| Shape::Line(geojson::unwrap_dateline(&xy(l)))))
                }
                PathKind::Ring => {
                    let rings: Vec<Vec<[f64; 2]>> = part.iter().map(|r| xy(r)).collect();
                    out.push(geojson::polygon(rings.iter().map(Vec::as_slice)));
                }
            }
        }
        out
    }
}

/// The positions of a feature being edited: each of its geometries.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub geoms: Vec<Geom>,
}

impl Model {
    /// Read the geometries of `feature` from `rope` for editing.
    pub fn read(rope: &Rope, feature: &geojson::Feature) -> Result<Model, Refusal> {
        if feature.geoms.is_empty() {
            return Err(Refusal::NoGeometry);
        }
        let geoms = feature
            .geoms
            .iter()
            .map(|g| {
                let kind = Kind::parse(&g.kind).ok_or(Refusal::NotLonLat)?;
                Geom::parse(kind, &rope.byte_slice(g.coords.clone()).to_string())
            })
            .collect::<Result<_, _>>()?;
        Ok(Model { geoms })
    }

    /// Every path: where it starts, what it is, and its positions.
    pub fn paths(&self) -> impl Iterator<Item = (Vertex, PathKind, &[Pos])> {
        self.geoms.iter().enumerate().flat_map(|(gi, g)| {
            g.parts.iter().enumerate().flat_map(move |(pi, part)| {
                part.iter().enumerate().map(move |(ri, path)| {
                    let v = Vertex { geom: gi, part: pi, path: ri, index: 0 };
                    (v, g.kind.path(), path.as_slice())
                })
            })
        })
    }

    fn path_mut(&mut self, v: Vertex) -> Option<&mut Vec<Pos>> {
        self.geoms.get_mut(v.geom)?.parts.get_mut(v.part)?.get_mut(v.path)
    }

    pub fn position(&self, v: Vertex) -> Option<&Pos> {
        self.geoms.get(v.geom)?.parts.get(v.part)?.get(v.path)?.get(v.index)
    }

    pub fn shapes(&self) -> Vec<Shape> {
        self.geoms.iter().flat_map(Geom::shapes).collect()
    }

    /// Move a position.
    pub fn set(&mut self, v: Vertex, lon: f64, lat: f64, decimals: usize) {
        if let Some(p) = self.path_mut(v).and_then(|path| path.get_mut(v.index)) {
            p.set(lon, lat, decimals);
        }
    }

    /// Put `pos` into a line or ring at `v`, pushing along what was there.
    /// Points take no more positions.
    pub fn insert(&mut self, v: Vertex, pos: Pos) -> Option<Vertex> {
        if self.geoms.get(v.geom)?.kind.path() == PathKind::Point {
            return None;
        }
        let path = self.path_mut(v)?;
        if v.index > path.len() {
            return None;
        }
        path.insert(v.index, pos);
        Some(v)
    }

    /// Take out a position. A path left too short goes with it when it is a
    /// hole or one part of several; otherwise the edit is refused.
    pub fn remove(&mut self, v: Vertex) -> Result<Removal, Refusal> {
        let missing = Err(Refusal::NoGeometry);
        let Some(g) = self.geoms.get_mut(v.geom) else { return missing };
        let kind = g.kind.path();
        let parts = g.parts.len();
        let Some(part) = g.parts.get_mut(v.part) else { return missing };
        let Some(path) = part.get_mut(v.path) else { return missing };
        if v.index >= path.len() {
            return missing;
        }
        let min = match kind {
            PathKind::Point => 1,
            PathKind::Line => 2,
            PathKind::Ring => 3,
        };
        if path.len() > min {
            path.remove(v.index);
            return Ok(Removal::Position(Vertex { index: v.index.saturating_sub(1), ..v }));
        }
        if kind == PathKind::Ring && v.path > 0 {
            part.remove(v.path);
        } else if parts > 1 {
            g.parts.remove(v.part);
        } else {
            return match kind {
                PathKind::Point => Ok(Removal::Feature),
                PathKind::Line => Err(Refusal::LineTooShort),
                PathKind::Ring => Err(Refusal::PolygonTooSmall),
            };
        }
        Ok(Removal::Path)
    }

    /// The position after (or before) `v`, going on into the next path; the
    /// first (or last) of all when there is no `v`.
    pub fn step(&self, v: Option<Vertex>, forward: bool) -> Option<Vertex> {
        let all: Vec<Vertex> = self
            .paths()
            .flat_map(|(start, _, pts)| (0..pts.len()).map(move |index| Vertex { index, ..start }))
            .collect();
        if all.is_empty() {
            return None;
        }
        let at = v.and_then(|v| all.iter().position(|&x| x == v));
        let n = all.len();
        Some(
            all[match (at, forward) {
                (None, true) => 0,
                (None, false) => n - 1,
                (Some(i), true) => (i + 1) % n,
                (Some(i), false) => (i + n - 1) % n,
            }],
        )
    }

    /// The edit that writes geometry `gi` back over its coordinates in
    /// `rope`: feature `fi` of object `oi` of `doc`.
    pub fn write(
        &self,
        rope: &Rope,
        doc: &GeoDoc,
        (oi, fi): (usize, usize),
        gi: usize,
    ) -> Option<Change> {
        let span = doc.objects.get(oi)?.features.get(fi)?.geoms.get(gi)?.coords.clone();
        let style = Style::detect(&sample(rope, span.clone()));
        let text = write(&self.geoms.get(gi)?.tree(), &style, &indent_at(rope, span.start)).text;
        Some(Change { start: span.start, end: span.end, text, scope: Scope::Feature(oi, fi) })
    }
}

fn unclose(mut ring: Vec<Pos>) -> Vec<Pos> {
    if ring.len() > 1 && ring.first().map(Pos::xy) == ring.last().map(Pos::xy) {
        ring.pop();
    }
    ring
}

fn items(n: &Node) -> Result<&[Node], Refusal> {
    match n {
        Node::List(items) => Ok(items),
        _ => Err(Refusal::NotLonLat),
    }
}

fn positions(n: &Node) -> Result<Vec<Pos>, Refusal> {
    items(n)?.iter().map(position).collect()
}

fn position(n: &Node) -> Result<Pos, Refusal> {
    let nums: Vec<String> = items(n)?
        .iter()
        .map(|i| match i {
            Node::Num(s) => Ok(s.clone()),
            _ => Err(Refusal::NotLonLat),
        })
        .collect::<Result<_, _>>()?;
    let value = |i: usize| nums.get(i).and_then(|s| json::number(s)).ok_or(Refusal::NotLonLat);
    let (lon, lat) = (value(0)?, value(1)?);
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return Err(Refusal::NotLonLat);
    }
    Ok(Pos { lon, lat, nums })
}

/// JSON to write, or as read from a `coordinates` array.
#[derive(Debug, Clone)]
enum Node {
    /// A number, as its text.
    Num(String),
    Str(String),
    List(Vec<Node>),
    Obj(Vec<(&'static str, Node)>),
    /// Text from the document, written as it is.
    Raw(String),
    /// Note where this value starts in what is written.
    Mark(Box<Node>),
}

/// Nested arrays of numbers, and nothing else.
#[derive(Default)]
struct Numbers {
    stack: Vec<Vec<Node>>,
    root: Option<Node>,
    bad: bool,
}

impl Sink for Numbers {
    fn begin_object(&mut self, _at: usize) {
        self.bad = true;
    }
    fn begin_array(&mut self, _at: usize) {
        self.stack.push(Vec::new());
    }
    fn end_array(&mut self, _span: Range<usize>) {
        let Some(items) = self.stack.pop() else { return };
        match self.stack.last_mut() {
            Some(parent) => parent.push(Node::List(items)),
            None if self.root.is_none() => self.root = Some(Node::List(items)),
            None => self.bad = true,
        }
    }
    fn number(&mut self, raw: &str, _span: Range<usize>) {
        match self.stack.last_mut() {
            Some(parent) => parent.push(Node::Num(raw.to_string())),
            None => self.bad = true,
        }
    }
    fn string(&mut self, _raw: &str, _span: Range<usize>) {
        self.bad = true;
    }
    fn literal(&mut self, _lit: Lit, _span: Range<usize>) {
        self.bad = true;
    }
}

fn parse_numbers(text: &str) -> Option<Node> {
    let mut sink = Numbers::default();
    let opts = Options { comments: true, trailing_commas: true, multiple_roots: false };
    let errors = json::parse(text, opts, &mut sink);
    if !errors.is_empty() || sink.bad {
        return None;
    }
    sink.root
}

/// How the JSON around an edit is laid out, for what is written into it to
/// match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Style {
    /// A member or an element to a line, indented by this much a level;
    /// `None` writes all on one line.
    pub indent: Option<String>,
    /// Every number on a line of its own as well, as `JSON.stringify` lays
    /// it out, rather than a position to a line.
    pub numbers_apart: bool,
    /// A space after every comma and colon.
    pub spaced: bool,
    /// A space inside every bracket and brace: `[ 1, 2 ]`.
    pub padded: bool,
    pub eol: &'static str,
}

impl Style {
    /// The layout a new file gets.
    pub fn pretty() -> Style {
        Style {
            indent: Some("  ".into()),
            numbers_apart: false,
            spaced: true,
            padded: false,
            eol: "\n",
        }
    }

    /// The layout of `sample`.
    pub fn detect(sample: &str) -> Style {
        let eol = if sample.contains("\r\n") { "\r\n" } else { "\n" };
        let bytes = sample.as_bytes();
        let (mut spaced, mut tight, mut padded, mut snug) = (0, 0, 0, 0);
        for (i, &b) in bytes.iter().enumerate() {
            let next = bytes.get(i + 1).copied();
            match b {
                b',' | b':' => match next {
                    Some(b' ') => spaced += 1,
                    Some(b'\n' | b'\r') | None => {}
                    Some(_) => tight += 1,
                },
                b'[' | b'{' => match next {
                    Some(b' ')
                        if !matches!(
                            bytes.get(i + 2),
                            Some(b' ' | b'\n' | b'\r' | b']' | b'}')
                        ) =>
                    {
                        padded += 1
                    }
                    Some(b' ' | b'\n' | b'\r' | b']' | b'}') | None => {}
                    Some(_) => snug += 1,
                },
                _ => {}
            }
        }
        if !sample.contains('\n') {
            return Style {
                indent: None,
                numbers_apart: false,
                spaced: spaced >= tight,
                padded: padded > snug,
                eol,
            };
        }
        let mut tabs = false;
        let mut unit = 0usize;
        let mut numbers_apart = false;
        for line in sample.lines().skip(1) {
            let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
            tabs |= line.starts_with('\t');
            if lead > 0 && !tabs {
                unit = gcd(unit, lead);
            }
            let bare = line.trim().trim_end_matches(',');
            numbers_apart |= !bare.is_empty() && json::number(bare).is_some();
        }
        let indent = if tabs {
            "\t".to_string()
        } else {
            " ".repeat(if (1..=8).contains(&unit) { unit } else { 2 })
        };
        Style {
            indent: Some(indent),
            numbers_apart,
            spaced: spaced >= tight,
            padded: padded > snug,
            eol,
        }
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// What [`write`] made: the text, and where each marked value starts in it.
struct Written {
    text: String,
    marks: Vec<usize>,
}

/// `node` as text laid out in `style`, for a place whose line is indented by
/// `base`. The first line is not indented: it follows whatever is before it.
fn write(node: &Node, style: &Style, base: &str) -> Written {
    let mut w = Writer { out: String::new(), style, base, marks: Vec::new() };
    w.node(node, 0);
    Written { text: w.out, marks: w.marks }
}

struct Writer<'a> {
    out: String,
    style: &'a Style,
    base: &'a str,
    marks: Vec<usize>,
}

impl Writer<'_> {
    fn indent(&mut self, depth: usize) {
        self.out.push_str(self.base);
        if let Some(unit) = &self.style.indent {
            for _ in 0..depth {
                self.out.push_str(unit);
            }
        }
    }

    fn node(&mut self, node: &Node, depth: usize) {
        match node {
            Node::Num(n) => self.out.push_str(n),
            Node::Str(s) => self.out.push_str(&quote(s)),
            Node::Raw(text) => {
                // Its lines after the first move in as far as it moved down.
                for (i, line) in text.split('\n').enumerate() {
                    if i > 0 {
                        self.out.push('\n');
                        if !line.trim().is_empty()
                            && let Some(unit) = &self.style.indent
                        {
                            (0..depth).for_each(|_| self.out.push_str(unit));
                        }
                    }
                    self.out.push_str(line);
                }
            }
            Node::Mark(inner) => {
                self.marks.push(self.out.len());
                self.node(inner, depth);
            }
            Node::List(items) => {
                let inline = self.style.indent.is_none()
                    || (!self.style.numbers_apart
                        && items.iter().all(|i| matches!(i, Node::Num(_))));
                self.container(['[', ']'], items.len(), depth, inline, |w, i, d| {
                    w.node(&items[i], d)
                });
            }
            Node::Obj(members) => {
                let inline = self.style.indent.is_none();
                let colon = if self.style.spaced || !inline { ": " } else { ":" };
                self.container(['{', '}'], members.len(), depth, inline, |w, i, d| {
                    w.out.push_str(&quote(members[i].0));
                    w.out.push_str(colon);
                    w.node(&members[i].1, d);
                });
            }
        }
    }

    fn container(
        &mut self,
        [open, close]: [char; 2],
        len: usize,
        depth: usize,
        inline: bool,
        mut each: impl FnMut(&mut Self, usize, usize),
    ) {
        self.out.push(open);
        if len == 0 {
            self.out.push(close);
            return;
        }
        if inline {
            let pad = if self.style.padded { " " } else { "" };
            self.out.push_str(pad);
            for i in 0..len {
                if i > 0 {
                    self.out.push_str(if self.style.spaced { ", " } else { "," });
                }
                each(self, i, depth);
            }
            self.out.push_str(pad);
        } else {
            let eol = self.style.eol;
            self.out.push_str(eol);
            for i in 0..len {
                self.indent(depth + 1);
                each(self, i, depth + 1);
                if i + 1 < len {
                    self.out.push(',');
                }
                self.out.push_str(eol);
            }
            self.indent(depth);
        }
        self.out.push(close);
    }
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Up to [`SAMPLE`] bytes of `rope` from `range`, cut where a character starts.
fn sample(rope: &Rope, range: Range<usize>) -> String {
    let floor = |b: usize| {
        let b = b.min(rope.len_bytes());
        rope.char_to_byte(rope.byte_to_char(b))
    };
    let end = floor(range.end.min(range.start.saturating_add(SAMPLE)));
    let start = floor(range.start).min(end);
    rope.byte_slice(start..end).to_string()
}

/// The spaces and tabs the line holding byte `at` starts with.
fn indent_at(rope: &Rope, at: usize) -> String {
    let line = rope.byte_to_line(at.min(rope.len_bytes()));
    rope.line(line).chars().take_while(|c| matches!(c, ' ' | '\t')).collect()
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// The first byte at or after `at` that is not white space.
fn skip_space(rope: &Rope, mut at: usize) -> usize {
    while at < rope.len_bytes() && is_space(rope.byte(at)) {
        at += 1;
    }
    at
}

/// The byte after the last one before `at` that is not white space.
fn skip_space_back(rope: &Rope, mut at: usize) -> usize {
    while at > 0 && is_space(rope.byte(at - 1)) {
        at -= 1;
    }
    at
}

/// An edit of the editor's text made on the map, in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextEdit {
    /// Replace `[start, end)` by `text`, as one undo step of its own.
    Replace { start: usize, end: usize, text: String },
    /// Undo the last step, which covers `[start, end)` now and leaves `len`
    /// bytes there.
    Undo { start: usize, end: usize, len: usize },
    /// Make the step undone last again, likewise.
    Redo { start: usize, end: usize, len: usize },
}

/// A replacement of the bytes `[start, end)` of the text by `text`.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub scope: Scope,
}

/// What a [`Change`] does to the GeoJSON, which says how much of it to read
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Something inside feature `.1` of object `.0`, which is read again.
    Feature(usize, usize),
    /// A feature added to the collection `oi` as its `fi`th, its text `len`
    /// bytes at `at`.
    Added { oi: usize, fi: usize, at: usize, len: usize },
    /// That feature taken out again.
    Removed { oi: usize, fi: usize, at: usize, len: usize },
    /// Anything else: the whole document is read again. `select` is where the
    /// feature (or object) to pick afterwards starts.
    Document { select: Option<usize> },
}

/// Make `change` in `rope`, and bring `doc` — read from `rope` before — up to
/// date with it. The answer is the change that undoes it.
pub fn apply(rope: &mut Rope, doc: &mut GeoDoc, change: &Change) -> Change {
    let removed = rope.byte_slice(change.start..change.end).to_string();
    let chars = rope.byte_to_char(change.start)..rope.byte_to_char(change.end);
    rope.remove(chars.clone());
    rope.insert(chars.start, &change.text);
    let (start, end, len) = (change.start, change.end, change.text.len());
    let followed = match change.scope {
        Scope::Feature(oi, fi) => {
            let old = doc.objects.get(oi).and_then(|o| o.features.get(fi)).map(|f| f.skipped);
            old.and_then(|old| {
                doc.shift(start, end, len);
                let span = doc.objects[oi].features[fi].span.clone();
                let f =
                    geojson::feature_at(&rope.byte_slice(span.clone()).to_string(), span.start)?;
                doc.skipped = (doc.skipped + f.skipped).saturating_sub(old);
                let o = &mut doc.objects[oi];
                o.features[fi] = f;
                o.rebound();
                Some(Scope::Feature(oi, fi))
            })
        }
        Scope::Added { oi, fi, at, len: flen } => {
            let fits = doc.objects.get(oi).is_some_and(|o| fi <= o.features.len());
            fits.then(|| {
                doc.shift(start, end, len);
                let f = geojson::feature_at(&rope.byte_slice(at..at + flen).to_string(), at)?;
                doc.skipped += f.skipped;
                let o = &mut doc.objects[oi];
                o.features.insert(fi, f);
                o.rebound();
                Some(Scope::Removed { oi, fi, at, len: flen })
            })
            .flatten()
        }
        Scope::Removed { oi, fi, at, len: flen } => {
            let fits = doc.objects.get(oi).is_some_and(|o| fi < o.features.len());
            fits.then(|| {
                let o = &mut doc.objects[oi];
                let f = o.features.remove(fi);
                o.rebound();
                doc.skipped = doc.skipped.saturating_sub(f.skipped);
                doc.shift(start, end, len);
                Scope::Added { oi, fi, at, len: flen }
            })
        }
        Scope::Document { .. } => None,
    };
    let scope = followed.unwrap_or_else(|| {
        *doc = geojson::extract(&rope.to_string());
        Scope::Document { select: None }
    });
    Change { start, end: start + len, text: removed, scope }
}

/// Where a new feature goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Into the features of this collection.
    Collection(usize),
    /// The text is blank: it becomes a FeatureCollection.
    NewFile,
    /// The document is this one Feature or geometry: it becomes a
    /// FeatureCollection holding it.
    Promote(usize),
    /// A document a feature to a line: onto a new line.
    Lines,
    /// Into a new FeatureCollection at the editor's cursor, over the bytes
    /// `[start, end)`: nothing, or the `null` the cursor is on.
    AtCursor { start: usize, end: usize },
}

/// Whether `path` is a document of its own rather than a part of one.
fn is_root(path: &str) -> bool {
    path.strip_prefix('$').is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Where a new feature drawn on the map goes: into the collection chosen, or
/// the one the picked feature is in, or the document's first; failing that,
/// into a collection made for it.
pub fn target(
    rope: &Rope,
    doc: &GeoDoc,
    object: Option<usize>,
    picked: Option<(usize, usize)>,
    cursor: usize,
) -> Result<Target, Refusal> {
    let collection = |oi: usize| doc.objects.get(oi).is_some_and(|o| o.features_array.is_some());
    if let Some(oi) = object.filter(|&o| collection(o)) {
        return Ok(Target::Collection(oi));
    }
    if let Some((oi, _)) = picked.filter(|&(o, _)| collection(o)) {
        return Ok(Target::Collection(oi));
    }
    if let Some(oi) = (0..doc.objects.len()).find(|&o| collection(o)) {
        return Ok(Target::Collection(oi));
    }
    if rope.chars().all(char::is_whitespace) {
        return Ok(Target::NewFile);
    }
    if !doc.objects.is_empty() && doc.objects.iter().all(|o| is_root(&o.path)) {
        return Ok(if doc.objects.len() == 1 && doc.objects[0].path == "$" {
            Target::Promote(0)
        } else {
            Target::Lines
        });
    }
    at_cursor(rope, doc, cursor)
}

/// The editor's cursor as a place for a new FeatureCollection: where a value
/// goes — after a colon, an opening bracket or a comma, before a comma or a
/// closing bracket — and not inside GeoJSON already there. A `null` there is
/// what the collection replaces.
fn at_cursor(rope: &Rope, doc: &GeoDoc, cursor: usize) -> Result<Target, Refusal> {
    let cursor = cursor.min(rope.len_bytes());
    if doc.objects.iter().any(|o| o.span.start < cursor && cursor < o.span.end) {
        return Err(Refusal::CursorInside);
    }
    let byte = |at: usize| (at < rope.len_bytes()).then(|| rope.byte(at));
    let before = skip_space_back(rope, cursor);
    let after = skip_space(rope, cursor);
    let null = (0..4).all(|i| byte(after + i) == Some(b"null"[i]));
    let end = if null { skip_space(rope, after + 4) } else { after };
    let opens = before > 0 && matches!(byte(before - 1), Some(b':' | b'[' | b','));
    let closes = matches!(byte(end), None | Some(b',' | b']' | b'}'));
    if !(opens && closes) {
        return Err(Refusal::CursorInside);
    }
    Ok(if null {
        Target::AtCursor { start: after, end: after + 4 }
    } else {
        Target::AtCursor { start: cursor, end: cursor }
    })
}

fn feature_node(geom: &Geom) -> Node {
    Node::Obj(vec![
        ("type", Node::Str("Feature".into())),
        ("properties", Node::Obj(Vec::new())),
        (
            "geometry",
            Node::Obj(vec![
                ("type", Node::Str(geom.kind.name().into())),
                ("coordinates", geom.tree()),
            ]),
        ),
    ])
}

fn collection_node(features: Vec<Node>) -> Node {
    Node::Obj(vec![
        ("type", Node::Str("FeatureCollection".into())),
        ("features", Node::List(features)),
    ])
}

/// The edit that adds `geom`, as a new feature with no properties, where
/// `target` says.
pub fn add_feature(rope: &Rope, doc: &GeoDoc, target: Target, geom: &Geom) -> Option<Change> {
    let feature = Node::Mark(Box::new(feature_node(geom)));
    let len = rope.len_bytes();
    let eol = Style::detect(&sample(rope, 0..len)).eol;
    Some(match target {
        Target::Collection(oi) => return add_to_collection(rope, doc, oi, feature),
        Target::NewFile => {
            let w = write(&collection_node(vec![feature]), &Style::pretty(), "");
            let select = w.marks.first().copied();
            Change { start: 0, end: len, text: w.text + "\n", scope: Scope::Document { select } }
        }
        Target::Promote(oi) => {
            let o = doc.objects.get(oi)?;
            let old = rope.byte_slice(o.span.clone()).to_string();
            let old = if o.kind == "Feature" {
                Node::Raw(old)
            } else {
                Node::Obj(vec![
                    ("type", Node::Str("Feature".into())),
                    ("properties", Node::Obj(Vec::new())),
                    ("geometry", Node::Raw(old)),
                ])
            };
            let style = Style::detect(&sample(rope, o.span.clone()));
            let w =
                write(&collection_node(vec![old, feature]), &style, &indent_at(rope, o.span.start));
            let select = w.marks.first().map(|m| o.span.start + m);
            Change {
                start: o.span.start,
                end: o.span.end,
                text: w.text,
                scope: Scope::Document { select },
            }
        }
        Target::Lines => {
            let compact = Style { indent: None, ..Style::detect(&sample(rope, 0..len)) };
            let lead = if rope.bytes_at(len).prev() == Some(b'\n') { "" } else { eol };
            let w = write(&feature, &compact, "");
            let select = Some(len + lead.len());
            Change {
                start: len,
                end: len,
                text: format!("{lead}{}{eol}", w.text),
                scope: Scope::Document { select },
            }
        }
        Target::AtCursor { start, end } => {
            let style = Style::detect(&sample(rope, 0..len));
            let w = write(&collection_node(vec![feature]), &style, &indent_at(rope, start));
            let select = w.marks.first().map(|m| start + m);
            Change { start, end, text: w.text, scope: Scope::Document { select } }
        }
    })
}

/// A feature appended to the `features` of collection `oi`, laid out like the
/// features before it.
fn add_to_collection(rope: &Rope, doc: &GeoDoc, oi: usize, feature: Node) -> Option<Change> {
    let o = doc.objects.get(oi)?;
    let array = o.features_array.clone()?;
    let fi = o.features.len();
    match o.features.last() {
        Some(last) => {
            let style = Style::detect(&sample(rope, last.span.clone()));
            // The same white space in front of it as in front of the last.
            let before = skip_space_back(rope, last.span.start);
            let mut gap = rope.byte_slice(before..last.span.start).to_string();
            if gap.is_empty() && style.spaced {
                gap = " ".into();
            }
            let base = match gap.rfind('\n') {
                Some(i) => gap[i + 1..].to_string(),
                None => indent_at(rope, last.span.start),
            };
            let w = write(&feature, &style, &base);
            let at = last.span.end;
            let scope = Scope::Added { oi, fi, at: at + 1 + gap.len(), len: w.text.len() };
            Some(Change { start: at, end: at, text: format!(",{gap}{}", w.text), scope })
        }
        None => {
            let inner = array.start + 1..array.end.saturating_sub(1).max(array.start + 1);
            let style = Style::detect(&sample(rope, o.span.clone()));
            let base = indent_at(rope, array.start);
            let (text, offset, len) = match &style.indent {
                Some(unit) => {
                    let inside = format!("{base}{unit}");
                    let w = write(&feature, &style, &inside);
                    let lead = format!("{}{inside}", style.eol);
                    let text = format!("{lead}{}{}{base}", w.text, style.eol);
                    (text, lead.len(), w.text.len())
                }
                None => {
                    let text = write(&feature, &style, "").text;
                    let len = text.len();
                    (text, 0, len)
                }
            };
            let scope = Scope::Added { oi, fi, at: inner.start + offset, len };
            Some(Change { start: inner.start, end: inner.end, text, scope })
        }
    }
}

/// The edit that makes an empty FeatureCollection: the whole of a blank text,
/// or at the editor's cursor.
pub fn new_collection(rope: &Rope, doc: &GeoDoc, cursor: usize) -> Result<Change, Refusal> {
    let len = rope.len_bytes();
    let empty = collection_node(Vec::new());
    if rope.chars().all(char::is_whitespace) {
        let text = write(&empty, &Style::pretty(), "").text + "\n";
        return Ok(Change { start: 0, end: len, text, scope: Scope::Document { select: Some(0) } });
    }
    let Target::AtCursor { start, end } = at_cursor(rope, doc, cursor)? else { unreachable!() };
    let style = Style::detect(&sample(rope, 0..len));
    let text = write(&empty, &style, &indent_at(rope, start)).text;
    Ok(Change { start, end, text, scope: Scope::Document { select: Some(start) } })
}

/// The edit that takes out feature `fi` of object `oi` — with the comma and
/// space that kept it apart from the next, or the one before — or a whole
/// document of a feature, with its line.
pub fn remove_feature(rope: &Rope, doc: &GeoDoc, oi: usize, fi: usize) -> Result<Change, Refusal> {
    let o = doc.objects.get(oi).ok_or(Refusal::Nested)?;
    let f = o.features.get(fi).ok_or(Refusal::Nested)?;
    let span = f.span.clone();
    if o.features_array.is_some() {
        let after = skip_space(rope, span.end);
        let before = skip_space_back(rope, span.start);
        let byte = |at: usize| (at < rope.len_bytes()).then(|| rope.byte(at));
        let (start, end) = if byte(after) == Some(b',') {
            (span.start, skip_space(rope, after + 1))
        } else if before > 0 && byte(before - 1) == Some(b',') {
            (before - 1, span.end)
        } else if before > 0 && byte(before - 1) == Some(b'[') && byte(after) == Some(b']') {
            (before, after)
        } else {
            (span.start, span.end)
        };
        let scope = Scope::Removed { oi, fi, at: span.start, len: span.len() };
        return Ok(Change { start, end, text: String::new(), scope });
    }
    if !is_root(&o.path) {
        return Err(Refusal::Nested);
    }
    let mut end = span.end;
    while end < rope.len_bytes() && matches!(rope.byte(end), b' ' | b'\t') {
        end += 1;
    }
    if end < rope.len_bytes() && rope.byte(end) == b'\r' {
        end += 1;
    }
    if end < rope.len_bytes() && rope.byte(end) == b'\n' {
        end += 1;
    }
    Ok(Change {
        start: span.start,
        end,
        text: String::new(),
        scope: Scope::Document { select: None },
    })
}

/// Where a feature's name is, or could go.
#[derive(Default)]
struct Naming {
    depth: usize,
    key: Option<String>,
    inner_key: Option<String>,
    feature: bool,
    /// The `properties` value: its bytes, and whether it is an object.
    props: Option<(Range<usize>, bool)>,
    in_props: bool,
    /// The value of the naming property, and its rank among [`NAME_KEYS`].
    name: Option<(usize, Range<usize>)>,
}

impl Naming {
    fn scalar(&mut self, raw: &str, span: Range<usize>, null: bool) {
        match self.depth {
            1 => match self.key.take().as_deref() {
                Some("type") => self.feature = json::unescape(raw) == "Feature",
                Some("properties") if null => self.props = Some((span, false)),
                _ => {}
            },
            2 if self.in_props => self.value(span),
            _ => {}
        }
    }

    fn value(&mut self, span: Range<usize>) {
        let key = self.inner_key.take();
        if let Some(rank) = key.and_then(|k| NAME_KEYS.iter().position(|n| *n == k))
            && self.name.as_ref().is_none_or(|(r, _)| rank < *r)
        {
            self.name = Some((rank, span));
        }
    }
}

impl Sink for Naming {
    fn begin_object(&mut self, at: usize) {
        if self.depth == 1 && self.key.as_deref() == Some("properties") {
            self.in_props = true;
            self.props = Some((at..at, true));
        }
        self.depth += 1;
    }
    fn end_object(&mut self, span: Range<usize>) {
        self.depth -= 1;
        if self.depth == 1 && self.in_props {
            self.in_props = false;
            self.props = Some((span, true));
        }
        self.after_container();
    }
    fn begin_array(&mut self, _at: usize) {
        self.depth += 1;
    }
    fn end_array(&mut self, _span: Range<usize>) {
        self.depth -= 1;
        self.after_container();
    }
    fn key(&mut self, raw: &str, _span: Range<usize>) {
        let key = json::unescape(raw).into_owned();
        match self.depth {
            1 => self.key = Some(key),
            2 if self.in_props => self.inner_key = Some(key),
            _ => {}
        }
    }
    fn string(&mut self, raw: &str, span: Range<usize>) {
        self.scalar(raw, span, false);
    }
    fn number(&mut self, raw: &str, span: Range<usize>) {
        self.scalar(raw, span, false);
    }
    fn literal(&mut self, lit: Lit, span: Range<usize>) {
        self.scalar("", span, lit == Lit::Null);
    }
}

impl Naming {
    fn after_container(&mut self) {
        match self.depth {
            1 => self.key = None,
            2 if self.in_props => self.inner_key = None,
            _ => {}
        }
    }
}

/// The edit that names feature `fi` of object `oi` `name`: its naming
/// property set, or a `name` added to its properties.
pub fn set_name(
    rope: &Rope,
    doc: &GeoDoc,
    (oi, fi): (usize, usize),
    name: &str,
) -> Result<Change, Refusal> {
    let f = doc.objects.get(oi).and_then(|o| o.features.get(fi)).ok_or(Refusal::NotAFeature)?;
    let base = f.span.start;
    let text = rope.byte_slice(f.span.clone()).to_string();
    let mut n = Naming::default();
    let opts = Options { comments: true, trailing_commas: true, multiple_roots: false };
    json::parse(&text, opts, &mut n);
    if !n.feature {
        return Err(Refusal::NotAFeature);
    }
    let mut cut = text.len().min(SAMPLE);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let style = Style::detect(&text[..cut]);
    let colon = if style.spaced || style.indent.is_some() { ": " } else { ":" };
    let member = format!("\"name\"{colon}{}", quote(name));
    let scope = Scope::Feature(oi, fi);
    let change = |start: usize, end: usize, text: String| Change {
        start: base + start,
        end: base + end,
        text,
        scope,
    };
    Ok(match n.props {
        _ if n.name.is_some() => {
            let (_, span) = n.name.expect("just checked");
            change(span.start, span.end, quote(name))
        }
        Some((span, true)) => {
            let inner = &text[span.start + 1..span.end - 1];
            if inner.trim().is_empty() {
                change(span.start + 1, span.end - 1, member)
            } else {
                // In front of the first member, with the same space before it.
                let gap = &inner[..inner.len() - inner.trim_start().len()];
                let after = if gap.is_empty() && style.spaced { " " } else { "" };
                let text = format!("{gap}{member},{after}");
                change(span.start + 1, span.start + 1, text)
            }
        }
        Some((span, false)) => change(span.start, span.end, format!("{{{member}}}")),
        None => {
            let at = skip_space_back(rope, f.span.end - 1) - base;
            let lead = match &style.indent {
                Some(_) => format!("{}{}", style.eol, indent_at(rope, base + at - 1)),
                None if style.spaced => " ".into(),
                None => String::new(),
            };
            change(at, at, format!(",{lead}\"properties\"{colon}{{{member}}}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::geojson::extract;

    /// The text, strictly checked, and what reading it afresh finds — which
    /// the document followed through the edits must equal.
    fn check(rope: &Rope, doc: &GeoDoc) {
        let text = rope.to_string();
        let errors = json::parse(&text, Options::default(), &mut json::NullSink);
        assert!(errors.is_empty(), "{errors:?} in\n{text}");
        assert_eq!(format!("{doc:?}"), format!("{:?}", extract(&text)), "in\n{text}");
    }

    fn edit(text: &str, change: impl FnOnce(&Rope, &GeoDoc) -> Change) -> (String, Rope, GeoDoc) {
        let mut rope = Rope::from_str(text);
        let mut doc = extract(text);
        let c = change(&rope, &doc);
        let undo = apply(&mut rope, &mut doc, &c);
        check(&rope, &doc);
        let after = rope.to_string();
        // And back again.
        let (mut back, mut back_doc) = (rope.clone(), doc.clone());
        apply(&mut back, &mut back_doc, &undo);
        assert_eq!(back.to_string(), text);
        assert_eq!(format!("{back_doc:?}"), format!("{:?}", extract(text)));
        (after, rope, doc)
    }

    #[test]
    fn a_geometry_is_written_back_as_it_was_laid_out() {
        for (coords, base) in [
            ("[[[16.39,48.2],[16.45,48.2],[16.45,48.22],[16.39,48.2]]]", ""),
            ("[[[16.39, 48.2], [16.45, 48.2], [16.45, 48.22], [16.39, 48.2]]]", ""),
            ("[ [ [ 16.39, 48.2 ], [ 16.45, 48.2 ], [ 16.45, 48.22 ], [ 16.39, 48.2 ] ] ]", ""),
            (
                "[\n      [\n        [16.39, 48.2],\n        [16.45, 48.2],\n        [16.39, 48.2]\n      ]\n    ]",
                "    ",
            ),
            (
                "[\n\t\t[\n\t\t\t[\n\t\t\t\t16.390000,\n\t\t\t\t48.2\n\t\t\t],\n\t\t\t[\n\t\t\t\t16.45,\n\t\t\t\t48.2,\n\t\t\t\t120\n\t\t\t],\n\t\t\t[\n\t\t\t\t16.4,\n\t\t\t\t48.3\n\t\t\t],\n\t\t\t[\n\t\t\t\t16.390000,\n\t\t\t\t48.2\n\t\t\t]\n\t\t]\n\t]",
                "\t",
            ),
        ] {
            let geom = Geom::parse(Kind::Polygon, coords).unwrap();
            assert!(geom.parts[0][0].len() >= 2, "the closing position is not kept");
            let style = Style::detect(coords);
            assert_eq!(write(&geom.tree(), &style, base).text, coords, "{style:?}");
        }
        assert_eq!(Geom::parse(Kind::Point, "[500000, 4000000]"), Err(Refusal::NotLonLat));
        assert_eq!(Geom::parse(Kind::LineString, "[[1, 2], [\"x\", 3]]"), Err(Refusal::NotLonLat));
        assert_eq!(Geom::parse(Kind::Polygon, "[[1, 2], [3, 4]]"), Err(Refusal::NotLonLat));
    }

    #[test]
    fn layouts_are_told_apart() {
        let s = Style::detect(r#"{"type":"Feature","geometry":{"coordinates":[1,2]}}"#);
        assert!(s.indent.is_none() && !s.spaced && !s.padded);
        let s = Style::detect(r#"{ "type": "Feature", "coordinates": [ 1, 2 ] }"#);
        assert!(s.indent.is_none() && s.spaced && s.padded);
        let s = Style::detect("{\r\n    \"a\": [\r\n        1,\r\n        2\r\n    ]\r\n}");
        assert_eq!((s.indent.as_deref(), s.numbers_apart, s.eol), (Some("    "), true, "\r\n"));
        let s = Style::detect("[\n  [1.5, 2],\n  [3, 4]\n]");
        assert_eq!((s.indent.as_deref(), s.numbers_apart, s.spaced), (Some("  "), false, true));
    }

    #[test]
    fn positions_move_come_and_go_within_what_each_type_allows() {
        let mut m = Model {
            geoms: vec![
                Geom::parse(Kind::LineString, "[[0, 0], [1.000, 1], [2, 0]]").unwrap(),
                Geom::parse(
                    Kind::Polygon,
                    "[[[0,0],[10,0],[10,10],[0,0]],[[2,2],[3,2],[3,3],[2,2]]]",
                )
                .unwrap(),
                Geom::parse(Kind::Point, "[5, 5, 300]").unwrap(),
            ],
        };
        let v = |geom, path, index| Vertex { geom, part: 0, path, index };
        m.set(v(0, 0, 2), 2.123456789, -0.00000001, 4);
        assert_eq!(
            write(&m.geoms[0].tree(), &Style::detect("[1, 2]"), "").text,
            "[[0, 0], [1.000, 1], [2.1235, 0]]",
            "the numbers not moved are kept as written; a negative zero is a zero"
        );
        m.set(v(2, 0, 0), 190.0, 95.0, 2);
        assert_eq!(write(&m.geoms[2].tree(), &Style::detect("[1, 2]"), "").text, "[-170, 90, 300]");
        assert_eq!(m.insert(v(0, 0, 1), Pos::new(0.5, 0.5, 2)), Some(v(0, 0, 1)));
        assert_eq!(m.geoms[0].parts[0][0].len(), 4);
        assert_eq!(m.insert(v(2, 0, 0), Pos::new(0.5, 0.5, 2)), None, "a point takes no more");
        // A line keeps two positions.
        assert_eq!(m.remove(v(0, 0, 0)), Ok(Removal::Position(v(0, 0, 0))));
        assert_eq!(m.remove(v(0, 0, 0)), Ok(Removal::Position(v(0, 0, 0))));
        assert_eq!(m.remove(v(0, 0, 0)), Err(Refusal::LineTooShort));
        // A ring keeps three; a hole too small goes; the outer ring cannot.
        assert_eq!(m.remove(v(1, 1, 0)), Ok(Removal::Path));
        assert_eq!(m.geoms[1].parts[0].len(), 1, "the hole is gone");
        assert_eq!(m.remove(v(1, 0, 1)), Err(Refusal::PolygonTooSmall));
        assert_eq!(m.remove(v(2, 0, 0)), Ok(Removal::Feature), "a point's goes with the feature");
        assert_eq!(m.geoms[2].parts.len(), 1);
        // A part of several goes when it is too short.
        let mut multi = Model {
            geoms: vec![
                Geom::parse(Kind::MultiLineString, "[[[0,0],[1,1]],[[2,2],[3,3]]]").unwrap(),
            ],
        };
        assert_eq!(multi.remove(v(0, 0, 0)), Ok(Removal::Path));
        assert_eq!(multi.geoms[0].parts.len(), 1);
        assert_eq!(multi.step(None, true), Some(v(0, 0, 0)));
        assert_eq!(multi.step(Some(v(0, 0, 1)), true), Some(v(0, 0, 0)), "round the end");
        assert_eq!(multi.step(None, false), Some(v(0, 0, 1)));
    }

    const STRINGIFIED: &str = r#"{
  "type": "FeatureCollection",
  "features": [
    {
      "type": "Feature",
      "properties": {
        "name": "Prater"
      },
      "geometry": {
        "type": "Polygon",
        "coordinates": [
          [
            [
              16.39,
              48.2
            ],
            [
              16.45,
              48.2
            ],
            [
              16.45,
              48.22
            ],
            [
              16.39,
              48.2
            ]
          ]
        ]
      }
    },
    {
      "type": "Feature",
      "properties": {},
      "geometry": {
        "type": "Point",
        "coordinates": [
          16.37,
          48.21
        ]
      }
    }
  ]
}"#;

    const OGR: &str = "{\n\"type\": \"FeatureCollection\",\n\"name\": \"places\",\n\"features\": [\n{ \"type\": \"Feature\", \"properties\": { \"name\": \"Gate\" }, \"geometry\": { \"type\": \"Point\", \"coordinates\": [ 16.37, 48.21 ] } },\n{ \"type\": \"Feature\", \"properties\": { \"name\": \"Road\" }, \"geometry\": { \"type\": \"LineString\", \"coordinates\": [ [ 16.3, 48.1 ], [ 16.4, 48.2 ] ] } }\n]\n}\n";

    const MINIFIED: &str = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"name":"Gate"},"geometry":{"type":"Point","coordinates":[16.37,48.21]}}]}"#;

    const DUMPED: &str = r#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"name": "Gate"}, "geometry": {"type": "Point", "coordinates": [16.37, 48.21]}}]}"#;

    #[test]
    fn a_moved_position_rewrites_only_its_geometry() {
        let (text, ..) = edit(STRINGIFIED, |rope, doc| {
            let mut m = Model::read(rope, &doc.objects[0].features[0]).unwrap();
            m.set(Vertex { geom: 0, part: 0, path: 0, index: 1 }, 16.5, 48.25, 3);
            m.write(rope, doc, (0, 0), 0).unwrap()
        });
        let moved =
            STRINGIFIED.replacen("16.45,\n              48.2\n", "16.5,\n              48.25\n", 1);
        assert_eq!(text, moved, "nothing else changes");
        let (text, ..) = edit(OGR, |rope, doc| {
            let mut m = Model::read(rope, &doc.objects[0].features[1]).unwrap();
            m.insert(Vertex { geom: 0, part: 0, path: 0, index: 1 }, Pos::new(16.35, 48.15, 2));
            m.write(rope, doc, (0, 1), 0).unwrap()
        });
        assert!(text.contains(
            "\"coordinates\": [ [ 16.3, 48.1 ], [ 16.35, 48.15 ], [ 16.4, 48.2 ] ] } }\n]"
        ));
    }

    #[test]
    fn a_feature_added_to_a_collection_is_laid_out_like_its_neighbours() {
        let line = Geom::from_path(Kind::LineString, &[[1.0, 2.0], [3.0, 4.5]], 3);
        let add = |text: &str| {
            edit(text, |rope, doc| {
                let target = target(rope, doc, None, None, 0).unwrap();
                assert_eq!(target, Target::Collection(0));
                add_feature(rope, doc, target, &line).unwrap()
            })
            .0
        };
        let text = add(STRINGIFIED);
        assert!(
            text.ends_with(
                "    },\n    {\n      \"type\": \"Feature\",\n      \"properties\": {},\n      \"geometry\": {\n        \"type\": \"LineString\",\n        \"coordinates\": [\n          [\n            1,\n            2\n          ],\n          [\n            3,\n            4.5\n          ]\n        ]\n      }\n    }\n  ]\n}"
            ),
            "{text}"
        );
        let text = add(OGR);
        assert!(text.ends_with("] ] } },\n{ \"type\": \"Feature\", \"properties\": {}, \"geometry\": { \"type\": \"LineString\", \"coordinates\": [ [ 1, 2 ], [ 3, 4.5 ] ] } }\n]\n}\n"), "{text}");
        let text = add(MINIFIED);
        assert!(text.ends_with(r#"}},{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[1,2],[3,4.5]]}}]}"#), "{text}");
        let text = add(DUMPED);
        assert!(text.ends_with(r#"]}}, {"type": "Feature", "properties": {}, "geometry": {"type": "LineString", "coordinates": [[1, 2], [3, 4.5]]}}]}"#), "{text}");
        // Into an empty collection.
        let text = add("{\n  \"type\": \"FeatureCollection\",\n  \"features\": []\n}");
        assert!(
            text.contains("  \"features\": [\n    {\n      \"type\": \"Feature\",\n"),
            "{text}"
        );
        assert!(text.ends_with("      }\n    }\n  ]\n}"), "{text}");
        let text = add(r#"{"type":"FeatureCollection","features":[ ]}"#);
        assert!(text.ends_with(r#""features":[{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[1,2],[3,4.5]]}}]}"#), "{text}");
    }

    #[test]
    fn a_feature_taken_out_takes_its_separator_with_it() {
        let remove = |text: &str, fi: usize| {
            edit(text, |rope, doc| remove_feature(rope, doc, 0, fi).unwrap()).0
        };
        let text = remove(OGR, 0);
        assert!(
            text.contains(
                "\"features\": [\n{ \"type\": \"Feature\", \"properties\": { \"name\": \"Road\" }"
            ),
            "{text}"
        );
        let text = remove(OGR, 1);
        assert!(text.contains("[ 16.37, 48.21 ] } }\n]"), "{text}");
        let text = remove(MINIFIED, 0);
        assert!(text.ends_with(r#""features":[]}"#), "{text}");
        let text = remove(STRINGIFIED, 0);
        assert!(text.starts_with("{\n  \"type\": \"FeatureCollection\",\n  \"features\": [\n    {\n      \"type\": \"Feature\",\n      \"properties\": {},"), "{text}");
        // A document that is one feature goes whole; one nested in other JSON
        // stays.
        let (text, _, doc) = edit(
            "{\"type\":\"Feature\",\"properties\":{},\"geometry\":{\"type\":\"Point\",\"coordinates\":[1,2]}}\n",
            |rope, doc| remove_feature(rope, doc, 0, 0).unwrap(),
        );
        assert_eq!((text.as_str(), doc.objects.len()), ("", 0));
        let nested = r#"{"where": {"type": "Point", "coordinates": [1, 2]}}"#;
        assert_eq!(
            remove_feature(&Rope::from_str(nested), &extract(nested), 0, 0),
            Err(Refusal::Nested)
        );
    }

    #[test]
    fn a_new_feature_finds_somewhere_to_go() {
        let point = Geom::from_path(Kind::Point, &[[16.37, 48.21]], 2);
        let add = |text: &str, cursor: usize| {
            let rope = Rope::from_str(text);
            let doc = extract(text);
            let target = target(&rope, &doc, None, None, cursor)?;
            let mut rope = rope;
            let mut doc = doc;
            let c = add_feature(&rope, &doc, target, &point).unwrap();
            let Scope::Document { select: Some(at) } = c.scope else { panic!("{c:?}") };
            apply(&mut rope, &mut doc, &c);
            let picked = doc.objects.iter().flat_map(|o| &o.features).find(|f| f.span.start == at);
            assert!(picked.is_some_and(|f| f.shapes == [Shape::Point([16.37, 48.21])]), "{rope}");
            Ok::<_, Refusal>((target, rope.to_string(), doc))
        };
        let (to, text, doc) = add(" \n", 0).unwrap();
        assert_eq!(to, Target::NewFile);
        assert_eq!(doc.objects[0].kind, "FeatureCollection");
        assert!(
            text.starts_with("{\n  \"type\": \"FeatureCollection\",\n  \"features\": [\n    {\n"),
            "{text}"
        );
        assert!(text.contains("\"coordinates\": [16.37, 48.21]\n"), "{text}");
        let one = "{\n  \"type\": \"Polygon\",\n  \"coordinates\": [\n    [[0, 0], [1, 0], [1, 1], [0, 0]]\n  ]\n}\n";
        let (to, text, doc) = add(one, 0).unwrap();
        assert_eq!(to, Target::Promote(0));
        assert_eq!(doc.objects[0].features.len(), 2);
        assert!(text.contains("      \"geometry\": {\n        \"type\": \"Polygon\",\n        \"coordinates\": [\n          [[0, 0], [1, 0], [1, 1], [0, 0]]\n        ]\n      }\n"), "{text}");
        let lines = "{\"type\":\"Feature\",\"properties\":{},\"geometry\":null}\n{\"type\":\"Point\",\"coordinates\":[1,2]}";
        let (to, text, doc) = add(lines, 0).unwrap();
        assert_eq!(to, Target::Lines);
        assert_eq!(doc.objects.len(), 3);
        assert!(text.ends_with("[1,2]}\n{\"type\":\"Feature\",\"properties\":{},\"geometry\":{\"type\":\"Point\",\"coordinates\":[16.37,48.21]}}\n"));
        // Into other JSON only where the cursor is, and not inside GeoJSON.
        let other = r#"{"a": {"type": "Point", "coordinates": [1, 2]}, "b": null}"#;
        assert_eq!(add(other, 10).err(), Some(Refusal::CursorInside));
        let at = other.find("null").unwrap();
        let rope = Rope::from_str(other);
        let doc = extract(other);
        let null = Target::AtCursor { start: at, end: at + 4 };
        assert_eq!(target(&rope, &doc, None, None, at), Ok(null), "in place of the null");
        let c = new_collection(&rope, &doc, at - 1).unwrap();
        assert_eq!((c.start, c.end), (at, at + 4));
        assert_eq!(c.text, r#"{"type": "FeatureCollection", "features": []}"#);
        assert_eq!(new_collection(&rope, &doc, 10), Err(Refusal::CursorInside));
        // Nor where no value can go: before the document, or between members.
        assert_eq!(new_collection(&rope, &doc, 0), Err(Refusal::CursorInside));
        let between = other.find(" \"b\"").unwrap();
        assert_eq!(new_collection(&rope, &doc, between), Err(Refusal::CursorInside));
        let slots = Rope::from_str(r#"{"a": [1, ], "b": }"#);
        let slot_doc = extract(&slots.to_string());
        for at in [r#"{"a": [1, "#.len(), r#"{"a": [1, ], "b": "#.len()] {
            assert!(new_collection(&slots, &slot_doc, at).is_ok(), "{at}");
        }
    }

    #[test]
    fn a_feature_is_named_in_its_properties() {
        let name = |text: &str, fi: usize, to: &str| {
            edit(text, |rope, doc| set_name(rope, doc, (0, fi), to).unwrap()).0
        };
        let text = name(OGR, 0, "Tor \"1\"");
        assert!(text.contains(r#"{ "name": "Tor \"1\"" }"#), "{text}");
        let text = name(STRINGIFIED, 1, "Gate");
        assert!(text.contains("      \"properties\": {\"name\": \"Gate\"},"), "{text}");
        let text = name(DUMPED, 0, "x");
        assert!(text.contains(r#""properties": {"name": "x"}"#), "{text}");
        let pretty = "{\n  \"type\": \"Feature\",\n  \"properties\": {\n    \"pop\": 3\n  },\n  \"geometry\": null\n}";
        let text = name(pretty, 0, "Home");
        assert!(
            text.contains("  \"properties\": {\n    \"name\": \"Home\",\n    \"pop\": 3\n  },"),
            "{text}"
        );
        let text = name(
            r#"{"type":"Feature","properties":{"title":"a","pop":1},"geometry":null}"#,
            0,
            "b",
        );
        assert!(text.contains(r#"{"title":"b","pop":1}"#), "the naming property there is: {text}");
        let text = name(r#"{"type":"Feature","properties":null,"geometry":null}"#, 0, "b");
        assert!(text.contains(r#""properties":{"name":"b"}"#), "{text}");
        let text = name("{\n  \"type\": \"Feature\",\n  \"geometry\": null\n}", 0, "b");
        assert!(
            text.ends_with("  \"geometry\": null,\n  \"properties\": {\"name\": \"b\"}\n}"),
            "{text}"
        );
        let bare = r#"{"type":"Point","coordinates":[1,2]}"#;
        assert_eq!(
            set_name(&Rope::from_str(bare), &extract(bare), (0, 0), "x"),
            Err(Refusal::NotAFeature)
        );
    }

    #[test]
    fn decimals_follow_the_zoom() {
        assert_eq!(decimals(0.225), 2);
        assert_eq!(decimals(1e-4), 5);
        assert_eq!(decimals(1e-12), 7);
        assert_eq!(decimals(50.0), 1);
        assert_eq!(number(16.370000001, 5), "16.37");
        assert_eq!(number(-0.0001, 2), "0");
        assert_eq!(number(12.0, 3), "12");
    }
}
