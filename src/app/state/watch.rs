//! Filesystem watching: re-read a panel when something else changes the
//! directory it is showing, light up the 3D view as its tree is written to, and
//! feed the Activity log.
//!
//! The shape mirrors [`AppState::update_git`]: a cheap per-frame call that
//! notices what should be watched and re-arms, so nothing has to remember to
//! invalidate anything.
//!
//! **Events are batched, not sent one by one.** The watcher's callback runs on
//! notify's own thread and only appends to a shared [`FsInbox`], waking the
//! render loop with a single [`AppEvent::FsActivity`] when the inbox goes from
//! empty to not. A `cargo build` produces thousands of events a second; one
//! wake-up per batch is what keeps that from becoming thousands of redraws.
//!
//! **Listings are debounced.** An event only stamps the panel dirty, and the
//! render loop's 100 ms tick does the reload once the burst has died down, since
//! each re-listing costs a `read_dir` plus a `statvfs`.

use super::*;
use notify::{RecursiveMode, Watcher};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Events held for the render loop before newer ones are dropped. Only reached
/// when the loop is stalled, and then the listing debounce has what it needs.
const INBOX_MAX: usize = 10_000;

/// What happened to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Create,
    Modify,
    /// A file opened for writing was closed: it is done being written.
    Written,
    Remove,
    /// Renamed within the watched tree: `path` became `to`.
    Rename,
    /// Moved out of the watched tree (or the first half of a rename whose
    /// second half is on its way).
    MovedAway,
    /// Moved in from outside the watched tree (or a rename's second half).
    MovedHere,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEvent {
    pub kind: FsKind,
    pub path: PathBuf,
    /// A rename's new path.
    pub to: Option<PathBuf>,
    /// Known to be a directory (only creations say).
    pub dir: bool,
}

impl FsEvent {
    pub fn new(kind: FsKind, path: impl Into<PathBuf>) -> Self {
        FsEvent { kind, path: path.into(), to: None, dir: false }
    }

    /// Read a notify event. `None` for what nobody here cares about: reads and
    /// opens, which say nothing changed.
    pub fn from_notify(ev: notify::Event) -> Option<Self> {
        use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RenameMode};
        let mut paths = ev.paths.into_iter();
        let path = paths.next()?;
        let kind = match ev.kind {
            notify::EventKind::Access(AccessKind::Close(AccessMode::Write)) => FsKind::Written,
            notify::EventKind::Access(_) => return None,
            notify::EventKind::Create(k) => {
                return Some(FsEvent {
                    kind: FsKind::Create,
                    path,
                    to: None,
                    dir: k == CreateKind::Folder,
                });
            }
            notify::EventKind::Remove(_) => FsKind::Remove,
            notify::EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                return Some(FsEvent { kind: FsKind::Rename, path, to: paths.next(), dir: false });
            }
            notify::EventKind::Modify(ModifyKind::Name(RenameMode::From)) => FsKind::MovedAway,
            notify::EventKind::Modify(ModifyKind::Name(RenameMode::To)) => FsKind::MovedHere,
            _ => FsKind::Modify,
        };
        Some(FsEvent::new(kind, path))
    }
}

/// Where the watcher's thread leaves events for the render loop.
#[derive(Default)]
pub struct FsInbox {
    events: Mutex<VecDeque<FsEvent>>,
    /// A wake-up is on its way (or the loop is about to drain anyway).
    woken: AtomicBool,
    /// The OS dropped events of its own (an overflowing inotify queue): every
    /// watched directory has to be assumed changed.
    rescan: AtomicBool,
}

impl FsInbox {
    /// Add an event; `true` when this is the one that should wake the loop.
    pub fn push(&self, ev: FsEvent) -> bool {
        if let Ok(mut q) = self.events.lock()
            && q.len() < INBOX_MAX
        {
            q.push_back(ev);
        }
        !self.woken.swap(true, Ordering::AcqRel)
    }

    /// Everything waiting, and whether a rescan was asked for. Clears the wake
    /// flag first, so an event arriving mid-drain wakes the loop again.
    pub fn drain(&self) -> (Vec<FsEvent>, bool) {
        self.woken.store(false, Ordering::Release);
        let events = self.events.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default();
        (events, self.rescan.swap(false, Ordering::AcqRel))
    }

