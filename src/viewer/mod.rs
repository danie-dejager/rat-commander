//! Internal file viewer with text and hex modes, wrap toggle, and search.
//!
//! The content is exposed through a [`Source`] that is either a small in-memory
//! buffer or a **paged file on disk** — the latter never loads the whole file
//! into memory, reading only the bytes needed to render the current page or to
//! advance a search. Only a per-line offset index is kept (8 bytes per line).
//! Scrolling is by logical line (text) or 16-byte row (hex).

pub mod fingerprint;
pub mod loglevel;
pub mod markdown;
pub mod render;
pub mod search;

use crate::space3d::CamPose;
use crate::syntax::{ColorRun, Highlighter};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Maximum bytes read into an *in-memory* viewer (larger in-memory buffers are
/// truncated with a note). File-backed sources are paged and never truncated.
pub const MAX_VIEW_BYTES: usize = 64 * 1024 * 1024;

/// Most lines a single "Find all" will mark in the viewer. It pages files far
/// larger than the editor ever opens, so the sweep is bounded rather than
/// promising to mark every hit in a multi-gigabyte log.
const FOUND_LINES_MAX: usize = 50_000;

/// Most bytes one follow-mode poll indexes. A log that jumps by gigabytes
/// between two ticks is caught up over several of them rather than stalling
/// the frame that noticed.
const FOLLOW_SCAN_BUDGET: usize = 4 * 1024 * 1024;

/// Orbit and zoom rates for the model view, matching the 3D panel's.
const ORBIT_YAW: f32 = 0.16;
const ORBIT_PITCH: f32 = 0.10;
const ZOOM_IN: f32 = 0.85;
const ZOOM_OUT: f32 = 1.18;

/// Where the viewer reads bytes from.
enum Source {
    /// Small content held in memory (help text, remote-less small files).
    Mem(Vec<u8>),
    /// A seekable local file, read on demand (never fully loaded).
    File { file: RefCell<File>, len: usize },
}

impl Source {
    fn len(&self) -> usize {
        match self {
            Source::Mem(d) => d.len(),
            Source::File { len, .. } => *len,
        }
    }

    /// Read bytes `[start, end)` (clamped to the source length). Short/failed
    /// reads return what was obtained; callers tolerate partial results.
    fn read_range(&self, start: usize, end: usize) -> Vec<u8> {
        let end = end.min(self.len());
        if start >= end {
            return Vec::new();
        }
        match self {
            Source::Mem(d) => d[start..end].to_vec(),
            Source::File { file, .. } => {
                let mut f = file.borrow_mut();
                if f.seek(SeekFrom::Start(start as u64)).is_err() {
                    return Vec::new();
                }
                let mut buf = vec![0u8; end - start];
                let mut read = 0;
                while read < buf.len() {
                    match f.read(&mut buf[read..]) {
                        Ok(0) => break,
                        Ok(n) => read += n,
                        Err(_) => break,
                    }
                }
                buf.truncate(read);
                buf
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Text,
    Hex,
    /// The whole file as one picture — see [`fingerprint`].
    Map,
}

/// A decoded image shown fullscreen when F3 opens a supported image file. Falls
/// back to the raw text/hex view when a file can't be decoded, or via F8.
pub struct ViewerImage {
    /// The image scaled down for display (aspect preserved).
    pub img: image::RgbaImage,
    /// Cheap content signature for the graphics cache.
    pub sig: u64,
    /// Original pixel dimensions (before scaling), shown in the header.
    pub orig: (u32, u32),
}

/// A parsed mesh shown fullscreen when F3 opens a model file, with the orbit
/// camera looking at it. Falls back to the raw text/hex view when the file
/// cannot be parsed, or via F8 — exactly as [`ViewerImage`] does.
pub struct ViewerModel {
    pub mesh: crate::mesh::Mesh,
    /// Where the camera sits.
    ///
    /// Reuses the 3D panel's orbit rig so both surfaces answer the same keys
    /// with the same geometry. The exponential smoothing that view applies lives
    /// in `Space3d` rather than in `CamPose`, and is deliberately not brought
    /// along: the viewer redraws on input, not on a frame clock, so there would
    /// be no ticks to interpolate over.
    pub cam: CamPose,
    /// Distance that exactly frames the model. Zoom is stored as a multiple of
    /// it, so framing survives a terminal resize the way `Space3d`'s does.
    fitted: f32,
    /// Identity of the mesh itself, mixed into [`ViewerModel::sig`] so the
    /// graphics cache re-encodes for a different model and not merely for a
    /// moved camera.
    mesh_sig: u64,
}

/// Pitch limits. Unlike the 3D panel — which clamps above the ground plane
/// because its scene stands on one — a model has no floor, so the only limit
/// here is staying off the poles, where the camera basis degenerates.
const MODEL_PITCH: f32 = 1.50;
/// Zoom range, as multiples of the fitted distance.
const MODEL_ZOOM: (f32, f32) = (0.15, 8.0);

impl ViewerModel {
    pub fn new(mesh: crate::mesh::Mesh) -> Self {
        let mesh_sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            mesh.tris.len().hash(&mut h);
            // A bounded sample rather than every triangle: this runs on a mesh
            // of up to `MAX_TRIS`, and it only has to distinguish one opened
            // model from another, not verify one.
            for t in mesh.tris.iter().step_by(1 + mesh.tris.len() / 64) {
                for v in t.v {
                    v.x.to_bits().hash(&mut h);
                    v.y.to_bits().hash(&mut h);
                    v.z.to_bits().hash(&mut h);
                }
            }
            h.finish()
        };
        // Frame the bounding sphere in the vertical field of view, with a margin
        // so the silhouette does not touch the edge of the raster.
        let fitted = mesh.radius() / (crate::space3d::raster3d::FOV_Y * 0.5).sin() * 1.15;
        let cam = CamPose { target: mesh.centre(), dist: fitted, yaw: 0.6, pitch: 0.45 };
        ViewerModel { mesh, cam, fitted, mesh_sig }
    }

    pub fn orbit(&mut self, dyaw: f32, dpitch: f32) {
        self.cam.yaw += dyaw;
        self.cam.pitch = (self.cam.pitch + dpitch).clamp(-MODEL_PITCH, MODEL_PITCH);
    }

    /// Zoom by a factor; `k` below 1 moves closer.
    pub fn zoom_by(&mut self, k: f32) {
        let (lo, hi) = MODEL_ZOOM;
        self.cam.dist = (self.cam.dist * k).clamp(self.fitted * lo, self.fitted * hi);
    }

    pub fn reset(&mut self) {
        self.cam = CamPose { target: self.mesh.centre(), dist: self.fitted, yaw: 0.6, pitch: 0.45 };
    }

    /// Content signature for the graphics cache.
    ///
    /// The camera is quantised before hashing so that sub-pixel jitter does not
    /// invalidate a perfectly good encoded image — the same reasoning as the 3D
    /// panel's own `signature()`.
    pub fn sig(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.mesh_sig.hash(&mut h);
        ((self.cam.yaw * 512.0) as i32).hash(&mut h);
        ((self.cam.pitch * 512.0) as i32).hash(&mut h);
        ((self.cam.dist / self.fitted * 512.0) as i32).hash(&mut h);
        h.finish()
    }
}

/// How the "Goto" dialog interprets its entered value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotoMode {
    /// 1-based line number (text) or 16-byte row (hex).
    Line,
    /// Percentage through the file.
    Percent,
    /// Byte offset, entered in decimal.
    DecimalOffset,
    /// Byte offset, entered in hexadecimal.
    HexOffset,
}

/// Result of handling a key: whether the viewer should stay open.
/// A search as the dialog set it up. Repeating an identical one resumes from the
/// last hit; changing any field restarts from the top.
#[derive(Default, Clone, PartialEq, Eq)]
struct ViewSearch {
    query: String,
    regex: bool,
    case_sensitive: bool,
    whole_words: bool,
    backwards: bool,
    hex: bool,
}

pub enum ViewerSignal {
    Stay,
    Close,
    /// Ask the app to open the modal "Goto" dialog (F5).
    OpenGoto,
    /// Ask the app to open the modal search dialog (F7) — the same one the
    /// editor uses, so the viewer offers the same modes and options.
    OpenSearch,
    /// Ask the app to open the embedded user manual (F1), like the panel F1.
    OpenHelp,
    /// `b`: ask the app to run `git blame` on the file in the background.
    StartBlame,
    /// Enter on a blamed line: show the tree as it was at that line's commit.
    OpenBlameCommit,
}

/// Follow mode (`f`): the `tail -f` of the viewer.
struct Follow {
    /// The view was scrolled away from the end, so new lines are counted
    /// rather than scrolled to. Reaching the end again resumes.
    paused: bool,
    /// Lines that arrived while paused.
    new_lines: usize,
    /// Identity (device, inode) of the file being paged, so a log rotated out
    /// from under the viewer is noticed and the new file at the path picked up.
    id: Option<(u64, u64)>,
}

/// `b`: the blame column beside the text, and the line cursor it brings.
enum BlameView {
    /// Asked for; the app's background `git blame` answers with this generation.
    Loading(u64),
    Ready {
        blame: crate::git::blame::Blame,
        cursor: usize,
    },
}

pub struct ViewerState {
    pub name: String,
    /// The local file this viewer pages, when it is one — not a temp copy of a
    /// remote or archived file, and not in-memory text. Follow mode reopens a
    /// rotated log through it.
    path: Option<PathBuf>,
    follow: Option<Follow>,
    blame: Option<BlameView>,
    src: Source,
    truncated: bool,
    /// A temp file to delete when the viewer closes (a fetched remote file).
    temp: Option<PathBuf>,
    /// Incremental syntax highlighter (text mode only), when a syntax matched.
    hl: Option<Highlighter>,
    /// Byte offset of the start of each text line. Built lazily: only the lines
    /// within the first [`scanned`](Self::scanned) bytes are present until more
    /// is needed (scrolling, search, goto), so huge files open instantly.
    line_starts: Vec<usize>,
    /// Bytes scanned so far for `line_starts`; every newline in `[0, scanned)`
    /// has been recorded. Equals the file length once fully indexed.
    scanned: usize,
    pub mode: ViewMode,
    /// The file looks like Markdown (by extension), so the Markdown render mode
    /// and its F8 Raw/Render toggle are offered.
    is_markdown: bool,
    /// In text mode, whether to draw the Markdown approximation (true) or the raw
    /// text with syntax highlighting (false). Only meaningful when `is_markdown`.
    markdown_render: bool,
    pub wrap: bool,
    /// Top visible logical line (text) or top 16-byte row (hex).
    top: usize,
    /// Horizontal scroll (text, non-wrap).
    h_offset: usize,
    /// The search being repeated, exactly as the dialog set it up. `find_next`
    /// resumes from `last_match` while this is unchanged, so pressing F7-Enter on
    /// the same term walks the file; changing any option restarts from the top.
    search: ViewSearch,
    /// Term the search dialog opens pre-filled with — the last committed search,
    /// seeded from the app-wide memory so it survives across files/reopenings.
    search_seed: String,
    /// Lines holding a match, from the dialog's "Find all" (the same button the
    /// editor has). Kept until the next Find all or until the viewer closes.
    found_lines: std::collections::HashSet<usize>,
    /// Byte offset of the last match (for "find next").
    last_match: Option<usize>,
    /// Viewport size, updated by the renderer each frame.
    view_rows: usize,
    view_cols: usize,
    /// Content body and footer (F-key bar) rects, recorded by the renderer for
    /// mouse hit-testing.
    content_area: Rect,
    footer_area: Rect,
    /// Cached document outline (headings), built lazily on the first F6 press.
    outline: Option<Vec<markdown::OutlineItem>>,
    /// Whether the F6 outline navigator overlay is currently shown.
    outline_open: bool,
    /// Selected entry in the outline list.
    outline_sel: usize,
    /// First visible entry (scroll offset) of the outline list; kept in sync by
    /// the renderer so the selection stays on screen.
    outline_top: usize,
    /// Interior rect of the outline list, recorded by the renderer for mouse hits.
    outline_area: Rect,
    /// A decoded image, when this file could be shown as one (F3 on an image).
    image: Option<ViewerImage>,
    /// Whether the image (vs. the raw text/hex) is currently displayed — toggled
    /// with F8. Only meaningful when `image` is set.
    show_image: bool,
    /// A parsed mesh, when this file could be read as one (F3 on a model).
    model: Option<ViewerModel>,
    /// Whether the model (vs. the raw text/hex) is currently displayed —
    /// toggled with F8, like `show_image`.
    show_model: bool,
    /// Pointer position the in-progress model drag was last seen at, so an
    /// orbit is driven by the delta between frames rather than by absolutes.
    drag_from: Option<(u16, u16)>,
    /// The byte map, built the first time `Map` mode is entered and kept after.
    map: Option<fingerprint::Fingerprint>,
    /// Highlighted cell of the map, and what `Enter` jumps the hex view to.
    map_cell: usize,
    /// Whether the map is coloured by byte class rather than by density.
    map_by_class: bool,
    /// Cells per row, recorded by the renderer so cursor keys move by a row.
    pub(crate) map_cols: usize,
}

impl ViewerState {
    /// An in-memory viewer (help text, or already-loaded small content).
    pub fn new(name: String, mut data: Vec<u8>) -> Self {
        let truncated = data.len() > MAX_VIEW_BYTES;
        if truncated {
            data.truncate(MAX_VIEW_BYTES);
        }
        let line_starts = compute_line_starts(&data);
        let scanned = data.len();
        let is_markdown = is_markdown_name(&name);
        ViewerState {
            name,
            path: None,
            follow: None,
            blame: None,
            src: Source::Mem(data),
            truncated,
            temp: None,
            hl: None,
            line_starts,
            scanned,
            mode: ViewMode::Text,
            is_markdown,
            markdown_render: is_markdown,
            wrap: false,
            top: 0,
            h_offset: 0,
            search: ViewSearch::default(),
            search_seed: String::new(),
            found_lines: std::collections::HashSet::new(),
            last_match: None,
            view_rows: 1,
            view_cols: 1,
            content_area: Rect::default(),
            footer_area: Rect::default(),
            outline: None,
            outline_open: false,
            outline_sel: 0,
            outline_top: 0,
            outline_area: Rect::default(),
            image: None,
            show_image: false,
            model: None,
            show_model: false,
            drag_from: None,
            map: None,
            map_cell: 0,
            map_by_class: false,
            map_cols: 64,
        }
    }

    /// A file-backed (paged) viewer from an already-scanned file (built on the
    /// main thread so the blocking scan can run off-thread). When `temp` is set,
    /// that file is deleted on close.
    pub fn from_scanned(
        name: String,
        file: File,
        len: usize,
        line_starts: Vec<usize>,
        scanned: usize,
        temp: Option<PathBuf>,
    ) -> Self {
        let is_markdown = is_markdown_name(&name);
        ViewerState {
            name,
            path: None,
            follow: None,
            blame: None,
            src: Source::File { file: RefCell::new(file), len },
            truncated: false,
            temp,
            hl: None,
            line_starts,
            scanned,
            mode: ViewMode::Text,
            is_markdown,
            markdown_render: is_markdown,
            wrap: false,
            top: 0,
            h_offset: 0,
            search: ViewSearch::default(),
            search_seed: String::new(),
            found_lines: std::collections::HashSet::new(),
            last_match: None,
            view_rows: 1,
            view_cols: 1,
            content_area: Rect::default(),
            footer_area: Rect::default(),
            outline: None,
            outline_open: false,
            outline_sel: 0,
            outline_top: 0,
            outline_area: Rect::default(),
            image: None,
            show_image: false,
            model: None,
            show_model: false,
            drag_from: None,
            map: None,
            map_cell: 0,
            map_by_class: false,
            map_cols: 64,
        }
    }

    /// Convenience: open + scan a file in one call (used by tests).
    #[cfg(test)]
    pub fn open_file(name: String, path: PathBuf, temp: Option<PathBuf>) -> std::io::Result<Self> {
        let (file, len, line_starts, scanned) = scan_file(&path)?;
        Ok(Self::from_scanned(name, file, len, line_starts, scanned, temp))
    }

    /// Turn on syntax highlighting if a syntax matches the file name and the
    /// content is within the size cap. `dark` selects a fitting bundled theme.
    pub fn enable_syntax(&mut self, dark: bool) {
        if self.src.len() <= crate::syntax::HL_MAX_BYTES {
            self.hl = Highlighter::for_file(&self.name, dark);
        }
    }

    /// Record the local file this viewer is paging (see [`ViewerState::path`]).
    pub fn set_local_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// Whether follow mode is on, so the app keeps its tick running to poll.
    pub fn following(&self) -> bool {
        self.follow.is_some()
    }

    /// Follow mode's state for the header: `None` when off, else whether it is
    /// paused and how many lines arrived since.
    pub(crate) fn follow_status(&self) -> Option<(bool, usize)> {
        self.follow.as_ref().map(|f| (f.paused, f.new_lines))
    }

    /// `f`: start or stop following the file as it grows. Only a local file can
    /// be followed — a temp copy of a remote one would never change — and
    /// starting jumps to the end, as `End` does.
    fn toggle_follow(&mut self) {
        if self.follow.take().is_some() {
            return;
        }
        let Source::File { file, .. } = &self.src else { return };
        if self.path.is_none() {
            return;
        }
        let id = file.borrow().metadata().ok().and_then(|m| file_id(&m));
        self.follow = Some(Follow { paused: false, new_lines: 0, id });
        if self.mode == ViewMode::Text {
            self.index_fully();
        }
        if self.mode != ViewMode::Map {
            self.top = self.max_top();
        }
    }

    /// Follow mode's heartbeat, called on the app's ~100 ms tick: take in bytes
    /// appended since the last look, start over on a file that was truncated or
    /// rotated away, and keep the view on the last page unless it was scrolled
    /// off it. Returns whether anything changed. A cheap `fstat` when nothing did.
    pub fn poll_follow(&mut self) -> bool {
        let Some(follow) = self.follow.as_ref() else { return false };
        let Some(path) = self.path.clone() else { return false };
        let Source::File { file, len } = &self.src else { return false };
        let old_len = *len;

        // A rotated log: the name now belongs to another file. Page that one,
        // from its start, the way `tail -F` does. A path that is briefly missing
        // (moved away, not yet recreated) keeps the old handle.
        if let Some(known) = follow.id
            && std::fs::metadata(&path).ok().and_then(|m| file_id(&m)).is_some_and(|id| id != known)
            && let Ok(f) = File::open(&path)
        {
            let meta = f.metadata().ok();
            let len = meta.as_ref().map_or(0, |m| m.len() as usize);
            let id = meta.as_ref().and_then(file_id);
            self.src = Source::File { file: RefCell::new(f), len };
            self.restart();
            if let Some(fo) = self.follow.as_mut() {
                fo.id = id;
            }
            self.catch_up(0);
            return true;
        }

        let now = file.borrow().metadata().map_or(old_len, |m| m.len() as usize);
        if now == old_len {
            return false;
        }
        if let Source::File { len, .. } = &mut self.src {
            *len = now;
        }
        if now < old_len {
            // Truncated in place (`> file`, logrotate's copytruncate): every
            // offset indexed so far is meaningless now.
            self.restart();
            self.catch_up(0);
            return true;
        }
        // Grown. The line that was last may have been only partly written when
        // it was highlighted, so its colours (and everything after) are redone.
        let before = self.line_count();
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(before - 1);
        }
        // The same size cap `enable_syntax` applies on open: past it, colouring
        // from the top down to a far-away last page costs too much.
        if now > crate::syntax::HL_MAX_BYTES {
            self.hl = None;
        }
        self.outline = None;
        if self.mode != ViewMode::Map {
            self.map = None;
        }
        self.catch_up(before);
        true
    }

    /// Forget everything derived from the old content, after a truncation or a
    /// rotation replaced it.
    fn restart(&mut self) {
        self.line_starts = vec![0];
        self.scanned = 0;
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(0);
        }
        self.found_lines.clear();
        self.last_match = None;
        self.outline = None;
        self.map = None;
        if self.mode == ViewMode::Map {
            self.ensure_map();
        }
        self.top = 0;
    }

    /// Index what arrived (within the per-poll budget), then either scroll to
    /// the new end or, when paused, count the lines that were added. `before`
    /// is the line count the new lines are measured against.
    fn catch_up(&mut self, before: usize) {
        self.extend_to_byte(self.scanned + FOLLOW_SCAN_BUDGET);
        let paused = self.follow.as_ref().is_some_and(|f| f.paused);
        if paused {
            let added = self.line_count().saturating_sub(before);
            if let Some(f) = self.follow.as_mut() {
                f.new_lines += added;
            }
        } else if self.mode == ViewMode::Hex
            || (self.mode == ViewMode::Text && self.fully_indexed())
        {
            // A text end that is not indexed yet is unknown (`max_top` would be
            // `usize::MAX`); the next poll finishes the job.
            self.top = self.max_top();
        }
    }

    /// Pause follow mode when the view has been moved off the last page, and
    /// resume it when it is back there. Run after anything that can move the
    /// view: keys, the mouse, Goto and search.
    fn sync_follow(&mut self) {
        if self.follow.is_none() || self.mode == ViewMode::Map {
            return;
        }
        // Before the end is indexed there is no telling whether this is it.
        if self.mode == ViewMode::Text && !self.fully_indexed() {
            return;
        }
        let at_end = self.top >= self.max_top();
        if let Some(f) = self.follow.as_mut() {
            f.paused = !at_end;
            if at_end {
                f.new_lines = 0;
            }
        }
    }

    /// The local file this viewer pages, for the app to run `git blame` on.
    pub fn local_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether `b` can blame what is on screen: the raw text of a local file.
    fn can_blame(&self) -> bool {
        self.path.is_some()
            && self.mode == ViewMode::Text
            && !self.markdown_active()
            && self.active_image().is_none()
            && self.active_model().is_none()
    }

    /// Mark a blame as requested, answered by the event carrying `generation`.
    pub fn begin_blame(&mut self, generation: u64) {
        self.blame = Some(BlameView::Loading(generation));
    }

    /// Whether this viewer is waiting for the blame with `generation` — not a
    /// viewer opened since on another file, and not one whose `b` was undone.
    pub fn awaits_blame(&self, generation: u64) -> bool {
        matches!(self.blame, Some(BlameView::Loading(g)) if g == generation)
    }

    /// Show a finished blame, with the cursor on the first visible line.
    pub fn set_blame(&mut self, blame: crate::git::blame::Blame) {
        self.blame = Some(BlameView::Ready { blame, cursor: self.top });
    }

    pub fn cancel_blame(&mut self) {
        self.blame = None;
    }

    pub(crate) fn blame_loading(&self) -> bool {
        matches!(self.blame, Some(BlameView::Loading(_)))
    }

    /// The blame and its cursor line, when the column is showing: it only
    /// accompanies raw text, so hex, the map, a Markdown render, an image or a
    /// model put it away until you come back.
    pub(crate) fn active_blame(&self) -> Option<(&crate::git::blame::Blame, usize)> {
        match &self.blame {
            Some(BlameView::Ready { blame, cursor }) if self.can_blame() => Some((blame, *cursor)),
            _ => None,
        }
    }

    /// What Enter on the cursor line opens: the repository root, the owning
    /// commit and the file's path in it. `None` on a line no commit has yet.
    pub fn blame_target(&self) -> Option<(PathBuf, String, String)> {
        let (blame, cursor) = self.active_blame()?;
        let c = blame.commit_of(cursor).filter(|c| !c.uncommitted())?;
        Some((blame.toplevel.clone(), c.oid.clone(), c.path.clone()))
    }

    /// Move the blame cursor by `delta` lines, scrolling to keep it in view.
    fn move_blame_cursor(&mut self, delta: isize) {
        let Some(BlameView::Ready { cursor, .. }) = &self.blame else { return };
        let target = (*cursor as isize).saturating_add(delta).max(0) as usize;
        self.extend_to_line(target.saturating_add(1));
        let to = target.min(self.line_count().saturating_sub(1));
        if let Some(BlameView::Ready { cursor, .. }) = &mut self.blame {
            *cursor = to;
        }
        self.reveal_line(to);
    }

    /// Scroll just enough that line `li` is on screen.
    fn reveal_line(&mut self, li: usize) {
        if li < self.top {
            self.top = li;
            return;
        }
        let rows = self.view_rows.max(1);
        if !self.wrap {
            if li >= self.top + rows {
                self.top = li + 1 - rows;
            }
            return;
        }
        // Wrapped lines take several rows: move the top down until everything
        // from it through `li` fits.
        let width = self.view_cols.max(1);
        let height = |v: &Self, i: usize| v.line_str(i).chars().count().div_ceil(width).max(1);
        let mut used: usize = (self.top..=li).map(|i| height(self, i)).sum();
        while used > rows && self.top < li {
            used -= height(self, self.top);
            self.top += 1;
        }
    }

    /// The logical line drawn on content row `row`, accounting for wrap.
    fn line_at_row(&self, row: usize) -> Option<usize> {
        if !self.wrap {
            let li = self.top + row;
            return (li < self.line_count()).then_some(li);
        }
        let width = self.view_cols.max(1);
        let mut y = 0;
        for li in self.top..self.line_count() {
            y += self.line_str(li).chars().count().div_ceil(width).max(1);
            if row < y {
                return Some(li);
            }
            if y > self.view_rows {
                break;
            }
        }
        None
    }

    /// Whether plain text is coloured by log level: a file with no syntax of its
    /// own that is either named like a log or being followed.
    pub(crate) fn log_levels(&self) -> bool {
        !self.has_syntax() && (self.follow.is_some() || loglevel::is_log_name(&self.name))
    }

    fn has_syntax(&self) -> bool {
        self.hl.is_some()
    }

    /// Color runs for line `li` (computing highlight up to it on demand). Empty
    /// when highlighting is off. Returns owned runs so the caller can also read
    /// the line text without a borrow conflict.
    fn line_runs(&mut self, li: usize) -> Vec<ColorRun> {
        let total = self.line_starts.len();
        let Some(hl) = self.hl.as_mut() else {
            return Vec::new();
        };
        // Disjoint field borrows: `hl` (self.hl) vs. self.src / self.line_starts.
        while hl.processed() <= li && hl.processed() < total {
            let i = hl.processed();
            let start = self.line_starts[i];
            let end = self
                .line_starts
                .get(i + 1)
                .map(|&s| s.saturating_sub(1))
                .unwrap_or_else(|| self.src.len());
            let mut bytes = self.src.read_range(start, end.max(start));
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            let display = String::from_utf8_lossy(&bytes).replace('\t', "    ");
            hl.process_next(&display);
        }
        hl.line(li).to_vec()
    }

    /// 16 bytes (or fewer at EOF) of the hex row starting at byte `off`.
    pub(crate) fn hex_row(&self, off: usize) -> Vec<u8> {
        self.src.read_range(off, off + 16)
    }

    fn data_len(&self) -> usize {
        self.src.len()
    }

    fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Whether the whole file's line index has been built (so `line_count` is
    /// exact and the last line's extent is known).
    fn fully_indexed(&self) -> bool {
        self.scanned >= self.data_len()
    }

    /// Index one more chunk of the file, appending newline offsets. Returns
    /// `false` once fully indexed (a read error is treated as EOF so callers
    /// that loop can't spin).
    fn scan_one_chunk(&mut self) -> bool {
        let len = self.data_len();
        if self.scanned >= len {
            return false;
        }
        const CHUNK: usize = 256 * 1024;
        let end = (self.scanned + CHUNK).min(len);
        let buf = self.src.read_range(self.scanned, end);
        if buf.is_empty() {
            self.scanned = len; // give up rather than loop forever on a bad read
            return false;
        }
        for i in memchr::memchr_iter(b'\n', &buf) {
            self.line_starts.push(self.scanned + i + 1);
        }
        self.scanned += buf.len();
        true
    }

    /// Extend the index until at least `target` bytes have been scanned (or EOF).
    fn extend_to_byte(&mut self, target: usize) {
        let target = target.min(self.data_len());
        while self.scanned < target && self.scan_one_chunk() {}
    }

    /// Extend the index until logical line `target` is known (or EOF). Indexing
    /// one past the last visible line lets `line_str` find that line's end.
    fn extend_to_line(&mut self, target: usize) {
        while self.line_starts.len() <= target && self.scan_one_chunk() {}
    }

    /// Build the rest of the line index (for "go to end" / percentage jumps).
    fn index_fully(&mut self) {
        while self.scan_one_chunk() {}
    }

    fn hex_rows(&self) -> usize {
        self.data_len().div_ceil(16)
    }

    /// The largest allowed scroll offset: the top line that puts the end of the
    /// content on the last screen row, so the view can never scroll past the end
    /// into blank space. While a text file's line index is still partial the true
    /// end is unknown and there is nothing to clamp against yet (`usize::MAX`);
    /// callers only ever `.min()` against this.
    fn max_top(&self) -> usize {
        if self.mode == ViewMode::Text && !self.fully_indexed() {
            return usize::MAX;
        }
        let total = match self.mode {
            ViewMode::Text => self.line_count(),
            ViewMode::Hex => self.hex_rows(),
            // The map fits the view by construction, so there is nowhere to
            // scroll to and the top is pinned at zero.
            ViewMode::Map => return 0,
        };
        let rows = self.view_rows.max(1);
        let simple = total.saturating_sub(rows);
        if self.mode == ViewMode::Hex || !self.wrap {
            return simple;
        }
        // Wrapped text: a logical line can span several visual rows. Every line
        // is at least one row, so the answer lies in the window `simple..total`;
        // measure those lines and take the largest top whose lines still reach
        // the bottom row of the screen.
        let width = self.view_cols.max(1);
        let md = self.markdown_active();
        let mut in_code = md && self.in_code_fence_at(simple);
        let mut heights = Vec::with_capacity(total - simple);
        for i in simple..total {
            let line = self.line_str(i);
            heights.push(if md {
                markdown_rows(&line, &mut in_code, width)
            } else {
                line.chars().count().div_ceil(width).max(1)
            });
        }
        let mut acc = 0usize;
        for (off, h) in heights.iter().enumerate().rev() {
            acc += h;
            if acc >= rows {
                return simple + off;
            }
        }
        simple // shorter than one screen: no scrolling at all
    }

    /// Seed the F7 prompt's pre-filled term from the app-wide search memory.
    pub fn set_search_seed(&mut self, seed: String) {
        self.search_seed = seed;
    }

    /// Whether the viewer is showing the hex dump (F4), so the shared search
    /// dialog opens in Hex mode — matching what the editor does.
    pub fn is_hex(&self) -> bool {
        self.mode == ViewMode::Hex
    }

    /// The last search term (for writing back to the app-wide search memory).
    pub fn search_seed(&self) -> &str {
        &self.search_seed
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ViewerSignal {
        let signal = self.route_key(key);
        self.sync_follow();
        signal
    }

    fn route_key(&mut self, key: KeyEvent) -> ViewerSignal {
        // While the outline navigator is open it captures navigation keys.
        if self.outline_open {
            return self.handle_outline_key(key);
        }

        // The map takes the navigation keys too: they move its cursor, and
        // Enter carries that offset into the hex view — which is what makes this
        // a way of finding something rather than only a picture of it.
        if self.mode == ViewMode::Map && self.map.is_some() {
            let cols = self.map_cols.max(1) as isize;
            match key.code {
                KeyCode::Left => self.map_move(-1),
                KeyCode::Right => self.map_move(1),
                KeyCode::Up => self.map_move(-cols),
                KeyCode::Down => self.map_move(cols),
                KeyCode::PageUp => self.map_move(-cols * 8),
                KeyCode::PageDown => self.map_move(cols * 8),
                KeyCode::Home => self.map_cell = 0,
                KeyCode::End => {
                    self.map_cell =
                        self.map.as_ref().map_or(0, |f| f.cells.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    let off = self.map_offset() as usize;
                    self.mode = ViewMode::Hex;
                    self.extend_to_byte(off + 1);
                    self.top = self.offset_to_top(off).min(self.max_top());
                }
                KeyCode::F(8) => self.map_by_class = !self.map_by_class,
                _ => return self.handle_view_key(key),
            }
            return ViewerSignal::Stay;
        }

        // A displayed model takes the navigation keys: they orbit the camera,
        // there being no document on screen for them to scroll. The rates match
        // the 3D panel's own `space3d_key` so the two surfaces feel alike.
        if self.show_model
            && let Some(m) = self.model.as_mut()
        {
            match key.code {
                KeyCode::Left => m.orbit(-ORBIT_YAW, 0.0),
                KeyCode::Right => m.orbit(ORBIT_YAW, 0.0),
                KeyCode::Up => m.orbit(0.0, ORBIT_PITCH),
                KeyCode::Down => m.orbit(0.0, -ORBIT_PITCH),
                KeyCode::Char('+') | KeyCode::Char('=') => m.zoom_by(ZOOM_IN),
                KeyCode::Char('-') | KeyCode::Char('_') => m.zoom_by(ZOOM_OUT),
                KeyCode::Home => m.reset(),
                _ => return self.handle_view_key(key),
            }
            return ViewerSignal::Stay;
        }
        self.handle_view_key(key)
    }

    /// The ordinary viewer keys — everything a model orbit did not claim.
    fn handle_view_key(&mut self, key: KeyEvent) -> ViewerSignal {
        // While blaming, the navigation keys drive the line cursor, and Enter
        // opens the commit under it.
        if self.active_blame().is_some() {
            let page = self.view_rows.saturating_sub(1).max(1) as isize;
            match key.code {
                KeyCode::Up => self.move_blame_cursor(-1),
                KeyCode::Down => self.move_blame_cursor(1),
                KeyCode::PageUp => self.move_blame_cursor(-page),
                KeyCode::PageDown => self.move_blame_cursor(page),
                KeyCode::Home => self.move_blame_cursor(isize::MIN),
                KeyCode::End => {
                    self.index_fully();
                    self.move_blame_cursor(isize::MAX);
                }
                KeyCode::Enter if self.blame_target().is_some() => {
                    return ViewerSignal::OpenBlameCommit;
                }
                _ => return self.handle_plain_view_key(key),
            }
            return ViewerSignal::Stay;
        }
        self.handle_plain_view_key(key)
    }

    fn handle_plain_view_key(&mut self, key: KeyEvent) -> ViewerSignal {
        match key.code {
            // F3 toggles the viewer (open in the panels, close here), matching
            // the footer's "Quit" label; F10 / Esc / q also close.
            KeyCode::F(3) | KeyCode::F(10) | KeyCode::Esc | KeyCode::Char('q') => {
                return ViewerSignal::Close;
            }
            KeyCode::F(2) => self.wrap = !self.wrap,
            KeyCode::F(4) => {
                self.mode = match self.mode {
                    ViewMode::Text => ViewMode::Hex,
                    ViewMode::Hex => ViewMode::Map,
                    ViewMode::Map => ViewMode::Text,
                };
                if self.mode == ViewMode::Map {
                    self.ensure_map();
                }
                self.top = self.top.min(self.max_top());
            }
            KeyCode::F(1) => return ViewerSignal::OpenHelp,
            KeyCode::F(5) => return ViewerSignal::OpenGoto,
            // F6 (Markdown files in text mode): open the document outline.
            KeyCode::F(6) if self.is_markdown && self.mode == ViewMode::Text => self.open_outline(),
            // F8 (model files): toggle between the mesh and the raw text/hex.
            KeyCode::F(8) if self.model.is_some() => self.show_model = !self.show_model,
            // F8 (image files): toggle between the image and the raw text/hex.
            KeyCode::F(8) if self.image.is_some() => self.show_image = !self.show_image,
            // F8 (Markdown files only): toggle the Markdown render and the raw
            // (syntax-highlighted) text.
            KeyCode::F(8) if self.is_markdown && self.mode == ViewMode::Text => {
                self.markdown_render = !self.markdown_render;
            }
            KeyCode::F(7) => return ViewerSignal::OpenSearch,
            KeyCode::Char('n') => self.find_next(),
            KeyCode::Char('f') | KeyCode::Char('F') => self.toggle_follow(),
            // `b` toggles the blame column (and forgets one still loading).
            KeyCode::Char('b') | KeyCode::Char('B') => {
                if self.blame.take().is_none() && self.can_blame() {
                    return ViewerSignal::StartBlame;
                }
            }
            KeyCode::Down => self.scroll(1),
            KeyCode::Up => self.scroll(-1),
            KeyCode::PageDown => self.scroll(self.view_rows as isize - 1),
            KeyCode::PageUp => self.scroll(-(self.view_rows as isize - 1)),
            KeyCode::Home => self.top = 0,
            KeyCode::End => {
                // The true last line is only known once the whole file is indexed.
                if self.mode == ViewMode::Text {
                    self.index_fully();
                }
                self.top = self.max_top();
            }
            KeyCode::Left => self.h_offset = self.h_offset.saturating_sub(8),
            KeyCode::Right => self.h_offset += 8,
            _ => {}
        }
        ViewerSignal::Stay
    }

    /// Route a mouse event. The wheel scrolls; a click in the lower half of the
    /// body scrolls down a page and the upper half scrolls up; the F-key bar
    /// acts as buttons.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> ViewerSignal {
        let signal = self.route_mouse(ev);
        self.sync_follow();
        signal
    }

    fn route_mouse(&mut self, ev: MouseEvent) -> ViewerSignal {
        let (col, row) = (ev.column, ev.row);

        // The outline navigator, while open, captures the mouse (wheel scrolls it,
        // a click on an entry jumps there, a click outside dismisses it).
        if self.outline_open {
            return self.handle_outline_mouse(ev);
        }

        // F-key bar clicks.
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) && row == self.footer_area.y {
            let labels = self.footer_labels();
            return match crate::ui::fkeys::index_at(self.footer_area, &labels, col, row) {
                Some(i) => self.activate_fkey(i),
                None => ViewerSignal::Stay,
            };
        }

        // A displayed model takes the wheel and the drag: zoom and orbit, rather
        // than scrolling a document that is not on screen.
        if self.show_model
            && let Some(m) = self.model.as_mut()
        {
            match ev.kind {
                MouseEventKind::ScrollDown => m.zoom_by(ZOOM_OUT),
                MouseEventKind::ScrollUp => m.zoom_by(ZOOM_IN),
                MouseEventKind::Down(MouseButton::Left) => self.drag_from = Some((col, row)),
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some((px, py)) = self.drag_from {
                        let (dx, dy) = (col as i32 - px as i32, row as i32 - py as i32);
                        // A cell is about twice as tall as it is wide, so equal
                        // pointer travel must not cover twice the angle
                        // vertically — the same correction the 3D panel makes.
                        m.orbit(dx as f32 * -0.05, dy as f32 * 0.05);
                    }
                    self.drag_from = Some((col, row));
                }
                MouseEventKind::Up(MouseButton::Left) => self.drag_from = None,
                _ => {}
            }
            return ViewerSignal::Stay;
        }

        match ev.kind {
            MouseEventKind::ScrollDown => self.scroll(3),
            MouseEventKind::ScrollUp => self.scroll(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                let a = self.content_area;
                let inside = a.height > 0
                    && row >= a.y
                    && row < a.y + a.height
                    && col >= a.x
                    && col < a.x + a.width;
                if inside && self.active_blame().is_some() {
                    // While blaming, a click puts the cursor on that line.
                    if let Some(li) = self.line_at_row((row - a.y) as usize)
                        && let Some(BlameView::Ready { cursor, .. }) = &mut self.blame
                    {
                        *cursor = li;
                    }
                } else if inside {
                    // Below the vertical center pages down; above it pages up.
                    let mid = a.y + a.height / 2;
                    let page = (self.view_rows as isize - 1).max(1);
                    self.scroll(if row >= mid { page } else { -page });
                }
            }
            _ => {}
        }
        ViewerSignal::Stay
    }

    /// The F-key bar labels for the current mode (kept in sync with the footer
    /// renderer, which calls this).
    /// Whether the Markdown approximation should be drawn (a Markdown file in
    /// text mode with the render toggle on).
    pub(crate) fn markdown_active(&self) -> bool {
        self.is_markdown && self.markdown_render && self.mode == ViewMode::Text
    }

    /// Attach a decoded image and switch to showing it (F3 on an image file).
    pub fn set_image(&mut self, iv: ViewerImage) {
        self.image = Some(iv);
        self.show_image = true;
    }

    /// The decoded image, when it is currently being displayed (vs. the raw view).
    pub(crate) fn active_image(&self) -> Option<&ViewerImage> {
        self.show_image.then_some(self.image.as_ref()).flatten()
    }

    /// Attach a parsed mesh and switch to showing it (F3 on a model file).
    pub fn set_model(&mut self, m: ViewerModel) {
        self.model = Some(m);
        self.show_model = true;
    }

    /// The mesh, when it is currently being displayed (vs. the raw view).
    pub(crate) fn active_model(&self) -> Option<&ViewerModel> {
        self.show_model.then_some(self.model.as_ref()).flatten()
    }

    /// Whether a drag is under way that [`handle_mouse`] answers by orbiting the
    /// model — the pointer's travel since the press, which the viewer recorded
    /// itself, rather than anything the last frame drew.
    ///
    /// [`handle_mouse`]: ViewerState::handle_mouse
    pub fn orbiting(&self) -> bool {
        !self.outline_open && self.active_model().is_some() && self.drag_from.is_some()
    }

    /// Build the byte map, once. Sampled rather than read whole (see
    /// [`fingerprint`]), so this is bounded work even on a multi-gigabyte image
    /// and can run on the spot rather than going through a background task.
    fn ensure_map(&mut self) {
        if self.map.is_some() {
            return;
        }
        let fp = fingerprint::analyze(self.src.len() as u64, |a, b| {
            self.src.read_range(a as usize, b as usize)
        });
        self.map_cell = fp.cell_at(self.top_offset() as u64);
        self.map = Some(fp);
    }

    /// The byte map, when `Map` mode is showing.
    pub(crate) fn active_map(&self) -> Option<&fingerprint::Fingerprint> {
        (self.mode == ViewMode::Map).then_some(self.map.as_ref()).flatten()
    }

    pub(crate) fn map_cursor(&self) -> usize {
        self.map_cell
    }

    pub(crate) fn map_by_class(&self) -> bool {
        self.map_by_class
    }

    /// Move the map cursor by `d` cells, clamped to the file.
    fn map_move(&mut self, d: isize) {
        let Some(fp) = self.map.as_ref() else {
            return;
        };
        let last = fp.cells.len().saturating_sub(1);
        self.map_cell = (self.map_cell as isize + d).clamp(0, last as isize) as usize;
    }

    /// Byte offset the map cursor sits on.
    pub(crate) fn map_offset(&self) -> u64 {
        self.map.as_ref().and_then(|f| f.cells.get(self.map_cell)).map_or(0, |c| c.start)
    }

    pub(crate) fn footer_labels(&self) -> [&'static str; 10] {
        let wrap = if self.wrap { "Unwrap" } else { "Wrap" };
        // Names the mode F4 moves *to*, cycling Text → Hex → Map.
        let mode = match self.mode {
            ViewMode::Text => "Hex",
            ViewMode::Hex => "Map",
            ViewMode::Map => "Text",
        };
        // F8: for an image file, toggle Image/Raw; for a Markdown file in text
        // mode, "Raw" shows the source and "Render" the approximation.
        let f8 = if self.mode == ViewMode::Map {
            if self.map_by_class { "Density" } else { "Bytes" }
        } else if self.model.is_some() {
            if self.show_model { "Raw" } else { "Model" }
        } else if self.image.is_some() {
            if self.show_image { "Raw" } else { "Image" }
        } else if self.is_markdown && self.mode == ViewMode::Text {
            if self.markdown_render { "Raw" } else { "Render" }
        } else {
            ""
        };
        // F6: the document outline, offered for Markdown files in text mode.
        let outline = if self.is_markdown && self.mode == ViewMode::Text { "Outline" } else { "" };
        ["Help", wrap, "Quit", mode, "Goto", outline, "Search", f8, "Next", "Quit"]
    }

    /// Perform the action of F-key index `i` (0-based) from a bar click.
    fn activate_fkey(&mut self, i: usize) -> ViewerSignal {
        let code = match i {
            1 => KeyCode::F(2),                  // Wrap / Unwrap
            2 | 9 => return ViewerSignal::Close, // Quit
            3 => KeyCode::F(4),                  // Text / Hex
            4 => return ViewerSignal::OpenGoto,  // Goto
            5 => KeyCode::F(6),                  // Outline (Markdown)
            6 => KeyCode::F(7),                  // Search
            7 => KeyCode::F(8),                  // Raw / Render (Markdown)
            8 => KeyCode::Char('n'),             // Next match
            0 => return ViewerSignal::OpenHelp,  // Help
            _ => return ViewerSignal::Stay,      // empty slot: no-op
        };
        self.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Whether the F6 document-outline overlay is currently shown.
    #[allow(dead_code)] // accessor used by tests
    pub fn is_outline_open(&self) -> bool {
        self.outline_open
    }

    /// Open the document-outline navigator, building the heading list on first
    /// use and pre-selecting the heading at or before the current position.
    pub fn open_outline(&mut self) {
        if self.outline.is_none() {
            let items = self.build_outline();
            self.outline = Some(items);
        }
        self.outline_sel = self
            .outline
            .as_ref()
            .map_or(0, |o| o.iter().rposition(|it| it.line <= self.top).unwrap_or(0));
        self.outline_open = true;
    }

    /// Scan the whole document for ATX headings, producing the outline entries.
    /// Headings inside fenced code blocks are ignored.
    fn build_outline(&mut self) -> Vec<markdown::OutlineItem> {
        self.index_fully();
        let mut items = Vec::new();
        let mut in_fence = false;
        for li in 0..self.line_count() {
            let line = self.line_str(li);
            if markdown::is_fence(&line) {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
            if let Some((level, text)) = markdown::heading_of(&line) {
                items.push(markdown::OutlineItem { level, text, line: li });
            }
        }
        items
    }

    /// Keys while the outline overlay is open: navigate the list, jump on Enter,
    /// dismiss on Esc/F6.
    fn handle_outline_key(&mut self, key: KeyEvent) -> ViewerSignal {
        let len = self.outline.as_ref().map_or(0, |o| o.len());
        // A page is the visible height of the list (set by the renderer).
        let page = (self.outline_area.height as isize).max(1);
        match key.code {
            KeyCode::Esc | KeyCode::F(6) => self.outline_open = false,
            KeyCode::Enter => {
                self.jump_to_outline_sel();
                self.outline_open = false;
            }
            KeyCode::Up => self.outline_move(-1),
            KeyCode::Down => self.outline_move(1),
            KeyCode::PageUp => self.outline_move(-page),
            KeyCode::PageDown => self.outline_move(page),
            KeyCode::Home => self.outline_sel = 0,
            KeyCode::End => self.outline_sel = len.saturating_sub(1),
            _ => {}
        }
        ViewerSignal::Stay
    }

    /// Mouse while the outline overlay is open: wheel scrolls the selection, a
    /// left click on an entry jumps there, a click elsewhere dismisses it.
    fn handle_outline_mouse(&mut self, ev: MouseEvent) -> ViewerSignal {
        match ev.kind {
            MouseEventKind::ScrollDown => self.outline_move(1),
            MouseEventKind::ScrollUp => self.outline_move(-1),
            MouseEventKind::Down(MouseButton::Left) => {
                let a = self.outline_area;
                let inside = a.height > 0
                    && ev.row >= a.y
                    && ev.row < a.y + a.height
                    && ev.column >= a.x
                    && ev.column < a.x + a.width;
                if inside {
                    let idx = self.outline_top + (ev.row - a.y) as usize;
                    if idx < self.outline.as_ref().map_or(0, |o| o.len()) {
                        self.outline_sel = idx;
                        self.jump_to_outline_sel();
                    }
                }
                self.outline_open = false;
            }
            _ => {}
        }
        ViewerSignal::Stay
    }

    /// Move the outline selection by `delta`, clamped to the list bounds.
    fn outline_move(&mut self, delta: isize) {
        let len = self.outline.as_ref().map_or(0, |o| o.len());
        if len == 0 {
            return;
        }
        self.outline_sel =
            (self.outline_sel as isize).saturating_add(delta).clamp(0, len as isize - 1) as usize;
    }

    /// Scroll the view to the currently selected heading's source line.
    fn jump_to_outline_sel(&mut self) {
        let Some(line) =
            self.outline.as_ref().and_then(|o| o.get(self.outline_sel)).map(|it| it.line)
        else {
            return;
        };
        self.extend_to_line(line);
        self.top = line.min(self.max_top());
        self.h_offset = 0;
    }

    fn scroll(&mut self, delta: isize) {
        let target = (self.top as isize + delta).max(0) as usize;
        // Index far enough that `target` is reachable (and a page is renderable).
        if self.mode == ViewMode::Text {
            self.extend_to_line(target + self.view_rows);
        }
        self.top = target.min(self.max_top());
    }

    /// Jump to a position given by the Goto dialog. Returns whether the input
    /// parsed (so the caller can flag bad input). In text mode positions are
    /// logical lines; in hex mode they are 16-byte rows.
    pub fn goto(&mut self, value: &str, mode: GotoMode) -> bool {
        let v = value.trim();
        let text = self.mode == ViewMode::Text;
        // Parse first, then extend the index as far as the target needs before
        // computing the top row.
        let target = match mode {
            GotoMode::Line => {
                let Ok(n) = v.parse::<usize>() else { return false };
                let line = n.saturating_sub(1);
                if text {
                    self.extend_to_line(line);
                }
                line
            }
            GotoMode::Percent => {
                let Ok(p) = v.parse::<f64>() else { return false };
                // A percentage needs the exact total, so finish indexing.
                if text {
                    self.index_fully();
                }
                let total = match self.mode {
                    ViewMode::Text => self.line_count(),
                    ViewMode::Hex => self.hex_rows(),
                    ViewMode::Map => 1,
                };
                ((total.saturating_sub(1)) as f64 * p.clamp(0.0, 100.0) / 100.0).round() as usize
            }
            GotoMode::DecimalOffset => {
                let Ok(off) = v.parse::<usize>() else { return false };
                if text {
                    self.extend_to_byte(off + 1);
                }
                self.offset_to_top(off)
            }
            GotoMode::HexOffset => {
                let hex = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")).unwrap_or(v);
                let Ok(off) = usize::from_str_radix(hex, 16) else { return false };
                if text {
                    self.extend_to_byte(off + 1);
                }
                self.offset_to_top(off)
            }
        };
        self.top = target.min(self.max_top());
        self.sync_follow();
        true
    }

    /// Byte offset the view is currently positioned at.
    fn top_offset(&self) -> usize {
        match self.mode {
            ViewMode::Text => self.line_starts.get(self.top).copied().unwrap_or(0),
            ViewMode::Hex => self.top * 16,
            ViewMode::Map => self.map_offset() as usize,
        }
    }

    /// Map a byte offset to the top index for the current mode.
    fn offset_to_top(&self, off: usize) -> usize {
        match self.mode {
            ViewMode::Text => self.byte_to_line(off),
            ViewMode::Hex => off / 16,
            // The map has no scroll position of its own; it shows the whole
            // file at once and carries a cursor instead.
            ViewMode::Map => 0,
        }
    }

    /// Find the next occurrence of the query after the last match.
    /// Apply the search dialog's result. An identical repeat continues from the
    /// last hit — which is what makes pressing F7-Enter again walk the file —
    /// while any change of term or option restarts from the top.
    pub fn apply_search(&mut self, p: &crate::ui::dialog::SearchReplaceParams) {
        let want = ViewSearch {
            query: p.search.clone(),
            regex: p.regex,
            case_sensitive: p.case_sensitive,
            whole_words: p.whole_words,
            backwards: p.backwards,
            hex: p.hex,
        };
        if want != self.search {
            self.search = want;
            self.last_match = None;
        }
        self.search_seed = p.search.clone();
        if p.find_all {
            self.find_all()
        } else {
            self.find_next()
        }
        self.sync_follow();
    }

    /// Compile the current search, or `None` when it is unusable (bad regex, bad
    /// hex, empty term).
    fn needle(&self) -> Option<search::Needle> {
        let s = &self.search;
        search::Needle::build(&s.query, s.regex, s.case_sensitive, s.whole_words, s.hex)
    }

    /// Whether `line` holds a "Find all" match (the renderer tints it).
    pub(crate) fn line_found(&self, line: usize) -> bool {
        self.found_lines.contains(&line)
    }

    /// Number of lines the last "Find all" marked.
    #[allow(dead_code)] // accessor used by tests
    pub fn found_count(&self) -> usize {
        self.found_lines.len()
    }

    fn find_next(&mut self) {
        let Some(needle) = self.needle() else { return };
        let found = if self.search.backwards {
            // Wrap to the end when there is nothing before the current hit.
            let before = self.last_match.unwrap_or(0);
            self.scan_back(&needle, before).or_else(|| self.scan_back(&needle, self.data_len()))
        } else {
            let start = self.last_match.map(|m| m + 1).unwrap_or(0);
            self.scan(&needle, start).or_else(|| self.scan(&needle, 0))
        };
        if let Some(off) = found {
            self.last_match = Some(off);
            self.reveal(off);
        }
    }

    /// Scroll so the byte at `off` is on screen.
    fn reveal(&mut self, off: usize) {
        match self.mode {
            ViewMode::Text => {
                // The match may lie beyond the indexed region; index up to it.
                self.extend_to_byte(off + 1);
                self.top = self.byte_to_line(off).min(self.max_top());
            }
            ViewMode::Hex => self.top = (off / 16).min(self.max_top()),
            // A search hit while the map is up moves the map cursor, which is
            // the only position the view has.
            ViewMode::Map => {
                if let Some(fp) = self.map.as_ref() {
                    self.map_cell = fp.cell_at(off as u64);
                }
            }
        }
    }

    /// Highlight every line holding a match, replacing any previous set, and jump
    /// to the first. Capped at [`FOUND_LINES_MAX`]: the viewer pages files far too
    /// big to mark exhaustively, and a bounded set keeps this from turning into an
    /// unbounded scan of a multi-gigabyte log.
    fn find_all(&mut self) {
        self.found_lines.clear();
        let Some(needle) = self.needle() else { return };
        let mut at = 0usize;
        let mut first = None;
        while let Some(off) = self.scan(&needle, at) {
            if first.is_none() {
                first = Some(off);
            }
            self.extend_to_byte(off + 1);
            self.found_lines.insert(self.byte_to_line(off));
            if self.found_lines.len() >= FOUND_LINES_MAX {
                break;
            }
            at = off + 1;
        }
        if let Some(off) = first {
            self.last_match = Some(off);
            self.reveal(off);
        }
    }

    /// First match at or after `start`, reading the source in overlapping windows
    /// so a file-backed source is never loaded whole.
    fn scan(&self, needle: &search::Needle, start: usize) -> Option<usize> {
        const WINDOW: usize = 256 * 1024;
        let len = self.data_len();
        let overlap = needle.overlap();
        let mut pos = start.min(len);
        while pos + needle.min_len() <= len {
            let end = (pos + WINDOW.max(overlap + 1)).min(len);
            let buf = self.src.read_range(pos, end);
            if let Some(i) = needle.find(&buf, 0) {
                return Some(pos + i);
            }
            if end == len {
                break;
            }
            // Rewind so a match straddling the seam is still seen.
            pos = end - overlap;
        }
        None
    }

    /// Last match starting strictly before `before`. Scans forward keeping the
    /// most recent hit: a backwards search is the rarer path, and this reuses the
    /// same windowing rather than needing a second, reversed reader.
    fn scan_back(&self, needle: &search::Needle, before: usize) -> Option<usize> {
        let mut best = None;
        let mut at = 0usize;
        while let Some(off) = self.scan(needle, at) {
            if off >= before {
                break;
            }
            best = Some(off);
            at = off + 1;
        }
        best
    }

    fn byte_to_line(&self, off: usize) -> usize {
        match self.line_starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// Whether logical line `line` begins *inside* a fenced code block — i.e. an
    /// odd number of code-fence lines (` ``` ` / `~~~`) precede it. Lets the
    /// Markdown renderer tell whether the top of the viewport is already within a
    /// code box when the block's opening fence has scrolled off the top. Scans
    /// only the already-indexed prefix `0..line`, so it triggers no extra I/O.
    fn in_code_fence_at(&self, line: usize) -> bool {
        let mut inside = false;
        for li in 0..line.min(self.line_count()) {
            if markdown::is_fence(&self.line_str(li)) {
                inside = !inside;
            }
        }
        inside
    }

    /// Text of logical line `i`, with tabs expanded and CR stripped.
    fn line_str(&self, i: usize) -> String {
        let start = self.line_starts[i];
        let end = self
            .line_starts
            .get(i + 1)
            .map(|&s| s.saturating_sub(1)) // drop the '\n'
            .unwrap_or_else(|| self.data_len());
        let mut bytes = self.src.read_range(start, end.max(start));
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        String::from_utf8_lossy(&bytes).replace('\t', "    ")
    }
}

impl Drop for ViewerState {
    fn drop(&mut self) {
        if let Some(path) = self.temp.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Visual rows `line` occupies in the wrapped Markdown approximation at `width`
/// columns, toggling the code-fence state across calls — mirrors how
/// `render_markdown` lays lines out.
fn markdown_rows(line: &str, in_code: &mut bool, width: usize) -> usize {
    if markdown::is_fence(line) {
        *in_code = !*in_code;
        return 1; // drawn as a one-row box border
    }
    if *in_code {
        // The code box borders and padding take four columns (see
        // `push_code_line`); below that width the line isn't wrapped at all.
        let code_w = width.saturating_sub(4);
        return if code_w == 0 { 1 } else { line.chars().count().div_ceil(code_w).max(1) };
    }
    let chars: Vec<char> = line.chars().collect();
    markdown::display_len(&chars).div_ceil(width).max(1)
}

/// Whether `name` looks like a Markdown file (by extension).
fn is_markdown_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".md", ".markdown", ".mdown", ".mkd", ".mdx"].iter().any(|ext| lower.ends_with(ext))
}

/// Byte offsets where each text line begins (line 0 always starts at 0).
fn compute_line_starts(data: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for i in memchr::memchr_iter(b'\n', data) {
        starts.push(i + 1);
    }
    starts
}

/// A file's identity, (device, inode), for noticing a rotated log. Unix only:
/// elsewhere follow mode still handles growth and truncation, just not rotation.
#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Bytes scanned up-front when a file is opened. The rest of the line index is
/// built lazily as the user scrolls/searches, so even multi-gigabyte files open
/// instantly instead of waiting for a full newline scan.
const INITIAL_SCAN: usize = 1024 * 1024;

/// Open a file and build the *initial* slice of its line-start index (offsets
/// only). Returns the open handle, byte length, the partial index, and how many
/// bytes were scanned — all `Send`, so it can run in `spawn_blocking` and the
/// (non-`Send`) [`ViewerState`] is then assembled on the main thread. The viewer
/// extends the index on demand from there.
pub fn scan_file(path: &Path) -> std::io::Result<(File, usize, Vec<usize>, usize)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len() as usize;
    let (line_starts, scanned) = scan_line_starts(&mut file, INITIAL_SCAN.min(len))?;
    Ok((file, len, line_starts, scanned))
}

/// Scan up to `budget` bytes from the start of `file`, recording newline offsets.
/// Returns the partial index and the number of bytes actually scanned. Only
/// newline offsets are recorded — the file content is never held in memory.
fn scan_line_starts(file: &mut File, budget: usize) -> std::io::Result<(Vec<usize>, usize)> {
    let mut starts = vec![0usize];
    if budget == 0 {
        return Ok((starts, 0));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut off = 0usize;
    while off < budget {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for i in memchr::memchr_iter(b'\n', &buf[..n]) {
            starts.push(off + i + 1);
        }
        off += n;
    }
    Ok((starts, off))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_mode_defaults_on_and_f8_toggles_raw() {
        // A .md file opens in the Markdown approximation; F8 toggles raw text.
        let mut v = ViewerState::new("README.md".into(), b"# Title\n".to_vec());
        assert!(v.markdown_active(), "markdown files render markdown by default");
        assert_eq!(v.footer_labels()[7], "Raw", "F8 shows 'Raw' while rendering markdown");

        v.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        assert!(!v.markdown_active(), "F8 switches to raw text");
        assert_eq!(v.footer_labels()[7], "Render", "F8 shows 'Render' while raw");

        v.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        assert!(v.markdown_active(), "F8 toggles back to markdown");

        // Switching to hex (F4) hides the markdown toggle.
        v.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        assert_eq!(v.mode, ViewMode::Hex);
        assert_eq!(v.footer_labels()[7], "");

        // A non-markdown file never offers the markdown mode.
        let v = ViewerState::new("notes.txt".into(), b"# not a heading\n".to_vec());
        assert!(!v.markdown_active());
        assert_eq!(v.footer_labels()[7], "");
    }

    #[test]
    fn outline_extracts_headings_and_skips_fenced_code() {
        let md = concat!(
            "# Title\n",         // 0
            "intro\n",           // 1
            "## Section A\n",    // 2
            "```\n",             // 3  code fence opens
            "# not a heading\n", // 4  inside the fence — ignored
            "```\n",             // 5  fence closes
            "## Section B\n",    // 6
            "### Sub `B1`\n",    // 7  inline code stripped
            "text\n",            // 8
            "#### Deep\n",       // 9
        );
        let mut v = ViewerState::new("doc.md".into(), md.as_bytes().to_vec());
        let items = v.build_outline();
        let got: Vec<(usize, &str, usize)> =
            items.iter().map(|it| (it.level, it.text.as_str(), it.line)).collect();
        assert_eq!(
            got,
            vec![
                (1, "Title", 0),
                (2, "Section A", 2),
                (2, "Section B", 6),
                (3, "Sub B1", 7),
                (4, "Deep", 9),
            ]
        );
    }

    #[test]
    fn f6_opens_outline_navigates_and_jumps() {
        let md = concat!(
            "# One\n",    // 0
            "a\n",        // 1
            "## Two\n",   // 2
            "b\n",        // 3
            "## Three\n", // 4
            "c\n",        // 5
        );
        let mut v = ViewerState::new("d.md".into(), md.as_bytes().to_vec());
        let press = |v: &mut ViewerState, c: KeyCode| {
            v.handle_key(KeyEvent::new(c, KeyModifiers::NONE));
        };

        press(&mut v, KeyCode::F(6));
        assert!(v.is_outline_open());
        assert_eq!(v.outline.as_ref().unwrap().len(), 3);
        assert_eq!(v.outline_sel, 0, "starts on the heading at/before the top line");

        // Navigate to "Three" and jump: the view scrolls to its source line.
        press(&mut v, KeyCode::Down);
        press(&mut v, KeyCode::Down);
        assert_eq!(v.outline_sel, 2);
        press(&mut v, KeyCode::Enter);
        assert!(!v.is_outline_open(), "Enter closes the outline");
        assert_eq!(v.top, 4, "jumped to the 'Three' heading line");

        // Reopening reflects the new position; Esc dismisses without moving.
        press(&mut v, KeyCode::F(6));
        assert_eq!(v.outline_sel, 2);
        press(&mut v, KeyCode::Esc);
        assert!(!v.is_outline_open());
        assert_eq!(v.top, 4);
    }

    #[test]
    fn f6_outline_only_for_markdown_text_mode() {
        // A non-markdown file offers no outline and F6 is inert.
        let mut v = ViewerState::new("notes.txt".into(), b"# not markdown\n".to_vec());
        assert_eq!(v.footer_labels()[5], "");
        v.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert!(!v.is_outline_open());

        // Markdown in text mode: the label shows and F6 opens the outline.
        let mut v = ViewerState::new("r.md".into(), b"# H\n".to_vec());
        assert_eq!(v.footer_labels()[5], "Outline");
        v.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert!(v.is_outline_open());
        v.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Switching to hex mode hides the outline affordance.
        v.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        assert_eq!(v.mode, ViewMode::Hex);
        assert_eq!(v.footer_labels()[5], "");
        v.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert!(!v.is_outline_open(), "F6 does nothing in hex mode");
    }

    #[test]
    fn outline_overlay_renders_headings_and_highlights_selection() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let md = concat!("# One\n", "a\n", "## Two\n", "b\n", "### Three\n");
        let mut v = ViewerState::new("d.md".into(), md.as_bytes().to_vec());
        v.open_outline(); // selection starts on "One" (index 0)
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(50, 16)).unwrap();
        t.draw(|f| crate::viewer::render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();

        // Every heading title is drawn somewhere in the overlay.
        let rows: Vec<String> = (0..b.area.height)
            .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect())
            .collect();
        for title in ["One", "Two", "Three"] {
            assert!(rows.iter().any(|r| r.contains(title)), "outline shows '{title}'");
        }

        // The selected entry ("One") is drawn with the dialog selection background.
        // (The document's own "One" heading is also visible outside the centered
        // overlay box, so match the row that both shows "One" and is highlighted.)
        let sel_bg = theme.dialog_selection.bg.unwrap();
        let highlighted = rows.iter().enumerate().any(|(y, r)| {
            r.contains("One") && (0..b.area.width).any(|x| b[(x, y as u16)].bg == sel_bg)
        });
        assert!(highlighted, "the selected heading row is highlighted");
    }

    #[test]
    fn markdown_fenced_code_is_boxed_and_literal() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let md = concat!(
            "# Title\n",         // 0
            "```rust\n",         // 1  fence opens (language: rust)
            "fn main() {}\n",    // 2  code content, shown literally
            "# not a heading\n", // 3  '#' inside the fence stays literal
            "```\n",             // 4  fence closes
            "done\n",            // 5
        );
        let mut v = ViewerState::new("doc.md".into(), md.as_bytes().to_vec());
        assert!(v.markdown_active());
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
        t.draw(|f| crate::viewer::render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let rows: Vec<String> = (0..b.area.height)
            .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect())
            .collect();
        let all = rows.join("\n");

        // The block is framed with box-drawing corners and side borders.
        assert!(all.contains('┌') && all.contains('┐'), "top border drawn");
        assert!(all.contains('└') && all.contains('┘'), "bottom border drawn");
        assert!(all.contains('│'), "side borders drawn");
        // The language labels the opening border.
        assert!(rows.iter().any(|r| r.contains("rust")), "language shown on the box");
        // Code content is literal: the '#' line is NOT turned into a heading
        // (which would strip the marker), it is kept verbatim inside the box.
        assert!(rows.iter().any(|r| r.contains("# not a heading")), "'#' kept literally in code");
        assert!(rows.iter().any(|r| r.contains("fn main() {}")), "code body rendered");
    }

    #[test]
    fn markdown_box_still_frames_when_scrolled_into_the_block() {
        // Starting the viewport in the middle of a code block (the opening fence
        // scrolled off the top) still draws the side borders around the content.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let md = concat!(
            "```\n",  // 0  fence opens
            "aaaa\n", // 1
            "bbbb\n", // 2
            "cccc\n", // 3
            "```\n",  // 4  fence closes
        );
        let mut v = ViewerState::new("d.md".into(), md.as_bytes().to_vec());
        v.top = 2; // start on the "bbbb" content line, inside the fence
        assert!(v.in_code_fence_at(v.top), "top of the viewport is inside a fence");
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(30, 8)).unwrap();
        t.draw(|f| crate::viewer::render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let rows: Vec<String> = (0..b.area.height)
            .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect())
            .collect();
        // No opening corner is visible (it is above the viewport) but the content
        // is still framed by side borders and closed at the bottom.
        assert!(rows.iter().any(|r| r.contains("bbbb") && r.contains('│')), "content framed");
        assert!(rows.join("\n").contains('└'), "bottom border still drawn");
    }

    #[test]
    fn outline_headings_stay_legible_on_a_bright_dialog() {
        // On a theme with a bright dialog background, the per-level heading colors
        // are contrast-adjusted so every drawn entry remains readable.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let md = concat!("# One\n", "a\n", "## Two\n", "b\n", "### Three\n");
        let mut v = ViewerState::new("d.md".into(), md.as_bytes().to_vec());
        v.open_outline();
        let theme = crate::ui::theme::Theme::by_name("GitHub Light", true);
        let mut t = Terminal::new(TestBackend::new(50, 16)).unwrap();
        t.draw(|f| crate::viewer::render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();

        // Every non-selected outline entry cell (drawn on the dialog background)
        // must contrast with that background — no illegible headings.
        let dialog_bg = theme.dialog_bg;
        let luma = |c: ratatui::style::Color| match c {
            ratatui::style::Color::Rgb(r, g, b) => {
                0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64
            }
            _ => 128.0,
        };
        let bg_luma = luma(dialog_bg);
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                let cell = &b[(x, y)];
                if cell.bg == dialog_bg && cell.symbol().trim() != "" {
                    assert!(
                        (luma(cell.fg) - bg_luma).abs() >= 96.0,
                        "outline text {:?} must contrast with the dialog bg",
                        cell.symbol()
                    );
                }
            }
        }
    }

    #[test]
    fn outline_click_selects_and_jumps() {
        let md = concat!("# One\n", "a\n", "## Two\n", "b\n", "## Three\n");
        let mut v = ViewerState::new("d.md".into(), md.as_bytes().to_vec());
        v.open_outline();
        // Stand in for the renderer's recorded list rect (rows 2,3,4).
        v.outline_area = Rect::new(0, 2, 20, 3);
        v.outline_top = 0;
        // Click the third row → entry index 2 ("Three" on source line 4).
        v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 4));
        assert!(!v.is_outline_open(), "a click jumps and closes the outline");
        assert_eq!(v.top, 4);
    }

    /// The dialog's params for a plain (case-insensitive, literal) search.
    fn sp(term: &str) -> crate::ui::dialog::SearchReplaceParams {
        crate::ui::dialog::SearchReplaceParams {
            replace: false,
            search: term.into(),
            replacement: String::new(),
            regex: false,
            case_sensitive: false,
            whole_words: false,
            backwards: false,
            hex: false,
            find_all: false,
        }
    }

    #[test]
    fn f1_asks_the_app_to_open_the_manual() {
        // The viewer's F-key bar advertises "Help" at F1; it must actually do
        // something (open the manual), not sit there as a dead label.
        let mut v = ViewerState::new("t".into(), b"hello".to_vec());
        let sig = v.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert!(matches!(sig, ViewerSignal::OpenHelp));
    }

    #[test]
    fn f7_asks_the_app_for_the_shared_search_dialog() {
        // The viewer no longer owns an inline prompt: F7 hands off to the same
        // modal dialog the editor uses, so both offer the same options.
        let mut v = ViewerState::new("t".into(), b"alpha beta".to_vec());
        let sig = v.handle_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
        assert!(matches!(sig, ViewerSignal::OpenSearch));
        // In hex mode the app opens that dialog in Hex mode (see `search_dialog`).
        assert!(!v.is_hex());
        v.mode = ViewMode::Hex;
        assert!(v.is_hex());
    }

    #[test]
    fn repeating_a_search_advances_to_the_next_occurrence() {
        // The bug this guards: re-submitting the same term used to reset the
        // cursor and keep re-finding the first hit.
        let mut v = ViewerState::new("t".into(), b"two\nx\ntwo\ny\ntwo".to_vec());
        v.apply_search(&sp("two"));
        assert_eq!(v.top, 0, "first hit");
        v.apply_search(&sp("two"));
        assert_eq!(v.top, 2, "the same term again moves on");
        v.apply_search(&sp("two"));
        assert_eq!(v.top, 4);
        // Past the last hit it wraps back to the top.
        v.apply_search(&sp("two"));
        assert_eq!(v.top, 0, "wraps around");

        // The `n` key repeats the same search without reopening the dialog.
        v.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(v.top, 2, "'n' advances too");

        // Re-running the same term keeps advancing (we are on line 2, so on to 4).
        v.apply_search(&sp("two"));
        assert_eq!(v.top, 4);
        // A *different* term restarts from the top rather than resuming.
        v.apply_search(&sp("y"));
        assert_eq!(v.top, 3, "a new term searches from the start");
    }

    #[test]
    fn search_honours_the_dialog_options() {
        let mut v = ViewerState::new("t".into(), b"Hit\nhit\nhitting".to_vec());
        // Case-sensitive skips the capitalised line.
        let mut p = sp("hit");
        p.case_sensitive = true;
        v.apply_search(&p);
        assert_eq!(v.top, 1);
        // Whole words skips "hitting" — the next hit wraps back to line 1.
        let mut p = sp("hit");
        p.case_sensitive = true;
        p.whole_words = true;
        v.apply_search(&p);
        assert_eq!(v.top, 1);
        v.apply_search(&p);
        assert_eq!(v.top, 1, "'hitting' is not a whole word, so it wraps to the only hit");

        // Regex.
        let mut v = ViewerState::new("t".into(), b"aaa\nbbb\nabc".to_vec());
        let mut p = sp("a.c");
        p.regex = true;
        v.apply_search(&p);
        assert_eq!(v.top, 2);

        // `^`/`$` anchor per line, exactly as in the editor: the "foo" inside
        // "xfoo" is skipped for the one that starts a line.
        let mut v = ViewerState::new("t".into(), b"xfoo\nbar\nfoo end".to_vec());
        let mut p = sp("^foo");
        p.regex = true;
        v.apply_search(&p);
        assert_eq!(v.top, 2, "the line-initial foo is found, not the one in 'xfoo'");

        // Backwards walks up the file.
        let mut v = ViewerState::new("t".into(), b"hit\nx\nhit\ny\nhit".to_vec());
        v.apply_search(&sp("hit"));
        assert_eq!(v.top, 0);
        let mut p = sp("hit");
        p.backwards = true;
        v.apply_search(&p);
        assert_eq!(v.top, 4, "backwards from the first hit wraps to the last");
        v.apply_search(&p);
        assert_eq!(v.top, 2);
    }

    #[test]
    fn hex_mode_search_matches_raw_bytes() {
        let mut v = ViewerState::new("t".into(), b"one\nHello".to_vec());
        let mut p = sp("48 65"); // "He"
        p.hex = true;
        v.apply_search(&p);
        assert_eq!(v.top, 1, "the hex bytes are found on the second line");
    }

    #[test]
    fn find_all_highlights_matching_lines_until_the_next_one() {
        let mut v = ViewerState::new("t".into(), b"hit\nmiss\nHIT\nmiss\nhit".to_vec());
        let mut p = sp("hit");
        p.find_all = true;
        v.apply_search(&p);
        assert_eq!(v.found_count(), 3);
        for l in [0, 2, 4] {
            assert!(v.line_found(l), "line {l} is highlighted");
        }
        assert!(!v.line_found(1) && !v.line_found(3));

        // A plain search leaves the highlight alone...
        v.apply_search(&sp("miss"));
        assert!(v.line_found(0), "an ordinary search keeps the highlight");
        // ...and the next Find all replaces it.
        let mut p = sp("miss");
        p.find_all = true;
        v.apply_search(&p);
        assert_eq!(v.found_count(), 2);
        assert!(v.line_found(1) && !v.line_found(0));
    }

    #[test]
    fn line_indexing_and_content() {
        let v = ViewerState::new("t".into(), b"alpha\nbeta\r\ngamma".to_vec());
        assert_eq!(v.line_count(), 3);
        assert_eq!(v.line_str(0), "alpha");
        assert_eq!(v.line_str(1), "beta"); // CR stripped
        assert_eq!(v.line_str(2), "gamma");
    }

    #[test]
    fn search_finds_and_maps_to_line() {
        let mut v = ViewerState::new("t".into(), b"one\ntwo\nthree\nTWO".to_vec());
        v.apply_search(&sp("two"));
        // Case-insensitive: first match is on line 1 ("two").
        assert_eq!(v.top, 1);
        // Repeating moves on to the uppercase TWO on line 3.
        v.apply_search(&sp("two"));
        assert_eq!(v.top, 3);
    }

    #[test]
    fn lazy_index_builds_on_demand() {
        // A file large enough that a single read can't swallow it, so a small
        // initial budget leaves the index genuinely partial. Fixed-width lines
        // ("00000000\n" = 9 bytes) make offsets predictable.
        const N: usize = 80_000; // ~720 KB
        let mut bytes = Vec::with_capacity(N * 9);
        for i in 0..N {
            bytes.extend_from_slice(format!("{i:08}\n").as_bytes());
        }
        let path = std::env::temp_dir().join(format!("rc_viewer_lazy_{}.txt", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();

        let make = |budget: usize| {
            let mut f = File::open(&path).unwrap();
            let len = f.metadata().unwrap().len() as usize;
            let (starts, scanned) = scan_line_starts(&mut f, budget).unwrap();
            ViewerState::from_scanned("t".into(), f, len, starts, scanned, None)
        };

        // Open with only a tiny budget: the index starts as a short prefix.
        let mut v = make(100 * 1024);
        assert!(!v.fully_indexed(), "opens without scanning the whole file");
        assert!(v.line_count() < N, "only a prefix is indexed ({})", v.line_count());
        assert_eq!(v.line_str(0), "00000000");

        // Scrolling/extension reveals deeper lines with correct content.
        v.extend_to_line(70_000);
        assert_eq!(v.line_str(70_000), format!("{:08}", 70_000));

        // Goto by line extends as far as needed.
        assert!(v.goto("75000", GotoMode::Line));
        assert_eq!(v.top, 74_999);
        assert_eq!(v.line_str(74_999), format!("{:08}", 74_999));

        // A byte-offset goto maps to the right line after extending.
        assert!(v.goto(&format!("{}", 9 * 60_000), GotoMode::DecimalOffset));
        assert_eq!(v.top, 60_000);

        // Finishing the index yields the exact total (N lines + the empty line
        // after the trailing newline).
        v.index_fully();
        assert!(v.fully_indexed());
        assert_eq!(v.line_count(), N + 1);
        assert_eq!(v.line_str(N - 1), format!("{:08}", N - 1));

        // A percentage jump triggers a full scan and lands on the last line.
        let mut v2 = make(100 * 1024);
        assert!(!v2.fully_indexed());
        assert!(v2.goto("100", GotoMode::Percent));
        assert!(v2.fully_indexed(), "percent jump finishes indexing");
        assert_eq!(v2.top, N);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn small_file_is_fully_indexed_on_open() {
        let path = std::env::temp_dir().join(format!("rc_viewer_small_{}.txt", std::process::id()));
        std::fs::write(&path, b"a\nb\nc\n").unwrap();
        let v = ViewerState::open_file("t".into(), path.clone(), None).unwrap();
        assert!(v.fully_indexed());
        assert_eq!(v.line_count(), 4); // a, b, c, trailing empty
        assert_eq!(v.line_str(1), "b");
        std::fs::remove_file(&path).ok();
    }

    /// A file-backed viewer over `lines` lines of a fresh temp file, with its
    /// local path recorded (so it can be followed) and a 10-row layout.
    fn followable(tag: &str, lines: usize) -> (ViewerState, PathBuf) {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir()
            .join(format!("rc_follow_{tag}_{}_{nanos}.txt", std::process::id()));
        std::fs::write(&path, many_lines(lines)).unwrap();
        let mut v = ViewerState::open_file("t.txt".into(), path.clone(), None).unwrap();
        v.set_local_path(path.clone());
        with_layout(&mut v);
        (v, path)
    }

    fn append(path: &Path, bytes: &[u8]) {
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(path).unwrap().write_all(bytes).unwrap();
    }

    fn press(v: &mut ViewerState, code: KeyCode) -> ViewerSignal {
        v.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn follow_picks_up_appended_lines_and_sticks_to_the_end() {
        let (mut v, path) = followable("grow", 50); // 51 line starts, 10 rows
        press(&mut v, KeyCode::Char('f'));
        assert!(v.following());
        assert_eq!(v.top, 41, "starting to follow jumps to the last page");
        assert!(!v.poll_follow(), "nothing changed, nothing to do");

        append(&path, b"more0\nmore1\nmore2\nmore3\nmore4\n");
        assert!(v.poll_follow());
        assert_eq!(v.line_count(), 56);
        assert_eq!(v.top, 46, "the view moves with the end");
        assert_eq!(v.line_str(54), "more4");
        assert_eq!(v.follow_status(), Some((false, 0)));

        press(&mut v, KeyCode::Char('f'));
        assert!(!v.following(), "f again stops following");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn scrolling_up_pauses_follow_and_end_resumes() {
        let (mut v, path) = followable("pause", 50);
        press(&mut v, KeyCode::Char('f'));
        press(&mut v, KeyCode::Up);
        assert_eq!(v.follow_status(), Some((true, 0)), "leaving the end pauses");

        append(&path, b"a\nb\nc\n");
        assert!(v.poll_follow());
        assert_eq!(v.top, 40, "a paused view stays where it was scrolled to");
        assert_eq!(v.follow_status(), Some((true, 3)), "and counts what arrived");

        press(&mut v, KeyCode::End);
        assert_eq!(v.follow_status(), Some((false, 0)), "back at the end resumes");
        assert_eq!(v.top, 44);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_partly_written_last_line_is_completed_by_the_next_poll() {
        let (mut v, path) = followable("partial", 0);
        std::fs::write(&path, b"abc").unwrap();
        press(&mut v, KeyCode::Char('f'));
        assert!(v.poll_follow());
        assert_eq!(v.line_str(0), "abc");
        append(&path, b"def\nxyz\n");
        assert!(v.poll_follow());
        assert_eq!(v.line_str(0), "abcdef");
        assert_eq!(v.line_str(1), "xyz");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_truncated_file_is_read_again_from_the_start() {
        let (mut v, path) = followable("trunc", 50);
        press(&mut v, KeyCode::Char('f'));
        std::fs::write(&path, b"fresh\nstart\n").unwrap();
        assert!(v.poll_follow());
        assert_eq!(v.line_count(), 3);
        assert_eq!(v.line_str(0), "fresh");
        assert_eq!(v.top, 0);
        std::fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_rotated_log_is_followed_to_the_new_file() {
        let (mut v, path) = followable("rotate", 50);
        press(&mut v, KeyCode::Char('f'));
        let rotated = path.with_extension("txt.1");
        std::fs::rename(&path, &rotated).unwrap();
        assert!(!v.poll_follow(), "a missing path keeps the old handle");
        std::fs::write(&path, b"after rotation\n").unwrap();
        assert!(v.poll_follow());
        assert_eq!(v.line_str(0), "after rotation");
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&rotated).ok();
    }

    #[test]
    fn only_a_local_file_can_be_followed() {
        let mut mem = ViewerState::new("t".into(), many_lines(5));
        press(&mut mem, KeyCode::Char('f'));
        assert!(!mem.following(), "in-memory text never grows");

        let (mut v, path) = followable("nopath", 5);
        v.path = None; // a temp copy of a remote file
        press(&mut v, KeyCode::Char('f'));
        assert!(!v.following());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn header_shows_follow_state_and_log_lines_take_their_level_colour() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let theme = crate::ui::theme::Theme::mc();
        let (mut v, path) = followable("header", 0);
        std::fs::write(&path, b"INFO up\nERROR down\n").unwrap();
        press(&mut v, KeyCode::Char('f'));
        v.poll_follow();
        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        t.draw(|f| render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let row = |y: u16| -> String { (0..b.area.width).map(|x| b[(x, y)].symbol()).collect() };
        assert!(row(0).contains("[Follow]"), "header: {:?}", row(0));
        assert!(row(2).starts_with("ERROR down"));
        assert_eq!(b[(0, 2)].fg, theme.error_fg, "an error line is drawn in the error colour");
        assert_ne!(b[(0, 1)].fg, theme.error_fg);
        std::fs::remove_file(&path).ok();
    }

    /// A blame of `lines` lines: the first half by an old commit, the second by
    /// a newer one, and the last line not committed yet.
    fn two_commit_blame(lines: usize) -> crate::git::blame::Blame {
        use crate::git::blame::{Blame, BlameCommit};
        let commit = |oid: &str, author: &str, time: i64| BlameCommit {
            oid: oid.to_string(),
            author: author.to_string(),
            time,
            summary: format!("by {author}"),
            path: "src/t.txt".to_string(),
        };
        let mut owners: Vec<u32> = (0..lines).map(|l| u32::from(l >= lines / 2)).collect();
        if let Some(last) = owners.last_mut() {
            *last = 2;
        }
        Blame {
            commits: vec![
                commit(&"a".repeat(40), "Ada", 1_600_000_000),
                commit(&"b".repeat(40), "Bob", 1_750_000_000),
                commit(&"0".repeat(40), "Not Committed Yet", 1_760_000_000),
            ],
            lines: owners,
            toplevel: PathBuf::from("/repo"),
        }
    }

    #[test]
    fn b_asks_for_a_blame_and_b_again_puts_it_away() {
        let (mut v, path) = followable("blamekey", 20);
        assert!(matches!(press(&mut v, KeyCode::Char('b')), ViewerSignal::StartBlame));
        v.begin_blame(7);
        assert!(v.awaits_blame(7) && !v.awaits_blame(6));
        assert!(matches!(press(&mut v, KeyCode::Char('b')), ViewerSignal::Stay));
        assert!(!v.awaits_blame(7), "b while loading forgets the request");

        v.set_blame(two_commit_blame(20));
        assert!(v.active_blame().is_some());
        press(&mut v, KeyCode::Char('b'));
        assert!(v.active_blame().is_none(), "b hides the column");

        let mut mem = ViewerState::new("t".into(), many_lines(5));
        assert!(matches!(press(&mut mem, KeyCode::Char('b')), ViewerSignal::Stay));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn the_blame_cursor_moves_by_line_and_page_and_keeps_in_view() {
        let (mut v, path) = followable("blamecursor", 100); // 101 lines, 10 rows
        v.set_blame(two_commit_blame(100));
        let cursor = |v: &ViewerState| v.active_blame().unwrap().1;
        for _ in 0..12 {
            press(&mut v, KeyCode::Down);
        }
        assert_eq!((cursor(&v), v.top), (12, 3), "scrolled just enough to show it");
        press(&mut v, KeyCode::PageUp);
        assert_eq!((cursor(&v), v.top), (3, 3));
        press(&mut v, KeyCode::End);
        assert_eq!((cursor(&v), v.top), (100, 91));
        press(&mut v, KeyCode::Home);
        assert_eq!((cursor(&v), v.top), (0, 0));

        // A click puts the cursor on the clicked line rather than paging.
        v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 30, 5));
        assert_eq!((cursor(&v), v.top), (4, 0));

        // In hex the column is put away, and the arrows scroll again.
        press(&mut v, KeyCode::F(4));
        assert!(v.active_blame().is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn enter_opens_the_commit_of_a_committed_line_only() {
        let (mut v, path) = followable("blameenter", 10);
        v.set_blame(two_commit_blame(10));
        press(&mut v, KeyCode::Down);
        let (root, oid, file) = v.blame_target().expect("a committed line");
        assert_eq!((root, oid, file), (PathBuf::from("/repo"), "a".repeat(40), "src/t.txt".into()));
        assert!(matches!(press(&mut v, KeyCode::Enter), ViewerSignal::OpenBlameCommit));

        press(&mut v, KeyCode::End);
        press(&mut v, KeyCode::Up); // the uncommitted last line of the file
        assert!(v.blame_target().is_none());
        assert!(matches!(press(&mut v, KeyCode::Enter), ViewerSignal::Stay));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn the_blame_column_labels_each_run_once_and_shades_by_age() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let theme = crate::ui::theme::Theme::mc();
        let (mut v, path) = followable("blamedraw", 6);
        v.set_blame(two_commit_blame(6));
        let mut t = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let mut draw = |v: &mut ViewerState| {
            t.draw(|f| render::render(f, f.area(), v, &theme, None)).unwrap();
            t.backend().buffer().clone()
        };
        let row = |b: &ratatui::buffer::Buffer, y: u16| -> String {
            (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
        };

        let b = draw(&mut v);
        assert!(row(&b, 1).starts_with("▌ Ada          2020-09-13 line0"), "{:?}", row(&b, 1));
        assert!(row(&b, 2).starts_with("▌                         line1"), "labelled once a run");
        assert!(row(&b, 4).starts_with("▌ Bob          2025-06-15 line3"), "{:?}", row(&b, 4));
        assert!(row(&b, 6).starts_with("▌ uncommitted             line5"), "{:?}", row(&b, 6));
        assert_ne!(b[(0, 1)].fg, b[(0, 4)].fg, "older and newer commits shade differently");
        assert_eq!(b[(79, 1)].bg, theme.cursor_inactive.bg.unwrap(), "the cursor line is a bar");
        assert!(row(&b, 0).contains("[Blame]") && row(&b, 0).contains("aaaaaaa  Ada"));

        // Moving the cursor into the middle of a run labels it there too.
        press(&mut v, KeyCode::Down);
        let b = draw(&mut v);
        assert!(row(&b, 2).starts_with("▌ Ada          2020-09-13 line1"), "{:?}", row(&b, 2));
        std::fs::remove_file(&path).ok();
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE }
    }

    /// Body at rows 1..11, F-key bar at row 12, as the renderer would record.
    fn with_layout(v: &mut ViewerState) {
        v.content_area = Rect::new(0, 1, 40, 10);
        v.footer_area = Rect::new(0, 12, 40, 1);
        v.view_rows = 10;
        v.view_cols = 40;
    }

    fn many_lines(n: usize) -> Vec<u8> {
        (0..n).map(|i| format!("line{i}\n")).collect::<String>().into_bytes()
    }

    #[test]
    fn click_below_center_pages_down_above_pages_up() {
        let mut v = ViewerState::new("t".into(), many_lines(100));
        with_layout(&mut v);
        assert_eq!(v.top, 0);
        // Center of the body is row 6; a click below it pages down (view_rows-1).
        v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 8));
        assert_eq!(v.top, 9, "click below center pages down");
        // A click above center pages back up.
        v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 2));
        assert_eq!(v.top, 0, "click above center pages up");
    }

    #[test]
    fn wheel_scrolls_the_view() {
        let mut v = ViewerState::new("t".into(), many_lines(100));
        with_layout(&mut v);
        v.handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
        assert_eq!(v.top, 3, "wheel down scrolls three lines");
        v.handle_mouse(mouse(MouseEventKind::ScrollUp, 5, 5));
        assert_eq!(v.top, 0, "wheel up scrolls three lines back");
    }

    #[test]
    fn end_scrolls_to_the_last_full_page_not_past_it() {
        let mut v = ViewerState::new("t".into(), many_lines(100)); // 101 line starts
        with_layout(&mut v); // 10 rows on screen
        let press = |v: &mut ViewerState, code| {
            v.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        };
        press(&mut v, KeyCode::End);
        assert_eq!(v.top, 91, "END keeps the last screenful in view (101 lines - 10 rows)");
        // No form of scrolling can push the view past the end of the file.
        press(&mut v, KeyCode::Down);
        assert_eq!(v.top, 91);
        press(&mut v, KeyCode::PageDown);
        assert_eq!(v.top, 91);
        v.handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
        assert_eq!(v.top, 91);
        // A goto beyond the end clamps the same way.
        assert!(v.goto("100", GotoMode::Percent));
        assert_eq!(v.top, 91);
        press(&mut v, KeyCode::Home);
        assert_eq!(v.top, 0);
    }

    #[test]
    fn end_accounts_for_wrapped_lines() {
        // 20 lines of 100 chars wrap to 3 rows each at 40 columns; the trailing
        // empty line is 1 row. From line 17 the tail is 3+3+3+1 = 10 rows —
        // exactly one screen — so that is as far down as the view may go.
        let data = format!("{}\n", "x".repeat(100)).repeat(20).into_bytes();
        let mut v = ViewerState::new("t".into(), data);
        with_layout(&mut v); // 10 rows × 40 cols
        v.wrap = true;
        let press = |v: &mut ViewerState, code| {
            v.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        };
        press(&mut v, KeyCode::End);
        assert_eq!(v.top, 17, "wrapped visual rows are counted, not logical lines");
        press(&mut v, KeyCode::Down);
        assert_eq!(v.top, 17, "cannot scroll past the end with wrap on");
        // Without wrap the same file scrolls one line per row: 21 lines - 10 rows.
        v.wrap = false;
        press(&mut v, KeyCode::End);
        assert_eq!(v.top, 11);
    }

    #[test]
    fn render_pulls_the_view_back_to_the_last_full_page() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut v = ViewerState::new("t".into(), many_lines(20)); // 21 line starts
        with_layout(&mut v);
        v.top = 20; // where the old END (or a shrunken window) could have left it
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(50, 12)).unwrap(); // 10 content rows
        t.draw(|f| crate::viewer::render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        assert_eq!(v.top, 11, "drawing clamps the view to the last full screen");
        let b = t.backend().buffer();
        let row: String = (0..b.area.width).map(|x| b[(x, 9)].symbol().to_string()).collect();
        assert!(row.contains("line19"), "the file's last text stays visible: {row:?}");
    }

    #[test]
    fn fkey_bar_click_runs_the_function() {
        let mut v = ViewerState::new("t".into(), many_lines(10));
        with_layout(&mut v);
        // Footer width 40, 10 labels → 4 cells each; F4 (Hex) spans cols 12-15.
        assert_eq!(v.mode, ViewMode::Text);
        v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 12, 12));
        assert_eq!(v.mode, ViewMode::Hex, "clicking F4 toggles hex mode");
        // F10 (Quit) spans cols 36-39 → closes.
        let sig = v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 36, 12));
        assert!(matches!(sig, ViewerSignal::Close), "clicking F10 quits");
    }

    #[test]
    fn f3_closes_the_viewer() {
        // F3 opens the viewer from the panels, so it must also close it (the
        // footer labels F3 as "Quit"); F10 and Esc remain close keys too.
        let mut v = ViewerState::new("t".into(), many_lines(10));
        with_layout(&mut v);
        let press = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(
            matches!(v.handle_key(press(KeyCode::F(3))), ViewerSignal::Close),
            "F3 closes the viewer"
        );
        let mut v = ViewerState::new("t".into(), many_lines(10));
        with_layout(&mut v);
        assert!(
            matches!(v.handle_key(press(KeyCode::F(10))), ViewerSignal::Close),
            "F10 still closes the viewer"
        );
    }

    /// `n` lines of exactly 5 bytes each ("aaaa\n"), so byte offsets are tidy.
    fn fixed_lines(n: usize) -> Vec<u8> {
        (0..n).map(|_| "aaaa\n").collect::<String>().into_bytes()
    }

    #[test]
    fn goto_text_mode_line_percent_and_offsets() {
        let mut v = ViewerState::new("t".into(), fixed_lines(20)); // 21 line starts
        assert!(v.goto("10", GotoMode::Line));
        assert_eq!(v.top, 9, "1-based line number");
        assert!(v.goto("50", GotoMode::Percent));
        assert_eq!(v.top, 10, "50% of 20 = line 10");
        assert!(v.goto("5", GotoMode::DecimalOffset));
        assert_eq!(v.top, 1, "byte 5 is the start of line 1");
        assert!(v.goto("a", GotoMode::HexOffset));
        assert_eq!(v.top, 2, "0x0a = byte 10 → line 2");
        assert!(v.goto("0x0F", GotoMode::HexOffset));
        assert_eq!(v.top, 3, "0x0f = byte 15 → line 3");
        // Out-of-range clamps; garbage is rejected.
        assert!(v.goto("9999", GotoMode::Line));
        assert_eq!(v.top, v.max_top());
        assert!(!v.goto("nope", GotoMode::DecimalOffset));
    }

    #[test]
    fn goto_hex_mode_uses_rows() {
        let mut v = ViewerState::new("t".into(), vec![0u8; 100]); // 7 rows of 16
        v.mode = ViewMode::Hex;
        assert!(v.goto("3", GotoMode::Line));
        assert_eq!(v.top, 2, "line number is a 16-byte row in hex mode");
        assert!(v.goto("32", GotoMode::DecimalOffset));
        assert_eq!(v.top, 2, "byte 32 is row 2");
        assert!(v.goto("20", GotoMode::HexOffset));
        assert_eq!(v.top, 2, "0x20 = 32 → row 2");
    }

    #[test]
    fn f5_and_goto_label_click_request_the_dialog() {
        let mut v = ViewerState::new("t".into(), fixed_lines(10));
        with_layout(&mut v);
        assert!(matches!(
            v.handle_key(super::KeyEvent::new(super::KeyCode::F(5), super::KeyModifiers::NONE)),
            ViewerSignal::OpenGoto
        ));
        // The "Goto" label is F5 (index 4): cols 16-19 on a 40-wide bar.
        assert!(matches!(
            v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 16, 12)),
            ViewerSignal::OpenGoto
        ));
    }

    #[test]
    fn hex_rows_count() {
        let v = ViewerState::new("t".into(), vec![0u8; 33]);
        assert_eq!(v.hex_rows(), 3); // ceil(33/16)
    }

    #[test]
    fn file_backed_viewer_pages_without_loading_all() {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("rc_view_{}_{nanos}", std::process::id()));
        std::fs::write(&path, b"alpha\nbeta\r\nNEEDLE here\ngamma").unwrap();

        let mut v = ViewerState::open_file("t".into(), path.clone(), Some(path.clone())).unwrap();
        // Index and on-demand reads work the same as the in-memory viewer.
        assert_eq!(v.line_count(), 4);
        assert_eq!(v.line_str(0), "alpha");
        assert_eq!(v.line_str(1), "beta"); // CR stripped
        assert!(matches!(v.src, Source::File { .. }), "uses a paged file source");

        // Search reads through the file (windowed), not a memory copy.
        v.apply_search(&sp("needle"));
        assert_eq!(v.top, 2, "case-insensitive match maps to its line");

        // Hex row reads the requested 16-byte window on demand.
        assert_eq!(&v.hex_row(0)[..5], b"alpha");

        // The temp file is removed when the viewer is dropped.
        drop(v);
        assert!(!path.exists(), "temp file cleaned up on close");
    }

    #[test]
    fn hex_color_tints_the_hash() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut v = ViewerState::new("t".into(), b"x #ff501a y".to_vec());
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(40, 6)).unwrap();
        t.draw(|f| crate::viewer::render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let hash = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .find(|&(x, y)| b[(x, y)].symbol() == "#")
            .expect("'#' rendered");
        assert_eq!(
            b[hash].fg,
            ratatui::style::Color::Rgb(0xff, 0x50, 0x1a),
            "hash tinted with its color"
        );
    }

    #[test]
    fn syntax_highlight_colors_the_body() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("rc_hl_{}_{nanos}.rs", std::process::id()));
        std::fs::write(&path, b"fn main() { let x = 1; }\n").unwrap();

        let mut v =
            ViewerState::open_file("a.rs".into(), path.clone(), Some(path.clone())).unwrap();
        v.enable_syntax(true);
        assert!(v.has_syntax(), "rust syntax should be detected");

        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(80, 6)).unwrap();
        t.draw(|f| render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();

        // Body is on row 1 (row 0 is the header). Collect text + distinct colors.
        let mut text = String::new();
        let mut colors = std::collections::HashSet::new();
        for x in 0..b.area.width {
            let cell = &b[(x, 1)];
            text.push_str(cell.symbol());
            colors.insert(format!("{:?}", cell.fg));
        }
        assert!(text.contains("fn main"), "code is rendered");
        assert!(colors.len() > 1, "highlighting uses more than one color");

        drop(v);
    }

    #[test]
    fn image_mode_renders_and_f8_toggles_raw() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut v = ViewerState::new("photo.png".into(), b"\x89PNG not-really".to_vec());
        let img = image::RgbaImage::from_pixel(8, 6, image::Rgba([20, 180, 90, 255]));
        let sig = crate::util::img::image_sig(&img);
        v.set_image(ViewerImage { img, sig, orig: (800, 600) });
        assert!(v.active_image().is_some(), "opens showing the image");

        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
        t.draw(|f| render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let all: String = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .map(|(x, y)| b[(x, y)].symbol().to_string())
            .collect();
        // Header names the image and its original dimensions; body has half-blocks.
        assert!(all.contains("Image") && all.contains("800×600"), "header: {all:?}");
        assert!(all.contains('▀'), "ascii image drawn (no graphics)");
        // The F-key bar offers the Image/Raw toggle on F8.
        assert_eq!(v.footer_labels()[7], "Raw");

        // F8 toggles to the raw view and back.
        let f8 = KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE);
        v.handle_key(f8);
        assert!(v.active_image().is_none(), "F8 hides the image");
        assert_eq!(v.footer_labels()[7], "Image");
        v.handle_key(f8);
        assert!(v.active_image().is_some(), "F8 shows it again");
    }

    /// A tetrahedron as a binary STL: four facets, enough to render a solid.
    fn stl_bytes() -> Vec<u8> {
        let p = [[0.0f32, 0.0, 0.0], [10.0, 0.0, 0.0], [5.0, 0.0, 8.6], [5.0, 9.0, 2.9]];
        let faces = [(0, 2, 1), (0, 1, 3), (1, 2, 3), (2, 0, 3)];
        let mut b = vec![0u8; 80];
        b.extend_from_slice(&(faces.len() as u32).to_le_bytes());
        for (i, j, k) in faces {
            b.extend_from_slice(&[0u8; 12]); // stored normal, ignored on read
            for v in [p[i], p[j], p[k]] {
                for c in v {
                    b.extend_from_slice(&c.to_le_bytes());
                }
            }
            b.extend_from_slice(&0u16.to_le_bytes());
        }
        b
    }

    /// A file with two obviously different halves: zeroes, then spread bytes.
    fn two_region_bytes() -> Vec<u8> {
        let mut d = vec![0u8; 1 << 17];
        for (i, b) in d.iter_mut().enumerate().skip(1 << 16) {
            *b = (i.wrapping_mul(2654435761) >> 13) as u8;
        }
        d
    }

    #[test]
    fn f4_cycles_text_hex_map_and_the_map_renders() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut v = ViewerState::new("disk.img".into(), two_region_bytes());
        let f4 = KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE);
        assert_eq!(v.mode, ViewMode::Text);
        assert_eq!(v.footer_labels()[3], "Hex");
        v.handle_key(f4);
        assert_eq!(v.mode, ViewMode::Hex);
        assert_eq!(v.footer_labels()[3], "Map");
        v.handle_key(f4);
        assert_eq!(v.mode, ViewMode::Map);
        assert!(v.active_map().is_some(), "entering the map builds it");
        v.handle_key(f4);
        assert_eq!(v.mode, ViewMode::Text, "and cycles back round");

        v.handle_key(f4);
        v.handle_key(f4);
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(60, 16)).unwrap();
        t.draw(|f| render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let all: String = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .map(|(x, y)| b[(x, y)].symbol().to_string())
            .collect();
        assert!(all.contains("Map") && all.contains("entropy"), "header: {all:?}");
        assert!(all.contains('▀'), "the map is drawn as cell art");
    }

    #[test]
    fn the_map_cursor_moves_and_enter_lands_the_hex_view_on_that_offset() {
        let mut v = ViewerState::new("disk.img".into(), two_region_bytes());
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        v.handle_key(key(KeyCode::F(4)));
        v.handle_key(key(KeyCode::F(4)));
        assert_eq!(v.mode, ViewMode::Map);
        v.map_cols = 64;

        assert_eq!(v.map_cursor(), 0);
        v.handle_key(key(KeyCode::Right));
        assert_eq!(v.map_cursor(), 1, "→ steps one cell");
        v.handle_key(key(KeyCode::Down));
        assert_eq!(v.map_cursor(), 65, "↓ steps a whole row");
        v.handle_key(key(KeyCode::Home));
        assert_eq!(v.map_cursor(), 0);
        v.handle_key(key(KeyCode::End));
        let last = v.active_map().unwrap().cells.len() - 1;
        assert_eq!(v.map_cursor(), last, "End goes to the end of the file");
        // And cannot be pushed past it.
        v.handle_key(key(KeyCode::Right));
        assert_eq!(v.map_cursor(), last);

        // Enter carries the cursor's offset into the hex view.
        v.handle_key(key(KeyCode::Home));
        v.handle_key(key(KeyCode::Down));
        let want = v.map_offset();
        assert!(want > 0);
        v.handle_key(key(KeyCode::Enter));
        assert_eq!(v.mode, ViewMode::Hex, "Enter switches to the bytes");
        assert_eq!(v.top * 16, want as usize, "and lands on that offset's row");
    }

    #[test]
    fn the_map_shows_the_two_halves_of_the_file_differently() {
        let mut v = ViewerState::new("disk.img".into(), two_region_bytes());
        v.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        v.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        let fp = v.active_map().expect("built");
        let half = fp.cells.len() / 2;
        assert!(fp.cells[..half - 1].iter().all(|c| c.density < 0.05), "the zero half is flat");
        assert!(fp.cells[half + 1..].iter().all(|c| c.density > 0.8), "the noise half is not");
    }

    #[test]
    fn f8_switches_the_map_between_density_and_byte_class() {
        let mut v = ViewerState::new("disk.img".into(), two_region_bytes());
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        v.handle_key(key(KeyCode::F(4)));
        v.handle_key(key(KeyCode::F(4)));
        assert!(!v.map_by_class());
        assert_eq!(v.footer_labels()[7], "Bytes");
        v.handle_key(key(KeyCode::F(8)));
        assert!(v.map_by_class());
        assert_eq!(v.footer_labels()[7], "Density");
    }

    #[test]
    fn an_empty_file_opens_the_map_without_panicking() {
        let mut v = ViewerState::new("empty".into(), Vec::new());
        v.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        v.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        assert_eq!(v.mode, ViewMode::Map);
        assert!(v.active_map().unwrap().cells.is_empty());
        // Every navigation key must be a no-op rather than an index panic.
        for k in [KeyCode::Right, KeyCode::Down, KeyCode::End, KeyCode::Home, KeyCode::Enter] {
            v.handle_key(KeyEvent::new(k, KeyModifiers::NONE));
        }
    }

    #[test]
    fn model_mode_renders_and_f8_toggles_raw() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let bytes = stl_bytes();
        let mut v = ViewerState::new("part.stl".into(), bytes.clone());
        let mesh = crate::mesh::load(&bytes, "part.stl").expect("parses");
        assert_eq!(mesh.tris.len(), 4);
        v.set_model(ViewerModel::new(mesh));
        assert!(v.active_model().is_some(), "opens showing the model");

        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
        t.draw(|f| render::render(f, f.area(), &mut v, &theme, None)).unwrap();
        let b = t.backend().buffer();
        let all: String = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .map(|(x, y)| b[(x, y)].symbol().to_string())
            .collect();
        // The header names the format and the triangle count; the body is cell art.
        assert!(all.contains("STL") && all.contains("4 triangles"), "header: {all:?}");
        assert!(all.contains('▀'), "half-block art drawn (no graphics)");
        assert_eq!(v.footer_labels()[7], "Raw");

        // F8 toggles to the raw bytes and back.
        let f8 = KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE);
        v.handle_key(f8);
        assert!(v.active_model().is_none(), "F8 hides the model");
        assert_eq!(v.footer_labels()[7], "Model");
        v.handle_key(f8);
        assert!(v.active_model().is_some(), "F8 shows it again");
    }

    #[test]
    fn model_keys_orbit_and_reset_rather_than_scrolling() {
        let bytes = stl_bytes();
        let mut v = ViewerState::new("part.stl".into(), bytes.clone());
        v.set_model(ViewerModel::new(crate::mesh::load(&bytes, "part.stl").unwrap()));
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let start = v.active_model().unwrap().cam;

        v.handle_key(key(KeyCode::Right));
        let m = v.active_model().unwrap();
        assert!((m.cam.yaw - start.yaw - ORBIT_YAW).abs() < 1e-5, "Right orbits, not scrolls");
        assert_eq!(v.top, 0, "and the document did not move");

        v.handle_key(key(KeyCode::Char('+')));
        assert!(v.active_model().unwrap().cam.dist < start.dist, "+ moves closer");

        v.handle_key(key(KeyCode::Home));
        let m = v.active_model().unwrap();
        assert!((m.cam.yaw - start.yaw).abs() < 1e-6 && (m.cam.dist - start.dist).abs() < 1e-6);

        // Showing the raw bytes hands the same keys back to the document.
        v.handle_key(key(KeyCode::F(8)));
        v.handle_key(key(KeyCode::Down));
        assert!(v.active_model().is_none());
    }

    #[test]
    fn dragging_orbits_the_model_and_the_wheel_zooms_it() {
        let bytes = stl_bytes();
        let mut v = ViewerState::new("part.stl".into(), bytes.clone());
        v.set_model(ViewerModel::new(crate::mesh::load(&bytes, "part.stl").unwrap()));
        let start = v.active_model().unwrap().cam;
        let at =
            |kind, col, row| MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE };
        v.handle_mouse(at(MouseEventKind::Down(MouseButton::Left), 20, 10));
        v.handle_mouse(at(MouseEventKind::Drag(MouseButton::Left), 30, 10));
        let m = v.active_model().unwrap();
        assert!(m.cam.yaw < start.yaw, "dragging right orbits the camera");
        assert_eq!(v.top, 0, "and does not scroll the document");

        v.handle_mouse(at(MouseEventKind::ScrollUp, 20, 10));
        assert!(v.active_model().unwrap().cam.dist < start.dist, "wheel zooms in");

        // Releasing ends the drag, so the next press starts a fresh one rather
        // than snapping the camera across the gap between them.
        v.handle_mouse(at(MouseEventKind::Up(MouseButton::Left), 30, 10));
        let held = v.active_model().unwrap().cam.yaw;
        v.handle_mouse(at(MouseEventKind::Drag(MouseButton::Left), 90, 10));
        assert!((v.active_model().unwrap().cam.yaw - held).abs() < 1e-6);
    }

    #[test]
    fn only_a_drag_on_the_displayed_model_counts_as_an_orbit() {
        let bytes = stl_bytes();
        let mut v = ViewerState::new("part.stl".into(), bytes.clone());
        v.set_model(ViewerModel::new(crate::mesh::load(&bytes, "part.stl").unwrap()));
        let at =
            |kind, col, row| MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE };
        assert!(!v.orbiting(), "nothing is pressed");
        v.handle_mouse(at(MouseEventKind::Down(MouseButton::Left), 20, 10));
        assert!(v.orbiting(), "a press on the model arms an orbit");
        v.handle_mouse(at(MouseEventKind::Up(MouseButton::Left), 20, 10));
        assert!(!v.orbiting(), "and releasing ends it");

        v.handle_mouse(at(MouseEventKind::Down(MouseButton::Left), 20, 10));
        v.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        assert!(!v.orbiting(), "the raw bytes have no camera to turn");
    }

    #[test]
    fn the_model_signature_tracks_the_camera_so_the_cache_refreshes() {
        let bytes = stl_bytes();
        let mut m = ViewerModel::new(crate::mesh::load(&bytes, "part.stl").unwrap());
        let before = m.sig();
        m.orbit(0.5, 0.0);
        assert_ne!(before, m.sig(), "an orbit must invalidate the encoded image");
        m.orbit(-0.5, 0.0);
        assert_eq!(before, m.sig(), "and returning to the same pose must not");
    }

    #[test]
    fn pitch_is_clamped_off_the_poles_where_the_camera_basis_degenerates() {
        let bytes = stl_bytes();
        let mut m = ViewerModel::new(crate::mesh::load(&bytes, "part.stl").unwrap());
        for _ in 0..200 {
            m.orbit(0.0, 1.0);
        }
        assert!(m.cam.pitch <= MODEL_PITCH && m.cam.eye().y.is_finite());
        for _ in 0..400 {
            m.orbit(0.0, -1.0);
        }
        assert!(m.cam.pitch >= -MODEL_PITCH && m.cam.eye().y.is_finite());
    }
}
