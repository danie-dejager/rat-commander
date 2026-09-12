//! Drives the thumbnail grid: asks for the pictures of the cells on screen and
//! a page ahead, a few at a time, from the panel's own backend.

use super::*;

impl AppState {
    /// Keep each thumbnail grid's cache in step (directory, size setting) and
    /// start loading the pictures it is about to draw. Called once per loop
    /// iteration; cheap when everything visible is already known.
    pub fn update_thumbs(&mut self) {
        let size = self.config.thumb_size;
        let bg = crate::ui::graphics::raster::rgb(self.theme.panel_bg);
        let base = crate::space3d::ScenePalette::from_theme(&self.theme).platform;
        for side in 0..2 {
            let panel = &mut self.panels[side];
            if panel.format != ViewFormat::Thumbs {
                panel.thumbs = None;
                continue;
            }
            let cache = panel.thumbs.get_or_insert_with(Default::default);
            cache.size = size;
            cache.set_dir(panel.cwd.display());
            // What is on screen, and the page after it.
            let start = panel.offset;
            let end = (start + panel.page.max(1) * 2).min(panel.entries.len());
            for e in &panel.entries[start..end] {
                let Some(kind) = crate::thumbs::kind_of(e, &panel.cwd) else { continue };
                let key = crate::thumbs::ThumbKey::new(&panel.cwd, e, size, bg);
                if cache.contains(&key) {
                    continue;
                }
                cache.start(key.clone());
                let (backend, path, name) =
                    (panel.backend.clone(), panel.cwd.join(&e.name), e.name.clone());
                let (tx, slots, cancel) =
                    (self.tx.clone(), self.thumb_slots.clone(), cache.cancel.clone());
                let px = size.pixels();
                tokio::spawn(async move {
                    let Ok(_slot) = slots.acquire_owned().await else { return };
                    // Left behind while it waited its turn: nobody wants it now.
                    if cancel.is_cancelled() {
                        return;
                    }
                    let thumb = crate::thumbs::load(backend, path, name, kind, px, bg, base).await;
                    let _ = tx.send(AppEvent::Thumbnail { side, key, thumb }).await;
                });
            }
        }
    }
}
