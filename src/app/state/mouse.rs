//! Mouse handling: panel hit-testing and function-key bar clicks.

use super::*;

/// Two left clicks on the same entry within this window count as a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

impl AppState {
    /// Handle a mouse event. Left clicks/drags move the cursor and drive the
    /// menus and dialogs; right clicks/drags mark files; the wheel scrolls the
    /// panel under the pointer.
    pub async fn handle_mouse(&mut self, ev: MouseEvent) -> Flow {
        // As for keys: moving the mouse counts as being there, and over the
        // screensaver it only takes the screensaver down.
        self.last_input = Instant::now();
        if self.saver.is_some() {
            self.stop_saver();
            return Flow::Continue;
        }
        let area = self.last_area;
        let (col, row) = (ev.column, ev.row);
        let left_down = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left));

        // A modal dialog gets first claim on a left click.
        if self.dialog.is_some() {
            if left_down {
                let res = self.dialog.as_mut().unwrap().handle_click(area, col, row);
                // Live theme + language preview, mirroring the keyboard path.
                self.preview_settings_choices();
                return self.handle_dialog_result(res).await;
            }
            // The wheel scrolls dialogs with a scrollable region (e.g. the
            // multi-rename file lists); three rows per notch, like the viewer.
            let delta = match ev.kind {
                MouseEventKind::ScrollDown => 3,
                MouseEventKind::ScrollUp => -3,
                _ => return Flow::Continue,
            };
            let res = self.dialog.as_mut().unwrap().handle_scroll(delta);
            // Wheel-scrolling a settings Choice dropdown previews live too.
            self.preview_settings_choices();
            return self.handle_dialog_result(res).await;
        }

        // Then the pulldown menu.
        if self.menu.is_some() {
            if left_down {
                let signal = self.menu.as_mut().unwrap().click(area, col, row);
                return match signal {
                    MenuSignal::Stay => Flow::Continue,
                    MenuSignal::Close => {
                        self.menu = None;
                        Flow::Continue
                    }
                    MenuSignal::Activate(action) => {
                        self.menu = None;
                        self.run_menu_action(action).await
                    }
                };
            }
            return Flow::Continue;
        }

        // The disk manager handles its own clicks (cursor + double-click menus).
        if self.mountview.is_some() {
            let sig = self.mountview.as_mut().unwrap().handle_mouse(ev);
            self.apply_mount_signal(sig).await;
            return Flow::Continue;
        }

        // The editor and viewer handle their own mouse (cursor/marking/scroll).
        if self.editor.is_some() {
            let sig = self.editor.as_mut().unwrap().handle_mouse(ev);
            self.apply_editor_signal(sig).await;
            return Flow::Continue;
        }
        if self.viewer.is_some() {
            let sig = self.viewer.as_mut().unwrap().handle_mouse(ev);
            self.apply_viewer_signal(sig).await;
            return Flow::Continue;
        }
        // The theme editor hit-tests clicks/scroll against the zones it stored
        // during the last render.
        if self.theme_editor.is_some() {
            let sig = self.theme_editor.as_mut().unwrap().handle_mouse(ev);
            self.apply_theme_editor_signal(sig);
            return Flow::Continue;
        }

        // Disk explorer: a left click points the cursor at whatever is under it —
        // a listed file, a nested subdirectory box, or the box itself; a second
        // click on the same box (within the double-click window) dives into the
        // directory the cursor now points at. `usize::MAX` marks a disk click so
        // it never collides with a file-panel double-click.
        if self.diskview.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = ev.kind {
                const DISK: usize = usize::MAX;
                if let Some(i) = self.diskview.as_ref().unwrap().box_at(col, row) {
                    let dv = self.diskview.as_mut().unwrap();
                    dv.click(i, col, row);
                    let now = Instant::now();
                    let double = self.last_click.is_some_and(|(p, idx, t)| {
                        p == DISK && idx == i && now.duration_since(t) < DOUBLE_CLICK
                    });
                    if double {
                        self.last_click = None; // a third click shouldn't re-fire
                        let sig = self.diskview.as_mut().unwrap().descend();
                        self.apply_disk_signal(sig).await;
                    } else {
                        self.last_click = Some((DISK, i, now));
                    }
                }
            }
            return Flow::Continue;
        }

        // Network explorer. In the overview diagram a left click selects the IP
        // node under the pointer and opens its details (with reverse-DNS); the
        // wheel scrolls the grid. In the list panes the wheel scrolls the focused
        // pane. Other events are swallowed so they can't reach the hidden panels.
        if let Some(nv) = self.netview.as_mut() {
            if nv.focus == Pane::Overview {
                let mut sig = NetSignal::Stay;
                match ev.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(i) = nv.node_at(col, row) {
                            nv.overview_cursor = i;
                            if let Some((ci, ii)) =
                                nv.overview_nodes.get(i).map(|(c, r, _)| (*c, *r))
                            {
                                sig = nv.open_ip_detail_at(ci, ii);
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        let _ = nv.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
                    }
                    MouseEventKind::ScrollUp => {
                        let _ = nv.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
                    }
                    _ => {}
                }
                if let NetSignal::ResolveDns(ip) = sig {
                    self.start_reverse_dns(ip);
                }
                return Flow::Continue;
            }
            let code = match ev.kind {
                MouseEventKind::ScrollDown => Some(KeyCode::Down),
                MouseEventKind::ScrollUp => Some(KeyCode::Up),
                _ => None,
            };
            if let Some(code) = code {
                for _ in 0..3 {
                    let _ = nv.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
                }
            }
            return Flow::Continue;
        }

        // The remaining full-screen overlays don't use the mouse yet; swallow the
        // event so it can't move the hidden file-panel cursor underneath them.
        if self.procview.is_some() || self.diffview.is_some() {
            return Flow::Continue;
        }

        // A fresh press starts a new gesture; forget the last painted entry.
        if matches!(ev.kind, MouseEventKind::Down(_)) {
            self.paint_last = None;
        }

        // Base mode: the F-key bar, then the menu bar, then the file panels.
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A click on the bottom F-key bar acts as that function key.
                if let Some(flow) = self.fkey_bar_click(area, col, row).await {
                    return flow;
                }
                // A click on the menu-bar mini progress bar opens the list of
                // background operations.
                let menubar_row = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
                if let Some(mr) = self.menu_progress_rect(menubar_row)
                    && col >= mr.x
                    && col < mr.x + mr.width
                    && row == mr.y
                {
                    self.open_background_ops();
                    return Flow::Continue;
                }
                // A click on the menu bar (top row) opens that menu.
                if let Some(i) = MenuBarState::title_index_at(area, col, row) {
                    self.menu =
                        Some(MenuBarState::new(i, &self.session_list(), self.side_remote()));
                } else if let Some((side, index)) = self.tab_at(col, row) {
                    // A click on the tab strip switches to that tab. Tested
                    // before the listing, since the strip sits inside the panel.
                    self.active = side;
                    self.tab_select(side, index).await;
                } else if self.details_audio_mouse(ev) {
                    // A click on a Details view's audio picture or controls.
                } else if self.scrub_click(col, row) {
                    // A click on the time machine's track seeks to that point in
                    // the history; the ◀/▶ ends step one commit.
                } else if let Some((side, back)) = self.history_arrow_at(col, row) {
                    // A click on a panel's ◀/▶ history arrow steps it back/forward.
                    self.active = side;
                    if back {
                        self.go_back(side).await;
                    } else {
                        self.go_forward(side).await;
                    }
                } else if self.begin_drag_orbit(col, row) {
                    // Pressing on a 3D panel arms an orbit; the click still
                    // picks a box, so a press-and-release selects as before.
                    self.panel_point(col, row, PointAction::Cursor);
                } else if let Some((pi, idx)) = self.panel_point(col, row, PointAction::Cursor) {
                    // A second click on the same entry within the window opens it,
                    // exactly like pressing Enter (descend a dir, open a file).
                    let now = Instant::now();
                    let double = self.last_click.is_some_and(|(p, i, t)| {
                        p == pi && i == idx && now.duration_since(t) < DOUBLE_CLICK
                    });
                    if double {
                        self.last_click = None; // don't let a third click re-fire
                        // In the tree, a double-click opens the branch and points
                        // the other panel at it, just like pressing Enter.
                        if self.panels[self.active].is_tree() {
                            self.tree_enter().await;
                            return Flow::Continue;
                        }
                        // On an Activity log row, it shows the file, as Enter does.
                        if self.panels[self.active].activity.is_some() {
                            self.activity_enter().await;
                            return Flow::Continue;
                        }
                        return self.enter_dir().await;
                    }
                    self.last_click = Some((pi, idx, now));
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.details_audio_mouse(ev) || self.drag_orbit_step(col, row) {
                    return Flow::Continue;
                }
                self.panel_point(col, row, PointAction::Cursor);
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Arm an orbit rather than painting a selection: a 3D panel has
                // no marks to paint.
                if self.begin_drag_orbit(col, row) {
                    return Flow::Continue;
                }
                self.panel_point(col, row, PointAction::InvertPaint);
            }
            MouseEventKind::Drag(MouseButton::Right) => {
                if self.drag_orbit_step(col, row) {
                    return Flow::Continue;
                }
                self.panel_point(col, row, PointAction::InvertPaint);
            }
            MouseEventKind::Up(_) => {
                // Letting go of a drag along an audio picture seeks there.
                self.details_audio_mouse(ev);
                self.drag_orbit = None;
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if self.details_audio_mouse(ev) => {}
            MouseEventKind::ScrollDown => self.panel_wheel(col, row, true),
            MouseEventKind::ScrollUp => self.panel_wheel(col, row, false),
            _ => {}
        }
        Flow::Continue
    }

    /// Whether `ev` is answered without the hit-test geometry the last frame
    /// recorded, which lets the input batch in `app::run_loop` fold it into one
    /// frame together with the events around it instead of drawing first.
    ///
    /// That matters most for an orbit. The terminal reports a drag once per cell
    /// the pointer crosses, and on a graphics terminal every frame of a 3D view
    /// re-transmits its whole image: drawing one per report let the reports
    /// queue up faster than they were answered, and the camera went on turning
    /// long after the button came up.
    pub fn mouse_folds(&self, ev: &MouseEvent) -> bool {
        match ev.kind {
            // The wheel only asks which panel it is over, and no amount of
            // scrolling moves a panel. Nothing answers bare pointer motion or
            // a sideways scroll at all.
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
            | MouseEventKind::Moved => true,
            // An orbit turns the pointer's travel into an angle, measured from a
            // position it recorded itself.
            MouseEventKind::Drag(_) | MouseEventKind::Up(_) => self.orbiting(),
            MouseEventKind::Down(_) => false,
        }
    }

    /// Whether a drag in progress reaches a 3D camera — the viewer's model or a
    /// 3D panel — rather than moving a cursor or marking files.
    ///
    /// Follows the routing order of [`handle_mouse`]: whatever claims the mouse
    /// ahead of these two takes the drag from them.
    ///
    /// [`handle_mouse`]: AppState::handle_mouse
    fn orbiting(&self) -> bool {
        if self.dialog.is_some()
            || self.menu.is_some()
            || self.mountview.is_some()
            || self.editor.is_some()
        {
            return false;
        }
        if let Some(v) = &self.viewer {
            return v.orbiting();
        }
        if self.theme_editor.is_some()
            || self.diskview.is_some()
            || self.netview.is_some()
            || self.procview.is_some()
            || self.diffview.is_some()
        {
            return false;
        }
        self.drag_orbit.is_some() || self.details_audio_scrubbing()
    }

    /// Start tracking a drag over a 3D panel, so moving the pointer orbits the
    /// camera. Returns whether the pointer was over one.
    fn begin_drag_orbit(&mut self, col: u16, row: u16) -> bool {
        match self.panel_at(col, row).filter(|&i| self.panels[i].is_space3d()) {
            Some(i) => {
                self.active = i;
                self.drag_orbit = Some((i, col, row));
                true
            }
            None => false,
        }
    }

    /// Continue an armed orbit: turn how far the pointer moved into a change of
    /// angle. Returns whether the drag was handled.
    fn drag_orbit_step(&mut self, col: u16, row: u16) -> bool {
        let Some((i, px, py)) = self.drag_orbit else {
            return false;
        };
        let (dx, dy) = (col as i32 - px as i32, row as i32 - py as i32);
        self.drag_orbit = Some((i, col, row));
        if dx == 0 && dy == 0 {
            return true;
        }
        if let Some(sp) = self.panels[i].space3d.as_mut() {
            // A cell is about twice as tall as it is wide, so the same pointer
            // travel covers twice the angle vertically unless the rates differ.
            sp.orbit(dx as f32 * -0.05, dy as f32 * 0.05);
        }
        true
    }

    /// The panel whose rendered area contains `(col, row)`. A hidden panel — and
    /// the Details view, which has no listing of its own — records no geometry, so
    /// the pointer never lands on one.
    fn panel_at(&self, col: u16, row: u16) -> Option<usize> {
        (0..2).find(|&i| self.panels[i].hit.is_some_and(|h| h.in_panel(col, row)))
    }

    /// Wheel over a file panel: a notch moves the cursor a whole page, exactly
    /// like PgDn/PgUp. A page move past either end is clamped onto the first/last
    /// entry, so the wheel keeps paging right up to the ends of the listing; only
    /// once a page has nowhere left to go does a notch act as ↓/↑ instead — how
    /// Midnight Commander's wheel behaves. The panel under the pointer scrolls;
    /// which panel is active doesn't change.
    fn panel_wheel(&mut self, col: u16, row: u16, down: bool) {
        // The tree carries its own cursor over its own row list; the listing
        // views move the panel cursor over `entries`.
        fn cursor_of(p: &Panel) -> usize {
            if p.format == ViewFormat::Tree {
                p.tree.as_ref().map_or(0, |t| t.cursor)
            } else {
                p.cursor
            }
        }

        let Some(pi) = self.panel_at(col, row) else {
            return;
        };
        // In the 3D view the wheel zooms the camera rather than scrolling a list.
        if self.panels[pi].is_space3d() {
            if let Some(sp) = self.panels[pi].space3d.as_mut() {
                sp.zoom(if down { 1.18 } else { 0.85 });
            }
            return;
        }
        let p = &mut self.panels[pi];
        // `page` is the screenful the renderer measured (rows × columns in the
        // Brief grid) — the very step PgUp/PgDn take.
        let page = p.page.max(1) as isize;
        let delta = if down { page } else { -page };
        let before = cursor_of(p);
        p.move_cursor(delta);
        // Nothing moved: the cursor already sits on the first/last entry, where
        // the page key does nothing and the wheel falls back to the arrow key.
        if cursor_of(p) == before {
            p.move_cursor(delta.signum());
        }
    }

    /// Map a screen point to a panel entry: activate that panel, move the cursor
    /// onto the entry (every action), and optionally toggle/paint its mark.
    /// Returns the `(panel, entry)` that was hit, or `None` when the point misses
    /// the panels or any entry.
    fn panel_point(&mut self, col: u16, row: u16, action: PointAction) -> Option<(usize, usize)> {
        let pi = self.panel_at(col, row)?;
        self.active = pi;
        let p = &mut self.panels[pi];
        // 3D view: hit-test the click against the projected box silhouettes from
        // the last frame. There is nothing to mark, so every action just moves
        // the selection.
        if p.format == ViewFormat::Space3d {
            let hit = p.hit?;
            let sp = p.space3d.as_mut()?;
            // Cell coordinates → raster pixels, in whatever resolution the last
            // frame's bounds were measured in.
            let (bw, bh) = sp.bounds_px;
            if hit.body.width == 0 || hit.body.height == 0 || bw == 0 || bh == 0 {
                return None;
            }
            let fx = (col.saturating_sub(hit.body.x)) as f32 / hit.body.width as f32;
            let fy = (row.saturating_sub(hit.body.y)) as f32 / hit.body.height as f32;
            sp.pick(fx * bw as f32, fy * bh as f32);
            return Some((pi, 0));
        }
        // Tree view: map the click to a tree row and move the tree cursor. There
        // is no marking in the tree, so any action just positions the cursor.
        if p.format == ViewFormat::Tree {
            let len = p.tree.as_ref().map_or(0, |t| t.rows.len());
            let idx = p.hit?.index_at(col, row, len)?;
            if let Some(t) = p.tree.as_mut() {
                t.cursor = idx;
            }
            return Some((pi, idx));
        }
        // Activity log: a click picks a row; there is nothing to mark.
        if let Some(log) = p.activity.as_mut() {
            let idx = p.hit?.index_at(col, row, log.visible_len())?;
            log.cursor = idx;
            return Some((pi, idx));
        }
        let idx = p.hit?.index_at(col, row, p.entries.len())?;
        // The cursor follows the pointer for every action (incl. drags).
        p.cursor = idx;
        if matches!(action, PointAction::Cursor) {
            return Some((pi, idx));
        }
        // Invert the mark, but only once per entry as the drag enters it, so a
        // run of drag events over the same file doesn't flip it repeatedly.
        if self.paint_last == Some((pi, idx)) {
            return Some((pi, idx));
        }
        self.paint_last = Some((pi, idx));
        let p = &mut self.panels[pi];
        // Selection never touches the "..".
        if let Some(e) = p.entries.get(idx)
            && e.name != ".."
        {
            let name = e.name.clone();
            p.selection.toggle(&name);
        }
        Some((pi, idx))
    }

    /// If `(col, row)` falls on the bottom F-key bar, run the corresponding
    /// panel-mode function key and return its `Flow`; otherwise `None`.
    async fn fkey_bar_click(&mut self, area: Rect, col: u16, row: u16) -> Option<Flow> {
        let bar = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(1),
            width: area.width,
            height: 1,
        };
        let i = crate::ui::fkeys::index_at(bar, &crate::ui::fkeys::PANEL_LABELS, col, row)?;
        let key = KeyEvent::new(KeyCode::F(i as u8 + 1), KeyModifiers::NONE);
        Some(self.handle_panel_key(key).await)
    }
}

