//! Persistent configuration: external programs and behaviour flags.
//!
//! Stored as TOML under the user's XDG config directory. Loading never fails
//! hard — a missing or malformed file falls back to defaults so the app always
//! starts.

pub mod paths;

use serde::{Deserialize, Serialize};

/// How many commits the `git://` backend lists by default. A cap rather than a
/// preference: an unbounded `git log` on a kernel-sized history is over a
/// million rows, and nobody scrolls to the bottom of that.
pub const DEFAULT_GIT_REV_LIMIT: usize = 500;

/// One saved directory tab. Only what survives a restart: the listing, cursor
/// and history are all rebuilt from the directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TabRecord {
    pub dir: String,
    pub filter: String,
    pub view: PanelView,
}

/// A previously-used remote connection, remembered for the connect dialog's
/// dropdown. Passwords are intentionally *not* stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHistoryEntry {
    /// Protocol scheme prefix: `"sftp"`, `"ftp"`, or `"scp"`.
    pub protocol: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub path: String,
    /// FTP passive mode for this server (see [`crate::vfs::remote::RemoteCreds`]).
    /// Defaults to `true` so entries saved before this field existed reconnect in
    /// passive mode, the FTP default.
    #[serde(default = "crate::config::default_true")]
    pub passive: bool,
    /// SSH key file used for this server, so a key-authenticated host reconnects
    /// without retyping the path. Blank means "agent / default keys" (and is what
    /// entries saved before this field existed get). Passphrases are *not* stored.
    #[serde(default)]
    pub key_file: String,
}

/// serde default for [`RemoteHistoryEntry::passive`].
pub(crate) fn default_true() -> bool {
    true
}

impl RemoteHistoryEntry {
    /// One-line label for the dropdown, e.g. `user@host:22  /remote/path`.
    pub fn label(&self) -> String {
        let user = if self.user.is_empty() { String::new() } else { format!("{}@", self.user) };
        let path = if self.path.is_empty() { String::new() } else { format!("  {}", self.path) };
        format!("{user}{}:{}{path}", self.host, self.port)
    }
}

/// A panel's remembered view state: its listing format and sort order. Stored
/// per panel so the two sides restore independently across sessions.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelView {
    pub format: crate::panel::ViewFormat,
    pub sort: crate::panel::sort::SortConfig,
}

/// How the editor treats long lines (Options → General → "Wrap mode").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WrapMode {
    /// Long lines run off the right edge; the view scrolls sideways.
    #[default]
    None,
    /// Long lines are *shown* across several rows. The file is not changed.
    Dynamic,
    /// Typing past the wrap column breaks the line for real, as a typewriter
    /// would — the newline is written into the file.
    Typewriter,
}

impl WrapMode {
    /// The three modes in dialog order, with the labels the form shows.
    pub const ALL: [(WrapMode, &'static str); 3] = [
        (WrapMode::None, "None"),
        (WrapMode::Dynamic, "Dynamic paragraphing"),
        (WrapMode::Typewriter, "Type writer wrap"),
    ];

    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(m, _)| *m == self).map(|(_, l)| *l).unwrap_or("None")
    }

    /// The mode a dialog label selects (unknown text falls back to `None`).
    pub fn from_label(label: &str) -> Self {
        Self::ALL.iter().find(|(_, l)| *l == label).map(|(m, _)| *m).unwrap_or(WrapMode::None)
    }
}

/// How an audio file is drawn in the viewer and the Details view (Settings →
/// Panels → "Audio view").
///
/// Both come from the same background analysis; only the picture differs. See
/// `crate::audio`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioDisplay {
    /// Frequency over time, loudness as colour.
    #[default]
    Spectrogram,
    /// Amplitude over time: the peak envelope with the RMS level inside it.
    Waveform,
}

impl AudioDisplay {
    /// Both pictures in dialog order, with the labels the form shows. These
    /// double as the stored chooser values, so they are never translated.
    pub const ALL: [(AudioDisplay, &'static str); 2] =
        [(AudioDisplay::Spectrogram, "Spectrogram"), (AudioDisplay::Waveform, "Waveform")];

    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(m, _)| *m == self).map(|(_, l)| *l).unwrap_or("Spectrogram")
    }

    /// The picture a dialog label selects (unknown text falls back to the
    /// spectrogram).
    pub fn from_label(label: &str) -> Self {
        Self::ALL.iter().find(|(_, l)| *l == label).map(|(m, _)| *m).unwrap_or_default()
    }

    /// The other picture, for the viewer's F2.
    pub fn toggled(self) -> Self {
        match self {
            AudioDisplay::Spectrogram => AudioDisplay::Waveform,
            AudioDisplay::Waveform => AudioDisplay::Spectrogram,
        }
    }
}

/// Which look the panel's 3D view draws (Settings → Visual → "3D style").
///
/// Both styles come from the same renderer and the same size cache; only the
/// layout, the shapes and the background differ. See `crate::space3d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Space3dStyle {
    /// Shaded cubes hanging in the panel background, children on rings below
    /// their parent.
    #[default]
    Cubes,
    /// SGI IRIX `fsn`: platforms standing on a ground plane under a sky
    /// gradient, the files on them drawn as solids shaped by type.
    Fsn,
}

