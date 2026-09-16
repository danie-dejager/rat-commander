//! Editing the GeoJSON on the map.
//!
//! With editing on, the picked feature shows a square handle on each of its
//! positions and a dot in the middle of each segment: a handle dragged moves
//! its position, a dot dragged (or clicked) adds one there, and Del takes out
//! the selected position — or the feature, with none selected. The tools draw
//! new features: a point with one click, a line or a polygon a click per
//! position, finished with Enter or a click back on the last position (or the
//! first, for a polygon). Every tool works from the keyboard too: the arrow
//! keys pan, Space places a position at the crosshair, Shift and an arrow
//! nudge the selected position a cell.
//!
//! Each change is made to the text straight away, through [`geo::apply`], and
//! handed to the app as a [`TextEdit`] for the editor to make as one undo step.
//! Ctrl-Z and Ctrl-Y here step back and forth through those, and only those.

use super::*;
use crate::geo::draw::Overlay;
use crate::geo::edit::{
    self as geo, Change, Geom, Kind, Model, PathKind, Pos, Refusal, Removal, Scope, Target, Vertex,
};
use crate::geo::geojson::{NAME_KEYS, shapes_bounds, unwrap_dateline};

/// A handle is shown for about one position in this many cells of the map;
/// past that the feature is too fine to edit at this zoom.
const CELLS_PER_HANDLE: u32 = 6;

/// What clicks on the map do while editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Tool {
    /// Pick features, and move, add and remove their positions.
    #[default]
    Select,
    /// Draw a new feature of this type.
    Draw(Kind),
}

/// A position being dragged: where the drag started, where the position was,
/// whether it has moved, and whether the drag added it.
pub(super) struct Grab {
    vertex: Vertex,
    col: u16,
    row: u16,
    from: [f64; 2],
    moved: bool,
    added: bool,
}

#[derive(Default)]
pub(super) struct Editing {
    tool: Tool,
    /// The positions placed for the feature being drawn.
    sketch: Vec<[f64; 2]>,
    /// The picked feature's positions, read when first needed.
    model: Option<((usize, usize), Result<Model, Refusal>)>,
    /// The selected position, and the feature it is on.
    vertex: Option<((usize, usize), Vertex)>,
    pub(super) grab: Option<Grab>,
    /// A name being typed, and the caret in it.
    naming: Option<(String, usize)>,
    /// What the last action could not do.
    note: Option<&'static str>,
    /// The keyboard is drawing, at the crosshair, rather than the mouse.
    by_key: bool,
}

/// One edit made, and the edit that takes it back.
struct Step {
    redo: Change,
    undo: Change,
}

#[derive(Default)]
pub(super) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
}

/// A handle on the map: a position, or the middle of a segment — whose
/// `vertex` is then where a position added there goes.
#[derive(Debug, Clone, Copy)]
struct Handle {
    vertex: Vertex,
    at: [f64; 2],
    midpoint: bool,
}

/// What to tell someone an edit was refused for.
fn refusal(r: Refusal) -> &'static str {
    match r {
        Refusal::NoGeometry => "This feature has no geometry to edit",
        Refusal::NotLonLat => "These coordinates are not longitude and latitude",
        Refusal::LineTooShort => "A line needs at least two positions",
        Refusal::PolygonTooSmall => "A polygon needs at least three positions",
        Refusal::NotAFeature => "Only a Feature has a name",
        Refusal::Nested => "This GeoJSON is part of other JSON: remove it in the text",
        Refusal::CursorInside => {
            "Put the editor's cursor where the new FeatureCollection should go"
        }
    }
}

fn in_view(p: &crate::geo::view::Projection, q: [f64; 2]) -> bool {
    let (south, north) = p.lat_range();
    (south..=north).contains(&q[1]) && p.copies(q[0], q[0]).next().is_some()
}

impl GeoMapDialog {
    pub(super) fn toggle_editing(&mut self) {
        self.release_grab();
        self.editing = match self.editing {
            Some(_) => None,
            None => Some(Box::default()),
        };
    }

    fn note(&mut self, r: Refusal) {
        if let Some(e) = self.editing.as_mut() {
            e.note = Some(refusal(r));
        }
    }

    /// How far from the middle of a cell the pointer can be from what it is
    /// on: half the cell's diagonal on the canvas, and a little more.
    fn reach(&self) -> f64 {
        let ((w, h), m) = (self.canvas, self.map_rect);
        let cw = f64::from(w) / f64::from(m.width.max(1));
        let ch = f64::from(h) / f64::from(m.height.max(1));
        cw.hypot(ch) * 0.6
    }

    /// Places worth keeping in a position placed at this zoom.
    fn decimals(&self) -> usize {
        let (w, h) = self.canvas;
        geo::decimals(self.view.project(w, h).deg_per_px())
    }

    /// The picked feature's positions, read from the text the first time.
    fn model(&mut self) -> Option<Result<&mut Model, Refusal>> {
        let at = self.feature?;
        let State::Ready { doc, .. } = &self.state else { return None };
        let e = self.editing.as_mut()?;
        if e.model.as_ref().is_none_or(|(m, _)| *m != at) {
            let f = doc.objects.get(at.0)?.features.get(at.1)?;
            e.model = Some((at, Model::read(&self.text, f)));
        }
        e.model.as_mut().map(|(_, m)| m.as_mut().map_err(|r| *r))
    }

    /// The selected position, while it is on the picked feature.
    fn vertex(&self) -> Option<Vertex> {
        let e = self.editing.as_ref()?;
        e.vertex.filter(|(at, _)| Some(*at) == self.feature).map(|(_, v)| v)
    }

    fn select(&mut self, v: Option<Vertex>) {
        let at = self.feature;
        if let Some(e) = self.editing.as_mut() {
            e.vertex = v.zip(at).map(|(v, at)| (at, v));
        }
    }

