//! Per-panel directory tabs.
//!
//! A tab is a *saved panel position*, not a second panel: the directory, how it
//! is being looked at (view format, sort, filter), where the cursor and marks
//! were, and the back/forward history that led there. Everything else — the
//! listing itself, the git status, the layout geometry — is rebuilt when the tab
//! is activated, because it is derived from the directory anyway.
//!
//! The backend is deliberately *not* saved. It is resolved from `cwd` through
//! the registry on activation, the same way [`crate::app::state::RemoteSession`]
//! stores a scheme rather than an `Arc<dyn Vfs>`: a tab left on a connection that
//! has since been closed then fails to open cleanly, instead of silently keeping
//! a dead backend alive for the life of the program.

use super::{SortConfig, ViewFormat, selection::Selection};
use crate::vfs::VfsPath;

/// One saved tab position.
#[derive(Debug, Clone)]
pub struct TabState {
    pub cwd: VfsPath,
    pub format: ViewFormat,
    pub sort: SortConfig,
    pub filter: Option<String>,
    pub selection: Selection,
    pub cursor: usize,
    /// The name under the cursor when the tab was left. Restoring by *name*
    /// rather than index matters because the directory can change while you are
    /// on another tab — this is the same rule `Panel::reload_keeping` follows.
    pub cursor_name: Option<String>,
    pub offset: usize,
    pub back: Vec<VfsPath>,
    pub forward: Vec<VfsPath>,
}

impl TabState {
    /// A fresh tab showing `cwd`, inheriting how `format`/`sort` are set up so a
    /// new tab looks like the one it was opened from.
    pub fn new(cwd: VfsPath, format: ViewFormat, sort: SortConfig) -> Self {
        TabState {
            cwd,
            format,
            sort,
            filter: None,
            selection: Selection::default(),
            cursor: 0,
            cursor_name: None,
            offset: 0,
            back: Vec::new(),
            forward: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::Panel;
    use crate::vfs::registry::Registry;

    fn panel_at(dir: &str) -> Panel {
        let reg = Registry::new();
        Panel::new(reg.local(), VfsPath::local(dir))
    }

    #[test]
    fn a_new_panel_has_exactly_one_tab() {
        let p = panel_at("/tmp");
        assert_eq!(p.tabs.len(), 1);
        assert_eq!(p.tab, 0);
        assert!(p.tab_hits.is_empty());
    }

    #[test]
    fn capture_and_apply_round_trip_a_position() {
        let mut p = panel_at("/tmp");
        p.cursor = 7;
        p.offset = 3;
        p.filter = Some("*.rs".into());
        p.format = ViewFormat::Brief;
        p.selection.toggle("a.txt");
        p.selection.toggle("b.txt");
        p.back = vec![VfsPath::local("/one"), VfsPath::local("/two")];
        p.forward = vec![VfsPath::local("/three")];

        let saved = p.capture_tab();

        // Move somewhere else entirely, then come back.
        let mut q = panel_at("/etc");
        q.apply_tab(&saved);

        assert_eq!(q.cwd, VfsPath::local("/tmp"));
        assert_eq!(q.cursor, 7);
        assert_eq!(q.offset, 3);
        assert_eq!(q.filter.as_deref(), Some("*.rs"));
        assert_eq!(q.format, ViewFormat::Brief);
        assert!(q.selection.is_marked("a.txt") && q.selection.is_marked("b.txt"));
        assert_eq!(q.back.len(), 2);
        assert_eq!(q.forward.len(), 1);
    }

    #[test]
    fn applying_a_tab_drops_the_previous_directorys_derived_state() {
        let mut p = panel_at("/tmp");
        p.error = Some("stale".into());
        p.result_paths = Some(vec![VfsPath::local("/hit")]);
        p.apply_tab(&TabState::new(
            VfsPath::local("/etc"),
            ViewFormat::Full,
            SortConfig::default(),
        ));
        // None of this belongs to the directory we just switched to.
        assert!(p.entries.is_empty());
        assert!(p.error.is_none());
        assert!(p.result_paths.is_none());
        assert!(p.git.is_none());
    }

    #[test]
    fn sync_active_tab_folds_the_live_position_back_in() {
        let mut p = panel_at("/tmp");
        p.tabs.push(TabState::new(VfsPath::local("/etc"), ViewFormat::Full, SortConfig::default()));
        p.cursor = 4;
        p.sync_active_tab();
        assert_eq!(p.tabs[0].cursor, 4, "the active tab's entry is up to date");
        assert_eq!(p.tabs[1].cursor, 0, "the other tab is untouched");
    }
}