impl Space3dStyle {
    /// The two styles in dialog order, with the labels the form shows. These
    /// double as the stored chooser values, so they are never translated.
    pub const ALL: [(Space3dStyle, &'static str); 2] =
        [(Space3dStyle::Cubes, "Cubes"), (Space3dStyle::Fsn, "Spare no expense")];

    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(m, _)| *m == self).map(|(_, l)| *l).unwrap_or("Cubes")
    }

    /// The style a dialog label selects (unknown text falls back to `Cubes`).
    pub fn from_label(label: &str) -> Self {
        Self::ALL.iter().find(|(_, l)| *l == label).map(|(m, _)| *m).unwrap_or(Space3dStyle::Cubes)
    }
}

/// Which animation the screensaver plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaverKind {
    /// A different one each time it starts.
    #[default]
    Random,
    /// Norton Commander's: flying through a field of stars.
    Starfield,
    /// Columns of glyphs raining down.
    Matrix,
    /// A big clock drifting around the screen.
    Clock,
    /// Pipes growing across the screen.
    Pipes,
}

impl SaverKind {
    /// The choices in dialog order, with the labels the form shows (and stores,
    /// so they are never translated).
    pub const ALL: [(SaverKind, &'static str); 5] = [
        (SaverKind::Random, "Random"),
        (SaverKind::Starfield, "Starfield"),
        (SaverKind::Matrix, "Matrix"),
        (SaverKind::Clock, "Clock"),
        (SaverKind::Pipes, "Pipes"),
    ];

    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(k, _)| *k == self).map(|(_, l)| *l).unwrap_or("Random")
    }

    /// The kind a dialog label selects (unknown text falls back to `Random`).
    pub fn from_label(label: &str) -> Self {
        Self::ALL.iter().find(|(_, l)| *l == label).map(|(k, _)| *k).unwrap_or(SaverKind::Random)
    }
}

/// How big the thumbnail grid's cells are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThumbSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl ThumbSize {
    /// The choices in dialog order, with the labels the form shows (and stores).
    pub const ALL: [(ThumbSize, &'static str); 3] =
        [(ThumbSize::Small, "Small"), (ThumbSize::Medium, "Medium"), (ThumbSize::Large, "Large")];

    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(s, _)| *s == self).map(|(_, l)| *l).unwrap_or("Medium")
    }

    pub fn from_label(label: &str) -> Self {
        Self::ALL.iter().find(|(_, l)| *l == label).map(|(s, _)| *s).unwrap_or_default()
    }

    /// A grid cell in terminal cells, `(width, height)`: the image, a name row
    /// under it, and a one-cell gutter right and below.
    pub fn cell(self) -> (u16, u16) {
        match self {
            ThumbSize::Small => (12, 7),
            ThumbSize::Medium => (18, 9),
            ThumbSize::Large => (26, 13),
        }
    }

    /// The longest edge thumbnails are decoded to, in pixels: enough for the
    /// cell on an ordinary font, without holding full photos in memory.
    pub fn pixels(self) -> u32 {
        match self {
            ThumbSize::Small => 128,
            ThumbSize::Medium => 200,
            ThumbSize::Large => 320,
        }
    }
}

/// The screensaver idle times offered in Settings, in minutes; 0 is off.
pub const SAVER_MINUTES: [u16; 7] = [0, 1, 2, 5, 10, 15, 30];

/// How the Settings form shows an idle time.
pub fn saver_minutes_label(minutes: u16) -> String {
    if minutes == 0 { "Off".to_string() } else { format!("{minutes} min") }
}

/// The idle time a Settings label stands for (`"Off"` and anything else unreadable
/// is 0).
pub fn saver_minutes_from_label(label: &str) -> u16 {
    label.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// The internal editor's behaviour settings (Options → General in the editor's
/// F9 menu), persisted so they survive a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorOptions {
    pub wrap_mode: WrapMode,
    /// Column the paragraph formatter and typewriter wrap break at.
    pub word_wrap_line_length: usize,
    /// Columns a Tab advances by (and the width of one indent step).
    pub tab_spacing: usize,
    /// Tab inserts that many spaces instead of a tab character.
    pub fill_tabs_with_spaces: bool,
    /// Backspace over indentation removes a whole tab stop at a time.
    pub backspace_through_tabs: bool,
    /// Enter copies the current line's leading whitespace to the new line.
    pub return_does_autoindent: bool,
    /// F2 asks for confirmation before writing the file.
    pub confirm_before_saving: bool,
    /// Remember the cursor position per file and restore it on re-open.
    pub save_file_position: bool,
    /// Mark trailing whitespace so it can't hide.
    pub visible_trailing_spaces: bool,
    /// Draw tab characters as an arrow rather than blank space.
    pub visible_tabs: bool,
    /// Colour the buffer by syntax.
    pub syntax_highlighting: bool,
    /// Leave the cursor after a block that F5/paste just inserted (rather than
    /// before it).
    pub cursor_after_inserted_block: bool,
    /// A plain (unshifted) cursor move keeps the marked block rather than
    /// dropping it.
    pub persistent_selection: bool,
    /// A run of typing undoes in one step instead of character by character.
    pub group_undo: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        EditorOptions {
            wrap_mode: WrapMode::None,
            word_wrap_line_length: 72,
            // Four, not mcedit's eight: it is what this editor's Tab has always
            // inserted, and changing it would silently re-indent people's files.
            tab_spacing: 4,
            fill_tabs_with_spaces: true,
            backspace_through_tabs: false,
            return_does_autoindent: true,
            confirm_before_saving: true,
            save_file_position: true,
            visible_trailing_spaces: false,
            visible_tabs: false,
            syntax_highlighting: true,
            cursor_after_inserted_block: true,
            persistent_selection: true,
            group_undo: false,
        }
    }
}