    /// The picked feature's handles, each path carried on past ±180 where it
    /// crosses; or what to say when they are not to be shown.
    fn handles(&mut self) -> Result<Vec<Handle>, &'static str> {
        let (w, h) = self.canvas;
        let cells = u32::from(self.map_rect.width) * u32::from(self.map_rect.height);
        let limit = (cells / CELLS_PER_HANDLE).clamp(20, 2000) as usize;
        let p = self.view.project(w, h);
        let model = match self.model() {
            None => return Ok(Vec::new()),
            Some(Err(r)) => return Err(refusal(r)),
            Some(Ok(m)) => m,
        };
        let mut out = Vec::new();
        for (start, kind, pts) in model.paths() {
            let xy: Vec<[f64; 2]> = pts.iter().map(Pos::xy).collect();
            if kind == PathKind::Point {
                out.extend(xy.iter().map(|&at| Handle { vertex: start, at, midpoint: false }));
                continue;
            }
            let xy = unwrap_dateline(&xy);
            let n = xy.len();
            for (index, &at) in xy.iter().enumerate() {
                out.push(Handle { vertex: Vertex { index, ..start }, at, midpoint: false });
            }
            let segments = if kind == PathKind::Ring { n } else { n.saturating_sub(1) };
            for i in 0..segments {
                let (a, mut b) = (xy[i], xy[(i + 1) % n]);
                b[0] += ((a[0] - b[0]) / 360.0).round() * 360.0;
                out.push(Handle {
                    vertex: Vertex { index: i + 1, ..start },
                    at: [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5],
                    midpoint: true,
                });
            }
        }
        if out.iter().filter(|h| !h.midpoint && in_view(&p, h.at)).count() > limit {
            return Err("Zoom in to edit the positions");
        }
        Ok(out)
    }

    /// The handle nearest canvas position (`x`, `y`), within about a cell; a
    /// position before a midpoint as near.
    fn handle_at(&mut self, x: f64, y: f64) -> Option<Handle> {
        let (w, h) = self.canvas;
        let reach = self.reach();
        let p = self.view.project(w, h);
        let mut best: Option<(f64, Handle)> = None;
        for handle in self.handles().ok()? {
            let behind = if handle.midpoint { reach * 0.5 } else { 0.0 };
            for off in p.copies(handle.at[0], handle.at[0]) {
                let (hx, hy) = p.xy(handle.at[0] + off, handle.at[1]);
                let d = (f64::from(hx) - x).hypot(f64::from(hy) - y);
                if d <= reach && best.as_ref().is_none_or(|(bd, _)| d + behind < *bd) {
                    best = Some((d + behind, handle));
                }
            }
        }
        best.map(|(_, h)| h)
    }

    /// Everything editing draws over the map.
    pub(super) fn overlay(&mut self) -> Option<Overlay> {
        let e = self.editing.as_ref()?;
        let tool = e.tool;
        let mut o = Overlay::default();
        match tool {
            Tool::Draw(kind) => {
                o.sketch = e.sketch.clone();
                o.closed = kind == Kind::Polygon;
                if kind != Kind::Point && !e.sketch.is_empty() {
                    let centre = (self.view.clon, self.view.clat);
                    let next = if e.by_key { Some(centre) } else { self.pointer };
                    o.next = next.map(|(lon, lat)| [lon, lat]);
                }
                o.crosshair = true;
            }
            Tool::Select => {
                let selected = self.vertex();
                for h in self.handles().unwrap_or_default() {
                    if h.midpoint {
                        o.midpoints.push(h.at);
                    } else {
                        o.vertices.push((h.at, Some(h.vertex) == selected));
                    }
                }
            }
        }
        Some(o)
    }

    /// Where a feature being drawn goes, to show: a collection's path, or a
    /// new collection.
    pub(super) fn draw_target(&self) -> Option<String> {
        let e = self.editing.as_ref()?;
        if !matches!(e.tool, Tool::Draw(_)) {
            return None;
        }
        let doc = self.doc()?;
        Some(match geo::target(&self.text, doc, self.object, self.feature, self.cursor_byte) {
            Ok(Target::Collection(oi)) => doc.objects.get(oi)?.path.clone(),
            Ok(_) => "+ FeatureCollection".into(),
            Err(_) => return None,
        })
    }

    /// Make `change` — to the text here, to the document, and, through the
    /// app, to the editor's text — and keep it to undo.
    fn commit(&mut self, change: Change) {
        let State::Ready { doc, .. } = &mut self.state else { return };
        if self.text.byte_slice(change.start..change.end) == change.text.as_str() {
            return;
        }
        let undo = geo::apply(&mut self.text, doc, &change);
        self.outbox.push(TextEdit::Replace {
            start: change.start,
            end: change.end,
            text: change.text.clone(),
        });
        self.follow_cursor(change.start, change.end, change.text.len());
        let scope = change.scope;
        self.history.redo.clear();
        self.history.undo.push(Step { redo: change, undo });
        self.after(scope);
    }

    /// Ctrl-Z, or Ctrl-Y with `redo`.
    fn undo(&mut self, redo: bool) {
        let step = if redo { self.history.redo.pop() } else { self.history.undo.pop() };
        let Some(step) = step else { return };
        let State::Ready { doc, .. } = &mut self.state else { return };
        let change = if redo { &step.redo } else { &step.undo };
        geo::apply(&mut self.text, doc, change);
        let (start, end, len) = (change.start, change.end, change.text.len());
        self.outbox.push(if redo {
            TextEdit::Redo { start, end, len }
        } else {
            TextEdit::Undo { start, end, len }
        });
        self.follow_cursor(start, end, len);
        let scope = change.scope;
        if redo {
            self.history.undo.push(step);
        } else {
            self.history.redo.push(step);
        }
        self.after(scope);
    }

    /// The editor's cursor after `[start, end)` became `len` bytes, as the
    /// editor moves it.
    fn follow_cursor(&mut self, start: usize, end: usize, len: usize) {
        let c = self.cursor_byte;
        self.cursor_byte = if c >= end {
            c - end + start + len
        } else if c > start {
            start
        } else {
            c
        };
    }

    /// The document changed: pick what the change made, and forget what was
    /// read from what it replaced.
    fn after(&mut self, scope: Scope) {
        self.revision += 1;
        if let Some(e) = self.editing.as_mut() {
            e.model = None;
            e.grab = None;
        }
        let Some(doc) = self.doc() else { return };
        let objects = doc.objects.len();
        match scope {
            Scope::Feature(..) => {}
            Scope::Added { oi, fi, .. } => self.feature = Some((oi, fi)),
            Scope::Removed { oi, fi, .. } => {
                self.feature = match self.feature {
                    Some((o, f)) if o == oi && f == fi => None,
                    Some((o, f)) if o == oi && f > fi => Some((o, f - 1)),
                    other => other,
                };
            }
            Scope::Document { select } => {
                let feature = select.and_then(|at| {
                    doc.objects.iter().enumerate().find_map(|(oi, o)| {
                        o.features.iter().position(|f| f.span.start == at).map(|fi| (oi, fi))
                    })
                });
                let object =
                    select.and_then(|at| doc.objects.iter().position(|o| o.span.start == at));
                self.feature = feature;
                self.object = match (feature, object) {
                    (Some((oi, _)), _) if self.object.is_some() && objects > 1 => Some(oi),
                    (None, Some(oi)) if objects > 1 => Some(oi),
                    _ => self.object.filter(|&o| o < objects),
                };
            }
        }
    }

    /// Draw the picked feature's edited positions, before they are written.
    fn show_model(&mut self) {
        let Some((oi, fi)) = self.feature else { return };
        let Some(Some((_, Ok(model)))) = self.editing.as_ref().map(|e| &e.model) else { return };
        let shapes = model.shapes();
        let State::Ready { doc, .. } = &mut self.state else { return };
        let Some(o) = doc.objects.get_mut(oi) else { return };
        if let Some(f) = o.features.get_mut(fi) {
            f.bounds = shapes_bounds(&shapes);
            f.shapes = shapes;
        }
        o.rebound();
        self.revision += 1;
    }

    /// Write geometry `gi` of the picked feature back from its edited positions.
    fn write_geom(&mut self, gi: usize) {
        let Some(at) = self.feature else { return };
        let State::Ready { doc, .. } = &self.state else { return };
        let Some(Some((_, Ok(model)))) = self.editing.as_ref().map(|e| &e.model) else { return };
        if let Some(change) = model.write(&self.text, doc, at, gi) {
            self.commit(change);
        }
    }

    // -- Positions ---------------------------------------------------------

    fn grab(&mut self, handle: Handle, col: u16, row: u16) {
        let decimals = self.decimals();
        let Some(Ok(model)) = self.model() else { return };
        let vertex = if handle.midpoint {
            let pos = Pos::new(handle.at[0], handle.at[1], decimals);
            let Some(v) = model.insert(handle.vertex, pos) else { return };
            v
        } else {
            handle.vertex
        };
        let Some(from) = model.position(vertex).map(Pos::xy) else { return };
        self.select(Some(vertex));
        if let Some(e) = self.editing.as_mut() {
            e.grab = Some(Grab { vertex, col, row, from, moved: false, added: handle.midpoint });
        }
        if handle.midpoint {
            self.show_model();
        }
    }

    fn drag_grab(&mut self, col: u16, row: u16) {
        let (w, h) = self.canvas;
        let m = self.map_rect;
        let decimals = self.decimals();
        let p = self.view.project(w, h);
        let Some(e) = self.editing.as_mut() else { return };
        let Some(g) = e.grab.as_mut() else { return };
        let dx = (f64::from(col) - f64::from(g.col)) * f64::from(w) / f64::from(m.width.max(1));
        let dy = (f64::from(row) - f64::from(g.row)) * f64::from(h) / f64::from(m.height.max(1));
        if dx == 0.0 && dy == 0.0 && !g.moved {
            return;
        }
        g.moved = true;
        let (x, y) = p.xy(g.from[0], g.from[1]);
        let (lon, lat) = p.lonlat(f64::from(x) + dx, f64::from(y) + dy);
        let v = g.vertex;
        if let Some((_, Ok(model))) = e.model.as_mut() {
            model.set(v, lon, lat, decimals);
        }
        self.show_model();
    }

    /// The drag is over: write what it did.
    fn release_grab(&mut self) {
        let Some(g) = self.editing.as_mut().and_then(|e| e.grab.take()) else { return };
        if g.moved || g.added {
            self.write_geom(g.vertex.geom);
        }
    }

    /// Shift and an arrow: move the selected position a cell.
    fn nudge(&mut self, dx: f64, dy: f64) {
        let Some(v) = self.vertex() else { return };
        let (w, h) = self.canvas;
        let m = self.map_rect;
        let decimals = self.decimals();
        let p = self.view.project(w, h);
        let (sx, sy) =
            (f64::from(w) / f64::from(m.width.max(1)), f64::from(h) / f64::from(m.height.max(1)));
        let Some(Ok(model)) = self.model() else { return };
        let Some(q) = model.position(v).map(Pos::xy) else { return };
        let (x, y) = p.xy(q[0], q[1]);
        let (lon, lat) = p.lonlat(f64::from(x) + dx * sx, f64::from(y) + dy * sy);
        model.set(v, lon, lat, decimals);
        self.write_geom(v.geom);
    }

    /// Insert: a position after the selected one — halfway to the next, or on
    /// past the end of a line.
    fn insert_after(&mut self) {
        let Some(v) = self.vertex() else { return };
        let decimals = self.decimals();
        let Some(Ok(model)) = self.model() else { return };
        let Some(path) = model.geoms.get(v.geom).and_then(|g| g.parts.get(v.part)?.get(v.path))
        else {
            return;
        };
        let kind = model.geoms[v.geom].kind.path();
        let Some(a) = path.get(v.index).map(Pos::xy) else { return };
        let near = |mut b: [f64; 2]| {
            b[0] += ((a[0] - b[0]) / 360.0).round() * 360.0;
            b
        };
        let at = match path.get(v.index + 1) {
            _ if kind == PathKind::Point => return,
            Some(b) => {
                let b = near(b.xy());
                [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
            }
            None if kind == PathKind::Ring => {
                let b = near(path[0].xy());
                [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
            }
            None if v.index > 0 => {
                let b = near(path[v.index - 1].xy());
                [2.0 * a[0] - b[0], 2.0 * a[1] - b[1]]
            }
            None => return,
        };
        let pos = Pos::new(at[0], at[1], decimals);
        if let Some(added) = model.insert(Vertex { index: v.index + 1, ..v }, pos) {
            self.select(Some(added));
            self.write_geom(v.geom);
        }
    }

    /// `[` and `]`: select the previous or next position, bringing it into view.
    fn step_vertex(&mut self, forward: bool) {
        let current = self.vertex();
        let Some(Ok(model)) = self.model() else { return };
        let Some(next) = model.step(current, forward) else { return };
        let Some(q) = model.position(next).map(Pos::xy) else { return };
        self.select(Some(next));
        let (w, h) = self.canvas;
        if !in_view(&self.view.project(w, h), q) {
            self.view.clon = q[0];
            self.view.clat = q[1];
            self.view.clamp();
        }
    }

    /// Del: the selected position, or with none selected the picked feature.
    fn remove(&mut self) {
        if self.feature.is_none() {
            return;
        }
        let Some(v) = self.vertex() else {
            self.remove_feature();
            return;
        };
        let removal = match self.model() {
            Some(Ok(model)) => model.remove(v),
            Some(Err(r)) => Err(r),
            None => return,
        };
        match removal {
            Ok(Removal::Position(next)) => {
                self.select(Some(next));
                self.write_geom(v.geom);
            }
            Ok(Removal::Path) => {
                self.select(None);
                self.write_geom(v.geom);
            }
            Ok(Removal::Feature) => self.remove_feature(),
            Err(r) => self.note(r),
        }
    }

    fn remove_feature(&mut self) {
        let Some((oi, fi)) = self.feature else { return };
        let Some(doc) = self.doc() else { return };
        match geo::remove_feature(&self.text, doc, oi, fi) {
            Ok(change) => self.commit(change),
            Err(r) => self.note(r),
        }
    }

    // -- Drawing -----------------------------------------------------------

    /// 1, 2 or 3: draw a new point, line or polygon — or, pressed again, stop.
    fn start_tool(&mut self, kind: Kind) {
        let Some(doc) = self.doc() else { return };
        let fits = geo::target(&self.text, doc, self.object, self.feature, self.cursor_byte);
        let Some(e) = self.editing.as_mut() else { return };
        e.sketch.clear();
        e.naming = None;
        if e.tool == Tool::Draw(kind) {
            e.tool = Tool::Select;
            return;
        }
        // Say at once when a new feature would have nowhere to go.
        match fits {
            Ok(_) => e.tool = Tool::Draw(kind),
            Err(r) => self.note(r),
        }
    }

    /// A click, or Space at the crosshair, while drawing: a position there —
    /// or, back on the last position placed (or the first of a polygon), the
    /// end of the feature.
    fn place(&mut self, x: f64, y: f64) {
        let (w, h) = self.canvas;
        let reach = self.reach();
        let p = self.view.project(w, h);
        let Some(e) = self.editing.as_ref() else { return };
        let Tool::Draw(kind) = e.tool else { return };
        let near = |q: &[f64; 2]| {
            p.copies(q[0], q[0]).any(|off| {
                let (qx, qy) = p.xy(q[0] + off, q[1]);
                (f64::from(qx) - x).hypot(f64::from(qy) - y) <= reach
            })
        };
        let n = e.sketch.len();
        let back = n > 0
            && (near(&e.sketch[n - 1]) || (kind == Kind::Polygon && n >= 3 && near(&e.sketch[0])));
        if back {
            self.finish();
            return;
        }
        let (lon, lat) = p.lonlat(x, y);
        if let Some(e) = self.editing.as_mut() {
            e.sketch.push([lon, lat]);
        }
        if kind == Kind::Point {
            self.finish();
        }
    }

    /// Backspace while drawing: take back the last position.
    fn unplace(&mut self) {
        if let Some(e) = self.editing.as_mut() {
            e.sketch.pop();
        }
    }

    /// Enter while drawing: the feature drawn goes into the GeoJSON.
    fn finish(&mut self) {
        let decimals = self.decimals();
        let Some(e) = self.editing.as_ref() else { return };
        let Tool::Draw(kind) = e.tool else { return };
        let (need, short) = match kind {
            Kind::Polygon => (3, Refusal::PolygonTooSmall),
            _ => (2, Refusal::LineTooShort),
        };
        if e.sketch.is_empty() {
            return;
        }
        if kind != Kind::Point && e.sketch.len() < need {
            self.note(short);
            return;
        }
        let geom = Geom::from_path(kind, &e.sketch, decimals);
        let Some(doc) = self.doc() else { return };
        let change = geo::target(&self.text, doc, self.object, self.feature, self.cursor_byte)
            .map(|target| geo::add_feature(&self.text, doc, target, &geom));
        match change {
            Ok(Some(change)) => {
                if let Some(e) = self.editing.as_mut() {
                    e.sketch.clear();
                }
                self.commit(change);
            }
            Ok(None) => {}
            Err(r) => self.note(r),
        }
    }

    // -- Names and collections ---------------------------------------------

    /// r: type a name for the picked feature.
    fn start_naming(&mut self) {
        let Some((oi, fi)) = self.feature else { return };
        let Some(o) = self.doc().and_then(|d| d.objects.get(oi)) else { return };
        if o.features_array.is_none() && o.kind != "Feature" {
            self.note(Refusal::NotAFeature);
            return;
        }
        let name = o.features.get(fi).and_then(|f| {
            NAME_KEYS
                .iter()
                .find_map(|k| f.props.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone()))
        });
        let name = name.unwrap_or_default();
        if let Some(e) = self.editing.as_mut() {
            let caret = name.chars().count();
            e.naming = Some((name, caret));
        }
    }

    fn naming_key(&mut self, key: KeyEvent) {
        let Some(e) = self.editing.as_mut() else { return };
        let Some((name, caret)) = e.naming.as_mut() else { return };
        match key.code {
            KeyCode::Esc => e.naming = None,
            KeyCode::Enter => {
                let Some((name, _)) = e.naming.take() else { return };
                let (Some(at), Some(doc)) = (self.feature, self.doc()) else { return };
                match geo::set_name(&self.text, doc, at, &name) {
                    Ok(change) => self.commit(change),
                    Err(r) => self.note(r),
                }
            }
            _ => edit_text(name, caret, key),
        }
    }

    /// c: an empty FeatureCollection, for features to be drawn into.
    fn new_collection(&mut self) {
        let Some(doc) = self.doc() else { return };
        match geo::new_collection(&self.text, doc, self.cursor_byte) {
            Ok(change) => self.commit(change),
            Err(r) => self.note(r),
        }
    }

    // -- Input -------------------------------------------------------------

    /// A key while editing; `None` leaves it to the map.
    pub(super) fn edit_key(&mut self, key: KeyEvent) -> Option<DialogResult> {
        self.release_grab();
        let e = self.editing.as_mut()?;
        if e.naming.is_some() {
            self.naming_key(key);
            return Some(DialogResult::None);
        }
        e.note = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let drawing = matches!(e.tool, Tool::Draw(_));
        let sketching = drawing && !e.sketch.is_empty();
        if drawing {
            e.by_key |= matches!(
                key.code,
                KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
            );
        }
        let selected = self.vertex().is_some();
        let (w, h) = self.canvas;
        match key.code {
            KeyCode::Char('z') if ctrl && sketching => self.unplace(),
            KeyCode::Char('z') if ctrl => self.undo(false),
            KeyCode::Char('y') if ctrl => self.undo(true),
            _ if ctrl => return None,
            KeyCode::Char('e') => self.toggle_editing(),
            KeyCode::Char('1') => self.start_tool(Kind::Point),
            KeyCode::Char('2') => self.start_tool(Kind::LineString),
            KeyCode::Char('3') => self.start_tool(Kind::Polygon),
            KeyCode::Char('c') => self.new_collection(),
            KeyCode::Esc => self.escape(),
            KeyCode::Char(' ') if drawing => self.place(f64::from(w) * 0.5, f64::from(h) * 0.5),
            KeyCode::Enter if drawing => self.finish(),
            KeyCode::Backspace if drawing => self.unplace(),
            KeyCode::Delete | KeyCode::Backspace => self.remove(),
            KeyCode::Insert | KeyCode::Char('i') => self.insert_after(),
            KeyCode::Char('[') => self.step_vertex(false),
            KeyCode::Char(']') => self.step_vertex(true),
            KeyCode::Char('r') => self.start_naming(),
            KeyCode::Left if shift && selected => self.nudge(-1.0, 0.0),
            KeyCode::Right if shift && selected => self.nudge(1.0, 0.0),
            KeyCode::Up if shift && selected => self.nudge(0.0, -1.0),
            KeyCode::Down if shift && selected => self.nudge(0.0, 1.0),
            _ => return None,
        }
        Some(DialogResult::None)
    }

    /// Esc, a step at a time: the positions placed, the tool, the selected
    /// position, and editing itself.
    fn escape(&mut self) {
        let selected = self.vertex().is_some();
        let Some(e) = self.editing.as_mut() else { return };
        if matches!(e.tool, Tool::Draw(_)) && !e.sketch.is_empty() {
            e.sketch.clear();
        } else if matches!(e.tool, Tool::Draw(_)) {
            e.tool = Tool::Select;
        } else if selected {
            e.vertex = None;
        } else {
            self.editing = None;
        }
    }

    /// The mouse while editing; `None` leaves the event to the map.
    pub(super) fn edit_mouse(&mut self, ev: MouseEvent, in_map: bool) -> Option<DialogResult> {
        let e = self.editing.as_mut()?;
        let (col, row) = (ev.column, ev.row);
        let tool = e.tool;
        let grabbing = e.grab.is_some();
        if matches!(ev.kind, MouseEventKind::Down(_)) {
            e.note = None;
            e.naming = None;
        }
        if in_map && !matches!(ev.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
            e.by_key = false;
        }
        match ev.kind {
            MouseEventKind::Drag(MouseButton::Left) if grabbing => self.drag_grab(col, row),
            MouseEventKind::Up(MouseButton::Left) if grabbing => self.release_grab(),
            MouseEventKind::Down(MouseButton::Left) if in_map && tool == Tool::Select => {
                let (x, y) = self.canvas_at(col, row);
                let handle = self.handle_at(x, y)?;
                self.grab(handle, col, row);
            }
            MouseEventKind::Up(MouseButton::Left) if in_map && matches!(tool, Tool::Draw(_)) => {
                // A click, not the end of a drag that panned.
                if self.drag.take().is_some_and(|(_, _, moved)| !moved) {
                    let (x, y) = self.canvas_at(col, row);
                    self.place(x, y);
                }
            }
            MouseEventKind::Down(MouseButton::Right) if in_map => match tool {
                Tool::Draw(_) => self.unplace(),
                Tool::Select => {
                    let (x, y) = self.canvas_at(col, row);
                    if let Some(h) = self.handle_at(x, y).filter(|h| !h.midpoint) {
                        self.select(Some(h.vertex));
                        self.remove();
                    }
                }
            },
            _ => return None,
        }
        Some(DialogResult::None)
    }

    // -- Drawing the rows --------------------------------------------------

    /// The tools, along a row above the properties: each its key and name,
    /// the one in use highlighted, as far as the row goes.
    pub(super) fn render_tools(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let Some(e) = self.editing.as_ref() else { return };
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let dim = base.fg(theme.panel_border);
        let plain = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let items = [
            ("1", "Point", plain('1'), e.tool == Tool::Draw(Kind::Point), true),
            ("2", "Line", plain('2'), e.tool == Tool::Draw(Kind::LineString), true),
            ("3", "Polygon", plain('3'), e.tool == Tool::Draw(Kind::Polygon), true),
            ("Del", "Remove", KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE), false, true),
            ("^Z", "Undo", ctrl('z'), false, !self.history.undo.is_empty()),
            ("^Y", "Redo", ctrl('y'), false, !self.history.redo.is_empty()),
            ("r", "Rename", plain('r'), false, true),
            ("c", "New collection", plain('c'), false, true),
        ];
        let mut spans = vec![Span::styled(" ", base)];
        let mut x = area.x + 1;
        let right = area.x + area.width;
        for (key, label, event, active, enabled) in items {
            let label = crate::l10n::trd(label);
            let width =
                (key.len() + 1 + unicode_width::UnicodeWidthStr::width(label.as_str())) as u16;
            if x + width > right {
                break;
            }
            let (key_style, label_style) = match (active, enabled) {
                (true, _) => (theme.dialog_selection, theme.dialog_selection),
                (false, true) => (base.fg(theme.hotkey_fg), base),
                (false, false) => (dim, dim),
            };
            spans.push(Span::styled(key, key_style));
            spans.push(Span::styled(" ", label_style));
            spans.push(Span::styled(label, label_style));
            spans.push(Span::styled("  ", base));
            self.tools.push((Rect { x, width, ..area }, event));
            x += width + 2;
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(base), area);
    }

    /// The row under the tools: the name being typed, what could not be done,
    /// or what the tool in use does.
    pub(super) fn render_edit_row(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let dim = base.fg(theme.panel_border);
        let tr = crate::l10n::trd;
        let Some(e) = self.editing.as_ref() else { return };
        let (tool, note) = (e.tool, e.note);
        if let Some((name, caret)) = &e.naming {
            let label = format!(" {}: ", tr("Name"));
            let lw = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
            f.render_widget(Paragraph::new(label).style(base), area);
            let field = Rect {
                x: area.x + lw.min(area.width),
                width: area.width.saturating_sub(lw + 1),
                ..area
            };
            if field.width > 1 {
                let input = Style::default().fg(theme.input_fg).bg(theme.input_bg);
                let chars: Vec<char> = name.chars().collect();
                let first = caret.saturating_sub(field.width as usize - 1);
                let shown: String = chars[first..].iter().take(field.width as usize).collect();
                f.render_widget(Paragraph::new(shown).style(input), field);
                let cx = field.x + (caret - first) as u16;
                f.set_cursor_position(Position::new(cx, area.y));
            }
            return;
        }
        let line = if let Some(note) = note {
            Line::from(Span::styled(format!(" {}", tr(note)), base.fg(theme.error_fg)))
        } else {
            let hint = |text: &str| Span::styled(format!(" {}", tr(text)), dim);
            match tool {
                Tool::Draw(Kind::Point) => Line::from(hint(
                    "Click the map or press Space to place a point; Esc stops drawing",
                )),
                Tool::Draw(_) => Line::from(hint(
                    "Click or press Space to add positions; Enter finishes, Backspace takes one back, Esc cancels",
                )),
                Tool::Select => {
                    let selected = self.vertex().is_some();
                    let name = self.feature.and_then(|(oi, fi)| {
                        self.doc()?.objects.get(oi)?.features.get(fi)?.name.clone()
                    });
                    match (self.feature.is_some(), self.handles()) {
                        (false, _) => Line::from(hint(
                            "Click a feature to edit it, or draw a new one with 1, 2 or 3",
                        )),
                        (true, Err(text)) => Line::from(hint(text)),
                        (true, Ok(_)) => {
                            let mut spans = Vec::new();
                            if let Some(name) = name.filter(|_| !selected) {
                                spans.push(Span::styled(
                                    format!(" {name} "),
                                    base.add_modifier(Modifier::BOLD),
                                ));
                            }
                            spans.push(hint(if selected {
                                "Drag or Shift+arrows move the position, Insert adds one after it, Del removes it"
                            } else {
                                "Drag a position to move it or a midpoint to add one; [ ] select positions, Del removes the feature"
                            }));
                            Line::from(spans)
                        }
                    }
                }
            }
        };
        f.render_widget(Paragraph::new(line).style(base), area);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{draw, key, mouse};
    use super::*;
    use crate::geo::geojson::{Shape, extract};

    /// The editor's text, as the edits the map hands over make it.
    #[derive(Default)]
    struct Editor {
        text: String,
        undo: Vec<(usize, String, String)>,
        redo: Vec<(usize, String, String)>,
    }

    impl Editor {
        fn take(&mut self, d: &mut GeoMapDialog) {
            for edit in d.take_edits() {
                match edit {
                    TextEdit::Replace { start, end, text } => {
                        let old = self.text[start..end].to_string();
                        self.text.replace_range(start..end, &text);
                        self.undo.push((start, old, text));
                        self.redo.clear();
                    }
                    TextEdit::Undo { start, end, len } => {
                        let (at, old, new) = self.undo.pop().expect("a step to undo");
                        assert_eq!((at, at + new.len(), old.len()), (start, end, len));
                        self.text.replace_range(at..at + new.len(), &old);
                        self.redo.push((at, old, new));
                    }
                    TextEdit::Redo { start, end, len } => {
                        let (at, old, new) = self.redo.pop().expect("a step to redo");
                        assert_eq!((at, at + old.len(), new.len()), (start, end, len));
                        self.text.replace_range(at..at + old.len(), &new);
                        self.undo.push((at, old, new));
                    }
                }
            }
            assert_eq!(self.text, d.text.to_string(), "the editor and the map agree");
            let fresh = extract(&self.text);
            assert_eq!(format!("{:?}", d.doc().unwrap()), format!("{fresh:?}"));
        }
    }

    fn open(text: &str) -> (GeoMapDialog, Editor) {
        let mut d = GeoMapDialog::loading("places.geojson", 1, 0, Rope::from_str(text));
        d.set_doc(extract(text));
        draw(&mut d, 100, 30);
        (d, Editor { text: text.into(), ..Editor::default() })
    }

    /// The cell a longitude and latitude is drawn in.
    fn cell_of(d: &GeoMapDialog, lon: f64, lat: f64) -> (u16, u16) {
        let (w, h) = d.canvas;
        let m = d.map_rect;
        let (x, y) = d.view.project(w, h).xy(lon, lat);
        (
            m.x + (f64::from(x) * f64::from(m.width) / f64::from(w)) as u16,
            m.y + (f64::from(y) * f64::from(m.height) / f64::from(h)) as u16,
        )
    }

    fn click(d: &mut GeoMapDialog, (col, row): (u16, u16)) {
        mouse(d, MouseEventKind::Down(MouseButton::Left), col, row);
        mouse(d, MouseEventKind::Up(MouseButton::Left), col, row);
    }

    fn ctrl(d: &mut GeoMapDialog, c: char) {
        d.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    #[test]
    fn an_empty_file_opens_for_drawing_and_its_first_feature_makes_a_collection() {
        let (mut d, mut ed) = open("");
        assert!(d.editing.is_some(), "nothing to look at: editing starts");
        let rows = draw(&mut d, 100, 30);
        assert!(rows.iter().any(|r| r.contains("No GeoJSON found in this file")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("1 Point") && r.contains("3 Polygon")), "{rows:?}");
        key(&mut d, KeyCode::Char('2'));
        let m = d.map_rect;
        let (a, b) = ((m.x + 10, m.y + 5), (m.x + 30, m.y + 12));
        click(&mut d, a);
        click(&mut d, b);
        let rows = draw(&mut d, 100, 30);
        assert!(rows.iter().any(|r| r.contains("→ + FeatureCollection")), "{rows:?}");
        click(&mut d, b);
        ed.take(&mut d);
        assert!(ed.text.starts_with("{\n  \"type\": \"FeatureCollection\",\n"), "{}", ed.text);
        let doc = d.doc().unwrap();
        assert!(matches!(&doc.objects[0].features[0].shapes[..], [Shape::Line(l)] if l.len() == 2));
        assert_eq!(d.feature, Some((0, 0)), "the new feature is picked");

        // A polygon closed on its first position goes into the collection.
        key(&mut d, KeyCode::Char('3'));
        let c = (m.x + 20, m.y + 20);
        for at in [a, b, c, a] {
            click(&mut d, at);
        }
        ed.take(&mut d);
        assert_eq!(d.doc().unwrap().objects[0].features.len(), 2);
        assert_eq!(d.feature, Some((0, 1)));
        // From the keyboard: a point at the crosshair.
        key(&mut d, KeyCode::Char('1'));
        key(&mut d, KeyCode::Char(' '));
        ed.take(&mut d);
        let doc = d.doc().unwrap();
        assert_eq!(doc.objects[0].features.len(), 3);
        let Shape::Point(q) = doc.objects[0].features[2].shapes[0] else { panic!() };
        assert!((q[0] - d.view.clon).abs() < 1.0 && (q[1] - d.view.clat).abs() < 1.0);

        // Undone step by step, back to nothing, and made again.
        for _ in 0..3 {
            ctrl(&mut d, 'z');
        }
        ed.take(&mut d);
        assert_eq!(ed.text, "");
        ctrl(&mut d, 'z');
        assert!(d.take_edits().is_empty(), "no further back than the map's own edits");
        ctrl(&mut d, 'y');
        ed.take(&mut d);
        assert!(d.doc().unwrap().objects[0].features.len() == 1);
    }

    const PARK: &str = r#"{"type": "FeatureCollection", "features": [
  {"type": "Feature", "properties": {"name": "Prater"}, "geometry": {"type": "Polygon", "coordinates": [[[16.39, 48.2], [16.45, 48.2], [16.45, 48.22], [16.39, 48.22], [16.39, 48.2]]]}},
  {"type": "Feature", "properties": {}, "geometry": {"type": "LineString", "coordinates": [[16.3, 48.1], [16.31, 48.11]]}}
]}"#;

    fn ring(d: &GeoMapDialog) -> Vec<[f64; 2]> {
        let Shape::Polygon(rings) = &d.doc().unwrap().objects[0].features[0].shapes[0] else {
            panic!()
        };
        rings[0].clone()
    }

    #[test]
    fn positions_are_dragged_added_and_removed() {
        let (mut d, mut ed) = open(PARK);
        key(&mut d, KeyCode::Char('e'));
        key(&mut d, KeyCode::Char('n'));
        assert_eq!(d.feature, Some((0, 0)));
        key(&mut d, KeyCode::Home);
        let rows = draw(&mut d, 100, 30);
        assert!(rows.iter().any(|r| r.contains('■')), "handles on the positions: {rows:?}");

        // Drag the south-east corner three cells right.
        let (col, row) = cell_of(&d, 16.45, 48.2);
        mouse(&mut d, MouseEventKind::Down(MouseButton::Left), col, row);
        assert!(d.panning(), "a drag of a position folds like a pan");
        mouse(&mut d, MouseEventKind::Drag(MouseButton::Left), col + 3, row);
        assert!(d.take_edits().is_empty(), "nothing is written while dragging");
        mouse(&mut d, MouseEventKind::Up(MouseButton::Left), col + 3, row);
        ed.take(&mut d);
        assert!(!ed.text.contains("[16.45, 48.2]"), "{}", ed.text);
        assert!(ed.text.contains("\"name\": \"Prater\""), "the rest is as it was");
        assert_eq!(d.vertex(), Some(Vertex { geom: 0, part: 0, path: 0, index: 1 }));
        let east = ring(&d).iter().map(|q| q[0]).fold(f64::MIN, f64::max);
        assert!(east > 16.452, "{east}");

        // A click on the middle of the west side adds a position there.
        let (col, row) = cell_of(&d, 16.39, 48.21);
        click(&mut d, (col, row));
        ed.take(&mut d);
        assert_eq!(ring(&d).len(), 6, "five positions and the closing one");
        assert_eq!(d.vertex().map(|v| v.index), Some(4));
        // Insert adds another after it; Del takes it out again.
        key(&mut d, KeyCode::Insert);
        ed.take(&mut d);
        assert_eq!(ring(&d).len(), 7);
        key(&mut d, KeyCode::Delete);
        ed.take(&mut d);
        assert_eq!(ring(&d).len(), 6);
        // Shift and an arrow nudges; ] steps on.
        let before = ring(&d);
        d.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        ed.take(&mut d);
        assert_ne!(ring(&d), before);
        key(&mut d, KeyCode::Char(']'));
        assert_eq!(d.vertex().map(|v| v.index), Some(0), "round from the last to the first");

        // The line cannot lose its second-to-last position.
        key(&mut d, KeyCode::Char('n'));
        key(&mut d, KeyCode::Char(']'));
        key(&mut d, KeyCode::Delete);
        assert!(d.take_edits().is_empty());
        let rows = draw(&mut d, 100, 30);
        assert!(rows.iter().any(|r| r.contains("A line needs at least two positions")), "{rows:?}");
        // With no position selected Del takes the feature.
        key(&mut d, KeyCode::Esc);
        assert_eq!(d.vertex(), None);
        key(&mut d, KeyCode::Delete);
        ed.take(&mut d);
        assert_eq!(d.doc().unwrap().objects[0].features.len(), 1);
        assert!(ed.text.ends_with("]]]}}\n]}"), "{}", ed.text);
        ctrl(&mut d, 'z');
        ed.take(&mut d);
        assert_eq!(d.feature, Some((0, 1)), "the feature back, and picked");
        // Esc steps out of editing, then closes.
        key(&mut d, KeyCode::Esc);
        assert!(d.editing.is_none());
        assert!(matches!(key(&mut d, KeyCode::Esc), DialogResult::Cancel));
    }

    #[test]
    fn a_feature_is_named_and_a_collection_made_at_the_cursor() {
        let (mut d, mut ed) = open(PARK);
        key(&mut d, KeyCode::Char('e'));
        key(&mut d, KeyCode::Char('p'));
        assert_eq!(d.feature, Some((0, 1)));
        key(&mut d, KeyCode::Char('r'));
        for c in "Allee".chars() {
            key(&mut d, KeyCode::Char(c));
        }
        let rows = draw(&mut d, 100, 30);
        assert!(rows.iter().any(|r| r.contains("Name: Allee")), "{rows:?}");
        key(&mut d, KeyCode::Enter);
        ed.take(&mut d);
        assert!(ed.text.contains(r#""properties": {"name": "Allee"}"#), "{}", ed.text);
        assert_eq!(d.doc().unwrap().objects[0].features[1].name.as_deref(), Some("Allee"));

        // Collections where the editor's cursor is, outside any GeoJSON.
        let other = r#"{"parks": , "roads": }"#;
        let (mut d, mut ed) = open(other);
        d.cursor_byte = other.find(", \"roads\"").unwrap();
        key(&mut d, KeyCode::Char('c'));
        ed.take(&mut d);
        assert_eq!(
            ed.text,
            r#"{"parks": {"type": "FeatureCollection", "features": []}, "roads": }"#
        );
        key(&mut d, KeyCode::Char('1'));
        key(&mut d, KeyCode::Char(' '));
        ed.take(&mut d);
        assert_eq!(d.doc().unwrap().objects[0].features.len(), 1);
        // A second collection in the other place; features follow the choice.
        d.cursor_byte = ed.text.rfind('}').unwrap();
        key(&mut d, KeyCode::Char('c'));
        ed.take(&mut d);
        assert_eq!(d.doc().unwrap().objects.len(), 2);
        assert_eq!(d.object, Some(1), "the new collection is chosen");
        key(&mut d, KeyCode::Char(' '));
        ed.take(&mut d);
        assert!(ed.text.ends_with(r#""roads": {"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [0, 20]}}]}}"#), "{}", ed.text);
        // Not inside GeoJSON, though.
        d.cursor_byte = ed.text.find("features").unwrap();
        key(&mut d, KeyCode::Char('c'));
        assert!(d.take_edits().is_empty());
        let rows = draw(&mut d, 100, 30);
        assert!(rows.iter().any(|r| r.contains("Put the editor's cursor")), "{rows:?}");
    }

    #[test]
    fn editing_draws_at_any_size_in_cells_and_pixels() {
        for (w, h) in [(24, 8), (40, 12), (60, 20), (160, 50)] {
            let (mut d, _) = open(PARK);
            key(&mut d, KeyCode::Char('e'));
            key(&mut d, KeyCode::Char('n'));
            draw(&mut d, w, h);
            key(&mut d, KeyCode::Char('3'));
            key(&mut d, KeyCode::Char(' '));
            key(&mut d, KeyCode::Left);
            key(&mut d, KeyCode::Char(' '));
            draw(&mut d, w, h);
        }
        let (mut d, _) = open(PARK);
        key(&mut d, KeyCode::Char('e'));
        key(&mut d, KeyCode::Char('n'));
        let theme = Theme::mc();
        let mut gfx = crate::ui::graphics::Gfx::test_halfblocks();
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        t.draw(|f| d.render(f, f.area(), &theme, Some(&mut gfx))).unwrap();
    }
}
