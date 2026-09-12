//! The 3D view's time machine: one revision's size tree at a time, and the
//! bookkeeping that lets you drag through a repository's history without
//! spawning a `git` process per commit.
//!
//! The [`Space3d`](crate::space3d::Space3d) scene needs no changes to animate
//! this. It keys every node by path, so handing it a wholly different tree makes
//! directories present in both revisions *grow or shrink*, ones that are gone
//! fade out where they stood, and ones not yet created grow out of their parent.
//! Scrubbing is therefore a morph, not a series of jump cuts — and all this
//! module has to do is deliver the right tree, promptly.
//!
//! **Why a fresh tree per revision rather than an edit of the last.** A crawler's
//! tree only ever grows, deliberately, so boxes never shrink mid-scan. Going back
//! in time is exactly the case where they must, so each revision gets a tree of
//! its own and the old one is dropped.

use crate::sizes::SizeTree;
use crate::vfs::git::Rev;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a scrub has to settle before its revision is actually fetched.
/// Holding a step key down moves the cursor and the label immediately; only the
/// tree waits, so the readout never feels laggy while the work stays bounded.
const DEBOUNCE: Duration = Duration::from_millis(120);

/// Trees kept in memory. A commit's tree is immutable, so a cached one is never
/// stale — the only reason to drop any is memory.
const CACHE_MAX: usize = 64;

/// What the panel is scrubbing through.
pub struct Timeline {
    /// The panel showing the 3D view — the one whose scrub row is drawn.
    pub side: usize,
    /// The work tree the revisions belong to, and the root of every size tree
    /// built here, so paths match what the scene already knows.
    pub root: PathBuf,
    /// Newest first, as `git log` reports them.
    pub revs: Vec<Rev>,
    /// Which revision the scrub row is pointing at.
    pub index: usize,
    /// The revision whose tree the scene is currently showing.
    loaded: Option<String>,
    cache: HashMap<String, Arc<SizeTree>>,
    order: VecDeque<String>,
    /// At most one fetch runs at a time; a new target while one is in flight
    /// simply replaces the target.
    inflight: Option<String>,
    /// Bumped on every retarget, so a reply for a revision we have already
    /// scrubbed past is dropped rather than shown.
    generation: u64,
    /// When the current target was last moved, for the debounce.
    pending_since: Option<Instant>,
    /// Stamped into each built tree's `dirs_seen`, which is what the scene
    /// compares against its own `synced_at` to notice a swap.
    epoch: u64,
    /// Which way the last step went, so the next revision along can be fetched
    /// before it is asked for.
    dir: i8,
}

/// What the app loop should do for this timeline right now.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetch {
    /// Nothing to do.
    Idle,
    /// Fetch this revision's tree (`generation` guards the reply).
    Want { oid: String, generation: u64 },
}

impl Timeline {
    /// Open a timeline over `revs`, newest first, pointed at the newest.
    pub fn new(side: usize, root: PathBuf, revs: Vec<Rev>) -> Self {
        Timeline {
            side,
            root,
            revs,
            index: 0,
            loaded: None,
            cache: HashMap::new(),
            order: VecDeque::new(),
            inflight: None,
            generation: 0,
            pending_since: Some(Instant::now()),
            epoch: 0,
            dir: -1,
        }
    }

    pub fn current(&self) -> Option<&Rev> {
        self.revs.get(self.index)
    }

    /// Move `by` revisions. Negative goes back in time (down the list, since the
    /// list is newest first); positive goes forward.
    ///
    /// The index and the label move at once; only the fetch is debounced.
    pub fn step(&mut self, by: i64) {
        if self.revs.is_empty() {
            return;
        }
        let last = self.revs.len() as i64 - 1;
        // `revs` is newest first, so going *back in time* means moving up the
        // index. Negative `by` is back in time, hence the subtraction.
        let want = (self.index as i64 - by).clamp(0, last);
        self.seek(want as usize);
        if by != 0 {
            self.dir = if by < 0 { 1 } else { -1 };
        }
    }

    /// Point at a specific revision.
    pub fn seek(&mut self, index: usize) {
        let index = index.min(self.revs.len().saturating_sub(1));
        if index == self.index && self.loaded.is_some() {
            return;
        }
        self.index = index;
        self.pending_since = Some(Instant::now());
        self.generation = self.generation.wrapping_add(1);
    }

