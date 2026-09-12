//! Filesystem watching: re-read a panel when something else changes the
//! directory it is showing.
//!
//! The shape mirrors [`AppState::update_git`]: a cheap per-frame call that
//! notices when a panel's directory changed and re-arms, so nothing has to
//! remember to invalidate anything.
//!
//! Events are deliberately *not* acted on as they arrive. A single `cp` or
//! `git checkout` produces a burst of them, and each re-listing costs a
//! `read_dir` plus a `statvfs`, so an event only stamps the panel dirty and the
//! render loop's 100 ms tick does the reload once the burst has died down.

use super::*;
use notify::{RecursiveMode, Watcher};

/// Whether panel `side`'s directory should be watched **recursively**.
///
/// It should when the *other* panel is drawing it in 3D: that view shows the
/// subdirectories too, so a file landing two levels down is something to react
/// to, where for the flat listing it is not.
pub(in crate::app::state) fn wants_deep(st: &AppState, side: usize) -> bool {
    st.config.space3d_activity
        && st.panels[1 - side].format == crate::panel::ViewFormat::Space3d
        && st.panels[1 - side].space3d.is_some()
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

impl AppState {
    /// Re-arm the filesystem watchers when either panel's directory changed.
    /// Called once per frame from the render loop, next to `update_git`; cheap
    /// when nothing has moved.
    pub fn update_watches(&mut self) {
        for side in 0..2 {
            let key = watch_key(&self.panels[side], self.config.auto_refresh);
            let deep = wants_deep(self, side);
            if key == self.watch_key[side] && deep == self.watch_deep[side] {
                continue;
            }
            // Drop the old subscription before taking the new one, so a panel
            // stepping through a tree doesn't accumulate watches.
            if !self.watch_key[side].is_empty()
                && let Some(w) = self.watcher.as_mut()
            {
                let _ = w.unwatch(Path::new(&self.watch_key[side]));
            }
            self.watch_key[side] = key.clone();
            // A pending reload belonged to the directory we just left.
            self.watch_dirty[side] = None;
            if key.is_empty() {
                continue;
            }
            if self.watcher.is_none() {
                self.watcher = self.build_watcher();
            }
            self.watch_deep[side] = deep;
            if let Some(w) = self.watcher.as_mut() {
                let mode =
                    if deep { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
                let mut armed = w.watch(Path::new(&key), mode).is_ok();
                // A recursive watch is the one that realistically fails: it adds
                // a descriptor per directory, and a deep tree can exhaust the
                // per-user limit. Fall back to watching just this directory, so
                // losing the 3D glow does not also cost the panel its ordinary
                // auto-refresh.
                if !armed && deep {
                    armed = w.watch(Path::new(&key), RecursiveMode::NonRecursive).is_ok();
                    self.watch_deep[side] = false;
                }
                if !armed {
                    // Watching can fail for ordinary reasons — an inotify limit,
                    // a directory that vanished between listing and arming.
                    // Ctrl-R still works, so forget the key and carry on rather
                    // than reporting it.
                    self.watch_key[side] = String::new();
                }
            }
        }
    }

    /// Create the shared watcher. Its callback runs on notify's own thread, so
    /// it only posts an event onto the app channel; `try_send` (not `send`)
    /// because a full channel means a reload is already queued.
    fn build_watcher(&self) -> Option<notify::RecommendedWatcher> {
        let tx = self.tx.clone();
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // Access-time-only events would re-read the panel every time a file
            // is merely read; everything else is a real change to the listing.
            if let Ok(ev) = res
                && !matches!(ev.kind, notify::EventKind::Access(_))
                && let Some(path) = ev.paths.into_iter().next()
            {
                let _ = tx.try_send(AppEvent::DirChanged { path });
            }
        })
        .ok()
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
            let key = self.watch_key[side].clone();
            if key.is_empty() {
                continue;
            }
            let root = Path::new(&key);
            if root == dir || root == path {
                self.watch_dirty[side] = Some(now);
            }
            // A 3D view of this panel's tree lights the directory that changed,
            // however deep it is — which is the point of the recursive watch.
            // The listing itself is deliberately *not* marked dirty for a change
            // further down, because it does not show one.
            if self.watch_deep[side]
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
        let now = Instant::now();
        let mut reloaded = false;
        for side in 0..2 {
            if !due(self.watch_dirty[side], now) {
                continue;
            }
            self.watch_dirty[side] = None;
            // Re-check the guards: the panel may have been panelized or sent to
            // a remote between the event and now.
            if watch_key(&self.panels[side], self.config.auto_refresh).is_empty() {
                continue;
            }
            // `reload` keeps the cursor on its named entry and prunes the marks
            // against the new listing, so a refresh is not felt.
            let _ = self.panels[side].reload().await;
            reloaded = true;
        }
        if reloaded {
            // The working tree may have changed under us as well.
            self.invalidate_git();
        }
    }

    /// Whether a debounced reload is still pending, so the render loop keeps
    /// ticking long enough to run it on an otherwise idle screen.
    pub(in crate::app::state) fn watch_pending(&self) -> bool {
        self.watch_dirty.iter().any(Option::is_some)
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
}

#[cfg(test)]
mod live_tests {
    use super::*;

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

        // inotify delivery is asynchronous; give it a bounded chance rather than
        // a fixed sleep, so the test is neither flaky nor slow.
        let got = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(AppEvent::DirChanged { path }) = rx.recv().await
                    && path.starts_with(&root)
                {
                    return true;
                }
            }
        })
        .await;
        assert!(got.unwrap_or(false), "a real write produced a DirChanged event");

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
        assert!(st.watch_deep[0], "a 3D panel opposite arms the recursive watch");

        std::fs::write(deep.join("built.o"), b"x").unwrap();

        let got = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(AppEvent::DirChanged { path }) = rx.recv().await
                    && path.starts_with(&deep)
                {
                    return path;
                }
            }
        })
        .await;
        let path = got.expect("a deep write reached the app channel");
        st.note_dir_changed(&path);
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
    }
}
