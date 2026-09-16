//! Byte-by-byte comparison of two binary files: both shown side by side as hex
//! and ASCII, paged from disk however large they are, every differing byte
//! coloured. A scan in the background finds the runs of differences, so the
//! next or previous one is a keypress away while it is still going.
//!
//! Bytes are compared at the same offset: a byte inserted in one file shifts
//! everything after it, and the rest reads as different.

use crate::bt::source::{ByteSource, FileSource};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const BYTES_PER_ROW: u64 = 16;
/// How much of a file's start decides whether it is binary.
const SNIFF: usize = 8 << 10;
/// Bytes the scan compares at a time.
const CHUNK: usize = 1 << 20;
/// Differences this close together are one run, stepped over as one.
const GAP: u64 = 16;
/// The scan stops once it has found this many runs.
const MAX_RUNS: usize = 1_000_000;

/// Whether data starting with `head` is binary: a NUL among its first bytes,
/// as git decides.
pub fn is_binary(head: &[u8]) -> bool {
    head[..head.len().min(SNIFF)].contains(&0)
}

/// Whether the file at `path` is binary, by its first bytes.
pub fn file_is_binary(path: &Path) -> io::Result<bool> {
    let mut head = Vec::with_capacity(SNIFF);
    std::fs::File::open(path)?.take(SNIFF as u64).read_to_end(&mut head)?;
    Ok(is_binary(&head))
}

/// Where one side's bytes come from.
#[derive(Clone)]
pub enum Origin {
    File(PathBuf),
    Mem(Arc<[u8]>),
}

/// One side's bytes as drawn: a file read a page at a time, or data in memory.
enum Source {
    File(FileSource),
    Mem(Arc<[u8]>),
}

impl Source {
    fn open(origin: &Origin) -> io::Result<Source> {
        Ok(match origin {
            Origin::File(p) => Source::File(FileSource::open(p, BTreeMap::new())?),
            Origin::Mem(d) => Source::Mem(d.clone()),
        })
    }

    fn len(&self) -> u64 {
        match self {
            Source::File(f) => f.len(),
            Source::Mem(d) => d.len() as u64,
        }
    }

    /// The bytes from `off` on, up to `n` (fewer at the end).
    fn read(&mut self, off: u64, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let got = match self {
            Source::File(f) => f.read_at(off, &mut buf),
            Source::Mem(d) => {
                let start = usize::try_from(off).unwrap_or(usize::MAX).min(d.len());
                let k = (d.len() - start).min(n);
                buf[..k].copy_from_slice(&d[start..start + k]);
                k
            }
        };
        buf.truncate(got);
        buf
    }
}

/// What the scan has found so far, shared with its thread.
#[derive(Default)]
struct Scan {
    /// Runs of differing bytes, `(start, end)` with `end` exclusive, in order.
    runs: Mutex<Vec<(u64, u64)>>,
    /// Every difference before this offset is in `runs`.
    done_to: AtomicU64,
    finished: AtomicBool,
    /// It stopped at [`MAX_RUNS`].
    capped: AtomicBool,
    cancel: AtomicBool,
    error: Mutex<Option<String>>,
}

/// A side read front to back by the scan.
enum Reader {
    File(std::fs::File),
    Mem(Arc<[u8]>, usize),
}

impl Reader {
    fn open(origin: &Origin) -> io::Result<Reader> {
        Ok(match origin {
            Origin::File(p) => Reader::File(std::fs::File::open(p)?),
            Origin::Mem(d) => Reader::Mem(d.clone(), 0),
        })
    }

    /// The next `buf.len()` bytes.
    fn next(&mut self, buf: &mut [u8]) -> io::Result<()> {
        match self {
            Reader::File(f) => f.read_exact(buf),
            Reader::Mem(d, at) => {
                let src = d
                    .get(*at..*at + buf.len())
                    .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
                buf.copy_from_slice(src);
                *at += buf.len();
                Ok(())
            }
        }
    }
}

/// Add the `n` differing bytes at `at` to the run being built, or close it and
/// start another when they are too far from it.
fn add_run(open: &mut Option<(u64, u64)>, found: &mut Vec<(u64, u64)>, at: u64, n: u64) {
    match open {
        Some((_, end)) if at <= *end + GAP => *end = at + n,
        _ => {
            if let Some(run) = open.replace((at, at + n)) {
                found.push(run);
            }
        }
    }
}

