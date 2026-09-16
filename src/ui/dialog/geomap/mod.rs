//! The GeoJSON map: what an edited JSON file draws, over a map of the world.
//!
//! Near the size of the screen, because the map is the point. A row along the
//! top says what is shown and where the pointer is; when the file holds more
//! than one piece of GeoJSON a list down the left picks one (the others stay
//! on the map, dimmed); the selected feature's properties run along the
//! bottom, over the Go to and Close buttons. The map is true pixels on a
//! graphics terminal and braille characters on any other.
//!
//! Nothing pops up over the map: a graphics terminal draws the image above any
//! text, so a popup there would be hidden under it.
//!
//! **Edit features** turns the map into an editor of the GeoJSON ([`edit`]):
//! a row of tools appears over the properties, and every change is made to the
//! editor's text as it happens.

mod edit;

use super::DialogResult;
use super::Submit;
use super::widgets::*;
use crate::geo::draw::{self, Scene};
use crate::geo::edit::TextEdit;
use crate::geo::geojson::{Bounds, GeoDoc};
use crate::geo::palette::{CellPalette, MapPalette};
use crate::geo::view::MapView;
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ropey::Rope;
use std::hash::{Hash, Hasher};

/// Widest pixel map built, as for the model viewer: every pan redraws it, and
/// a full-screen image at native resolution would re-encode megapixels a frame.
const MAP_MAX_PX: u32 = 1600;
/// A zoom step, for a key press or a wheel notch.
const ZOOM_STEP: f64 = 0.8;
/// Widest the object list gets.
const LIST_WIDTH: u16 = 34;

enum State {
    /// Waiting for the document read in the background.
    Loading(u64),
    Ready {
        doc: Box<GeoDoc>,
        generation: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Map,
}

pub struct GeoMapDialog {
    name: String,
    state: State,
    /// The object chosen in the list: `None` for all of them.
    object: Option<usize>,
    /// The feature picked out: (object, feature).
    feature: Option<(usize, usize)>,
    view: MapView,
    /// The view is to be fitted to the selection on the next frame, when the
    /// map's size is known.
    fit_pending: bool,
    focus: Focus,
    /// The editor's cursor, as a byte offset, to pick the object it is in.
    cursor_byte: usize,
    /// A drag under way: where the pointer was, and whether it has moved.
    drag: Option<(u16, u16, bool)>,
    /// The longitude and latitude under the pointer.
    pointer: Option<(f64, f64)>,
    // From the last frame, for the mouse.
    map_rect: Rect,
    list_rect: Rect,
    list_top: usize,
    goto_rect: Rect,
    edit_rect: Rect,
    close_rect: Rect,
    /// The map's canvas size and its size per cell.
    canvas: (u32, u32),
    /// The editor's text as it stands: an edit made on the map is made to it
    /// as well as to the editor's.
    text: Rope,
    /// Bumped by every change to the document, for the graphics cache.
    revision: u64,
    /// What editing is doing, while it is on.
    editing: Option<Box<edit::Editing>>,
    /// The edits made on the map, to undo and redo. They outlast turning
    /// editing off and on again.
    history: edit::History,
    /// Edits for the app to make in the editor, oldest first.
    outbox: Vec<TextEdit>,
    /// The tool row's entries from the last frame: where each is, and the key
    /// it stands for.
    tools: Vec<(Rect, KeyEvent)>,
}

impl GeoMapDialog {
    /// The dialog over the file `name`, whose text is `text`, while the
    /// document with `generation` is read from it.
    pub fn loading(
        name: impl Into<String>,
        generation: u64,
        cursor_byte: usize,
        text: Rope,
    ) -> Self {
        GeoMapDialog {
            name: name.into(),
            state: State::Loading(generation),
            object: None,
            feature: None,
            view: MapView::default(),
            fit_pending: true,
            focus: Focus::Map,
            cursor_byte,
            drag: None,
            pointer: None,
            map_rect: Rect::default(),
            list_rect: Rect::default(),
            list_top: 0,
            goto_rect: Rect::default(),
            edit_rect: Rect::default(),
            close_rect: Rect::default(),
            canvas: (0, 0),
            text,
            revision: 0,
            editing: None,
            history: edit::History::default(),
            outbox: Vec::new(),
            tools: Vec::new(),
        }
    }

    /// Whether this dialog is waiting for the document with `generation`.
    pub fn awaits(&self, generation: u64) -> bool {
        matches!(self.state, State::Loading(g) if g == generation)
    }

