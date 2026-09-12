//! The Activity log: a panel view format listing, live, what is being created,
//! written, removed and renamed anywhere under the *other* panel's directory —
//! "what is this installer writing?", "what does this build touch?".
//!
//! It is fed from the same recursive filesystem watch that lights up the 3D
//! view (see `app::state::watch`). The newest events are at the top, and
//! bursts are folded together: a file written to four hundred times reads as
//! one row with a count, not four hundred rows.

pub mod render;

#[cfg(test)]
mod tests;

use crate::app::state::watch::{FsEvent, FsKind};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Rows kept; older ones fall off the bottom.
const KEEP: usize = 2000;
/// How recent a row has to be for a new event on the same path to fold into it.
const FOLD_WINDOW: Duration = Duration::from_secs(5);
/// How many of the newest rows a new event is folded into.
const FOLD_DEPTH: usize = 8;
/// Events held back while paused; past this they are counted, not kept.
const HOLD: usize = 10_000;
/// Seconds of event rate kept for the sparkline.
pub const RATE_SECONDS: usize = 60;

/// One row of the log.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// When it (last) happened.
    pub at: Instant,
    /// When the row began.
    pub first: Instant,
    pub kind: FsKind,
    /// The path, relative to the log's root.
    pub rel: PathBuf,
    /// A rename's new path, relative to the root.
    pub to: Option<PathBuf>,
    pub dir: bool,
    /// Events folded into this row.
    pub count: u32,
}

#[derive(Default)]
pub struct ActivityLog {
    /// The directory being logged: the other panel's. `None` when that is not
    /// a plain local directory, which has nothing to watch.
    pub root: Option<PathBuf>,
    /// Newest first.
    entries: VecDeque<Entry>,
    /// Paused: events are held back, and the rows stay still to be read.
    pub paused: bool,
    held: Vec<(FsEvent, Instant)>,
    /// Events that arrived while paused, including any past [`HOLD`].
    held_count: usize,
    /// The listing filter (the panel's), matched against relative paths.
    filter: Option<String>,
    matcher: Option<crate::panel::FilterMatch>,
    /// Cursor and scroll offset into the filtered rows.
    pub cursor: usize,
    pub offset: usize,
    /// Events per second over the last minute, oldest first, and the second the
    /// last bucket stands for.
    rate: VecDeque<u32>,
    rate_second: Option<Instant>,
    /// The recursive watch was refused: only the root directory itself is seen.
    pub partial: bool,
}

