//! Disk-usage explorer: a full-screen treemap of the current directory's
//! subdirectories, sized by their on-disk usage. Symlinks are never followed or
//! counted — only real files contribute to a directory's size.

pub mod render;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use std::path::{Path, PathBuf};

/// One subdirectory box.
#[derive(Debug, Clone)]
pub struct DiskEntry {
    pub name: String,
    /// Total on-disk size of the subtree (bytes), excluding symlinks.
    pub size: u64,
    /// The largest files in this subtree (largest first), each with its path
    /// relative to this box's directory. Shown as a list inside the box.
    pub files: Vec<FileEntry>,
}

/// A single large file inside a box's subtree.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Path relative to the box's directory (e.g. `cache/blobs/ab12`).
    pub rel: String,
    pub size: u64,
}

/// Which half of the explorer the cursor is working in. `Tab` toggles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The treemap: arrows move between boxes and `Enter` descends into one.
    Boxes,
    /// The selected box's file list: arrows walk the rows and `Del` removes one.
    Files,
}

/// The axis a remembered travel line runs along — see [`DiskView::nav`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// What handling a key in the disk explorer asks the app to do.
pub enum DiskSignal {
    Stay,
    Close,
    /// (Re)scan `self.cwd` — the view already updated its cwd.
    Rescan,
    /// Exit and point the active file panel at this directory (Shift-Enter).
    GoTo(PathBuf),
    /// Ask to delete the file the cursor is on inside a box. `label` is the
    /// `dir/relative/path` shown in the confirmation.
    DeleteFile { path: PathBuf, label: String },
}

pub struct DiskView {
    pub cwd: PathBuf,
    pub entries: Vec<DiskEntry>,
    pub selected: usize,
    pub scanning: bool,
    /// Scan progress: immediate subdirectories sized (`done`) of the total.
    pub scan_done: usize,
    pub scan_total: usize,
    /// Bumps on every scan so stale background results can be ignored.
    pub generation: u64,
    /// Box rectangles from the last render, for spatial arrow navigation.
    pub rects: Vec<Rect>,
    /// Which half of the explorer has the cursor.
    pub focus: Focus,
    /// Which row of the selected box's file list the cursor is on. Only
    /// meaningful while [`Focus::Files`]; use [`DiskView::on_file`] to read it.
    pub file_sel: usize,
    /// The line the cursor is travelling along, and the axis it runs on: moving
    /// left/right keeps the row the run started from, up/down keeps the column —
    /// the way an editor remembers your column while you move through lines of
    /// different lengths. Re-derived from the current box whenever the axis
    /// changes or the selection moves by other means. Without it each hop picks
    /// its line afresh from whatever box it just landed on, so travelling right
    /// and then back left walks a different set of boxes.
    nav: Option<(Axis, f32)>,
    /// File-row rectangles from the last render as `(entry, file, rect)`: every
    /// file row actually drawn. Drives mouse hit-testing and bounds the cursor,
    /// so it can only step onto files that are really on screen.
    pub file_rects: Vec<(usize, usize, Rect)>,
}

impl DiskView {
    pub fn new(cwd: PathBuf) -> Self {
        DiskView {
            cwd,
            entries: Vec::new(),
            selected: 0,
            scanning: true,
            scan_done: 0,
            scan_total: 0,
            generation: 0,
            rects: Vec::new(),
            focus: Focus::Boxes,
            file_sel: 0,
            nav: None,
            file_rects: Vec::new(),
        }
    }

