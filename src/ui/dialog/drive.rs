//! Drive / connection / session picker (Alt-F1 left, Alt-F2 right).

use super::widgets::*;
use super::{DialogResult, Submit};
use crate::mount::Volume;
use crate::vfs::remote::Protocol;
use std::path::PathBuf;

/// Gap between two buttons on the same row.
const GAP: usize = 1;
/// The Local button's label (also its width budget when packing its row).
const LOCAL_LABEL: &str = " Local ";
/// Volume names are shortened to this many characters so one long mount point
/// cannot stretch the dialog.
const VOL_NAME_MAX: usize = 18;
/// Widest a button row is ever packed, however wide the panel is; further
/// buttons wrap onto a new row.
const MAX_ROW_W: usize = 34;

// ---------------------------------------------------------------------------
// Drive / connection / session picker (Alt-F1 left, Alt-F2 right)
// ---------------------------------------------------------------------------

/// One button in the picker. Kept `Copy` (no owned strings) — session labels are
/// looked up by id from the dialog's `sessions` list at render time.
#[derive(Clone, Copy)]
enum DriveItem {
    /// A local drive letter (Windows).
    Drive(char),
    /// Return the panel to its last local directory (sessions stay open).
    Local,
    /// Jump to a mounted volume, by index into the dialog's `volumes` list.
    Volume { idx: usize },
    /// Switch to an already-open remote session.
    Session { id: usize },
    /// Disconnect (tear down) a remote session — the `✕` button.
    DisconnectSession { id: usize },
    /// Open a new remote connection of this protocol.
    Connect(Protocol),
}

impl DriveItem {
    fn drive(&self) -> Option<char> {
        match self {
            DriveItem::Drive(c) => Some(*c),
            _ => None,
        }
    }
}

/// A Norton-style picker for a panel's source: drive-letter buttons (Windows) on
/// the first row(s), then Local plus a button per mounted volume (Unix: disks
/// and NAS shares under `/mnt`, automounted media under `/run/media`, …); then —
/// unless the other panel is already remote — one row per open remote session
/// (switch + `✕` disconnect) and an SFTP/FTP/SCP row for new connections. Arrow
/// keys move the highlight; a drive letter or a click jumps straight to that
/// button.
pub struct DriveDialog {
    /// Which panel this is for (0 = left, 1 = right).
    side: usize,
    items: Vec<DriveItem>,
    /// Open sessions available to switch to: `(id, label)`. Used to resolve the
    /// text of `Session`/`DisconnectSession` buttons at render time.
    sessions: Vec<(usize, String)>,
    /// Mounted volumes offered beside Local: `(button name, mount point)`, with
    /// the name already shortened to `VOL_NAME_MAX`.
    volumes: Vec<(String, PathBuf)>,
    /// How many leading `items` are drive letters (the grid).
    drive_count: usize,
    cursor: usize,
    /// `(button_row, center_x)` per item, recorded at render for Up/Down nav.
    layout: Vec<(usize, u16)>,
    /// Clickable rects → item index, recorded at render.
    zones: Vec<(Rect, usize)>,
}

impl DriveDialog {
    pub fn new(
        side: usize,
        drives: Vec<char>,
        current: Option<char>,
        current_session: Option<usize>,
        volumes: Vec<Volume>,
        sessions: Vec<(usize, String)>,
        show_remote: bool,
    ) -> Self {
        let mut items: Vec<DriveItem> = drives.iter().map(|&c| DriveItem::Drive(c)).collect();
        let drive_count = items.len();
        let volumes: Vec<(String, PathBuf)> = volumes
            .into_iter()
            .map(|v| (crate::util::text::ellipsize(&v.name, VOL_NAME_MAX), v.path))
            .collect();

        // The Local button is always offered, below the grid, followed by one
        // button per mounted volume. `rows` groups them into display rows.
        items.push(DriveItem::Local);
        let local_index = items.len() - 1;
        for idx in 0..volumes.len() {
            items.push(DriveItem::Volume { idx });
        }

        // Sessions + connect buttons are hidden when the other panel is remote.
        if show_remote {
            for (id, _) in &sessions {
                items.push(DriveItem::Session { id: *id });
                items.push(DriveItem::DisconnectSession { id: *id });
            }
            for p in [Protocol::Sftp, Protocol::Ftp, Protocol::Ftps, Protocol::Scp] {
                items.push(DriveItem::Connect(p));
            }
        }

        // Highlight the current session, else the current drive, else Local.
        let cursor = if let Some(sid) = current_session {
            items
                .iter()
                .position(|it| matches!(it, DriveItem::Session { id } if *id == sid))
                .unwrap_or(local_index)
        } else if let Some(c) = current {
            drives.iter().position(|&d| d == c).unwrap_or(local_index)
        } else {
            local_index
        };

        DriveDialog {
            side,
            items,
            sessions,
            volumes,
            drive_count,
            cursor,
            layout: Vec::new(),
            zones: Vec::new(),
        }
    }