/// Hand the runs found to the view. `false` once there are too many to go on.
fn publish(scan: &Scan, found: &mut Vec<(u64, u64)>) -> bool {
    if found.is_empty() {
        return true;
    }
    let mut runs = scan.runs.lock().unwrap_or_else(|e| e.into_inner());
    let room = MAX_RUNS.saturating_sub(runs.len());
    if found.len() > room {
        runs.extend(found.drain(..room));
        found.clear();
        scan.capped.store(true, Ordering::Relaxed);
        return false;
    }
    runs.append(found);
    true
}

/// Compare the two sides chunk by chunk, publishing runs as they close.
fn run_scan(left: &Origin, right: &Origin, lens: [u64; 2], scan: &Scan) -> io::Result<()> {
    let mut a = Reader::open(left)?;
    let mut b = Reader::open(right)?;
    let common = lens[0].min(lens[1]);
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let mut open = None;
    let mut found = Vec::new();
    let mut off = 0u64;
    while off < common {
        if scan.cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let n = (common - off).min(CHUNK as u64) as usize;
        a.next(&mut ba[..n])?;
        b.next(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            for (i, (x, y)) in ba[..n].iter().zip(&bb[..n]).enumerate() {
                if x != y {
                    add_run(&mut open, &mut found, off + i as u64, 1);
                }
            }
        }
        off += n as u64;
        // The open run may still grow into the next chunk; the rest can't.
        if !publish(scan, &mut found) {
            return Ok(());
        }
        let settled = open.map_or(off, |(start, _)| start);
        scan.done_to.store(settled, Ordering::Relaxed);
    }
    // What one file has past the other's end differs as a whole.
    let longest = lens[0].max(lens[1]);
    if longest > common {
        add_run(&mut open, &mut found, common, longest - common);
    }
    found.extend(open);
    publish(scan, &mut found);
    scan.done_to.store(longest, Ordering::Relaxed);
    Ok(())
}

/// What the app should do after the view handles a key.
#[derive(Debug, PartialEq, Eq)]
pub enum HexDiffSignal {
    Stay,
    Close,
    /// Ask for an offset to go to.
    Goto,
}

/// How the two files share the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Hex and ASCII of each, side by side.
    Wide,
    /// Only the hex of each, side by side.
    HexOnly,
    /// One above the other.
    Stacked,
}

pub struct HexDiffView {
    pub names: [String; 2],
    sources: [Source; 2],
    lens: [u64; 2],
    scan: Arc<Scan>,
    /// The scan has finished and been drawn so.
    seen_finished: bool,
    pub cursor: u64,
    /// The first row shown, as a byte offset.
    pub(super) top: u64,
    /// Rows of bytes on screen (per file when stacked).
    pub(super) view_rows: usize,
    /// A step to the next (`true`) or previous difference waiting on the scan.
    waiting: Option<bool>,
    pub status: String,
    /// Where the rows were drawn and how, for the mouse.
    pub(super) body: Rect,
    pub(super) layout: Layout,
}

impl Drop for HexDiffView {
    fn drop(&mut self) {
        self.scan.cancel.store(true, Ordering::Relaxed);
    }
}

impl HexDiffView {
    /// Compare `names`' bytes from `origins`, starting the scan.
    pub fn open(names: [String; 2], origins: [Origin; 2]) -> io::Result<Self> {
        let sources = [Source::open(&origins[0])?, Source::open(&origins[1])?];
        let lens = [sources[0].len(), sources[1].len()];
        let v = Self::new(names, sources, lens);
        let shared = v.scan.clone();
        std::thread::Builder::new().name("hexdiff-scan".into()).spawn(move || {
            if let Err(e) = run_scan(&origins[0], &origins[1], lens, &shared) {
                *shared.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
            }
            shared.finished.store(true, Ordering::Relaxed);
        })?;
        Ok(v)
    }