    /// Total size across all boxes (the current directory's subtree total).
    pub fn total(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DiskSignal {
        // Shift/Ctrl modifiers on Enter — only some terminals report these.
        let go_mod = key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        match key.code {
            // Esc backs out of the file list first, and only closes once the
            // cursor is back on the treemap. q/F10 always close outright.
            KeyCode::F(10) | KeyCode::Char('q') | KeyCode::Char('Q') => DiskSignal::Close,
            KeyCode::Esc => {
                if self.focus == Focus::Files {
                    self.focus = Focus::Boxes;
                    DiskSignal::Stay
                } else {
                    DiskSignal::Close
                }
            }
            KeyCode::Backspace => {
                if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
                    self.cwd = parent;
                    self.reset_cursor();
                    DiskSignal::Rescan
                } else {
                    DiskSignal::Stay
                }
            }
            // Tab moves the cursor between the treemap and the selected box's
            // file list — the only way in or out of the list, so the arrows stay
            // purely spatial in the treemap.
            KeyCode::Tab | KeyCode::BackTab => {
                self.toggle_focus();
                DiskSignal::Stay
            }
            // Ctrl/Shift-Enter (when the terminal reports the modifier) or 'g' as
            // a reliable fallback: leave the explorer at the selected directory.
            KeyCode::Enter if go_mod => self.go_to(),
            KeyCode::Char('g') | KeyCode::Char('G') => self.go_to(),
            // Enter dives into the selected box's directory, making it the new
            // root. The file list holds no directories, so it stays put.
            KeyCode::Enter => match self.focus {
                Focus::Boxes => self.descend(),
                Focus::Files => DiskSignal::Stay,
            },
            // Del removes the highlighted file, which only exists while the file
            // list has the cursor.
            KeyCode::Delete | KeyCode::F(8) => self.delete_request(),
            // Arrows walk whichever half has the cursor: rows in the file list,
            // boxes (or one box's nested boxes) in the treemap.
            KeyCode::Down | KeyCode::Up | KeyCode::Left | KeyCode::Right => {
                match self.focus {
                    Focus::Files => match key.code {
                        KeyCode::Down => self.step_file(1),
                        KeyCode::Up => self.step_file(-1),
                        _ => {} // ←/→ keep the cursor in the list
                    },
                    Focus::Boxes => self.move_selection(key.code),
                }
                DiskSignal::Stay
            }
            _ => DiskSignal::Stay,
        }
    }

    /// Put the cursor back on the first box, as after a directory change or a
    /// rescan — the entries it pointed into are gone, so every part of it (the
    /// box, the nested box, the file row and which half has focus) must reset
    /// together or it ends up pointing at the wrong thing.
    pub fn reset_cursor(&mut self) {
        self.selected = 0;
        self.focus = Focus::Boxes;
        self.file_sel = 0;
        self.nav = None;
    }

    /// Tab: swap the cursor between the treemap and the file list. Moving into
    /// the list is refused when the selected box has no files to show, so the
    /// cursor never lands somewhere invisible.
    fn toggle_focus(&mut self) {
        match self.focus {
            Focus::Boxes if self.files_shown() > 0 => {
                self.focus = Focus::Files;
                self.file_sel = self.file_sel.min(self.files_shown() - 1);
            }
            Focus::Boxes => {}
            Focus::Files => self.focus = Focus::Boxes,
        }
    }

    /// The file row the cursor is on, or `None` when the treemap has the cursor.
    pub fn on_file(&self) -> Option<usize> {
        (self.focus == Focus::Files).then_some(self.file_sel)
    }

    /// ↑/↓ within the selected box's file list, clamped to the rows on screen.
    fn step_file(&mut self, delta: isize) {
        let shown = self.files_shown();
        if shown == 0 {
            return;
        }
        let next = (self.file_sel as isize + delta).clamp(0, shown as isize - 1);
        self.file_sel = next as usize;
    }

    /// How many of the selected box's files the last frame actually drew — the
    /// cursor never steps onto a file that isn't visible.
    fn files_shown(&self) -> usize {
        let drawn = self.file_rects.iter().filter(|(e, _, _)| *e == self.selected).count();
        // A deletion shortens the list before the next frame redraws it, so the
        // rectangles can briefly outnumber the files they were drawn for.
        drawn.min(self.entries.get(self.selected).map_or(0, |e| e.files.len()))
    }

    /// The selected file's absolute path and its `dir/relative/path` label.
    pub fn selected_file(&self) -> Option<(PathBuf, String)> {
        let k = self.on_file()?;
        let entry = self.entries.get(self.selected)?;
        let file = entry.files.get(k)?;
        let path = self.cwd.join(&entry.name).join(&file.rel);
        Some((path, format!("{}/{}", entry.name, file.rel)))
    }

    /// The directory the cursor points at: the selected box.
    pub fn selected_dir(&self) -> Option<PathBuf> {
        let entry = self.entries.get(self.selected)?;
        Some(self.cwd.join(&entry.name))
    }

    fn delete_request(&self) -> DiskSignal {
        match self.selected_file() {
            Some((path, label)) => DiskSignal::DeleteFile { path, label },
            None => DiskSignal::Stay,
        }
    }