impl AppState {
    /// A click on the time machine's scrub track. Returns whether it landed
    /// there, so the ordinary panel click does not also fire.
    ///
    /// The ends step one commit each; anywhere along the bar seeks in
    /// proportion, so dragging across it runs through the history.
    pub(in crate::app::state) fn scrub_click(&mut self, col: u16, row: u16) -> bool {
        let Some(tl) = self.timeline.as_ref() else { return false };
        let Some(area) = self.panels[tl.side].scrub_area else { return false };
        if row != area.y || col < area.x || col >= area.x + area.width {
            return false;
        }
        let total = tl.revs.len();
        if total == 0 {
            return true;
        }
        // Mirrors `render_scrub_row`'s layout: a leading ◀, the bar, the
        // position readout, and a trailing ▶.
        let pos_w = format!(" {}/{} ", tl.position().0, total).chars().count() as u16;
        let lead = area.x + 2;
        let bar_w = area.width.saturating_sub(pos_w + 4).max(1);
        let x = col.saturating_sub(area.x);
        if x < 2 {
            self.timeline_step(-1);
        } else if col >= area.x + area.width - 2 {
            self.timeline_step(1);
        } else if col >= lead && col < lead + bar_w {
            // The inverse of `render_scrub_row`'s placement, so a click lands on
            // the commit the marker is sitting on. The bar runs oldest on the
            // left, so the index — which counts newest first — is taken back
            // from the end.
            let along = (col - lead) as usize;
            let span = (bar_w as usize).saturating_sub(1).max(1);
            let oldest_first = (along * (total - 1)) / span;
            if let Some(t) = self.timeline.as_mut() {
                t.seek(total - 1 - oldest_first.min(total - 1));
            }
        }
        true
    }
}