    fn new(names: [String; 2], sources: [Source; 2], lens: [u64; 2]) -> Self {
        let scan = Arc::new(Scan::default());
        HexDiffView {
            names,
            sources,
            lens,
            scan,
            seen_finished: false,
            cursor: 0,
            top: 0,
            view_rows: 1,
            waiting: None,
            status: String::new(),
            body: Rect::default(),
            layout: Layout::Wide,
        }
    }

    /// The longer file's length.
    pub fn len(&self) -> u64 {
        self.lens[0].max(self.lens[1])
    }

    pub fn lens(&self) -> [u64; 2] {
        self.lens
    }

    /// Whether the scan is still going (the view wants ticks).
    pub fn busy(&self) -> bool {
        !self.seen_finished
    }

    /// How far the scan has got, in percent.
    pub fn progress(&self) -> u64 {
        let len = self.len();
        if len == 0 {
            return 100;
        }
        (self.scan.done_to.load(Ordering::Relaxed).min(len) as u128 * 100 / len as u128) as u64
    }

    /// The number of runs found, whether the scan stopped counting, and the
    /// index of the run the cursor is in.
    pub fn runs_status(&self) -> (usize, bool, Option<usize>) {
        let runs = self.scan.runs.lock().unwrap_or_else(|e| e.into_inner());
        let i = runs.partition_point(|r| r.0 <= self.cursor);
        let inside = i.checked_sub(1).filter(|&k| runs[k].1 > self.cursor);
        (runs.len(), self.scan.capped.load(Ordering::Relaxed), inside)
    }

    /// Why the scan failed, if it did.
    pub fn error(&self) -> Option<String> {
        self.scan.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// On the tick: follow the scan, and take a step that was waiting on it.
    /// Returns whether there is anything new to draw.
    pub fn poll(&mut self) -> bool {
        if self.seen_finished {
            return false;
        }
        if let Some(forward) = self.waiting {
            self.step(forward);
        }
        if self.scan.finished.load(Ordering::Relaxed) {
            self.seen_finished = true;
            if let Some(e) = self.error() {
                self.status = e;
            }
        }
        true
    }

    /// The bytes of each file from `off` on, up to `n`.
    pub(super) fn rows(&mut self, off: u64, n: usize) -> [Vec<u8>; 2] {
        let [a, b] = &mut self.sources;
        [a.read(off, n), b.read(off, n)]
    }

    /// Put the cursor on `off` (clamped to the files), a third of the way down
    /// the screen when it has to scroll.
    pub fn goto(&mut self, off: u64) {
        self.cursor = off.min(self.len().saturating_sub(1));
        let row = self.cursor / BYTES_PER_ROW;
        let rows = self.view_rows.max(1) as u64;
        let top_row = self.top / BYTES_PER_ROW;
        if row < top_row || row >= top_row + rows {
            self.top = row.saturating_sub(rows / 3) * BYTES_PER_ROW;
        }
    }

    /// Go to the next (`forward`) or previous run of differences — or, while
    /// the scan hasn't got that far yet, once it has.
    fn step(&mut self, forward: bool) {
        let finished = self.scan.finished.load(Ordering::Relaxed);
        let done_to = self.scan.done_to.load(Ordering::Relaxed);
        let target = {
            let runs = self.scan.runs.lock().unwrap_or_else(|e| e.into_inner());
            if forward {
                runs.get(runs.partition_point(|r| r.0 <= self.cursor)).map(|r| r.0)
            } else if !finished && done_to < self.cursor {
                None
            } else {
                runs.partition_point(|r| r.0 < self.cursor).checked_sub(1).map(|k| runs[k].0)
            }
        };
        match target {
            Some(off) => {
                self.waiting = None;
                self.goto(off);
            }
            None if !finished => self.waiting = Some(forward),
            None => {
                self.waiting = None;
                self.status = if forward {
                    "No more differences".into()
                } else {
                    "No earlier differences".into()
                };
            }
        }
    }

    /// Whether a step is waiting on the scan.
    pub fn waiting(&self) -> bool {
        self.waiting.is_some()
    }

    fn move_by(&mut self, delta: i64) {
        let last = self.len().saturating_sub(1);
        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as u64).min(last)
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> HexDiffSignal {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.status.clear();
        let page = (self.view_rows.max(2) as i64 - 1) * BYTES_PER_ROW as i64;
        let row = BYTES_PER_ROW as i64;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return HexDiffSignal::Close,
            KeyCode::F(5) | KeyCode::Char('g') => return HexDiffSignal::Goto,
            KeyCode::Down if ctrl => return self.stepped(true),
            KeyCode::Up if ctrl => return self.stepped(false),
            KeyCode::Char('n') => return self.stepped(true),
            KeyCode::Char('N') => return self.stepped(false),
            _ => {}
        }
        self.waiting = None;
        match key.code {
            KeyCode::Up => self.move_by(-row),
            KeyCode::Down => self.move_by(row),
            KeyCode::Left => self.move_by(-1),
            KeyCode::Right => self.move_by(1),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::Home if ctrl => self.cursor = 0,
            KeyCode::End if ctrl => self.cursor = self.len().saturating_sub(1),
            KeyCode::Home => self.cursor -= self.cursor % BYTES_PER_ROW,
            KeyCode::End => {
                let end = self.cursor - self.cursor % BYTES_PER_ROW + BYTES_PER_ROW - 1;
                self.cursor = end.min(self.len().saturating_sub(1));
            }
            _ => {}
        }
        HexDiffSignal::Stay
    }