    /// Drop a just-deleted file from the box that listed it and shrink that box
    /// by its size, so the treemap and the file list reflect the deletion right
    /// away instead of waiting for a rescan.
    pub fn note_file_deleted(&mut self, path: &Path) {
        for (i, entry) in self.entries.iter_mut().enumerate() {
            let dir = self.cwd.join(&entry.name);
            let Some(k) = entry.files.iter().position(|f| dir.join(&f.rel) == path) else {
                continue;
            };
            let gone = entry.files.remove(k);
            entry.size = entry.size.saturating_sub(gone.size);
            // Keep the cursor where the deleted row was, clamping onto the last
            // row when the list has shrunk past it; once the list is empty the
            // cursor falls back to the treemap, since there is no row to be on.
            if i == self.selected {
                self.file_sel = self.file_sel.min(entry.files.len().saturating_sub(1));
                if entry.files.is_empty() {
                    self.focus = Focus::Boxes;
                }
            }
            return;
        }
    }

    fn go_to(&self) -> DiskSignal {
        match self.selected_dir() {
            Some(path) => DiskSignal::GoTo(path),
            None => DiskSignal::Stay,
        }
    }

    /// The entry whose box contains the screen point `(col, row)`, using the box
    /// rectangles recorded at the last render. `None` if the point misses every box.
    pub fn box_at(&self, col: u16, row: u16) -> Option<usize> {
        self.rects.iter().position(|r| {
            col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
        })
    }

    /// The `(entry, file)` whose drawn file row contains `(col, row)`, using the
    /// rectangles recorded at the last render.
    pub fn file_at(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        hit(&self.file_rects, col, row)
    }

    /// Point the cursor at what the mouse clicked: a file row focuses the list,
    /// bare box space selects the box. A click re-aims the cursor, so it also
    /// forgets the travel line the arrows were following.
    pub fn click(&mut self, i: usize, col: u16, row: u16) {
        self.selected = i;
        self.nav = None;
        match self.file_at(col, row).filter(|(e, _)| *e == i) {
            Some((_, k)) => {
                self.focus = Focus::Files;
                self.file_sel = k;
            }
            None => self.focus = Focus::Boxes,
        }
    }

    /// Dive into the selected box's directory, making it the new root and
    /// rescanning. Used by `Enter` and by the mouse's double-click.
    pub fn descend(&mut self) -> DiskSignal {
        match self.selected_dir() {
            Some(path) => {
                self.cwd = path;
                self.reset_cursor();
                DiskSignal::Rescan
            }
            None => DiskSignal::Stay,
        }
    }

    /// Move the cursor to the neighbouring box in the given direction, along the
    /// line the current run of arrow presses is travelling on (see [`Self::nav`]).
    fn move_selection(&mut self, dir: KeyCode) {
        if self.rects.len() != self.entries.len() || self.entries.is_empty() {
            return;
        }
        let axis = match dir {
            KeyCode::Left | KeyCode::Right => Axis::Horizontal,
            _ => Axis::Vertical,
        };
        let from = self.rects[self.selected.min(self.rects.len() - 1)];
        // Keep the line while the run stays on one axis; re-derive it from the
        // current box the moment the axis changes.
        let line = match self.nav {
            Some((a, v)) if a == axis => v,
            _ => match axis {
                Axis::Horizontal => center(from).1,
                Axis::Vertical => center(from).0,
            },
        };
        self.nav = Some((axis, line));
        if let Some(next) = neighbour(&self.rects, from, dir, line) {
            self.selected = next;
            self.file_sel = 0;
        }
    }
}