    /// Whether anything is waiting (for a wake-up that could not be sent).
    pub fn pending(&self) -> bool {
        self.events.lock().is_ok_and(|q| !q.is_empty()) || self.rescan.load(Ordering::Relaxed)
    }
}

/// Whether panel `side`'s directory should be watched **recursively**: when
/// the other panel draws its whole tree — the 3D view (unless its glow is turned
/// off), or the Activity log.
pub(in crate::app::state) fn wants_deep(st: &AppState, side: usize) -> bool {
    let other = &st.panels[1 - side];
    (st.config.space3d_activity
        && other.format == crate::panel::ViewFormat::Space3d
        && other.space3d.is_some())
        || other.format == crate::panel::ViewFormat::Activity
}

/// How long a directory must go quiet before it is re-read. Long enough to
/// collapse the burst from one command, short enough to feel immediate.
pub(in crate::app::state) const DEBOUNCE: Duration = Duration::from_millis(300);

/// The directory to watch for `panel`, or `""` when it should not be watched.
///
/// Only plain local directories qualify. A remote or in-archive path has nothing
/// an OS watcher could subscribe to, and a **panelized** listing (find-file
/// results) is not a directory at all — reloading one would silently throw the
/// results away, since [`Panel::reload_keeping`] clears `result_paths`.
pub(in crate::app::state) fn watch_key(panel: &Panel, enabled: bool) -> String {
    if !enabled || panel.is_panelized() || !panel.cwd.is_plain_local() {
        return String::new();
    }
    panel.cwd.path.to_string_lossy().into_owned()
}

/// Whether a directory stamped dirty at `stamp` is due for its reload at `now`.
pub(in crate::app::state) fn due(stamp: Option<Instant>, now: Instant) -> bool {
    stamp.is_some_and(|t| now.duration_since(t) >= DEBOUNCE)
}

/// A change to what the watcher is subscribed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app::state) enum WatchOp {
    Unwatch(PathBuf),
    /// Watch the path, recursively when `true`.
    Watch(PathBuf, bool),
}

/// The subscriptions to drop and to add to get from `armed` to `desired` (both
/// path → recursive). Drops come first: removing a recursive watch also removes
/// the watches under it, so it must not undo one just added there.
pub(in crate::app::state) fn plan_watches(
    desired: &BTreeMap<PathBuf, bool>,
    armed: &BTreeMap<PathBuf, bool>,
) -> Vec<WatchOp> {
    let mut ops: Vec<WatchOp> = armed
        .iter()
        .filter(|(p, rec)| desired.get(*p) != Some(rec))
        .map(|(p, _)| WatchOp::Unwatch(p.clone()))
        .collect();
    ops.extend(
        desired
            .iter()
            .filter(|(p, rec)| armed.get(*p) != Some(rec))
            .map(|(p, rec)| WatchOp::Watch(p.clone(), *rec)),
    );
    ops
}

/// Combine the panels' wishes into one set of subscriptions: a directory both
/// panels show is watched once, recursively if either wants that; a watch that
/// was refused is not asked for again (a refused recursive one degrades to the
/// directory alone); and anything inside a recursively watched tree is already
/// covered by it.
pub(in crate::app::state) fn desired_watches(
    wishes: &[(PathBuf, bool)],
    refused: &HashSet<(PathBuf, bool)>,
) -> BTreeMap<PathBuf, bool> {
    let mut want: BTreeMap<PathBuf, bool> = BTreeMap::new();
    for (path, deep) in wishes {
        let deep = *deep && !refused.contains(&(path.clone(), true));
        if !deep && refused.contains(&(path.clone(), false)) {
            continue;
        }
        *want.entry(path.clone()).or_insert(false) |= deep;
    }
    let trees: Vec<PathBuf> = want.iter().filter(|(_, r)| **r).map(|(p, _)| p.clone()).collect();
    want.retain(|p, _| !trees.iter().any(|t| t != p && p.starts_with(t)));
    want
}