/// User configuration, serialized to `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// External editor command (e.g. "vim", "code --wait"). Empty = use the
    /// internal editor.
    pub editor: String,
    /// External viewer/pager command (e.g. "less", "bat"). Empty = use the
    /// internal viewer.
    pub viewer: String,
    /// Prefer the internal viewer even when `viewer` is set.
    pub use_internal_viewer: bool,
    /// Prefer the internal editor even when `editor` is set.
    pub use_internal_editor: bool,
    /// Shell program for the `Ctrl-O` subshell and the command line (e.g.
    /// `pwsh`, `C:\Program Files\PowerShell\7\pwsh.exe`, `/bin/fish`).
    /// A program path only — arguments are added by Rat Commander to match the
    /// shell's dialect. Empty = detect it (see [`crate::shell::preferred`]).
    #[serde(default)]
    pub shell: String,
    /// Ask for confirmation before deleting.
    pub confirm_delete: bool,
    /// Send deleted files to the freedesktop trash (F8) instead of unlinking
    /// them, keeping Shift-F8 as the permanent delete. Defaults on, and is
    /// ignored where there is no trash to use (see [`crate::trash`]).
    #[serde(default = "crate::config::default_true")]
    pub use_trash: bool,
    /// Ask before overwriting an existing destination during copy/move.
    pub confirm_overwrite: bool,
    /// Ask before opening/executing a file with its default application.
    pub confirm_execute: bool,
    /// Ask before unmounting a filesystem in the disk manager.
    pub confirm_unmount: bool,
    /// Ask for confirmation before quitting.
    pub confirm_exit: bool,
    /// Active color theme (palette name).
    pub theme: String,
    /// Active UI language (the language file's display name, e.g. "Deutsch").
    /// `None` = English (the default).
    #[serde(default)]
    pub language: Option<String>,
    /// Reshape + bidi-reorder right-to-left text (Arabic/Persian) into visual
    /// order so it reads correctly on terminals without native bidi support.
    /// Turn off on terminals that do their own bidi (mlterm, modern VTE, …).
    /// (Missing from an old config → the struct default, `true`.)
    pub reshape_rtl: bool,
    /// Terminal pixel-graphics for the progress bars, process-explorer graphs and
    /// disk-explorer treemap: `auto` (use Kitty/Sixel/iTerm2 if the terminal
    /// supports it, else fall back to cell rendering), `off`, or a forced
    /// `kitty` / `sixel` / `iterm`. (Missing from an old config → the struct
    /// default, `"auto"`.)
    pub graphics: String,
    /// 24-bit color override; `None` = auto-detect from the terminal.
    pub truecolor: Option<bool>,
    /// Enable animations (gradient motion, CPU histogram).
    pub animation: bool,
    /// Show the CPU/memory status widget in the menu bar.
    pub system_status: bool,
    /// Show the shell command line below the panels (Midnight Commander's
    /// Layout → "Command prompt"). When off, the row is reclaimed by the panels
    /// and typing a printable character starts a quick search instead of
    /// entering text. (Missing from an old config → the struct default, `true`.)
    pub command_prompt: bool,
    /// Draw a Nerd Font glyph per file type in the listing instead of the plain
    /// `ls -F` classify characters (`/`, `*`, `@`, `!`). Needs a Nerd Font
    /// installed in the terminal, so it is off unless asked for. (Missing from an
    /// old config → the struct default, `false`.)
    pub nerd_font: bool,
    /// Re-read a panel automatically when something else changes the directory
    /// it is showing, instead of waiting for `Ctrl-R`. Only plain local
    /// directories are watched. Turn it off on sluggish network mounts or
    /// enormous directories, where the re-listing costs more than it saves.
    /// (Missing from an old config → the struct default, `true`.)
    pub auto_refresh: bool,
    /// End a partly-written row with an erase-to-end-of-line instead of padding
    /// it with spaces, so the terminal's own mouse selection copies each line
    /// without the trailing whitespace — the way Midnight Commander's does (see
    /// [`crate::ui::trim`]). Nothing changes on screen on a terminal that erases
    /// in the current background colour, which is every one we know of; turn it
    /// off if the right-hand side of the editor or viewer loses its background.
    /// (Missing from an old config → the struct default, `true`.)
    #[serde(default = "crate::config::default_true")]
    pub strip_trailing_spaces: bool,
    /// Number of columns in the Brief (multi-column names) view.
    /// (Missing from an old config → the struct default, `2`.)
    pub brief_columns: usize,
    /// Maximum number of command-line entries kept in the persistent history
    /// (`history` file next to this config). `0` disables it. (Missing from an
    /// old config → the struct default, `100`.)
    pub command_history_max: usize,
    /// Which look the panel's 3D view draws. (Missing from an old config → the
    /// struct default, `Cubes`.) Must stay **above** `panels`: `Config::save`
    /// writes TOML, where a scalar key cannot follow an array of tables.
    #[serde(default)]
    pub space3d_style: Space3dStyle,
    /// Whether the 3D view lights up directories as things are written into
    /// them. Arming this needs a **recursive** filesystem watch on the tree
    /// being drawn, which costs a watch descriptor per directory, so it is worth
    /// being able to turn off on a very large or a network-mounted tree.
    /// (Missing from an old config → on.)
    #[serde(default = "crate::config::default_true")]
    pub space3d_activity: bool,
    /// Whether the Details view draws a git activity calendar for an item in a
    /// work tree. Each item it describes costs a `git log`, which a huge
    /// repository may make worth turning off. (Missing from an old config → on.)
    #[serde(default = "crate::config::default_true")]
    pub details_activity: bool,
    /// How audio files are drawn: a spectrogram or a waveform. (Missing from an
    /// old config → the spectrogram.) Must stay **above** `panels`, like
    /// `space3d_style`.
    #[serde(default)]
    pub audio_display: AudioDisplay,
    /// Minutes without a key press or mouse movement before the screensaver
    /// starts; 0 turns it off (the default: on a remote session its redrawing
    /// costs bandwidth nobody is watching).
    pub screensaver_minutes: u16,
    /// Which animation the screensaver plays.
    pub screensaver: SaverKind,
    /// How big the thumbnail grid's cells are.
    pub thumb_size: ThumbSize,
    /// Per-panel view format and sort order, remembered across sessions
    /// (index 0 = left panel, 1 = right panel).
    #[serde(default)]
    pub panels: [PanelView; 2],
    /// Recently used remote connections (most recent first), for the connect
    /// dialog's history dropdown.
    #[serde(default)]
    pub recent_remotes: Vec<RemoteHistoryEntry>,
    /// Bookmarked local directories (absolute paths), listed and jumpable from
    /// the command palette (Ctrl-P). (Missing from an old config → empty.)
    #[serde(default)]
    pub bookmarks: Vec<String>,

    // -- Session layout, restored on the next launch (all default-on-absent) --
    /// Each panel's last *local* directory (index 0 = left, 1 = right). Empty
    /// when the panel was on a remote/archive location (not restorable without
    /// credentials) or the saved directory no longer exists.
    #[serde(default)]
    pub panel_dirs: [String; 2],
    /// Each panel's persistent listing filter (`Alt-Shift-I`); empty = none.
    #[serde(default)]
    pub panel_filters: [String; 2],
    /// Each panel's extra directory tabs, in tab order. Only plain-local tabs
    /// are saved — a remote one would need credentials we deliberately don't
    /// keep, exactly as for `panel_dirs`. Empty (the default, and what an older
    /// config yields) simply means "one tab", so nothing changes for anyone who
    /// never opens a second one.
    #[serde(default)]
    pub panel_tabs: [Vec<TabRecord>; 2],
    /// Which of `panel_tabs` each panel had in front.
    #[serde(default)]
    pub panel_tab_active: [usize; 2],
    /// Panel split: `true` = horizontal (stacked), `false` = vertical (the
    /// classic side-by-side default).
    #[serde(default)]
    pub split_horizontal: bool,
    /// Which panels were hidden (`Ctrl-F1`/`Ctrl-F2`).
    #[serde(default)]
    pub panel_hidden: [bool; 2],
    /// Half-height mode (`Ctrl-F4`).
    #[serde(default)]
    pub half_height: bool,
    /// The active panel (0 = left/top, 1 = right/bottom).
    #[serde(default)]
    pub active_panel: usize,

    /// The internal editor's behaviour settings (its Options → General dialog).
    #[serde(default)]
    pub editor_options: EditorOptions,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            editor: String::new(),
            viewer: String::new(),
            use_internal_viewer: true,
            use_internal_editor: true,
            shell: String::new(),
            confirm_delete: true,
            use_trash: true,
            confirm_overwrite: true,
            confirm_execute: false,
            confirm_unmount: true,
            confirm_exit: true,
            theme: "Rat Commander".to_string(),
            language: None,
            reshape_rtl: true,
            graphics: "auto".to_string(),
            truecolor: None,
            animation: false,
            system_status: true,
            command_prompt: true,
            nerd_font: false,
            auto_refresh: true,
            strip_trailing_spaces: true,
            brief_columns: 2,
            command_history_max: 100,
            space3d_style: Space3dStyle::default(),
            space3d_activity: true,
            details_activity: true,
            audio_display: AudioDisplay::default(),
            screensaver_minutes: 0,
            screensaver: SaverKind::default(),
            thumb_size: ThumbSize::default(),
            panels: [PanelView::default(); 2],
            recent_remotes: Vec::new(),
            bookmarks: Vec::new(),
            panel_dirs: [String::new(), String::new()],
            panel_filters: [String::new(), String::new()],
            panel_tabs: [Vec::new(), Vec::new()],
            panel_tab_active: [0, 0],
            split_horizontal: false,
            panel_hidden: [false, false],
            half_height: false,
            active_panel: 0,
            editor_options: EditorOptions::default(),
        }
    }
}