    fn has_drives(&self) -> bool {
        self.drive_count > 0
    }

    /// The display text for item `i` (session labels resolved from `sessions`).
    fn item_label(&self, i: usize) -> String {
        match self.items[i] {
            DriveItem::Drive(c) => format!("  {c}  "),
            DriveItem::Local => LOCAL_LABEL.to_string(),
            DriveItem::Volume { idx } => format!(" {} ", self.volumes[idx].0),
            DriveItem::Connect(Protocol::Sftp) => " SFTP ".to_string(),
            DriveItem::Connect(Protocol::Ftp) => " FTP ".to_string(),
            DriveItem::Connect(Protocol::Ftps) => " FTPS ".to_string(),
            DriveItem::Connect(Protocol::Scp) => " SCP ".to_string(),
            DriveItem::Session { id } => {
                let lbl = self
                    .sessions
                    .iter()
                    .find(|(sid, _)| *sid == id)
                    .map(|(_, l)| l.as_str())
                    .unwrap_or("");
                format!(" {lbl} ")
            }
            DriveItem::DisconnectSession { .. } => " ✕ ".to_string(),
        }
    }

    fn label_width(&self, i: usize) -> usize {
        self.item_label(i).chars().count()
    }

    /// The item indices of the rows *below* the drive grid, in display order:
    /// Local with the volume buttons, then one row per open session (switch +
    /// `✕`), then the connect row. Rows longer than `budget` columns wrap, so a
    /// machine with many mounts stays inside a narrow panel.
    fn rows(&self, budget: usize) -> Vec<Vec<usize>> {
        let mut rows: Vec<Vec<usize>> = Vec::new();
        let mut row: Vec<usize> = Vec::new();
        let mut row_w = 0;
        for i in self.drive_count..self.items.len() {
            let w = self.label_width(i);
            let wraps = !row.is_empty() && row_w + GAP + w > budget;
            let opens_row = match self.items[i] {
                // Local opens the volume row; volumes join it while they fit.
                DriveItem::Local | DriveItem::Session { .. } => true,
                DriveItem::Volume { .. } => wraps,
                // A session's ✕ always stays beside its switch button.
                DriveItem::DisconnectSession { .. } => false,
                // The connect row opens at SFTP, the first protocol offered.
                DriveItem::Connect(Protocol::Sftp) => true,
                DriveItem::Connect(_) => wraps,
                DriveItem::Drive(_) => true,
            };
            if opens_row && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                row_w = 0;
            }
            if !row.is_empty() {
                row_w += GAP;
            }
            row.push(i);
            row_w += w;
        }
        if !row.is_empty() {
            rows.push(row);
        }
        rows
    }

    /// What activating button `i` asks the app to do. Volume buttons are
    /// resolved here because the dialog, not the `Copy` item, owns the paths.
    fn submit_item(&self, i: usize) -> Submit {
        match self.items[i] {
            DriveItem::Drive(c) => Submit::SetDrive(self.side, c),
            DriveItem::Local => Submit::GoLocal(self.side),
            DriveItem::Volume { idx } => Submit::GoVolume(self.side, self.volumes[idx].1.clone()),
            DriveItem::Session { id } => Submit::SwitchSession(self.side, id),
            DriveItem::DisconnectSession { id } => Submit::AskDisconnectSession(id),
            DriveItem::Connect(p) => Submit::OpenConnect(self.side, p),
        }
    }

    /// Which panel this picker targets (0 = left, 1 = right), so the renderer can
    /// anchor it over that panel.
    pub(crate) fn side(&self) -> usize {
        self.side
    }

    fn move_vert(&mut self, dir: isize) {
        if self.layout.len() != self.items.len() {
            return;
        }
        let (row, x) = self.layout[self.cursor];
        let target = row as isize + dir;
        if let Some((i, _)) = self
            .layout
            .iter()
            .enumerate()
            .filter(|(_, (r, _))| *r as isize == target)
            .min_by_key(|(_, (_, cx))| (*cx as i32 - x as i32).abs())
        {
            self.cursor = i;
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        let n = self.items.len();
        if n == 0 {
            return DialogResult::Cancel;
        }
        match key.code {
            KeyCode::Esc => DialogResult::Cancel,
            KeyCode::Enter => DialogResult::Submit(self.submit_item(self.cursor)),
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                DialogResult::None
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(n - 1);
                DialogResult::None
            }
            KeyCode::Up => {
                self.move_vert(-1);
                DialogResult::None
            }
            KeyCode::Down => {
                self.move_vert(1);
                DialogResult::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                DialogResult::None
            }
            KeyCode::End => {
                self.cursor = n - 1;
                DialogResult::None
            }
            // A drive letter jumps straight to that drive.
            KeyCode::Char(c) => {
                let up = c.to_ascii_uppercase();
                match self.items.iter().position(|it| it.drive() == Some(up)) {
                    Some(i) => DialogResult::Submit(self.submit_item(i)),
                    None => DialogResult::None,
                }
            }
            _ => DialogResult::None,
        }
    }

    pub(crate) fn handle_click(&mut self, _area: Rect, col: u16, row: u16) -> DialogResult {
        for (r, i) in &self.zones {
            if col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height {
                return DialogResult::Submit(self.submit_item(*i));
            }
        }
        DialogResult::None
    }

    pub(crate) fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        gfx: Option<&mut Gfx>,
    ) {
        let mut gfx = gfx;
        const DCELL: usize = 5; // "  X  " drive cell

        // Drive grid: how many letter cells fit per row.
        let avail = (area.width.saturating_sub(8) as usize).max(DCELL);
        let dcols = (((avail + GAP) / (DCELL + GAP)).max(1)).min(self.drive_count.max(1));
        let drive_rows = if self.has_drives() { self.drive_count.div_ceil(dcols) } else { 0 };
        let drive_w =
            if self.has_drives() { dcols * DCELL + dcols.saturating_sub(1) * GAP } else { 0 };

        // Rows below the grid, packed to whichever is narrower: the panel or
        // MAX_ROW_W. Width of each below-grid row (Local + volumes, session
        // pairs, connect row).
        let below = self.rows(avail.min(MAX_ROW_W));
        let row_width = |this: &Self, row: &[usize]| -> usize {
            row.iter().map(|&i| this.label_width(i)).sum::<usize>()
                + row.len().saturating_sub(1) * GAP
        };
        let below_w = below.iter().map(|r| row_width(self, r)).max().unwrap_or(0);

        let noun = if self.has_drives() { "drive" } else { "location" };
        let msg_w = "Choose right :".len() + noun.len(); // upper bound

        let content_w = drive_w.max(below_w).max(msg_w).max(18);
        let box_w = (content_w as u16 + 6).min(area.width.saturating_sub(2));
        let gap_rows = if self.has_drives() { 1 } else { 0 };
        // borders + pads + msg (constant 6) + drive grid + gap + one line per
        // below-grid row.
        let box_h = (drive_rows as u16) + (gap_rows as u16) + below.len() as u16 + 6;
        let rect = centered(area, box_w, box_h);
        draw_shadow(f, rect, theme);
        f.render_widget(Clear, rect);
        let title = if self.has_drives() { "Drive Letter" } else { "Location" };
        let block = dialog_block(title, theme);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        f.render_widget(Paragraph::new("").style(Style::default().bg(theme.dialog_bg)), inner);

        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let accent = base.fg(theme.hotkey_fg).add_modifier(Modifier::BOLD);
        let side_word = if self.side == 0 { "left" } else { "right" };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Choose ", base),
                Span::styled(side_word, accent),
                Span::styled(format!(" {noun}:"), base),
            ]))
            .alignment(ratatui::layout::Alignment::Center)
            .style(base),
            Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: 1 },
        );

        self.zones.clear();
        self.layout = vec![(0, 0); self.items.len()];
        let grid_y = inner.y + 3;

        // Drive buttons (grid).
        if self.has_drives() {
            let gx = inner.x + inner.width.saturating_sub(drive_w as u16) / 2;
            for i in 0..self.drive_count {
                let (col, brow) = (i % dcols, i / dcols);
                let x = gx + (col * (DCELL + GAP)) as u16;
                let cell = Rect { x, y: grid_y + brow as u16, width: DCELL as u16, height: 1 };
                let label = self.item_label(i);
                self.draw_button(f, theme, i, cell, brow, &label, gfx.as_deref_mut());
            }
        }

        // Rows below the grid: Local + volumes, one per session pair, then the
        // connect row. Logical row indices continue after the drive grid so
        // Up/Down step between the grid and these rows.
        let below_y0 = grid_y + (drive_rows + gap_rows) as u16;
        for (k, row) in below.iter().enumerate() {
            let rw = row_width(self, row) as u16;
            let mut cx = inner.x + inner.width.saturating_sub(rw) / 2;
            let y = below_y0 + k as u16;
            let logical_row = drive_rows + k;
            for &i in row {
                let w = self.label_width(i) as u16;
                let label = self.item_label(i);
                self.draw_button(
                    f,
                    theme,
                    i,
                    Rect { x: cx, y, width: w, height: 1 },
                    logical_row,
                    &label,
                    gfx.as_deref_mut(),
                );
                cx += w + GAP as u16;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_button(
        &mut self,
        f: &mut Frame,
        theme: &Theme,
        i: usize,
        rect: Rect,
        row: usize,
        label: &str,
        gfx: Option<&mut Gfx>,
    ) {
        let focused = i == self.cursor;
        if !gfx_button(f, gfx, Slot::Button(i as u16), rect, label, focused, theme) {
            let style = if focused { theme.button_focused } else { theme.button };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(label.to_string(), style))),
                rect,
            );
        }
        self.zones.push((rect, i));
        self.layout[i] = (row, rect.x + rect.width / 2);
    }
}