    /// Whether a fetch is still owed, so the loop keeps ticking until it lands.
    pub fn pending(&self) -> bool {
        self.pending_since.is_some() || self.inflight.is_some()
    }

    /// What to fetch now, if anything. Called once per loop tick.
    ///
    /// Returns [`Fetch::Idle`] when the wanted tree is already cached (the
    /// common case while scrubbing back and forth over ground already covered),
    /// while the debounce is still running, or while a fetch is in flight.
    pub fn poll(&mut self, now: Instant) -> Fetch {
        let Some(rev) = self.revs.get(self.index) else { return Fetch::Idle };
        let oid = rev.oid.clone();

        // Already showing it, or able to show it immediately.
        if self.loaded.as_deref() == Some(oid.as_str()) {
            self.pending_since = None;
            return self.prefetch();
        }
        if self.cache.contains_key(&oid) {
            self.loaded = Some(oid);
            self.pending_since = None;
            return self.prefetch();
        }
        if self.inflight.is_some() {
            return Fetch::Idle;
        }
        match self.pending_since {
            Some(since) if now.duration_since(since) < DEBOUNCE => Fetch::Idle,
            _ => {
                self.pending_since = None;
                self.inflight = Some(oid.clone());
                Fetch::Want { oid, generation: self.generation }
            }
        }
    }

    /// Fetch the next revision along the direction of travel, so that while you
    /// are looking at one commit the one you are heading for is already coming.
    fn prefetch(&mut self) -> Fetch {
        if self.inflight.is_some() {
            return Fetch::Idle;
        }
        // `dir` is already an index delta: `revs` is newest first, so heading
        // back in time walks *up* the list.
        let ahead = self.index as i64 + self.dir as i64;
        let Ok(ahead) = usize::try_from(ahead) else { return Fetch::Idle };
        let Some(rev) = self.revs.get(ahead) else { return Fetch::Idle };
        if self.cache.contains_key(&rev.oid) {
            return Fetch::Idle;
        }
        let oid = rev.oid.clone();
        self.inflight = Some(oid.clone());
        Fetch::Want { oid, generation: self.generation }
    }

    /// Take delivery of a fetched revision.
    ///
    /// A tree for a revision that has since been scrubbed past is still *kept* —
    /// it cost a `git` call already and may well be scrubbed back over — but it
    /// does not become what the scene shows.
    pub fn deliver(&mut self, oid: String, entries: Vec<(String, u64)>) {
        if self.inflight.as_deref() == Some(oid.as_str()) {
            self.inflight = None;
        }
        self.epoch = self.epoch.wrapping_add(1);
        let flat = entries.iter().map(|(p, s)| (p.as_str(), *s));
        let tree = Arc::new(crate::sizes::from_paths(&self.root, flat, self.epoch));
        self.insert(oid.clone(), tree);
        if self.revs.get(self.index).map(|r| r.oid.as_str()) == Some(oid.as_str()) {
            self.loaded = Some(oid);
        }
    }

    /// Note that a fetch failed, so the timeline does not wait on it forever.
    pub fn fail(&mut self, oid: &str) {
        if self.inflight.as_deref() == Some(oid) {
            self.inflight = None;
        }
    }

    fn insert(&mut self, oid: String, tree: Arc<SizeTree>) {
        if self.cache.insert(oid.clone(), tree).is_none() {
            self.order.push_back(oid);
        }
        while self.order.len() > CACHE_MAX {
            if let Some(old) = self.order.pop_front() {
                // Never evict what is on screen.
                if Some(old.as_str()) == self.loaded.as_deref() {
                    self.order.push_back(old);
                    break;
                }
                self.cache.remove(&old);
            }
        }
    }

    /// The tree the scene should be drawn from, if one has arrived yet.
    pub fn tree(&self) -> Option<&Arc<SizeTree>> {
        self.cache.get(self.loaded.as_deref()?)
    }

    /// How the scrub row names where you are.
    pub fn label(&self) -> String {
        match self.current() {
            Some(r) => format!("{} {}", r.short, r.subject),
            None => crate::l10n::tr("no revisions").to_string(),
        }
    }

    /// Position in the history, as `(current, total)` counted oldest-first so it
    /// reads the way the scrub row is drawn.
    pub fn position(&self) -> (usize, usize) {
        let total = self.revs.len();
        (total.saturating_sub(self.index), total)
    }
}

#[cfg(test)]
mod tests;