/// A trimmed non-empty copy of `s`, else `None`.
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// A shell command from environment variable `var`, if set and non-empty.
fn env_command(var: &str) -> Option<String> {
    std::env::var(var).ok().and_then(|v| non_empty(&v))
}

impl Config {
    /// Load the config, falling back to defaults on any error.
    pub fn load() -> Self {
        // Nothing overrides XDG for the test binary, so the config path is the
        // real user directory even under `cargo test`: reading it would make
        // every test that builds an `AppState` depend on the developer's own
        // settings (active panel, filters, theme, nerd fonts). Tests always
        // start from the defaults; parsing is covered by the round-trip tests
        // below, and `save` is stubbed out for the same reason.
        if cfg!(test) {
            return Config::default();
        }
        let Some(path) = paths::config_file() else {
            return Config::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    /// Persist the config to disk. Returns an error string on failure.
    pub fn save(&self) -> Result<(), String> {
        // The config path is the real user directory even under `cargo test`
        // (nothing overrides XDG for the test binary), so a test that exercises
        // a settings toggle would rewrite the developer's own config.toml.
        // Serialization is still checked by the round-trip tests below.
        if cfg!(test) {
            return Ok(());
        }
        let path = paths::config_file().ok_or("no config directory available")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }

    /// The external editor command: the configured `editor`, or — when that is
    /// empty — the `$VISUAL` then `$EDITOR` environment variables (the Unix
    /// convention). `None` when none is set, meaning the internal editor is used.
    pub fn external_editor(&self) -> Option<String> {
        non_empty(&self.editor).or_else(|| env_command("VISUAL")).or_else(|| env_command("EDITOR"))
    }

    /// The external viewer/pager command: the configured `viewer`, or `$PAGER`.
    pub fn external_viewer(&self) -> Option<String> {
        non_empty(&self.viewer).or_else(|| env_command("PAGER"))
    }

    /// Whether to use the internal viewer for the given situation.
    pub fn wants_internal_viewer(&self) -> bool {
        self.use_internal_viewer || self.external_viewer().is_none()
    }

    /// Whether to use the internal editor.
    pub fn wants_internal_editor(&self) -> bool {
        self.use_internal_editor || self.external_editor().is_none()
    }

    /// Record a successful remote connection at the front of the history,
    /// de-duplicating the same server and capping the list.
    pub fn add_recent_remote(&mut self, entry: RemoteHistoryEntry) {
        self.recent_remotes.retain(|e| {
            !(e.protocol == entry.protocol
                && e.host == entry.host
                && e.port == entry.port
                && e.user == entry.user)
        });
        self.recent_remotes.insert(0, entry);
        self.recent_remotes.truncate(20);
    }
}

/// Load the persisted command-line history (oldest first), keeping at most the
/// `max` most-recent entries. A missing file or any error yields an empty list.
pub fn load_command_history(max: usize) -> Vec<String> {
    // Not the developer's own history under `cargo test` (see `Config::load`);
    // `load_history_from` is exercised directly on a temporary file.
    if cfg!(test) {
        return Vec::new();
    }
    paths::history_file().map(|p| load_history_from(&p, max)).unwrap_or_default()
}

/// Persist the command-line history (oldest first), one entry per line, keeping
/// at most the `max` most-recent entries. Best-effort; errors are ignored.
pub fn save_command_history(history: &[String], max: usize) {
    // Never write over the developer's real history file from a test run.
    if cfg!(test) {
        return;
    }
    if let Some(p) = paths::history_file() {
        save_history_to(&p, history, max);
    }
}

fn load_history_from(path: &std::path::Path, max: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<String> =
        text.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect();
    if lines.len() > max {
        lines.drain(..lines.len() - max);
    }
    lines
}

fn save_history_to(path: &std::path::Path, history: &[String], max: usize) {
    // One command per line, so skip entries with embedded newlines (a pasted
    // multi-line command) and blank entries.
    let clean: Vec<&String> =
        history.iter().filter(|e| !e.trim().is_empty() && !e.contains(['\n', '\r'])).collect();
    let start = clean.len().saturating_sub(max);
    let body: String = clean[start..].iter().map(|e| format!("{e}\n")).collect();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, body);
}

/// How many files' cursor positions the editor remembers.
const EDITOR_POSITIONS_MAX: usize = 50;

/// One remembered editor cursor position: a file key (its [`VfsPath::display`])
/// and the 0-based line/column the cursor was on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EditorPos {
    key: String,
    line: usize,
    col: usize,
}