/// The box to step to from `from` in direction `dir`, travelling along `line`
/// (a row for horizontal moves, a column for vertical ones).
///
/// Boxes are ranked by whether `line` actually crosses them, then by how far the
/// line falls outside them, then by distance along the travel axis — measured
/// between facing *edges*, not centres. Centre-to-centre scoring made the move
/// depend on the size of the box you happened to be standing on, so stepping
/// right and then left again walked a different set of boxes.
fn neighbour(rects: &[Rect], from: Rect, dir: KeyCode, line: f32) -> Option<usize> {
    let mut best: Option<(f32, f32, usize)> = None;
    for (i, r) in rects.iter().enumerate() {
        if *r == from || r.width == 0 || r.height == 0 {
            continue;
        }
        // Gap between the box we're leaving and this one, along the travel axis.
        // Negative means it isn't past our edge, so it isn't in that direction.
        let along = match dir {
            KeyCode::Left => from.x as f32 - (r.x + r.width) as f32,
            KeyCode::Right => r.x as f32 - (from.x + from.width) as f32,
            KeyCode::Up => from.y as f32 - (r.y + r.height) as f32,
            KeyCode::Down => r.y as f32 - (from.y + from.height) as f32,
            _ => return None,
        };
        if along < -0.5 {
            continue;
        }
        // How far the travel line sits outside this box's span; 0 when it crosses.
        let (lo, hi) = match dir {
            KeyCode::Left | KeyCode::Right => (r.y as f32, (r.y + r.height) as f32),
            _ => (r.x as f32, (r.x + r.width) as f32),
        };
        let off = if line < lo {
            lo - line
        } else if line > hi {
            line - hi
        } else {
            0.0
        };
        let better = best.is_none_or(|(bo, ba, _)| {
            off.total_cmp(&bo).then(along.total_cmp(&ba)).is_lt()
        });
        if better {
            best = Some((off, along, i));
        }
    }
    best.map(|(_, _, i)| i)
}

/// The last `(entry, index)` whose recorded rectangle covers `(col, row)`.
/// Searched in reverse so a nested rectangle drawn over another one wins.
fn hit(rects: &[(usize, usize, Rect)], col: u16, row: u16) -> Option<(usize, usize)> {
    rects.iter().rev().find_map(|(e, k, r)| {
        (col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
            .then_some((*e, *k))
    })
}

fn center(r: Rect) -> (f32, f32) {
    (r.x as f32 + r.width as f32 / 2.0, r.y as f32 + r.height as f32 / 2.0)
}

/// Format bytes like `2.1 GB`, `512 MB`, `4.0 KB`, `123 B` (1024-based).
pub fn human_gb(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// How many of the largest files to remember per box, for the in-box listing.
const TOP_FILES: usize = 32;


/// Scan the immediate subdirectories of `dir`, computing each one's total
/// on-disk size and its largest files (symlinks are skipped, never followed).
/// Sorted largest-first.
#[allow(dead_code)] // convenience wrapper used by tests
pub fn scan_dir(dir: &Path) -> Vec<DiskEntry> {
    scan_dir_with(dir, |_, _| {})
}

/// Like [`scan_dir`], but invokes `progress(done, total)` after enumerating the
/// subdirectories (done = 0) and again as each one is sized, so a long scan can
/// drive a progress bar.
pub fn scan_dir_with(dir: &Path, mut progress: impl FnMut(usize, usize)) -> Vec<DiskEntry> {
    // First enumerate the immediate (non-symlink) subdirectories so we know the
    // total up front, then size them one at a time.
    let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for de in rd.flatten() {
            let Ok(ft) = de.file_type() else { continue };
            if ft.is_symlink() || !ft.is_dir() {
                continue;
            }
            subdirs.push((de.file_name().to_string_lossy().into_owned(), de.path()));
        }
    }
    let total = subdirs.len();
    progress(0, total);

    let mut out = Vec::with_capacity(total);
    for (i, (name, path)) in subdirs.into_iter().enumerate() {
        let (size, files) = subtree_stats(&path);
        out.push(DiskEntry { name, size, files });
        progress(i + 1, total);
    }
    out.sort_by(|a, b| b.size.cmp(&a.size).then(a.name.cmp(&b.name)));
    out
}

/// Recursive on-disk size of `path` plus its [`TOP_FILES`] largest files
/// (relative paths, largest first), excluding symlinks (not followed). A
/// bounded min-heap keeps memory flat regardless of how many files exist.
fn subtree_stats(path: &Path) -> (u64, Vec<FileEntry>) {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let mut total = 0u64;
    let mut heap: BinaryHeap<Reverse<(u64, String)>> = BinaryHeap::new();
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_file()
            && let Ok(meta) = entry.metadata()
        {
            let len = on_disk_len(&meta);
            total += len;
            let rel = entry
                .path()
                .strip_prefix(path)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .into_owned();
            heap.push(Reverse((len, rel)));
            if heap.len() > TOP_FILES {
                heap.pop(); // drop the current smallest
            }
        }
    }
    let mut files: Vec<FileEntry> = heap
        .into_iter()
        .map(|Reverse((size, rel))| FileEntry { rel, size })
        .collect();
    files.sort_by(|a, b| b.size.cmp(&a.size).then(a.rel.cmp(&b.rel)));
    (total, files)
}