    fn stepped(&mut self, forward: bool) -> HexDiffSignal {
        self.step(forward);
        HexDiffSignal::Stay
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::ScrollUp => self.move_by(-3 * BYTES_PER_ROW as i64),
            MouseEventKind::ScrollDown => self.move_by(3 * BYTES_PER_ROW as i64),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(off) = super::hexrender::offset_at(self, ev.column, ev.row) {
                    self.cursor = off;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(bytes: &[u8]) -> Origin {
        Origin::Mem(bytes.to_vec().into())
    }

    fn scanned(a: &[u8], b: &[u8]) -> HexDiffView {
        let mut v = HexDiffView::open(["a".into(), "b".into()], [mem(a), mem(b)]).unwrap();
        while v.poll() {
            std::thread::yield_now();
        }
        v
    }

    fn runs(v: &HexDiffView) -> Vec<(u64, u64)> {
        v.scan.runs.lock().unwrap().clone()
    }

    #[test]
    fn binary_means_a_nul_near_the_start() {
        assert!(is_binary(b"\x7fELF\x02\x01\x01\x00"));
        assert!(!is_binary("plain text, even ünïcode\n".as_bytes()));
        let mut late = vec![b'a'; SNIFF];
        late.push(0);
        assert!(!is_binary(&late), "only the first 8 KiB decide");
    }

    #[test]
    fn nearby_differences_make_one_run_and_the_longer_tail_another() {
        let a = vec![0u8; 100];
        let mut b = a.clone();
        b[10] = 1;
        b[20] = 1; // within 16 bytes of the one before: the same run
        b[60] = 1;
        b.extend([7, 7, 7]);
        let v = scanned(&a, &b);
        assert_eq!(runs(&v), vec![(10, 21), (60, 61), (100, 103)]);
        assert_eq!(v.progress(), 100);
        assert_eq!(scanned(&a, &a).runs_status().0, 0, "identical files");
    }

    #[test]
    fn runs_join_across_the_scans_chunks() {
        let a = vec![0u8; CHUNK + 64];
        let mut b = a.clone();
        b[CHUNK - 2] = 1;
        b[CHUNK + 3] = 1;
        let v = scanned(&a, &b);
        assert_eq!(runs(&v), vec![((CHUNK - 2) as u64, (CHUNK + 4) as u64)]);
    }

    #[test]
    fn the_scan_stops_counting_at_the_cap() {
        let scan = Scan::default();
        let mut found: Vec<(u64, u64)> =
            (0..MAX_RUNS as u64 + 5).map(|i| (i * 20, i * 20 + 1)).collect();
        assert!(!publish(&scan, &mut found));
        assert_eq!(scan.runs.lock().unwrap().len(), MAX_RUNS);
        assert!(scan.capped.load(Ordering::Relaxed));
    }

    #[test]
    fn files_on_disk_compare_like_memory() {
        let dir = std::env::temp_dir();
        let pa = dir.join(format!("rc_hexdiff_a_{}", std::process::id()));
        let pb = dir.join(format!("rc_hexdiff_b_{}", std::process::id()));
        let a: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut b = a.clone();
        b[4000] ^= 0xff;
        std::fs::write(&pa, &a).unwrap();
        std::fs::write(&pb, &b).unwrap();
        assert!(!file_is_binary(&pa).unwrap() || a[..SNIFF.min(a.len())].contains(&0));
        let mut v = HexDiffView::open(
            ["a".into(), "b".into()],
            [Origin::File(pa.clone()), Origin::File(pb.clone())],
        )
        .unwrap();
        while v.poll() {
            std::thread::yield_now();
        }
        assert_eq!(runs(&v), vec![(4000, 4001)]);
        assert_eq!(
            v.rows(3999, 3),
            [vec![a[3999], a[4000], a[4001]], vec![b[3999], b[4000], b[4001]]]
        );
        std::fs::remove_file(pa).ok();
        std::fs::remove_file(pb).ok();
    }

    #[test]
    fn stepping_goes_from_run_to_run_and_says_when_there_are_no_more() {
        let a = vec![0u8; 400];
        let mut b = a.clone();
        b[50] = 1;
        b[300] = 1;
        let mut v = scanned(&a, &b);
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let ctrl = |c| KeyEvent::new(c, KeyModifiers::CONTROL);
        v.handle_key(ctrl(KeyCode::Down));
        assert_eq!(v.cursor, 50);
        assert_eq!(v.runs_status(), (2, false, Some(0)));
        v.handle_key(key(KeyCode::Char('n')));
        assert_eq!(v.cursor, 300);
        v.handle_key(key(KeyCode::Char('n')));
        assert_eq!((v.cursor, v.status.as_str()), (300, "No more differences"));
        v.handle_key(ctrl(KeyCode::Up));
        assert_eq!(v.cursor, 50);
        v.handle_key(key(KeyCode::Char('N')));
        assert_eq!(v.status, "No earlier differences");
        // Plain movement, clamped to the longer file.
        v.handle_key(ctrl(KeyCode::End));
        assert_eq!(v.cursor, 399);
        v.handle_key(key(KeyCode::Home));
        assert_eq!(v.cursor, 384);
        v.handle_key(key(KeyCode::Right));
        v.handle_key(key(KeyCode::Down));
        assert_eq!(v.cursor, 399);
        assert_eq!(v.handle_key(key(KeyCode::F(5))), HexDiffSignal::Goto);
        assert_eq!(v.handle_key(key(KeyCode::Esc)), HexDiffSignal::Close);
    }

    #[test]
    fn a_step_past_the_scan_waits_for_it() {
        // A view whose scan is driven by hand.
        let data: Arc<[u8]> = vec![0u8; 256].into();
        let sources = [Source::Mem(data.clone()), Source::Mem(data)];
        let mut v = HexDiffView::new(["a".into(), "b".into()], sources, [256, 256]);
        v.cursor = 100;
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        v.handle_key(key(KeyCode::Char('n')));
        assert!(v.waiting(), "nothing found past the cursor yet");
        v.handle_key(key(KeyCode::Char('N')));
        assert!(v.waiting(), "nor scanned up to it");
        assert!(v.poll());
        assert_eq!(v.cursor, 100);
        // The scan reaches a run past the cursor: the waiting step takes it.
        v.handle_key(key(KeyCode::Char('n')));
        v.scan.runs.lock().unwrap().push((180, 190));
        v.scan.done_to.store(180, Ordering::Relaxed);
        v.poll();
        assert_eq!((v.cursor, v.waiting()), (180, false));
        // Moving on drops a step still waiting.
        v.handle_key(key(KeyCode::Char('n')));
        assert!(v.waiting());
        v.handle_key(key(KeyCode::Left));
        assert!(!v.waiting());
        // Once the scan is done a step that finds nothing says so.
        v.scan.finished.store(true, Ordering::Relaxed);
        v.handle_key(key(KeyCode::Char('n')));
        assert_eq!(v.cursor, 180, "back onto the run just past the cursor");
        v.handle_key(key(KeyCode::Char('n')));
        assert_eq!(v.status, "No more differences");
        assert!(v.poll(), "the finish is drawn once");
        assert!(!v.poll() && !v.busy());
    }
}