/// The editor's cursor-position memory, most-recent first.
#[derive(Debug, Default, Serialize, Deserialize)]
struct EditorPositions {
    #[serde(default)]
    positions: Vec<EditorPos>,
}

/// The remembered `(line, col)` cursor position for the file `key` (a
/// [`crate::vfs::VfsPath::display`] string), or `None` if not remembered.
pub fn load_editor_position(key: &str) -> Option<(usize, usize)> {
    // Remembered positions are the developer's own under `cargo test`, and would
    // move the cursor in the editor tests; `position_from` is tested directly.
    if cfg!(test) {
        return None;
    }
    paths::editor_positions_file().and_then(|p| position_from(&p, key))
}

/// Remember the cursor `(line, col)` for the file `key`, moving it to the front
/// and evicting the oldest beyond the 50-file cap. Best-effort; errors ignored.
pub fn save_editor_position(key: &str, line: usize, col: usize) {
    // The editor tests close buffers; that must not rewrite the real file.
    if cfg!(test) {
        return;
    }
    if let Some(p) = paths::editor_positions_file() {
        store_position_to(&p, key, line, col);
    }
}

fn read_editor_positions(path: &std::path::Path) -> EditorPositions {
    std::fs::read_to_string(path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

fn position_from(path: &std::path::Path, key: &str) -> Option<(usize, usize)> {
    read_editor_positions(path)
        .positions
        .into_iter()
        .find(|e| e.key == key)
        .map(|e| (e.line, e.col))
}

fn store_position_to(path: &std::path::Path, key: &str, line: usize, col: usize) {
    let mut data = read_editor_positions(path);
    data.positions.retain(|e| e.key != key);
    data.positions.insert(0, EditorPos { key: key.to_string(), line, col });
    data.positions.truncate(EDITOR_POSITIONS_MAX);
    let Ok(text) = toml::to_string(&data) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(host: &str, path: &str) -> RemoteHistoryEntry {
        RemoteHistoryEntry {
            protocol: "sftp".into(),
            host: host.into(),
            port: 22,
            user: "u".into(),
            path: path.into(),
            passive: true,
            key_file: String::new(),
        }
    }

    #[test]
    fn panel_views_round_trip_through_toml() {
        use crate::panel::ViewFormat;
        use crate::panel::sort::SortKey;

        let mut c = Config::default();
        c.panels[0].format = ViewFormat::Brief;
        c.panels[0].sort.key = SortKey::Size;
        c.panels[0].sort.reverse = true;
        c.panels[1].format = ViewFormat::Details;
        c.panels[1].sort.key = SortKey::Extension;

        // Serialize + parse back, as save()/load() would.
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();

        assert_eq!(back.panels[0].format, ViewFormat::Brief);
        assert_eq!(back.panels[0].sort.key, SortKey::Size);
        assert!(back.panels[0].sort.reverse);
        assert_eq!(back.panels[1].format, ViewFormat::Details);
        assert_eq!(back.panels[1].sort.key, SortKey::Extension);
    }

    #[test]
    fn old_config_without_panels_field_uses_defaults() {
        // A config file predating the panel-state field still parses.
        let back: Config = toml::from_str("theme = \"Nord\"\n").unwrap();
        assert_eq!(back.panels[0].format, crate::panel::ViewFormat::Full);
        assert_eq!(back.brief_columns, 2);
        // …and one predating the 3D style field keeps the classic look.
        assert_eq!(back.space3d_style, Space3dStyle::Cubes);
        // …including one predating the editor's own options table.
        assert_eq!(back.editor_options, EditorOptions::default());
    }

    #[test]
    fn editor_options_round_trip_through_toml() {
        let mut c = Config::default();
        c.editor_options.wrap_mode = WrapMode::Typewriter;
        c.editor_options.tab_spacing = 8;
        c.editor_options.visible_tabs = true;
        c.editor_options.confirm_before_saving = false;
        c.editor_options.word_wrap_line_length = 100;

        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.editor_options, c.editor_options);
        // The wrap mode is stored by name, so the file stays readable by hand.
        assert!(text.contains("wrap_mode = \"typewriter\""), "{text}");
    }

    #[test]
    fn the_screensaver_settings_round_trip_and_default_to_off() {
        let mut c = Config::default();
        assert_eq!(c.screensaver_minutes, 0, "off unless asked for");
        c.screensaver_minutes = 10;
        c.screensaver = SaverKind::Matrix;
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!((back.screensaver_minutes, back.screensaver), (10, SaverKind::Matrix));
        assert!(text.contains("screensaver = \"matrix\""), "{text}");
        assert!(text.find("screensaver").unwrap() < text.find("[[panels]]").unwrap());
        assert_eq!(saver_minutes_from_label(&saver_minutes_label(15)), 15);
        assert_eq!(saver_minutes_from_label("Off"), 0);
    }

    #[test]
    fn space3d_style_round_trips_through_toml() {
        let mut c = Config::default();
        assert_eq!(c.space3d_style, Space3dStyle::Cubes, "the classic look is the default");
        c.space3d_style = Space3dStyle::Fsn;

        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.space3d_style, Space3dStyle::Fsn);
        // Stored by name, so the file stays readable (and editable) by hand.
        assert!(text.contains("space3d_style = \"fsn\""), "{text}");
        // It has to be written before `panels`, which serializes as an array of
        // tables — TOML has no way back to a scalar key once a table has begun,
        // so a field placed after it would make `Config::save` fail outright.
        assert!(
            text.find("space3d_style").unwrap() < text.find("[[panels]]").unwrap(),
            "space3d_style must be written before the panels tables:\n{text}"
        );
    }

    #[test]
    fn audio_display_round_trips_through_toml() {
        let mut c = Config::default();
        assert_eq!(c.audio_display, AudioDisplay::Spectrogram, "the spectrogram is the default");
        c.audio_display = AudioDisplay::Waveform;

        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.audio_display, AudioDisplay::Waveform);
        assert!(text.contains("audio_display = \"waveform\""), "{text}");
        assert!(
            text.find("audio_display").unwrap() < text.find("[[panels]]").unwrap(),
            "audio_display must be written before the panels tables:\n{text}"
        );
    }

    #[test]
    fn audio_display_labels_map_both_ways() {
        for (display, label) in AudioDisplay::ALL {
            assert_eq!(AudioDisplay::from_label(label), display);
            assert_eq!(display.label(), label);
            assert_eq!(display.toggled().toggled(), display);
        }
        assert_eq!(AudioDisplay::from_label("nonsense"), AudioDisplay::Spectrogram);
    }

    #[test]
    fn space3d_style_labels_map_both_ways() {
        for (style, label) in Space3dStyle::ALL {
            assert_eq!(Space3dStyle::from_label(label), style);
            assert_eq!(style.label(), label);
        }
        // Anything unrecognized falls back to the classic look.
        assert_eq!(Space3dStyle::from_label("nonsense"), Space3dStyle::Cubes);
    }

    #[test]
    fn wrap_mode_labels_map_both_ways() {
        for (mode, label) in WrapMode::ALL {
            assert_eq!(WrapMode::from_label(label), mode);
            assert_eq!(mode.label(), label);
        }
        // Anything unrecognized falls back to the harmless default.
        assert_eq!(WrapMode::from_label("nonsense"), WrapMode::None);
    }

    #[test]
    fn add_recent_dedupes_caps_and_orders() {
        let mut c = Config::default();
        for i in 0..25 {
            c.add_recent_remote(entry(&format!("h{i}"), ""));
        }
        assert_eq!(c.recent_remotes.len(), 20, "capped at 20");
        assert_eq!(c.recent_remotes[0].host, "h24", "most recent first");

        // Re-adding an existing server moves it to the front and updates its path.
        c.add_recent_remote(entry("h10", "/new"));
        assert_eq!(c.recent_remotes[0].host, "h10");
        assert_eq!(c.recent_remotes[0].path, "/new");
        assert_eq!(c.recent_remotes.iter().filter(|e| e.host == "h10").count(), 1, "no duplicate");
    }

    #[test]
    fn command_history_round_trips_and_caps_at_max() {
        let dir = std::env::temp_dir().join(format!("rc_hist_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history");

        let hist: Vec<String> =
            ["one", "two", "three", "four"].iter().map(|s| s.to_string()).collect();
        // Save keeps only the most-recent `max` entries…
        save_history_to(&path, &hist, 2);
        assert_eq!(load_history_from(&path, 10), vec!["three".to_string(), "four".to_string()]);
        // …and load also caps (e.g. after the max was lowered).
        save_history_to(&path, &hist, 10);
        assert_eq!(load_history_from(&path, 1), vec!["four".to_string()]);
        // Blank / multi-line entries are not persisted; a missing file → empty.
        save_history_to(&path, &["ok".into(), "  ".into(), "a\nb".into()], 100);
        assert_eq!(load_history_from(&path, 100), vec!["ok".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_history_from(&path, 100).is_empty());
    }

    #[test]
    fn panel_tabs_round_trip_and_default_empty_for_old_configs() {
        // An older config simply has no tabs, which means "one tab" — nothing
        // about the panel changes for anyone who never opens a second one.
        let c: Config = toml::from_str("brief_columns = 2\n").unwrap();
        assert!(c.panel_tabs[0].is_empty() && c.panel_tabs[1].is_empty());
        assert_eq!(c.panel_tab_active, [0, 0]);

        // And a saved set survives a write/read cycle intact.
        let mut c = Config::default();
        c.panel_tabs[0] = vec![
            TabRecord { dir: "/tmp".into(), filter: "*.rs".into(), ..Default::default() },
            TabRecord { dir: "/etc".into(), filter: String::new(), ..Default::default() },
        ];
        c.panel_tab_active[0] = 1;
        let text = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.panel_tabs[0].len(), 2);
        assert_eq!(back.panel_tabs[0][0].dir, "/tmp");
        assert_eq!(back.panel_tabs[0][0].filter, "*.rs");
        assert_eq!(back.panel_tabs[0][1].dir, "/etc");
        assert_eq!(back.panel_tab_active[0], 1);
    }

    #[test]
    fn remote_history_defaults_key_file_empty_for_old_entries() {
        // An entry saved before the key-file field existed means "agent /
        // default keys", which is exactly the empty string.
        let e: RemoteHistoryEntry =
            toml::from_str("protocol = \"sftp\"\nhost = \"h\"\nport = 22\n").unwrap();
        assert!(e.key_file.is_empty());
        let e: RemoteHistoryEntry = toml::from_str(
            "protocol = \"sftp\"\nhost = \"h\"\nport = 22\nkey_file = \"~/.ssh/id_x\"\n",
        )
        .unwrap();
        assert_eq!(e.key_file, "~/.ssh/id_x");
    }

    #[test]
    fn remote_history_defaults_passive_true_for_old_entries() {
        // An entry saved before the PASV field existed reconnects in passive mode.
        let e: RemoteHistoryEntry =
            toml::from_str("protocol = \"ftp\"\nhost = \"h\"\nport = 21\n").unwrap();
        assert!(e.passive);
        // A stored value is honoured either way.
        let e: RemoteHistoryEntry =
            toml::from_str("protocol = \"ftp\"\nhost = \"h\"\nport = 21\npassive = false\n")
                .unwrap();
        assert!(!e.passive);
    }

    #[test]
    fn config_ignores_unknown_and_missing_fields() {
        // An old config without `command_history_max` gets the default; an
        // unknown key (e.g. the removed `quick_search`) is ignored.
        let c: Config = toml::from_str("quick_search = true\nbrief_columns = 3\n").unwrap();
        assert_eq!(c.command_history_max, 100);
        assert_eq!(c.brief_columns, 3);
    }

    #[test]
    fn editor_positions_round_trip_move_to_front_and_cap() {
        let dir = std::env::temp_dir().join(format!("rc_edpos_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("editor-positions.toml");

        // A missing file → no remembered position.
        assert_eq!(position_from(&path, "/a"), None);

        store_position_to(&path, "/a", 10, 2);
        store_position_to(&path, "/b", 5, 0);
        assert_eq!(position_from(&path, "/a"), Some((10, 2)));
        assert_eq!(position_from(&path, "/b"), Some((5, 0)));

        // Re-storing the same file updates it and moves it to the front.
        store_position_to(&path, "/a", 33, 7);
        assert_eq!(position_from(&path, "/a"), Some((33, 7)));
        assert_eq!(read_editor_positions(&path).positions[0].key, "/a");

        // Only the 50 most-recent files are kept.
        for i in 0..60 {
            store_position_to(&path, &format!("/f{i}"), i, 0);
        }
        let data = read_editor_positions(&path);
        assert_eq!(data.positions.len(), EDITOR_POSITIONS_MAX);
        assert_eq!(position_from(&path, "/f59"), Some((59, 0)), "newest kept");
        assert_eq!(position_from(&path, "/f0"), None, "oldest evicted");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod env_fallback_tests {
    use super::*;

    #[test]
    fn non_empty_trims_and_rejects_blank() {
        assert_eq!(non_empty("  vim  ").as_deref(), Some("vim"));
        assert_eq!(non_empty("   "), None);
        assert_eq!(non_empty(""), None);
    }

    #[test]
    fn external_program_prefers_the_configured_value() {
        // A configured command wins outright — the environment is never consulted,
        // so this is deterministic regardless of the test runner's env.
        let c = Config {
            use_internal_editor: false,
            use_internal_viewer: false,
            editor: "code --wait".into(),
            viewer: "  bat  ".into(),
            ..Config::default()
        };
        assert_eq!(c.external_editor().as_deref(), Some("code --wait"));
        assert_eq!(c.external_viewer().as_deref(), Some("bat"), "trimmed");
        // With the internal toggle off and an external configured, the external wins.
        assert!(!c.wants_internal_editor() && !c.wants_internal_viewer());
    }

    #[test]
    fn external_program_falls_back_to_visual_editor_pager_env() {
        // editor/viewer empty; internal toggles off so `wants_internal_*` reflects
        // purely whether an external command resolved (config or env).
        let c =
            Config { use_internal_editor: false, use_internal_viewer: false, ..Config::default() };
        // Save and clear the vars this test drives, restore them afterward.
        let vars = ["VISUAL", "EDITOR", "PAGER"];
        let saved: Vec<Option<String>> = vars.iter().map(|k| std::env::var(k).ok()).collect();
        let set = |k: &str, v: &str| unsafe { std::env::set_var(k, v) };
        let clear = |k: &str| unsafe { std::env::remove_var(k) };

        vars.iter().for_each(|k| clear(k));
        assert_eq!(c.external_editor(), None, "no config, no env → the internal editor");
        assert!(
            c.wants_internal_editor() && c.wants_internal_viewer(),
            "nothing external → internal"
        );

        set("EDITOR", "vi");
        assert_eq!(c.external_editor().as_deref(), Some("vi"));
        set("VISUAL", "nvim");
        assert_eq!(c.external_editor().as_deref(), Some("nvim"), "$VISUAL beats $EDITOR");
        set("PAGER", "less");
        assert_eq!(c.external_viewer().as_deref(), Some("less"));
        assert!(!c.wants_internal_editor() && !c.wants_internal_viewer(), "env external now used");

        for (k, v) in vars.iter().zip(saved) {
            match v {
                Some(val) => set(k, &val),
                None => clear(k),
            }
        }
    }
}