/// Bytes a file occupies on disk: allocated blocks on Unix, apparent size else.
#[cfg(unix)]
fn on_disk_len(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn on_disk_len(meta: &std::fs::Metadata) -> u64 {
    meta.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_gb_formats() {
        assert_eq!(human_gb(0), "0 B");
        assert_eq!(human_gb(512), "512 B");
        assert_eq!(human_gb(1024), "1.0 KB");
        assert_eq!(human_gb(2_252_341_248), "2.1 GB");
    }

    /// A box with no files.
    fn e(name: &str, size: u64) -> DiskEntry {
        DiskEntry { name: name.into(), size, files: vec![] }
    }

    #[test]
    fn arrow_moves_to_spatial_neighbor() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut dv = DiskView::new(PathBuf::from("/tmp"));
        dv.scanning = false;
        dv.entries = vec![e("a", 1), e("b", 1), e("c", 1)];
        // Two side-by-side boxes plus one below the first.
        dv.rects = vec![
            Rect { x: 0, y: 0, width: 10, height: 5 },
            Rect { x: 10, y: 0, width: 10, height: 5 },
            Rect { x: 0, y: 5, width: 10, height: 5 },
        ];
        dv.selected = 0;
        dv.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(dv.selected, 1, "right moves to the box on the right");
        dv.selected = 0;
        dv.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(dv.selected, 2, "down moves to the box below");
    }

    #[test]
    fn enter_dives_and_backspace_goes_up() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut dv = DiskView::new(PathBuf::from("/tmp/work"));
        dv.scanning = false;
        dv.entries = vec![e("sub", 1)];
        dv.selected = 0;
        assert!(matches!(
            dv.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            DiskSignal::Rescan
        ));
        assert_eq!(dv.cwd, PathBuf::from("/tmp/work/sub"));
        assert!(matches!(
            dv.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            DiskSignal::Rescan
        ));
        assert_eq!(dv.cwd, PathBuf::from("/tmp/work"));
        // Shift-Enter, Ctrl-Enter and 'g' all ask the app to go to the dir.
        dv.entries = vec![e("sub", 1)];
        for key in [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        ] {
            match dv.handle_key(key) {
                DiskSignal::GoTo(p) => assert_eq!(p, PathBuf::from("/tmp/work/sub")),
                _ => panic!("{key:?} should produce GoTo"),
            }
        }
    }

    /// Arrows stay in the treemap: they move between boxes and never fall into
    /// the file list, which `Tab` is the only way into. Issue: ↓ hijacking the
    /// cursor made the directories hard to navigate.
    #[test]
    fn arrows_stay_in_the_treemap_and_tab_reaches_the_files() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let press = |dv: &mut DiskView, c| dv.handle_key(KeyEvent::new(c, KeyModifiers::NONE));

        let mut dv = DiskView::new(PathBuf::from("/tmp/work"));
        dv.scanning = false;
        dv.entries = vec![
            DiskEntry {
                name: "cache".into(),
                size: 300,
                files: vec![
                    FileEntry { rel: "blobs/big".into(), size: 200 },
                    FileEntry { rel: "small".into(), size: 100 },
                ],
            },
            e("docs", 10),
        ];
        dv.rects = vec![
            Rect { x: 0, y: 0, width: 20, height: 6 },
            Rect { x: 0, y: 6, width: 20, height: 6 },
        ];
        dv.file_rects = vec![
            (0, 0, Rect { x: 1, y: 4, width: 18, height: 1 }),
            (0, 1, Rect { x: 1, y: 5, width: 18, height: 1 }),
        ];

        // ↓ moves to the box below rather than into the first box's file list.
        assert_eq!(dv.on_file(), None, "the cursor starts on the box itself");
        press(&mut dv, KeyCode::Down);
        assert_eq!((dv.selected, dv.on_file()), (1, None), "↓ moved on to the next box");
        press(&mut dv, KeyCode::Up);
        assert_eq!(dv.selected, 0, "and ↑ comes back");

        // Tab is what reaches the list; arrows then walk it and stop at its ends.
        press(&mut dv, KeyCode::Tab);
        assert_eq!(dv.on_file(), Some(0), "Tab lands on the biggest file");
        press(&mut dv, KeyCode::Down);
        assert_eq!(dv.on_file(), Some(1));
        press(&mut dv, KeyCode::Down);
        assert_eq!((dv.selected, dv.on_file()), (0, Some(1)), "↓ stops at the last row");
        press(&mut dv, KeyCode::Up);
        assert_eq!(dv.on_file(), Some(0));
        press(&mut dv, KeyCode::Up);
        assert_eq!((dv.selected, dv.on_file()), (0, Some(0)), "↑ stops at the first row");

        // Tab (or Esc) hands the cursor back to the treemap.
        press(&mut dv, KeyCode::Tab);
        assert_eq!(dv.on_file(), None, "Tab returns to the boxes");
        press(&mut dv, KeyCode::Tab);
        assert!(matches!(press(&mut dv, KeyCode::Esc), DiskSignal::Stay));
        assert_eq!(dv.on_file(), None, "Esc backs out of the list without closing");

        // A box with no files to show refuses the cursor rather than hiding it.
        dv.selected = 1;
        press(&mut dv, KeyCode::Tab);
        assert_eq!(dv.on_file(), None, "nothing to focus in an empty list");
    }

    /// Travelling one way and back again must retrace the same boxes. The move
    /// keeps the row (or column) the run started on, so a wide box passed on the
    /// way out can't redirect the way back — centre-to-centre scoring used to let
    /// exactly that happen, and →→←← landed somewhere else entirely.
    #[test]
    fn arrow_travel_is_reversible() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let press = |dv: &mut DiskView, c| dv.handle_key(KeyEvent::new(c, KeyModifiers::NONE));

        let mut dv = DiskView::new(PathBuf::from("/tmp"));
        dv.scanning = false;
        dv.entries = (0..5).map(|i| e(&format!("b{i}"), 1)).collect();
        // A row of boxes of differing heights: the tall one in the middle is what
        // centre-based scoring used to snag the return trip on.
        dv.rects = vec![
            Rect { x: 0, y: 4, width: 10, height: 4 },
            Rect { x: 10, y: 0, width: 10, height: 12 },
            Rect { x: 20, y: 4, width: 10, height: 4 },
            Rect { x: 30, y: 4, width: 10, height: 4 },
            Rect { x: 0, y: 12, width: 40, height: 6 },
        ];
        dv.selected = 0;

        // Walk right to the end, remembering the path.
        let mut out = vec![dv.selected];
        for _ in 0..3 {
            press(&mut dv, KeyCode::Right);
            out.push(dv.selected);
        }
        assert_eq!(out, vec![0, 1, 2, 3], "→ steps through the row in order");

        // Walking back left must retrace it exactly.
        let mut back = vec![dv.selected];
        for _ in 0..3 {
            press(&mut dv, KeyCode::Left);
            back.push(dv.selected);
        }
        out.reverse();
        assert_eq!(back, out, "← retraces the same boxes it came through");

        // Changing axis re-aims the line, and the vertical trip reverses too.
        press(&mut dv, KeyCode::Down);
        assert_eq!(dv.selected, 4, "↓ drops to the box below");
        press(&mut dv, KeyCode::Up);
        assert_eq!(dv.selected, 0, "↑ comes straight back");
    }

    /// Del asks to delete the file under the cursor (and nothing while the
    /// treemap has it); the box shrinks and drops the row straight away.
    #[test]
    fn delete_targets_the_file_under_the_cursor() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let press = |dv: &mut DiskView, c| dv.handle_key(KeyEvent::new(c, KeyModifiers::NONE));

        let mut dv = DiskView::new(PathBuf::from("/tmp/work"));
        dv.scanning = false;
        dv.entries = vec![DiskEntry {
            name: "cache".into(),
            size: 300,
            files: vec![
                FileEntry { rel: "blobs/big".into(), size: 200 },
                FileEntry { rel: "small".into(), size: 100 },
            ],
        }];
        dv.rects = vec![Rect { x: 0, y: 0, width: 20, height: 6 }];
        dv.file_rects = vec![
            (0, 0, Rect { x: 1, y: 3, width: 18, height: 1 }),
            (0, 1, Rect { x: 1, y: 4, width: 18, height: 1 }),
        ];

        // A box is a directory, so Del does nothing until the list has the cursor.
        assert!(matches!(press(&mut dv, KeyCode::Delete), DiskSignal::Stay));
        press(&mut dv, KeyCode::Tab);
        let (path, label) = match press(&mut dv, KeyCode::Delete) {
            DiskSignal::DeleteFile { path, label } => (path, label),
            _ => panic!("Del on a file should ask to delete it"),
        };
        assert_eq!(path, PathBuf::from("/tmp/work/cache/blobs/big"));
        assert_eq!(label, "cache/blobs/big");

        // Once it's gone the box shrinks and the row disappears at once.
        dv.note_file_deleted(&path);
        assert_eq!(dv.entries[0].size, 100, "the box lost the deleted file's bytes");
        assert_eq!(dv.entries[0].files.len(), 1);
        assert_eq!(dv.on_file(), Some(0), "the cursor holds the row the file vacated");
        // Deleting the last one leaves nothing to point at.
        let last = dv.cwd.join("cache").join("small");
        dv.note_file_deleted(&last);
        assert!(dv.entries[0].files.is_empty());
        assert_eq!(dv.on_file(), None, "the cursor falls back onto the treemap");
    }

    /// A click inside a box picks the file row it landed on, not just the box.
    #[test]
    fn file_at_hit_tests_the_rows_inside_a_box() {
        let mut dv = DiskView::new(PathBuf::from("/tmp/work"));
        dv.file_rects = vec![
            (0, 0, Rect { x: 1, y: 3, width: 18, height: 1 }),
            (0, 1, Rect { x: 1, y: 4, width: 18, height: 1 }),
        ];
        assert_eq!(dv.file_at(5, 4), Some((0, 1)));
        assert_eq!(dv.file_at(5, 1), None, "the box header is not a file row");
        assert_eq!(dv.file_at(50, 4), None, "outside every row");
    }

    #[test]
    fn box_at_hit_tests_and_click_enters() {
        let mut dv = DiskView::new(PathBuf::from("/tmp/work"));
        dv.scanning = false;
        dv.entries = vec![e("a", 1), e("b", 1)];
        // Two side-by-side boxes.
        dv.rects = vec![
            Rect { x: 0, y: 0, width: 10, height: 5 },
            Rect { x: 10, y: 0, width: 10, height: 5 },
        ];
        assert_eq!(dv.box_at(3, 2), Some(0), "point inside the first box");
        assert_eq!(dv.box_at(15, 4), Some(1), "point inside the second box");
        assert_eq!(dv.box_at(25, 2), None, "a miss returns None");
        // Selecting a box (as a mouse click does) then entering it dives in.
        dv.selected = 1;
        assert!(matches!(dv.descend(), DiskSignal::Rescan));
        assert_eq!(dv.cwd, PathBuf::from("/tmp/work/b"));
        assert_eq!(dv.selected, 0, "selection resets after diving");
    }

    #[test]
    fn scan_excludes_symlinks_and_sizes_subdirs() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rc_disk_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(root.join("big/sub")).unwrap();
        std::fs::create_dir_all(root.join("small")).unwrap();
        std::fs::write(root.join("big/a.bin"), vec![0u8; 8000]).unwrap();
        std::fs::write(root.join("big/sub/b.bin"), vec![0u8; 4000]).unwrap();
        std::fs::write(root.join("small/c.bin"), vec![0u8; 100]).unwrap();

        let entries = scan_dir(&root);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["big", "small"], "sorted largest-first");
        assert!(entries[0].size >= 12000, "big counts its whole subtree");
        assert!(entries[0].size > entries[1].size);

        // The largest files are collected with paths relative to the box dir,
        // largest first (a.bin > sub/b.bin).
        let files = &entries[0].files;
        assert_eq!(files.len(), 2, "both files captured");
        assert_eq!(files[0].rel, "a.bin");
        assert_eq!(files[1].rel, "sub/b.bin");
        assert!(files[0].size >= files[1].size);

        // A symlinked directory must not appear as a box.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("big"), root.join("link")).unwrap();
            let entries = scan_dir(&root);
            assert!(
                !entries.iter().any(|e| e.name == "link"),
                "symlinked dir is skipped"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }
}
