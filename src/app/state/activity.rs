//! Drives the Activity log panel format: points each log at the other panel's
//! directory, and acts on its keys. The events themselves arrive through the
//! filesystem watch (see `watch`).

use super::*;

impl AppState {
    /// Keep each Activity panel's log pointed at the other panel's directory,
    /// with the panel's filter. Called once per loop iteration, before the
    /// watches are re-armed (the log is what asks for the recursive watch).
    pub fn update_activity_logs(&mut self) {
        let now = Instant::now();
        for viewer in 0..2 {
            if self.panels[viewer].format != ViewFormat::Activity {
                self.panels[viewer].activity = None;
                continue;
            }
            let source = &self.panels[1 - viewer].cwd;
            let root = source.is_plain_local().then(|| source.path.clone());
            let partial = root.as_ref().is_some_and(|r| self.deep_watch_refused(r));
            let filter = self.panels[viewer].filter.clone();
            let log = self.panels[viewer].activity.get_or_insert_with(Default::default);
            log.set_root(root);
            log.set_filter(filter.as_deref());
            log.partial = partial;
            log.advance_rate(now);
        }
    }

    /// Whether an Activity log is on screen, whose ages and rate tick along.
    pub(in crate::app::state) fn activity_shown(&self) -> bool {
        self.panels.iter().any(|p| p.activity.is_some())
    }

    /// The Activity log's own keys, when the active panel shows one: Enter
    /// shows the row's file in the other panel, Insert pauses, Delete clears.
    /// Returns whether the key was taken.
    pub(in crate::app::state) async fn activity_key(&mut self, key: KeyEvent) -> bool {
        let side = self.active;
        let Some(log) = self.panels[side].activity.as_mut() else { return false };
        match key.code {
            KeyCode::Insert => log.toggle_pause(),
            KeyCode::Delete => log.clear(),
            KeyCode::Enter if key.modifiers.is_empty() => self.activity_enter().await,
            _ => return false,
        }
        true
    }

    /// Enter on a log row: point the other panel at the directory the file is in
    /// (with the cursor on it, if it still exists) and make that panel active,
    /// ready to act on the file.
    pub(in crate::app::state) async fn activity_enter(&mut self) {
        let side = self.active;
        let Some((dir, name)) = self.panels[side].activity.as_ref().and_then(|a| a.target()) else {
            return;
        };
        let source = 1 - side;
        let target = VfsPath::local(&dir);
        let backend = self.registry.local();
        if self.panels[source].try_enter(target, backend, name.as_deref()).await {
            self.active = source;
        }
    }
}