    /// The document has been read: show it, on the object the editor's cursor
    /// is in when there is one. With nothing in it yet, editing starts, for
    /// the first feature to be drawn.
    pub fn set_doc(&mut self, doc: GeoDoc) {
        if doc.objects.is_empty() {
            self.editing = Some(Box::default());
        }
        let generation = match self.state {
            State::Loading(g) => g,
            State::Ready { generation, .. } => generation,
        };
        let at = self.cursor_byte;
        self.object = doc.objects.iter().position(|o| o.span.contains(&at));
        if let Some(oi) = self.object {
            let o = &doc.objects[oi];
            self.feature = o.features.iter().position(|f| f.span.contains(&at)).map(|fi| (oi, fi));
        }
        if self.object.is_some() && doc.objects.len() > 1 {
            self.focus = Focus::List;
        }
        self.state = State::Ready { doc: Box::new(doc), generation };
        self.fit_pending = true;
    }

    fn doc(&self) -> Option<&GeoDoc> {
        match &self.state {
            State::Ready { doc, .. } => Some(doc),
            State::Loading(_) => None,
        }
    }

    /// Whether a drag is panning the map or moving a position on it, so the
    /// app folds its motion events.
    pub fn panning(&self) -> bool {
        self.drag.is_some() || self.editing.as_ref().is_some_and(|e| e.grab.is_some())
    }

    /// The edits made on the map since last asked, for the app to make in the
    /// editor.
    pub fn take_edits(&mut self) -> Vec<TextEdit> {
        std::mem::take(&mut self.outbox)
    }

    /// What the view should frame: the picked feature, the chosen object, or
    /// everything.
    fn focus_bounds(&self) -> Option<Bounds> {
        let doc = self.doc()?;
        if let Some((oi, fi)) = self.feature
            && let Some(b) =
                doc.objects.get(oi).and_then(|o| o.features.get(fi)).and_then(|f| f.bounds)
        {
            return Some(b);
        }
        match self.object {
            Some(oi) => doc.objects.get(oi).and_then(|o| o.bounds),
            None => doc.bounds(),
        }
    }

    fn fit(&mut self) {
        let (w, h) = self.canvas;
        if w == 0 {
            self.fit_pending = true;
            return;
        }
        self.fit_pending = false;
        match self.focus_bounds() {
            Some(b) => self.view = MapView::fit(&b, w, h),
            None => self.view = MapView { width: 360.0, ..MapView::default() },
        }
    }

    /// Step through the features — of the chosen object, or of every object.
    fn step_feature(&mut self, forward: bool) {
        let Some(doc) = self.doc() else { return };
        let all: Vec<(usize, usize)> = doc
            .objects
            .iter()
            .enumerate()
            .filter(|(oi, _)| self.object.is_none_or(|o| o == *oi))
            .flat_map(|(oi, o)| (0..o.features.len()).map(move |fi| (oi, fi)))
            .collect();
        if all.is_empty() {
            return;
        }
        let at = self.feature.and_then(|f| all.iter().position(|&x| x == f));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => all.len() - 1,
            (Some(i), true) => (i + 1) % all.len(),
            (Some(i), false) => (i + all.len() - 1) % all.len(),
        };
        self.feature = Some(all[next]);
        self.reveal_feature();
    }

    /// Bring the picked feature into view if it is not: centred when it fits,
    /// framed when it does not.
    fn reveal_feature(&mut self) {
        let (w, h) = self.canvas;
        let Some(b) = self.focus_bounds() else { return };
        if w == 0 {
            self.fit_pending = true;
            return;
        }
        let p = self.view.project(w, h);
        let (west, east) = p.lon_range();
        let (south, north) = p.lat_range();
        let fits = b.lon1 - b.lon0 < east - west && b.lat1 - b.lat0 < north - south;
        let (cx, cy) = ((b.lon0 + b.lon1) * 0.5, (b.lat0 + b.lat1) * 0.5);
        let visible = p
            .copies(b.lon0, b.lon1)
            .next()
            .is_some_and(|off| b.lon0 + off >= west && b.lon1 + off <= east)
            && b.lat0 >= south
            && b.lat1 <= north;
        if !fits {
            self.view = MapView::fit(&b, w, h);
        } else if !visible {
            self.view.clon = cx;
            self.view.clat = cy;
            self.view.clamp();
        }
    }

    /// The entries of the object list: "all" first, then each object.
    fn list_len(&self) -> usize {
        self.doc().map_or(0, |d| d.objects.len() + 1)
    }