impl ActivityLog {
    /// Point the log at `root`. A different root starts it afresh.
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        if root != self.root {
            *self = ActivityLog {
                filter: self.filter.take(),
                matcher: self.matcher.take(),
                ..Default::default()
            };
            self.root = root;
        }
    }

    pub fn set_filter(&mut self, filter: Option<&str>) {
        if filter != self.filter.as_deref() {
            self.filter = filter.map(str::to_string);
            self.matcher = filter.map(crate::panel::FilterMatch::new);
            self.cursor = 0;
            self.offset = 0;
        }
    }

    /// Take in one event. Ignored when it happened outside the root.
    pub fn record(&mut self, ev: &FsEvent, now: Instant) {
        let Some(root) = &self.root else { return };
        let Ok(rel) = ev.path.strip_prefix(root) else { return };
        if rel.as_os_str().is_empty() {
            return; // the root directory itself
        }
        let rel = rel.to_path_buf();
        let to = ev.to.as_ref().and_then(|t| t.strip_prefix(root).ok()).map(Path::to_path_buf);
        self.count_rate(now);
        if self.paused {
            self.held_count += 1;
            if self.held.len() < HOLD {
                self.held.push((ev.clone(), now));
            }
            return;
        }
        let before = self.visible_len();
        self.fold(Entry { at: now, first: now, kind: ev.kind, rel, to, dir: ev.dir, count: 1 });
        // Keep the cursor on the row it was on while new ones arrive above it;
        // at the very top it stays there, following the newest.
        if self.cursor > 0 {
            let added = self.visible_len().saturating_sub(before);
            self.cursor += added;
            self.offset += added;
        }
    }

    fn fold(&mut self, e: Entry) {
        let recent = |x: &Entry| e.at.duration_since(x.at) <= FOLD_WINDOW;
        if e.kind == FsKind::Rename {
            // The two halves notify reported before pairing them.
            let to = e.to.clone();
            let mut i = 0;
            let mut seen = 0;
            while i < self.entries.len() && seen < FOLD_DEPTH {
                let x = &self.entries[i];
                let half = (x.kind == FsKind::MovedAway && x.rel == e.rel)
                    || (x.kind == FsKind::MovedHere && Some(&x.rel) == to.as_ref());
                if half && recent(x) {
                    self.entries.remove(i);
                } else {
                    i += 1;
                }
                seen += 1;
            }
        }
        let writes = matches!(e.kind, FsKind::Modify | FsKind::Written);
        let depth = self.entries.len().min(FOLD_DEPTH);
        let hit = (0..depth).find(|&i| {
            let x = &self.entries[i];
            recent(x)
                && x.rel == e.rel
                && x.to == e.to
                && (x.kind == e.kind
                    || (writes
                        && matches!(x.kind, FsKind::Create | FsKind::Modify | FsKind::Written)))
        });
        match hit {
            Some(i) => {
                let mut x = self.entries.remove(i).expect("index is in range");
                x.count += 1;
                x.at = e.at;
                // A new file keeps saying it is new while it is first written;
                // one still being appended to long after says what it is doing.
                if x.kind != FsKind::Create || e.at.duration_since(x.first) > FOLD_WINDOW {
                    x.kind = e.kind;
                }
                self.entries.push_front(x);
            }
            None => {
                self.entries.push_front(e);
                self.entries.truncate(KEEP);
            }
        }
    }

    /// Pause (holding events back) or resume (taking them all in).
    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.held_count = 0;
        if !self.paused {
            for (ev, at) in std::mem::take(&mut self.held) {
                // Counted when they arrived; only listed now.
                let saved = self.rate.clone();
                self.record(&ev, at);
                self.rate = saved;
            }
        }
    }

    /// Events held back while paused.
    pub fn held(&self) -> usize {
        self.held_count
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.held.clear();
        self.held_count = 0;
        self.cursor = 0;
        self.offset = 0;
    }

    /// The rows the filter lets through, newest first.
    pub fn visible(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| self.shows(e))
    }

    pub fn visible_len(&self) -> usize {
        self.visible().count()
    }

    fn shows(&self, e: &Entry) -> bool {
        let Some(m) = &self.matcher else { return true };
        m.matches(&e.rel.to_string_lossy())
            || e.to.as_ref().is_some_and(|t| m.matches(&t.to_string_lossy()))
    }

    /// The row under the cursor.
    pub fn selected(&self) -> Option<&Entry> {
        self.visible().nth(self.cursor)
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.visible_len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize).saturating_add(delta).clamp(0, last.max(0)) as usize;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.visible_len().saturating_sub(1);
    }

    /// Where the row under the cursor lives now: the directory to show and the
    /// name to put the cursor on (a rename's new name; nothing for a directory
    /// or file that is gone, whose parent is still worth a look).
    pub fn target(&self) -> Option<(PathBuf, Option<String>)> {
        let root = self.root.as_ref()?;
        let e = self.selected()?;
        let rel = e.to.as_ref().unwrap_or(&e.rel);
        let path = root.join(rel);
        let dir = path.parent().map_or_else(|| root.clone(), Path::to_path_buf);
        let gone = matches!(e.kind, FsKind::Remove | FsKind::MovedAway) || !path.exists();
        let name =
            (!gone).then(|| path.file_name().map(|n| n.to_string_lossy().into_owned())).flatten();
        Some((dir, name))
    }

    /// Advance the rate buckets to `now`, then count one event in the current one.
    fn count_rate(&mut self, now: Instant) {
        self.advance_rate(now);
        if let Some(last) = self.rate.back_mut() {
            *last += 1;
        }
    }

    /// Advance the rate buckets to `now` without counting anything.
    pub fn advance_rate(&mut self, now: Instant) {
        let start = *self.rate_second.get_or_insert(now);
        if self.rate.is_empty() {
            self.rate.push_back(0);
        }
        let elapsed = now.saturating_duration_since(start).as_secs();
        for _ in 0..elapsed.min(RATE_SECONDS as u64) {
            self.rate.push_back(0);
            if self.rate.len() > RATE_SECONDS {
                self.rate.pop_front();
            }
        }
        if elapsed > 0 {
            self.rate_second = Some(start + Duration::from_secs(elapsed));
        }
    }

    /// Events per second, oldest first, ending with the second in progress.
    pub fn rate(&self) -> Vec<u64> {
        self.rate.iter().map(|&n| u64::from(n)).collect()
    }
}
