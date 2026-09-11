//! Scheduling for the shared directory-size cache.
//!
//! The crawler is steered, never restarted: everything that wants sizes calls
//! [`AppState::size_focus`] with the directory it is showing, and the crawler
//! puts that subtree at the front of its queue. A directory that was already
//! walked — as part of a parent's crawl, say — is not walked again, which is
//! what makes descending and going back up instant instead of a rescan.

use super::*;

impl AppState {
    /// Point the size crawler at `dir`, starting it if this is the first thing
    /// to ask for sizes this session.
    pub(in crate::app::state) fn size_focus(&mut self, dir: &Path) {
        match self.sizes.as_ref() {
            Some(c) => c.focus(dir),
            None => self.sizes = Some(crate::sizes::crawl::Crawler::spawn(dir.to_path_buf())),
        }
    }

    /// Whether a size crawl is in flight — drives the "scanning…" readout and
    /// keeps the animation tick alive while boxes are still growing.
    pub fn sizes_running(&self) -> bool {
        self.sizes.as_ref().is_some_and(|c| c.is_running())
    }

    /// Point each 3D panel at the *other* panel's directory, the way
    /// `update_details` points a Details panel at the other panel's cursor.
    /// Called once per loop iteration; cheap when nothing has moved.
    pub fn update_space3d(&mut self) {
        for viewer in 0..2 {
            if self.panels[viewer].format != crate::panel::ViewFormat::Space3d {
                continue;
            }
            // Before the crawlability check, so switching the setting re-forms
            // the scene even on a panel whose source is currently remote.
            let style = self.config.space3d_style;
            if let Some(sp) = self.panels[viewer].space3d.as_mut() {
                sp.set_style(style);
            }
            let source = 1 - viewer;
            // Only a real filesystem can be crawled, so a remote or archive
            // panel leaves the view showing whatever it last had.
            if !crate::space3d::is_crawlable(&self.panels[source].cwd) {
                continue;
            }
            let dir = self.panels[source].cwd.path.clone();
            // The entry that panel's cursor is on, when it is a real
            // subdirectory. The test mirrors the crawler's own rule — a real
            // directory, never a symlink — so the highlight can only ever point
            // at a box that exists. `..` leads out of the tree entirely.
            let under_cursor = self.panels[source].current_entry().and_then(|e| {
                let real_dir = e.kind == crate::vfs::VfsKind::Dir && e.symlink_target.is_none();
                (real_dir && e.name != ".." && e.name != ".").then(|| dir.join(&e.name))
            });
            if let Some(sp) = self.panels[viewer].space3d.as_mut() {
                sp.set_focus(&dir);
                sp.set_cursor(under_cursor.as_deref());
            }
        }
    }

    /// Re-point the crawler at whatever currently needs sizing, then refresh the
    /// views from the cache. Called once per loop iteration, like
    /// `update_details` and `update_git`; cheap when nothing has moved.
    pub fn update_sizes(&mut self) {
        // Whoever is in front gets the crawler's attention: the disk explorer
        // when it is open, otherwise a 3D panel — the active one for preference,
        // but *either* will do. A 3D view is normally on the panel you are not
        // driving (it describes the other one), so keying this off the active
        // panel alone would stop it updating the moment you switched panels.
        let want = match self.diskview.as_ref() {
            Some(d) => Some(d.cwd.clone()),
            None => [self.active, 1 - self.active]
                .iter()
                .find_map(|&i| self.panels[i].space3d.as_ref().map(|sp| sp.focus.clone())),
        };
        if let Some(dir) = want
            && self.sizes_focus.as_deref() != Some(dir.as_path())
        {
            self.size_focus(&dir);
            self.sizes_focus = Some(dir);
        }
        // Always re-project: this is guarded on the crawler's own counter, so it
        // costs nothing when nothing has moved.
        self.refresh_size_views();
    }

    /// Re-project the size-backed views from the cache, but only when the
    /// crawler has actually seen something new since last time — otherwise an
    /// idle view would rebuild its box list on every frame.
    pub(in crate::app::state) fn refresh_size_views(&mut self) {
        let Some(crawler) = self.sizes.as_ref() else {
            return;
        };
        crawler.with_tree(|tree| {
            if let Some(dv) = self.diskview.as_mut()
                && tree.dirs_seen != dv.synced_at
            {
                dv.synced_at = tree.dirs_seen;
                dv.sync_from(tree);
            }
            for p in self.panels.iter_mut() {
                if let Some(sp) = p.space3d.as_mut()
                    && tree.dirs_seen != sp.synced_at
                {
                    sp.synced_at = tree.dirs_seen;
                    sp.sync_from(tree);
                }
            }
        });
    }
}