impl AppState {
    /// Bring the watcher's subscriptions in line with what the panels show.
    /// Called once per frame from the render loop, next to `update_git`; cheap
    /// when nothing has moved.
    pub fn update_watches(&mut self) {
        let mut wishes = Vec::with_capacity(2);
        for side in 0..2 {
            let deep = wants_deep(self, side);
            let key = watch_key(&self.panels[side], self.config.auto_refresh || deep);
            if key != self.watch_key[side] {
                // A pending reload belonged to the directory we just left.
                self.watch_dirty[side] = None;
                self.watch_key[side] = key.clone();
            }
            if !key.is_empty() {
                wishes.push((PathBuf::from(key), deep));
            }
        }
        // A refusal is only remembered while it is still being asked for, so
        // coming back to a directory later gets another try.
        self.watch_refused
            .retain(|(p, rec)| wishes.iter().any(|(w, deep)| w == p && (*deep || !rec)));
        let desired = desired_watches(&wishes, &self.watch_refused);
        if desired == self.watch_armed {
            return;
        }
        if self.watcher.is_none() {
            self.watcher = self.build_watcher();
        }
        let Some(w) = self.watcher.as_mut() else { return };
        for op in plan_watches(&desired, &self.watch_armed) {
            match op {
                WatchOp::Unwatch(path) => {
                    let _ = w.unwatch(&path);
                    self.watch_armed.remove(&path);
                }
                WatchOp::Watch(path, recursive) => {
                    let mode = if recursive {
                        RecursiveMode::Recursive
                    } else {
                        RecursiveMode::NonRecursive
                    };
                    if w.watch(&path, mode).is_ok() {
                        self.watch_armed.insert(path, recursive);
                        continue;
                    }
                    // A recursive watch is the one that realistically fails: a
                    // descriptor per directory can exhaust the per-user limit,
                    // part-way through the walk. Take down what it did add, then
                    // settle for the directory alone, so losing the tree does not
                    // also cost the panel its auto-refresh. Either way, remember
                    // the refusal: retrying on every frame would re-walk the tree
                    // on every frame.
                    let _ = w.unwatch(&path);
                    self.watch_refused.insert((path.clone(), recursive));
                    if recursive && w.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                        self.watch_armed.insert(path, false);
                    }
                }
            }
        }
    }

    /// Whether the recursive watch on `dir` was refused, so a view of the whole
    /// tree can say it is only seeing the directory itself.
    pub(in crate::app::state) fn deep_watch_refused(&self, dir: &Path) -> bool {
        self.watch_refused.contains(&(dir.to_path_buf(), true))
    }

    /// Create the shared watcher. Its callback runs on notify's own thread, so it
    /// only fills the inbox, waking the render loop when the inbox was empty.
    fn build_watcher(&self) -> Option<notify::RecommendedWatcher> {
        let tx = self.tx.clone();
        let inbox = self.fs_inbox.clone();
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if ev.need_rescan() {
                inbox.rescan.store(true, Ordering::Relaxed);
            } else if let Some(fs) = FsEvent::from_notify(ev) {
                if !inbox.push(fs) {
                    return;
                }
            } else {
                return;
            }
            // A full channel means the loop is busy; the tick drains it instead.
            if tx.try_send(AppEvent::FsActivity).is_err() {
                inbox.woken.store(false, Ordering::Relaxed);
            }
        })
        .ok()
    }

    /// Take everything the watcher has collected: stamp panels for reloading,
    /// light the 3D view, and feed the Activity logs.
    pub(in crate::app::state) fn drain_fs_inbox(&mut self) {
        let (events, rescan) = self.fs_inbox.drain();
        if rescan {
            // The OS lost track: every watched directory may have changed.
            let now = Instant::now();
            for side in 0..2 {
                if self.config.auto_refresh && !self.watch_key[side].is_empty() {
                    self.watch_dirty[side] = Some(now);
                }
            }
        }
        for ev in &events {
            self.note_fs_event(ev);
        }
    }

    /// Act on one filesystem event.
    pub(in crate::app::state) fn note_fs_event(&mut self, ev: &FsEvent) {
        self.note_dir_changed(&ev.path);
        if let Some(to) = &ev.to {
            self.note_dir_changed(to);
        }
        let now = Instant::now();
        for panel in &mut self.panels {
            if let Some(log) = panel.activity.as_mut() {
                log.record(ev, now);
            }
        }
    }

    /// Stamp the panel(s) showing the directory `path` sits in as needing a
    /// re-read. Both sides can match — they are often on the same directory —
    /// but a panel showing something else is left alone.
    pub(in crate::app::state) fn note_dir_changed(&mut self, path: &Path) {
        // The event names the entry that changed, so the directory being watched
        // is its parent; a change to the watched directory itself also arrives.
        let dir = path.parent().unwrap_or(path);
        let now = Instant::now();
        for side in 0..2 {
            if self.watch_key[side].is_empty() {
                continue;
            }
            let root = Path::new(&self.watch_key[side]);
            if self.config.auto_refresh && (root == dir || root == path) {
                self.watch_dirty[side] = Some(now);
            }
            // A 3D view of this panel's tree lights the directory that changed,
            // however deep it is — which is the point of the recursive watch.
            // The listing itself is deliberately *not* marked dirty for a change
            // further down, because it does not show one.
            if self.config.space3d_activity
                && dir.starts_with(root)
                && let Some(sp) = self.panels[1 - side].space3d.as_mut()
            {
                sp.heat(dir);
            }
        }
    }

    /// Reload any panel whose debounce has expired. Called from the render
    /// loop's tick.
    pub async fn flush_dir_changes(&mut self) {
        // Events whose wake-up could not be sent (a full channel) are picked up
        // here instead.
        if self.fs_inbox.pending() {
            self.drain_fs_inbox();
        }
        let now = Instant::now();
        let mut reloaded = false;
        for side in 0..2 {
            if !due(self.watch_dirty[side], now) {
                continue;
            }
            self.watch_dirty[side] = None;
            // Re-check the guards: the panel may have been panelized or sent to
            // a remote between the event and now.
            if !self.config.auto_refresh || watch_key(&self.panels[side], true).is_empty() {
                continue;
            }
            // `refresh` keeps the cursor on its named entry (or its row, when the
            // entry itself went away) and prunes the marks against the new
            // listing, so a refresh is not felt.
            let _ = self.panels[side].refresh().await;
            reloaded = true;
        }
        if reloaded {
            // The working tree may have changed under us as well.
            self.invalidate_git();
        }
    }

    /// Whether a debounced reload (or an undelivered batch) is still pending, so
    /// the render loop keeps ticking long enough to handle it on an idle screen.
    pub(in crate::app::state) fn watch_pending(&self) -> bool {
        self.watch_dirty.iter().any(Option::is_some) || self.fs_inbox.pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_local_unpanelized_panels_are_watched() {
        let local = Registry::new().local();
        let panel_at = |p: VfsPath| Panel::new(local.clone(), p);

        let mut p = panel_at(VfsPath::local("/tmp/x"));
        assert_eq!(watch_key(&p, true), "/tmp/x");
        assert_eq!(watch_key(&p, false), "", "the setting turns it off");

        // A find-file panelization is a flat result list, not a directory:
        // reloading it would throw the results away.
        p.set_results(Vec::new(), Vec::new());
        assert_eq!(watch_key(&p, true), "", "panelized listings are left alone");

        // Remote and in-archive paths have nothing to subscribe to.
        let remote = VfsPath { scheme: "sftp-0".into(), path: "/srv".into(), container: None };
        assert_eq!(watch_key(&panel_at(remote), true), "");
        assert_eq!(watch_key(&panel_at(VfsPath::archive("/tmp/a.zip", "/")), true), "");
    }

    #[test]
    fn the_debounce_waits_for_the_directory_to_go_quiet() {
        let now = Instant::now();
        assert!(!due(None, now), "nothing pending is never due");
        assert!(!due(Some(now), now), "an event just now still waits");
        assert!(!due(Some(now), now + DEBOUNCE / 2), "and halfway through too");
        assert!(due(Some(now), now + DEBOUNCE), "due once the window elapses");
        assert!(due(Some(now), now + DEBOUNCE * 3), "and stays due");
    }

    fn map(items: &[(&str, bool)]) -> BTreeMap<PathBuf, bool> {
        items.iter().map(|(p, r)| (PathBuf::from(p), *r)).collect()
    }

    fn wishes(items: &[(&str, bool)]) -> Vec<(PathBuf, bool)> {
        items.iter().map(|(p, r)| (PathBuf::from(p), *r)).collect()
    }

    #[test]
    fn a_directory_both_panels_show_is_watched_once_and_deeply_if_either_wants() {
        let refused = HashSet::new();
        let want = desired_watches(&wishes(&[("/a", false), ("/a", true)]), &refused);
        assert_eq!(want, map(&[("/a", true)]));
        // One panel leaving leaves the other's watch standing, just shallower.
        let now = desired_watches(&wishes(&[("/a", false)]), &refused);
        assert_eq!(
            plan_watches(&now, &want),
            vec![WatchOp::Unwatch("/a".into()), WatchOp::Watch("/a".into(), false)]
        );
    }

    #[test]
    fn a_directory_inside_a_watched_tree_needs_no_watch_of_its_own() {
        let want = desired_watches(&wishes(&[("/a", true), ("/a/b", false)]), &HashSet::new());
        assert_eq!(want, map(&[("/a", true)]));
        let want = desired_watches(&wishes(&[("/a/b", true), ("/ab", false)]), &HashSet::new());
        assert_eq!(want, map(&[("/a/b", true), ("/ab", false)]), "a name prefix is not a parent");
    }

    #[test]
    fn a_refused_watch_is_not_asked_for_again() {
        let mut refused = HashSet::new();
        refused.insert((PathBuf::from("/huge"), true));
        let want = desired_watches(&wishes(&[("/huge", true)]), &refused);
        assert_eq!(want, map(&[("/huge", false)]), "the directory alone, not the tree");
        refused.insert((PathBuf::from("/huge"), false));
        assert!(desired_watches(&wishes(&[("/huge", true)]), &refused).is_empty());
    }

    #[test]
    fn nothing_changes_when_the_watches_already_match() {
        let armed = map(&[("/a", true), ("/b", false)]);
        assert!(plan_watches(&armed, &armed).is_empty());
        let ops = plan_watches(&map(&[("/b", false), ("/c", false)]), &armed);
        assert_eq!(ops, vec![WatchOp::Unwatch("/a".into()), WatchOp::Watch("/c".into(), false)]);
    }

    #[test]
    fn notify_events_are_read_with_their_kind_and_both_rename_paths() {
        use notify::event::{
            AccessKind, AccessMode, CreateKind, EventKind, ModifyKind, RenameMode,
        };
        let ev = |kind, paths: &[&str]| {
            let mut e = notify::Event::new(kind);
            for p in paths {
                e = e.add_path(PathBuf::from(p));
            }
            FsEvent::from_notify(e)
        };
        let both = ev(EventKind::Modify(ModifyKind::Name(RenameMode::Both)), &["/a/old", "/a/new"])
            .unwrap();
        assert_eq!((both.kind, both.to.as_deref()), (FsKind::Rename, Some(Path::new("/a/new"))));
        let written =
            ev(EventKind::Access(AccessKind::Close(AccessMode::Write)), &["/a/f"]).unwrap();
        assert_eq!(written.kind, FsKind::Written);
        assert!(ev(EventKind::Access(AccessKind::Open(AccessMode::Any)), &["/a/f"]).is_none());
        let dir = ev(EventKind::Create(CreateKind::Folder), &["/a/d"]).unwrap();
        assert!(dir.dir && dir.kind == FsKind::Create);
    }

    #[test]
    fn a_burst_wakes_the_loop_once() {
        let inbox = FsInbox::default();
        assert!(inbox.push(FsEvent::new(FsKind::Modify, "/a/1")));
        for i in 2..100 {
            assert!(!inbox.push(FsEvent::new(FsKind::Modify, format!("/a/{i}"))));
        }
        let (events, rescan) = inbox.drain();
        assert_eq!((events.len(), rescan), (99, false));
        assert!(!inbox.pending());
        assert!(
            inbox.push(FsEvent::new(FsKind::Modify, "/a/x")),
            "after a drain, the next one wakes"
        );
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Wait for the watcher to deliver an event matching `want`, draining the
    /// inbox into `st` on each wake-up.
    async fn wait_for(
        st: &mut AppState,
        rx: &mut crate::util::async_bridge::AppReceiver,
        want: impl Fn(&FsEvent) -> bool,
    ) -> Option<FsEvent> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(AppEvent::FsActivity) = rx.recv().await {
                    let (events, _) = st.fs_inbox.drain();
                    for ev in events {
                        st.note_fs_event(&ev);
                        if want(&ev) {
                            return ev;
                        }
                    }
                }
            }
        })
        .await
        .ok()
    }

    /// The one test that exercises the real OS watcher end to end. Everything
    /// else about auto-refresh is decision logic tested above; this proves the
    /// wiring in between actually delivers.
    #[tokio::test]
    async fn a_real_write_reaches_the_app_channel() {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("rc_live_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let (tx, mut rx) = crate::util::async_bridge::channel();
        let mut st = AppState::new(tx);
        st.panels[0].cwd = VfsPath::local(&root);
        st.panels[0].backend = st.registry.local();
        let _ = st.panels[0].reload().await;
        st.update_watches();
        assert!(st.watcher.is_some(), "a watcher was created");

        std::fs::write(root.join("new.txt"), b"hello").unwrap();
        let got = wait_for(&mut st, &mut rx, |ev| ev.path.starts_with(&root)).await;
        assert!(got.is_some(), "a real write reached the app");
        assert!(st.watch_pending(), "and stamped the panel for a reload");

        std::fs::remove_dir_all(&root).ok();
    }

    /// The end-to-end path the glow depends on: a 3D panel arms a **recursive**
    /// watch, and a write two levels down reaches the app and lights a box.
    #[tokio::test]
    async fn a_deep_write_under_a_3d_panel_lights_the_scene() {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("rc_deep_{}_{nanos}", std::process::id()));
        let deep = root.join("sub/deeper");
        std::fs::create_dir_all(&deep).unwrap();

        let (tx, mut rx) = crate::util::async_bridge::channel();
        let mut st = AppState::new(tx);
        // Panel 0 shows the tree; panel 1 draws it in 3D.
        st.panels[0].cwd = VfsPath::local(&root);
        st.panels[0].backend = st.registry.local();
        let _ = st.panels[0].reload().await;
        st.panels[1].format = crate::panel::ViewFormat::Space3d;
        st.panels[1].space3d = Some(crate::space3d::Space3d::new(root.clone()));
        st.update_watches();
        assert_eq!(
            st.watch_armed.get(&root),
            Some(&true),
            "a 3D panel opposite arms the recursive watch"
        );

        std::fs::write(deep.join("built.o"), b"x").unwrap();
        let got = wait_for(&mut st, &mut rx, |ev| ev.path.starts_with(&deep)).await;
        assert!(got.is_some(), "a deep write reached the app");
        let sp = st.panels[1].space3d.as_ref().unwrap();
        assert!(sp.needs_frames(), "and the scene now has a glow to animate");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_watch_mode_follows_the_other_panel_into_and_out_of_the_3d_format() {
        let (tx, _rx) = crate::util::async_bridge::channel();
        let mut st = AppState::new(tx);
        assert!(!wants_deep(&st, 0), "no 3D panel, no recursive watch");
        st.panels[1].format = crate::panel::ViewFormat::Space3d;
        st.panels[1].space3d = Some(crate::space3d::Space3d::new(PathBuf::from("/")));
        assert!(wants_deep(&st, 0), "panel 1 in 3D watches panel 0's tree deeply");
        assert!(!wants_deep(&st, 1), "and not the other way round");
        st.config.space3d_activity = false;
        assert!(!wants_deep(&st, 0), "the setting turns it off");
        st.panels[1].format = crate::panel::ViewFormat::Activity;
        assert!(wants_deep(&st, 0), "an Activity log opposite always wants the tree");
    }
}