    fn list_index(&self) -> usize {
        self.object.map_or(0, |o| o + 1)
    }

    fn choose(&mut self, entry: usize) {
        let len = self.doc().map_or(0, |d| d.objects.len());
        self.object = entry.checked_sub(1).filter(|&o| o < len);
        self.feature = None;
        self.fit();
    }

    /// Where the editor should go: the picked feature, else the chosen object.
    fn target(&self) -> Option<usize> {
        let doc = self.doc()?;
        if let Some((oi, fi)) = self.feature {
            return doc.objects.get(oi)?.features.get(fi).map(|f| f.span.start);
        }
        doc.objects.get(self.object.unwrap_or(0)).map(|o| o.span.start)
    }

    fn go_to(&self) -> DialogResult {
        match self.target() {
            Some(at) => DialogResult::Submit(Submit::EditorGotoOffset(at)),
            None => DialogResult::None,
        }
    }

    fn list_shown(&self) -> bool {
        self.doc().is_some_and(|d| d.objects.len() > 1)
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        if self.doc().is_some() {
            if self.editing.is_some() {
                if let Some(r) = self.edit_key(key) {
                    return r;
                }
            } else if key.code == KeyCode::Char('e') {
                self.toggle_editing();
                return DialogResult::None;
            }
        }
        let (w, h) = self.canvas;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(10) => return DialogResult::Cancel,
            KeyCode::Enter | KeyCode::Char('g') => return self.go_to(),
            KeyCode::Tab | KeyCode::BackTab if self.list_shown() => {
                self.focus = if self.focus == Focus::Map { Focus::List } else { Focus::Map };
            }
            KeyCode::Up | KeyCode::Down if self.focus == Focus::List && self.list_shown() => {
                let len = self.list_len();
                let i = self.list_index();
                let next = if key.code == KeyCode::Up {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(len - 1)
                };
                if next != i {
                    self.choose(next);
                }
            }
            KeyCode::Left => self.view.pan(f64::from(w) / 8.0, 0.0, w, h),
            KeyCode::Right => self.view.pan(-f64::from(w) / 8.0, 0.0, w, h),
            KeyCode::Up => self.view.pan(0.0, f64::from(h) / 8.0, w, h),
            KeyCode::Down => self.view.pan(0.0, -f64::from(h) / 8.0, w, h),
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.view.zoom_about(ZOOM_STEP, f64::from(w) / 2.0, f64::from(h) / 2.0, w, h);
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.view.zoom_about(1.0 / ZOOM_STEP, f64::from(w) / 2.0, f64::from(h) / 2.0, w, h);
            }
            KeyCode::Home => self.fit(),
            KeyCode::Char('w') => self.view = MapView { width: 360.0, ..MapView::default() },
            KeyCode::Char('n') | KeyCode::PageDown => self.step_feature(true),
            KeyCode::Char('p') | KeyCode::PageUp => self.step_feature(false),
            KeyCode::Char('N') if shift => self.step_feature(false),
            _ => {}
        }
        DialogResult::None
    }

    /// A left click, as the generic dialog click path delivers one.
    pub(crate) fn handle_click(&mut self, col: u16, row: u16) -> DialogResult {
        let down = |kind| MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE };
        let r = self.handle_mouse(down(MouseEventKind::Down(MouseButton::Left)));
        if !matches!(r, DialogResult::None) {
            return r;
        }
        self.handle_mouse(down(MouseEventKind::Up(MouseButton::Left)))
    }

    /// Every mouse event while the dialog is up: drag to pan, the wheel to
    /// zoom about the pointer, a click to pick a feature, a list entry or a
    /// button — or, while editing, to change the GeoJSON.
    pub(crate) fn handle_mouse(&mut self, ev: MouseEvent) -> DialogResult {
        let r = self.mouse(ev);
        let m = self.map_rect;
        let (col, row) = (ev.column, ev.row);
        let (w, h) = self.canvas;
        if m.width > 0 && col >= m.x && col < m.x + m.width && row >= m.y && row < m.y + m.height {
            let (x, y) = self.canvas_at(col, row);
            if w > 0 {
                self.pointer = Some(self.view.project(w, h).lonlat(x, y));
            }
        }
        r
    }

    /// The canvas position under cell (`col`, `row`): the middle of the cell.
    fn canvas_at(&self, col: u16, row: u16) -> (f64, f64) {
        let (m, (w, h)) = (self.map_rect, self.canvas);
        let x =
            (f64::from(col.saturating_sub(m.x)) + 0.5) * f64::from(w) / f64::from(m.width.max(1));
        let y =
            (f64::from(row.saturating_sub(m.y)) + 0.5) * f64::from(h) / f64::from(m.height.max(1));
        (x, y)
    }

    fn mouse(&mut self, ev: MouseEvent) -> DialogResult {
        let (col, row) = (ev.column, ev.row);
        let inside = |r: Rect| {
            r.width > 0 && col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
        };
        let in_map = inside(self.map_rect);
        let (w, h) = self.canvas;
        // The canvas position under the pointer: the middle of its cell.
        let at = |d: &Self| {
            let m = d.map_rect;
            let x = (f64::from(col.saturating_sub(m.x)) + 0.5) * f64::from(w)
                / f64::from(m.width.max(1));
            let y = (f64::from(row.saturating_sub(m.y)) + 0.5) * f64::from(h)
                / f64::from(m.height.max(1));
            (x, y)
        };
        if let Some(r) = self.edit_mouse(ev, in_map) {
            return r;
        }
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if inside(self.goto_rect) {
                    return self.go_to();
                }
                if inside(self.edit_rect) && self.doc().is_some() {
                    self.toggle_editing();
                    return DialogResult::None;
                }
                if inside(self.close_rect) {
                    return DialogResult::Cancel;
                }
                if let Some(&(_, key)) = self.tools.iter().find(|(r, _)| inside(*r)) {
                    return self.handle_key(key);
                }
                if inside(self.list_rect) {
                    let entry = self.list_top + (row - self.list_rect.y) as usize;
                    if entry < self.list_len() {
                        self.focus = Focus::List;
                        self.choose(entry);
                    }
                } else if in_map {
                    self.focus = Focus::Map;
                    self.drag = Some((col, row, false));
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((px, py, _)) = self.drag {
                    let m = self.map_rect;
                    let dx = f64::from(col) - f64::from(px);
                    let dy = f64::from(row) - f64::from(py);
                    if dx != 0.0 || dy != 0.0 {
                        let sx = f64::from(w) / f64::from(m.width.max(1));
                        let sy = f64::from(h) / f64::from(m.height.max(1));
                        self.view.pan(dx * sx, dy * sy, w, h);
                        self.drag = Some((col, row, true));
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some((_, _, moved)) = self.drag.take()
                    && !moved
                    && in_map
                    && let Some(doc) = self.doc()
                {
                    let (x, y) = at(self);
                    let scene = Scene {
                        view: self.view,
                        doc,
                        object: self.object,
                        selected: self.feature,
                        overlay: None,
                    };
                    // Within about a cell of the pointer.
                    let reach = (f64::from(w) / f64::from(self.map_rect.width.max(1))) as f32 * 1.5;
                    self.feature = draw::hit(&scene, w, h, x as f32, y as f32, reach);
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if in_map => {
                let (x, y) = at(self);
                let factor =
                    if ev.kind == MouseEventKind::ScrollUp { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
                self.view.zoom_about(factor, x, y, w, h);
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside(self.list_rect) => {
                let max_top = self.list_len().saturating_sub(self.list_rect.height as usize);
                self.list_top = if ev.kind == MouseEventKind::ScrollUp {
                    self.list_top.saturating_sub(3)
                } else {
                    (self.list_top + 3).min(max_top)
                };
            }
            _ => {}
        }
        DialogResult::None
    }

    pub(crate) fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        mut gfx: Option<&mut Gfx>,
    ) {
        let rect = centered(area, area.width.saturating_sub(4), area.height.saturating_sub(2));
        draw_shadow(f, rect, theme);
        f.render_widget(Clear, rect);
        let title = format!("{} — {}", crate::l10n::trd("GeoJSON map"), self.name);
        let block = dialog_block(&ellipsize(&title, rect.width.saturating_sub(6) as usize), theme);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        if inner.height < 5 || inner.width < 20 {
            return;
        }
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let info = Rect { height: 1, ..inner };
        let buttons = Rect { y: inner.y + inner.height - 1, height: 1, ..inner };
        let props = Rect { y: buttons.y - 1, height: 1, ..inner };
        // Editing puts its tools on a row of their own, when there is room.
        let tool_row = self.editing.is_some() && inner.height >= 7;
        let tools = Rect { y: props.y.saturating_sub(1), height: 1, ..inner };
        // Sixel can put an image a row lower than asked; the spacer keeps it
        // off the properties row.
        let low = gfx.as_ref().is_some_and(|g| g.available() && g.may_land_low());
        let body_h = inner.height - 3 - u16::from(low) - u16::from(tool_row);
        let body = Rect { y: inner.y + 1, height: body_h, ..inner };

        let list_w = if self.list_shown() { LIST_WIDTH.min(inner.width / 3) } else { 0 };
        self.list_rect = Rect { width: list_w, ..body };
        let gap = u16::from(list_w > 0);
        self.map_rect = Rect { x: body.x + list_w + gap, width: body.width - list_w - gap, ..body };

        // The canvas the view is worked out on, and how the mouse maps to it:
        // true pixels where the terminal can draw them, braille dots otherwise.
        let graphics = gfx.as_ref().is_some_and(|g| g.available());
        let (cw, ch) = match gfx.as_ref() {
            Some(g) if graphics => {
                let (pw, ph) = g.px_size(self.map_rect);
                let scale = (f64::from(MAP_MAX_PX) / f64::from(pw.max(ph).max(1))).min(1.0);
                (((f64::from(pw) * scale) as u32).max(1), ((f64::from(ph) * scale) as u32).max(1))
            }
            _ => (u32::from(self.map_rect.width) * 2, u32::from(self.map_rect.height) * 4),
        };
        self.canvas = (cw, ch);
        if self.fit_pending && self.doc().is_some() {
            self.fit();
        }
        let overlay = self.overlay();

        match &self.state {
            State::Loading(_) => {
                let msg = crate::l10n::trd("Reading…");
                let row = Rect { y: body.y + body.height / 2, height: 1, ..body };
                f.render_widget(
                    Paragraph::new(msg).style(base).alignment(ratatui::layout::Alignment::Center),
                    row,
                );
            }
            State::Ready { doc, generation } => {
                let scene = Scene {
                    view: self.view,
                    doc,
                    object: self.object,
                    selected: self.feature,
                    overlay: overlay.as_ref(),
                };
                match gfx.as_deref_mut() {
                    Some(g) if graphics => {
                        let pal = MapPalette::from_theme(theme);
                        let label_px = (g.cell().1 as f32 * 0.8 * cw as f32
                            / g.px_size(self.map_rect).0.max(1) as f32)
                            .max(9.0);
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        (self.view.sig(), cw, ch, pal, self.object, self.feature, generation)
                            .hash(&mut hasher);
                        (self.revision, overlay.as_ref().map(draw::Overlay::sig)).hash(&mut hasher);
                        label_px.to_bits().hash(&mut hasher);
                        let sig = hasher.finish();
                        g.draw_cached_scaled(f, self.map_rect, Slot::GeoMap, sig, || {
                            draw::raster(cw, ch, &scene, &pal, label_px)
                        });
                    }
                    _ => crate::geo::cells::render(
                        f,
                        self.map_rect,
                        &scene,
                        &CellPalette::from_theme(theme),
                    ),
                }
                crate::ui::gradient::mark_painted(self.map_rect);
            }
        }

        self.render_info(f, info, theme);
        if list_w > 0 {
            self.render_list(f, theme);
        }
        self.tools.clear();
        if tool_row {
            self.render_tools(f, tools, theme);
        }
        if self.editing.is_some() {
            self.render_edit_row(f, props, theme);
        } else {
            self.render_props(f, props, theme);
        }
        if low {
            f.render_widget(
                Paragraph::new("").style(base),
                Rect { y: body.y + body.height, height: 1, ..inner },
            );
        }

        // Go to, Edit features (Stop editing while it is on) and Close.
        let editing = self.editing.is_some();
        let labels = [
            crate::l10n::trd("Go to"),
            crate::l10n::trd(if editing { "Stop editing" } else { "Edit features" }),
            crate::l10n::trd("Close"),
        ];
        let widths: Vec<u16> = labels
            .iter()
            .map(|l| unicode_width::UnicodeWidthStr::width(l.as_str()) as u16 + 6)
            .collect();
        let total = widths.iter().sum::<u16>() + 4;
        let mut x = buttons.x + buttons.width.saturating_sub(total) / 2;
        let mut rects = [Rect::default(); 3];
        for (r, &bw) in rects.iter_mut().zip(&widths) {
            let right = buttons.x + buttons.width;
            *r = Rect { x: x.min(right), width: bw.min(right.saturating_sub(x)), ..buttons };
            x += bw + 2;
        }
        [self.goto_rect, self.edit_rect, self.close_rect] = rects;
        let focused = |i: usize| if editing { i == 1 } else { i == 0 };
        let mut drawn = total <= buttons.width
            && all_renderable(&[labels[0].as_str(), labels[1].as_str(), labels[2].as_str()]);
        for (i, (r, label)) in rects.into_iter().zip(&labels).enumerate() {
            if drawn {
                drawn = gfx_button(
                    f,
                    gfx.as_deref_mut(),
                    Slot::Button(i as u16),
                    r,
                    label,
                    focused(i),
                    theme,
                );
            }
        }
        if !drawn {
            let mut spans = Vec::new();
            for (i, label) in labels.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled("  ", base));
                }
                spans.push(button(&format!("[ {label} ]"), focused(i), theme));
            }
            let line = Line::from(spans);
            let used = unicode_width::UnicodeWidthStr::width(line.to_string().as_str()) as u16;
            let x0 = buttons.x + buttons.width.saturating_sub(used) / 2;
            // The text buttons are narrower than the pictures: hit-test them
            // where they are.
            let mut at = x0;
            let mut hits = [Rect::default(); 3];
            for (i, label) in labels.iter().enumerate() {
                let bw =
                    unicode_width::UnicodeWidthStr::width(format!("[ {label} ]").as_str()) as u16;
                hits[i] = Rect { x: at, width: bw, ..buttons };
                at += bw + 2;
            }
            [self.goto_rect, self.edit_rect, self.close_rect] = hits;
            f.render_widget(
                Paragraph::new(line).style(base).alignment(ratatui::layout::Alignment::Center),
                buttons,
            );
        }
    }

    /// The top row: what is shown, how much of it, and where the pointer is.
    fn render_info(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let dim = base.fg(theme.panel_border);
        let mut spans = vec![Span::styled(" ", base)];
        if let Some(doc) = self.doc().filter(|d| d.objects.is_empty()) {
            spans.push(Span::styled(crate::l10n::trd("No GeoJSON found in this file"), dim));
            if doc.errors > 0 {
                spans.push(Span::styled(
                    format!("  {} {}", doc.errors, crate::l10n::trd("syntax errors")),
                    base.fg(theme.error_fg),
                ));
            }
        } else if let Some(doc) = self.doc() {
            let (what, count) = match self.object.and_then(|o| doc.objects.get(o)) {
                Some(o) => (format!("{}  {}", o.path, o.kind), o.features.len()),
                None => (
                    format!("{} {}", doc.objects.len(), crate::l10n::trd("objects")),
                    doc.objects.iter().map(|o| o.features.len()).sum(),
                ),
            };
            spans.push(Span::styled(what, base.add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(format!("  {count} {}", crate::l10n::trd("features")), dim));
            if doc.skipped > 0 {
                spans.push(Span::styled(
                    format!("  {} {}", doc.skipped, crate::l10n::trd("positions skipped")),
                    base.fg(theme.error_fg),
                ));
            }
            if doc.errors > 0 {
                spans.push(Span::styled(
                    format!("  {} {}", doc.errors, crate::l10n::trd("syntax errors")),
                    base.fg(theme.error_fg),
                ));
            }
        }
        // Where a feature being drawn will go.
        if let Some(target) = self.draw_target() {
            spans.push(Span::styled(format!("  → {target}"), base.fg(theme.hotkey_fg)));
        }
        let used: usize =
            spans.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum();
        if let Some((lon, lat)) = self.pointer {
            let ns = if lat >= 0.0 { 'N' } else { 'S' };
            let ew = if lon >= 0.0 { 'E' } else { 'W' };
            let text = format!("{:.4}°{ns} {:.4}°{ew} ", lat.abs(), lon.abs());
            let pad = (area.width as usize).saturating_sub(used + text.chars().count());
            spans.push(Span::styled(" ".repeat(pad), base));
            spans.push(Span::styled(text, dim));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(base), area);
    }

    /// The object list: "all objects", then each object's path, type and size.
    fn render_list(&mut self, f: &mut Frame, theme: &Theme) {
        let area = self.list_rect;
        let selected = self.list_index();
        let height = area.height as usize;
        self.list_top = crate::util::scroll::scroll_to_visible(self.list_top, selected, height);
        let Some(doc) = self.doc() else { return };
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let dim = base.fg(theme.panel_border);
        let width = area.width as usize;
        let mut lines = Vec::with_capacity(height);
        let total: usize = doc.objects.iter().map(|o| o.features.len()).sum();
        let entries = std::iter::once((crate::l10n::trd("All objects"), String::new(), total))
            .chain(doc.objects.iter().map(|o| (o.path.clone(), o.kind.clone(), o.features.len())));
        for (i, (label, kind, count)) in entries.enumerate().skip(self.list_top).take(height) {
            let style = if i == selected {
                if self.focus == Focus::List {
                    theme.dialog_selection
                } else {
                    theme.cursor_inactive
                }
            } else {
                base
            };
            let quiet = if i == selected { style } else { dim };
            // The path, its type after it where there is room, and the count
            // at the right.
            let count = format!(" {count}");
            let room = width.saturating_sub(count.len());
            let name = ellipsize(&label, room);
            let name_w = unicode_width::UnicodeWidthStr::width(name.as_str());
            let kind = if kind.is_empty() {
                String::new()
            } else {
                ellipsize(&format!(" {kind}"), room.saturating_sub(name_w))
            };
            let pad =
                room.saturating_sub(name_w + unicode_width::UnicodeWidthStr::width(kind.as_str()));
            lines.push(Line::from(vec![
                Span::styled(name, style),
                Span::styled(kind, quiet),
                Span::styled(" ".repeat(pad), style),
                Span::styled(count, quiet),
            ]));
        }
        f.render_widget(Paragraph::new(lines).style(base), area);
    }

    /// The picked feature's name and properties, as far as the row goes.
    fn render_props(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let dim = base.fg(theme.panel_border);
        let feature =
            self.feature.and_then(|(oi, fi)| self.doc()?.objects.get(oi)?.features.get(fi));
        let line = match feature {
            Some(ft) => {
                let mut spans = vec![Span::styled(" ", base)];
                if let Some(name) = &ft.name {
                    spans
                        .push(Span::styled(format!("{name}  "), base.add_modifier(Modifier::BOLD)));
                }
                for (k, v) in &ft.props {
                    spans.push(Span::styled(format!("{k}: "), dim));
                    spans.push(Span::styled(format!("{v}  "), base));
                }
                Line::from(spans)
            }
            None => Line::from(Span::styled(
                format!(
                    " {}",
                    crate::l10n::trd(
                        "Drag to pan, wheel or +/- to zoom, click a feature, n/p to step through them"
                    )
                ),
                dim,
            )),
        };
        f.render_widget(Paragraph::new(line).style(base), area);
    }
}

/// The box around every feature of `doc` — for the tests.
#[cfg(test)]
fn everything(doc: &GeoDoc) -> Option<Bounds> {
    crate::geo::geojson::union(doc.objects.iter().filter_map(|o| o.bounds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::geojson::extract;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    const PLACES: &str = r#"{"parks": {"type":"FeatureCollection","features":[
                {"type":"Feature","properties":{"name":"Prater","area":6},"geometry":{"type":"Polygon","coordinates":[[[16.39,48.2],[16.45,48.2],[16.45,48.22],[16.39,48.22],[16.39,48.2]]]}},
                {"type":"Feature","properties":{"name":"Gate"},"geometry":{"type":"Point","coordinates":[16.37,48.21]}}]},
             "office": {"type":"Point","coordinates":[-0.12,51.5]}}"#;

    fn doc() -> GeoDoc {
        extract(PLACES)
    }

    fn ready(cursor: usize) -> GeoMapDialog {
        let mut d = GeoMapDialog::loading("places.json", 7, cursor, Rope::from_str(PLACES));
        assert!(d.awaits(7) && !d.awaits(8));
        d.set_doc(doc());
        d
    }

    pub(super) fn draw(d: &mut GeoMapDialog, w: u16, h: u16) -> Vec<String> {
        let theme = Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| d.render(f, f.area(), &theme, None)).unwrap();
        let b = t.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| b[(x, y)].symbol().to_string()).collect()).collect()
    }

    pub(super) fn key(d: &mut GeoMapDialog, code: KeyCode) -> DialogResult {
        d.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    pub(super) fn mouse(
        d: &mut GeoMapDialog,
        kind: MouseEventKind,
        col: u16,
        row: u16,
    ) -> DialogResult {
        d.handle_mouse(MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE })
    }

    #[test]
    fn it_opens_on_the_object_the_cursor_is_in_and_frames_it() {
        let mut d = ready(PLACES.find("\"Point\",\"coordinates\":[-0.12").unwrap());
        assert_eq!(d.object, Some(1), "the cursor is in the office point");
        let rows = draw(&mut d, 100, 30);
        assert!(rows.iter().any(|r| r.contains("GeoJSON map")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("$.office")), "the list names the objects");
        let (w, h) = d.canvas;
        let p = d.view.project(w, h);
        let (west, east) = p.lon_range();
        assert!(west < -0.12 && east > -0.12, "the view frames London: {:?}", d.view);
        // Home over all objects frames Vienna and London together.
        assert_eq!(d.feature, Some((1, 0)), "and on the feature it is in");
        d.object = None;
        d.feature = None;
        key(&mut d, KeyCode::Home);
        let b = everything(d.doc().unwrap()).unwrap();
        let p = d.view.project(w, h);
        assert!(p.lon_range().0 < b.lon0 && p.lon_range().1 > b.lon1);
    }

    #[test]
    fn keys_zoom_pan_step_and_go_to_the_feature() {
        let mut d = ready(0);
        draw(&mut d, 100, 30);
        let width = d.view.width;
        key(&mut d, KeyCode::Char('+'));
        assert!(d.view.width < width);
        let clon = d.view.clon;
        key(&mut d, KeyCode::Right);
        assert!(d.view.clon > clon);
        key(&mut d, KeyCode::Char('n'));
        assert_eq!(d.feature, Some((0, 0)));
        let DialogResult::Submit(Submit::EditorGotoOffset(at)) = key(&mut d, KeyCode::Enter) else {
            panic!("Enter goes to the feature");
        };
        let text = r#"{"parks": {"type":"FeatureCollection","features":["#;
        assert!(at > text.len(), "the first feature, inside the collection: {at}");
        assert!(matches!(key(&mut d, KeyCode::Esc), DialogResult::Cancel));
    }

    #[test]
    fn a_drag_pans_the_wheel_zooms_about_the_pointer_and_a_click_picks() {
        let mut d = ready(0);
        draw(&mut d, 100, 30);
        let m = d.map_rect;
        let (cx, cy) = (m.x + m.width / 2, m.y + m.height / 2);
        let before = d.view;
        mouse(&mut d, MouseEventKind::Down(MouseButton::Left), cx, cy);
        assert!(d.panning());
        mouse(&mut d, MouseEventKind::Drag(MouseButton::Left), cx + 5, cy);
        mouse(&mut d, MouseEventKind::Up(MouseButton::Left), cx + 5, cy);
        assert!(!d.panning());
        assert!(d.view.clon < before.clon, "dragging right moves the map right");
        // The wheel keeps what is under the pointer there.
        let (w, h) = d.canvas;
        let px = |d: &GeoMapDialog, col: u16, row: u16| {
            let x = (f64::from(col - m.x) + 0.5) * f64::from(w) / f64::from(m.width);
            let y = (f64::from(row - m.y) + 0.5) * f64::from(h) / f64::from(m.height);
            d.view.project(w, h).lonlat(x, y)
        };
        let (col, row) = (m.x + 10, m.y + 4);
        let under = px(&d, col, row);
        mouse(&mut d, MouseEventKind::ScrollUp, col, row);
        let after = px(&d, col, row);
        assert!((under.0 - after.0).abs() < 1e-9 && (under.1 - after.1).abs() < 1e-9);
        // A click on the gate point picks it.
        key(&mut d, KeyCode::Home);
        let p = d.view.project(w, h);
        let (x, y) = p.xy(16.37, 48.21);
        let col = m.x + (f64::from(x) * f64::from(m.width) / f64::from(w)) as u16;
        let row = m.y + (f64::from(y) * f64::from(m.height) / f64::from(h)) as u16;
        mouse(&mut d, MouseEventKind::Down(MouseButton::Left), col, row);
        mouse(&mut d, MouseEventKind::Up(MouseButton::Left), col, row);
        assert!(matches!(d.feature, Some((0, _))), "{:?}", d.feature);
        // The buttons answer clicks.
        let r = d.close_rect;
        assert!(matches!(
            mouse(&mut d, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y),
            DialogResult::Cancel
        ));
    }

    #[test]
    fn it_draws_at_any_size_and_while_loading() {
        let mut loading = GeoMapDialog::loading("x.json", 1, 0, Rope::new());
        let rows = draw(&mut loading, 60, 20);
        assert!(rows.iter().any(|r| r.contains("Reading")));
        for (w, h) in [(10, 4), (24, 8), (40, 12), (160, 50)] {
            let mut d = ready(0);
            draw(&mut d, w, h);
        }
        let mut d = ready(0);
        let theme = Theme::mc();
        let mut gfx = crate::ui::graphics::Gfx::test_halfblocks();
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(|f| d.render(f, f.area(), &theme, Some(&mut gfx))).unwrap();
    }
}
