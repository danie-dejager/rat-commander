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
    /// relative to this box's directory. Shown inside sufficiently large boxes.
    pub files: Vec<FileEntry>,
}

/// A single large file inside a box's subtree.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Path relative to the box's directory (e.g. `cache/blobs/ab12`).
    pub rel: String,
    pub size: u64,
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
    /// Which of the selected box's largest files the cursor is on, if the
    /// cursor has stepped inside the box; `None` when the box itself is
    /// selected. Entering a box with `Enter` still dives into the directory —
    /// this only picks out one of the files the box lists.
    pub file_sel: Option<usize>,
    /// File rectangles from the last render as `(entry, file, rect)`: every
    /// file cell/sub-box actually drawn. Drives mouse hit-testing and bounds
    /// the cursor, so it can only step onto files that are really on screen.
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
            file_sel: None,
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
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') | KeyCode::Char('Q') => {
                DiskSignal::Close
            }
            KeyCode::Backspace => {
                if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
                    self.cwd = parent;
                    self.selected = 0;
                    self.file_sel = None;
                    DiskSignal::Rescan
                } else {
                    DiskSignal::Stay
                }
            }
            // Ctrl/Shift-Enter (when the terminal reports the modifier) or 'g' as
            // a reliable fallback: leave the explorer at the selected directory.
            KeyCode::Enter if go_mod => self.go_to(),
            KeyCode::Char('g') | KeyCode::Char('G') => self.go_to(),
            KeyCode::Enter => self.enter_selected(),
            // Delete the file the cursor is on. Boxes are directories, so this
            // only fires once the cursor has stepped inside one.
            KeyCode::Delete | KeyCode::F(8) => self.delete_request(),
            // ↑/↓ walk into and through the selected box's file list before
            // moving on to the next box; ←/→ always move between boxes.
            KeyCode::Down => {
                self.step_files(1);
                DiskSignal::Stay
            }
            KeyCode::Up => {
                self.step_files(-1);
                DiskSignal::Stay
            }
            KeyCode::Left | KeyCode::Right => {
                self.file_sel = None;
                self.move_selection(key.code);
                DiskSignal::Stay
            }
            _ => DiskSignal::Stay,
        }
    }

    /// How many of the selected box's files the last frame actually drew — the
    /// cursor never steps onto a file that isn't visible.
    fn files_shown(&self) -> usize {
        let drawn = self.file_rects.iter().filter(|(e, _, _)| *e == self.selected).count();
        // A deletion shortens the list before the next frame redraws it, so the
        // rectangles can briefly outnumber the files they were drawn for.
        drawn.min(self.entries.get(self.selected).map_or(0, |e| e.files.len()))
    }

    /// ↑/↓: walk the selected box's file list, entering it from the box and
    /// leaving it again at either end (where the move continues to the box
    /// above/below, as before).
    fn step_files(&mut self, delta: isize) {
        let shown = self.files_shown();
        let next = match (self.file_sel, delta) {
            // Stepping down out of the box header and into its file list.
            (None, 1) if shown > 0 => Some(Some(0)),
            (Some(i), 1) if i + 1 < shown => Some(Some(i + 1)),
            (Some(0), -1) => Some(None), // back onto the box itself
            (Some(i), -1) => Some(Some(i - 1)),
            _ => None, // at either end: fall through to the box move
        };
        match next {
            Some(sel) => self.file_sel = sel,
            None => {
                self.file_sel = None;
                self.move_selection(if delta > 0 { KeyCode::Down } else { KeyCode::Up });
            }
        }
    }

    /// The selected file's absolute path and its `dir/relative/path` label.
    pub fn selected_file(&self) -> Option<(PathBuf, String)> {
        let k = self.file_sel?;
        let entry = self.entries.get(self.selected)?;
        let file = entry.files.get(k)?;
        let path = self.cwd.join(&entry.name).join(&file.rel);
        Some((path, format!("{}/{}", entry.name, file.rel)))
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
            // Keep the cursor where the deleted row was, or step back to the
            // box once its list has run out.
            if i == self.selected && let Some(sel) = self.file_sel {
                self.file_sel = match sel {
                    s if s >= entry.files.len() => entry.files.len().checked_sub(1),
                    s => Some(s),
                };
            }
            return;
        }
    }

    fn go_to(&self) -> DiskSignal {
        match self.entries.get(self.selected) {
            Some(e) => DiskSignal::GoTo(self.cwd.join(&e.name)),
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

    /// The `(entry, file)` whose drawn file cell/sub-box contains `(col, row)`,
    /// using the rectangles recorded at the last render.
    pub fn file_at(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        self.file_rects.iter().rev().find_map(|(e, k, r)| {
            (col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
                .then_some((*e, *k))
        })
    }

    /// Enter the currently-selected box (dive into that subdirectory). Used by the
    /// mouse (double-click) to mirror the Enter key.
    pub fn enter_selected(&mut self) -> DiskSignal {
        if let Some(e) = self.entries.get(self.selected) {
            self.cwd = self.cwd.join(&e.name);
            self.selected = 0;
            self.file_sel = None;
            DiskSignal::Rescan
        } else {
            DiskSignal::Stay
        }
    }

    /// Move the selection to the nearest box in the given direction, using the
    /// box centers from the last render.
    fn move_selection(&mut self, dir: KeyCode) {
        if self.rects.len() != self.entries.len() || self.entries.is_empty() {
            return;
        }
        let cur = center(self.rects[self.selected.min(self.rects.len() - 1)]);
        let mut best: Option<(f32, usize)> = None;
        for (i, r) in self.rects.iter().enumerate() {
            if i == self.selected {
                continue;
            }
            let c = center(*r);
            let (dx, dy) = (c.0 - cur.0, c.1 - cur.1);
            let in_dir = match dir {
                KeyCode::Left => dx < -0.5,
                KeyCode::Right => dx > 0.5,
                KeyCode::Up => dy < -0.5,
                KeyCode::Down => dy > 0.5,
                _ => false,
            };
            if !in_dir {
                continue;
            }
            // Distance along the travel axis, plus a penalty for drifting off it.
            let (primary, perp) = match dir {
                KeyCode::Left | KeyCode::Right => (dx.abs(), dy.abs()),
                _ => (dy.abs(), dx.abs()),
            };
            let score = primary + perp * 2.0;
            if best.is_none_or(|(b, _)| score < b) {
                best = Some((score, i));
            }
        }
        if let Some((_, i)) = best {
            self.selected = i;
            self.file_sel = None;
        }
    }
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

    #[test]
    fn arrow_moves_to_spatial_neighbor() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut dv = DiskView::new(PathBuf::from("/tmp"));
        dv.scanning = false;
        dv.entries = vec![
            DiskEntry { name: "a".into(), size: 1, files: vec![] },
            DiskEntry { name: "b".into(), size: 1, files: vec![] },
            DiskEntry { name: "c".into(), size: 1, files: vec![] },
        ];
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
        dv.entries = vec![DiskEntry { name: "sub".into(), size: 1, files: vec![] }];
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
        dv.entries = vec![DiskEntry { name: "sub".into(), size: 1, files: vec![] }];
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

    /// The cursor steps into the selected box's file list with ↓, walks it, and
    /// steps back out at either end — Enter still dives into the directory, and
    /// Del asks to remove the file the cursor is on. Issue #14.
    #[test]
    fn cursor_walks_the_files_listed_inside_a_box() {
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
            DiskEntry { name: "docs".into(), size: 10, files: vec![] },
        ];
        dv.rects = vec![
            Rect { x: 0, y: 0, width: 20, height: 6 },
            Rect { x: 0, y: 6, width: 20, height: 6 },
        ];
        // Two file rows drawn inside the first box (as the renderer would record).
        dv.file_rects = vec![
            (0, 0, Rect { x: 1, y: 3, width: 18, height: 1 }),
            (0, 1, Rect { x: 1, y: 4, width: 18, height: 1 }),
        ];

        // ↓ steps into the box's file list, then down it.
        assert_eq!(dv.file_sel, None, "the box itself starts selected");
        press(&mut dv, KeyCode::Down);
        assert_eq!(dv.file_sel, Some(0), "first ↓ steps onto the biggest file");
        press(&mut dv, KeyCode::Down);
        assert_eq!(dv.file_sel, Some(1));
        // Past the last file, ↓ leaves the box for the one below.
        press(&mut dv, KeyCode::Down);
        assert_eq!((dv.selected, dv.file_sel), (1, None), "moved on to the next box");

        // ↑ comes back, and walks the list in reverse before leaving it.
        press(&mut dv, KeyCode::Up);
        assert_eq!((dv.selected, dv.file_sel), (0, None), "back on the box itself");
        press(&mut dv, KeyCode::Down);
        press(&mut dv, KeyCode::Down);
        press(&mut dv, KeyCode::Up);
        assert_eq!(dv.file_sel, Some(0));
        press(&mut dv, KeyCode::Up);
        assert_eq!(dv.file_sel, None, "↑ off the top of the list re-selects the box");

        // ←/→ always move between boxes, dropping any file selection.
        press(&mut dv, KeyCode::Down);
        assert_eq!(dv.file_sel, Some(0));
        press(&mut dv, KeyCode::Right);
        assert_eq!(dv.file_sel, None, "sideways moves leave the file list");

        // Enter still dives into the directory, even with a file selected.
        dv.selected = 0;
        press(&mut dv, KeyCode::Down);
        assert!(matches!(press(&mut dv, KeyCode::Enter), DiskSignal::Rescan));
        assert_eq!(dv.cwd, PathBuf::from("/tmp/work/cache"));
    }

    /// Del asks to delete the file under the cursor (and nothing when the box
    /// itself is selected); the box shrinks and drops the row straight away.
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

        // A box is a directory, so Del does nothing until the cursor is on a file.
        assert!(matches!(press(&mut dv, KeyCode::Delete), DiskSignal::Stay));
        press(&mut dv, KeyCode::Down);
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
        assert_eq!(dv.file_sel, Some(0), "the cursor holds the row the file vacated");
        // Deleting the last one leaves nothing to point at.
        let last = dv.cwd.join("cache").join("small");
        dv.note_file_deleted(&last);
        assert!(dv.entries[0].files.is_empty());
        assert_eq!(dv.file_sel, None, "the cursor falls back onto the box");
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
        dv.entries = vec![
            DiskEntry { name: "a".into(), size: 1, files: vec![] },
            DiskEntry { name: "b".into(), size: 1, files: vec![] },
        ];
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
        assert!(matches!(dv.enter_selected(), DiskSignal::Rescan));
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
