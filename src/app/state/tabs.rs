//! Per-panel directory tabs: opening, closing and switching between them.
//!
//! The saved state itself lives in [`crate::panel::tabs::TabState`]; this is the
//! part that needs the app — resolving a tab's backend through the registry, and
//! honouring the one-remote-panel invariant when a tab switch changes which
//! backend a panel is on.

use super::*;

impl AppState {
    /// Open a new tab on `side`, showing the same directory as the current one.
    pub(in crate::app::state) async fn tab_new(&mut self, side: usize) {
        self.panels[side].sync_active_tab();
        let p = &self.panels[side];
        let fresh = crate::panel::tabs::TabState::new(p.cwd.clone(), p.format, p.sort);
        let at = self.panels[side].tab + 1;
        self.panels[side].tabs.insert(at, fresh);
        self.tab_select(side, at).await;
    }

    /// Close the tab at `index`. Closing the last remaining tab does nothing:
    /// a panel always shows something, and quitting is F10's job.
    pub(in crate::app::state) async fn tab_close(&mut self, side: usize, index: usize) {
        if self.panels[side].tabs.len() <= 1 || index >= self.panels[side].tabs.len() {
            return;
        }
        self.panels[side].tabs.remove(index);
        // Stay where we are if a *different* tab was closed; otherwise fall back
        // to the neighbour on the left.
        let active = self.panels[side].tab;
        let next = if index < active {
            active - 1
        } else if index == active {
            active.min(self.panels[side].tabs.len() - 1)
        } else {
            active
        };
        self.panels[side].tab = next;
        self.tab_select(side, next).await;
    }

    /// Show tab `index` on `side`.
    pub(in crate::app::state) async fn tab_select(&mut self, side: usize, index: usize) {
        let Some(target) = self.panels[side].tabs.get(index).cloned() else {
            return;
        };

        // Switching *to* a remote tab is a second remote panel if the other side
        // is already on one — the same invariant a connect has to satisfy.
        if target.cwd.is_remote()
            && self.panels[side].tab != index
            && self.other_panel_is_remote(side)
        {
            self.show_error(
                "The other panel is already on a remote connection. Return it to \
                 Local first — one panel must stay local."
                    .to_string(),
            );
            return;
        }

        // Resolve the backend from the path. A tab left on a connection that has
        // since been closed has no backend any more, and says so rather than
        // silently showing the wrong directory.
        let Ok(backend) = self.registry.resolve(&target.cwd) else {
            self.show_error(format!(
                "That tab's location is no longer available: {}",
                target.cwd.display()
            ));
            return;
        };

        // Remember where the tab we are leaving got to, including telling a
        // remote session its current directory so returning to it lands right.
        if self.panels[side].tab != index {
            self.snapshot_session_cwd(side);
        }
        self.panels[side].sync_active_tab();

        self.panels[side].tab = index;
        self.panels[side].apply_tab(&target);
        self.panels[side].backend = backend;
        // Reload asking for the remembered *name*: the listing may well have
        // changed while this tab was in the background, so an index alone would
        // land somewhere arbitrary.
        let _ = self.panels[side].reload_keeping(target.cursor_name.as_deref()).await;
        if self.panels[side].format == crate::panel::ViewFormat::Tree {
            self.panels[side].build_tree().await;
        } else if self.panels[side].is_space3d() {
            self.panels[side].build_space3d();
        }
    }

    /// The `(side, tab index)` under this screen position, if a tab label is
    /// there. Mirrors `history_arrow_at` for the strip's recorded rects.
    pub(in crate::app::state) fn tab_at(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        for (side, panel) in self.panels.iter().enumerate() {
            for (rect, index) in &panel.tab_hits {
                if col >= rect.x
                    && col < rect.x + rect.width
                    && row >= rect.y
                    && row < rect.y + rect.height
                {
                    return Some((side, *index));
                }
            }
        }
        None
    }

    /// Open the pickable list of this panel's tabs (Alt-J).
    pub(in crate::app::state) fn open_tab_picker(&mut self) {
        let side = self.active;
        // The active tab's saved entry can be stale until a switch happens, so
        // show the live directory for it rather than what was last stored.
        let p = &self.panels[side];
        let entries: Vec<VfsPath> = p
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| if i == p.tab { p.cwd.clone() } else { t.cwd.clone() })
            .collect();
        self.dialog =
            Some(Dialog::TabPicker(TabPickerDialog::new(side, entries, p.tab)));
    }

    /// Move to the next / previous tab, wrapping.
    pub(in crate::app::state) async fn tab_cycle(&mut self, side: usize, forward: bool) {
        let count = self.panels[side].tabs.len();
        if count <= 1 {
            return;
        }
        let cur = self.panels[side].tab;
        let next = if forward { (cur + 1) % count } else { (cur + count - 1) % count };
        self.tab_select(side, next).await;
    }
}
