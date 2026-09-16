//! Form dialog (settings, chmod, chown, symlink, connect, formatter).

use super::widgets::*;
use super::{DialogResult, DupCriteria, SettingsValues, Submit};
use crate::util::checksum::ChecksumKind;
use crate::vfs::remote::{Protocol, RemoteCreds};
use std::ops::Range;

/// The display label for a `graphics` config preference, for the settings chooser.
fn graphics_label(pref: &str) -> &'static str {
    match pref.trim().to_ascii_lowercase().as_str() {
        "off" => "Off",
        "kitty" => "Kitty",
        "sixel" => "Sixel",
        "iterm" | "iterm2" => "iTerm2",
        _ => "Auto",
    }
}

/// The canonical `graphics` config value for a chooser display label.
fn graphics_pref(label: &str) -> String {
    match label.trim().to_ascii_lowercase().as_str() {
        "off" => "off",
        "kitty" => "kitty",
        "sixel" => "sixel",
        "iterm2" | "iterm" => "iterm",
        _ => "auto",
    }
    .to_string()
}

/// Cells between the columns of a multi-column group. The columns are equal
/// width and any remainder of an odd interior falls in here, so a long value in
/// the left column can never bleed into the right one.
const GROUP_COL_GUTTER: u16 = 2;

/// A titled group box of a grouped form: `(title, field count, columns)`.
///
/// A group with more than one column fills **column-major**: the first
/// `ceil(count / columns)` fields run down the left column, the next down the
/// one beside it. That is what keeps `Down`/`Tab` reading as *down*, because
/// focus movement walks the fields in index order — see [`Form::focus_next`].
type Group = (&'static str, usize, usize);

/// The Settings dialog's tabs. The order here is not the order on screen — that
/// is [`SETTINGS_PAGES`]'s — so a tab can be moved without renumbering anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    Appearance,
    Panels,
    Programs,
    Confirmations,
    Language,
    Terminal,
}

/// One tab of the Settings dialog: its title and the group boxes on it.
struct SettingsPage {
    tab: SettingsTab,
    title: &'static str,
    groups: &'static [Group],
}

/// The Settings dialog's pages, left to right, in the order
/// [`FormDialog::settings`] builds their fields. Every page's fields are one
/// contiguous run of the form's flat field list, so the group counts must sum
/// to the number of settings fields.
///
/// **This form has to fit a classic 80x24 terminal**, and `centered` silently
/// clips the OK/Cancel row off the bottom where it cannot be clicked if it does
/// not. Its height is `TAB_STRIP_ROWS + tallest page + 4`, where a page is
/// `Σ(ceil(count / columns) + 2)` — so only the tallest page costs rows, and a
/// setting that doesn't fit belongs on another tab rather than in the palette.
const SETTINGS_PAGES: &[SettingsPage] = &[
    SettingsPage {
        tab: SettingsTab::Appearance,
        title: "Appearance",
        groups: &[("Display", 4, 1), ("Screensaver", 2, 1)],
    },
    SettingsPage {
        tab: SettingsTab::Panels,
        title: "Panels",
        groups: &[("Views", 5, 1), ("Activity", 3, 1)],
    },
    SettingsPage {
        tab: SettingsTab::Programs,
        title: "Programs",
        groups: &[("Editor and viewer", 4, 1), ("Command line", 3, 1)],
    },
    SettingsPage {
        tab: SettingsTab::Confirmations,
        title: "Confirmations",
        groups: &[("Confirmations", 5, 1), ("Trash", 1, 1)],
    },
    SettingsPage { tab: SettingsTab::Language, title: "Language", groups: &[("Language", 2, 1)] },
    SettingsPage {
        tab: SettingsTab::Terminal,
        title: "Terminal",
        groups: &[("Capabilities", 2, 1), ("Mouse selection", 1, 1)],
    },
];

/// Rows a tabbed form gives its tab strip: the strip itself, plus a blank row so
/// the first group box's title isn't read as a second row of tabs.
const TAB_STRIP_ROWS: u16 = 2;

/// Width of the grouped forms' box. 76 leaves room for every tab title on one
/// strip, and for a chooser's longest row — in German, "Design: Midnight
/// Commander Dark ▾" alone is 33 cells.
const GROUPED_FORM_WIDTH: u16 = 76;

/// Lines under a tabbed form's pages that describe the focused setting, below a
/// divider. Every description has to fit in them at [`HELP_WIDTH`], in every
/// language — the l10n tests check that.
pub(crate) const HELP_ROWS: usize = 3;

/// Cells a description line gets in a full-width box: the box less its border
/// and a one-cell inset either side.
#[cfg(test)]
pub(crate) const HELP_WIDTH: usize = GROUPED_FORM_WIDTH as usize - 4;

/// What each Settings field does, keyed by its label and shown while it has
/// focus. The text is the English source, translated when drawn, so the catalogs
/// need an entry for each. Only what the label alone doesn't make obvious is
/// worth the space: what the setting costs, when to turn it off, what it falls
/// back to.
const SETTINGS_HELP: &[(&str, &str)] = &[
    (
        "Theme",
        "The color theme of the whole interface. Moving through the list previews each one \
         live; Options → Edit themes… changes them or adds your own.",
    ),
    (
        "Animations",
        "Let a theme's animated color gradients flow. Needs Truecolor. Off keeps them still \
         and saves redraws, which helps over a slow remote connection.",
    ),
    (
        "Nerd Font symbols",
        "Show an icon for each file type in the listings instead of the / * @ markers. Needs \
         a Nerd Font as the terminal's font, or the icons show as boxes.",
    ),
    ("System status widget", "Show CPU and memory use at the right end of the menu bar."),
    (
        "Screensaver",
        "How long without a key press or mouse movement before the screensaver starts. Off \
         never starts it; Start screensaver in the command palette shows one now.",
    ),
    (
        "Screensaver style",
        "Which animation the screensaver plays: a starfield, matrix rain, a drifting clock or \
         growing pipes. Random picks a different one each time.",
    ),
    (
        "Brief view columns",
        "How many columns of file names the Brief view format puts side by side in a panel.",
    ),
    (
        "Thumbnail size",
        "The cell size of the Thumbnails view: Small fits the most pictures on screen, Large \
         shows the most detail.",
    ),
    (
        "3D style",
        "How the 3D view draws a tree: Cubes, shaded boxes hanging on rings, or Spare no \
         expense, an fsn-style landscape with files shaped by type.",
    ),
    (
        "Audio view",
        "How the viewer and the Details view draw an audio file: Spectrogram, its frequencies \
         over time, or Waveform, its loudness. F2 in the viewer switches between them.",
    ),
    (
        "Auto-play audio in the viewer",
        "Start playing an audio file as soon as F3 opens it in the viewer. The Details view \
         never plays by itself: there, playback starts only when you press ▶.",
    ),
    (
        "Auto-refresh panels",
        "Re-read a panel by itself when its directory changes on disk, instead of waiting for \
         Ctrl-R. Worth turning off on slow network mounts or huge directories.",
    ),
    (
        "3D view: show filesystem activity",
        "Make the 3D view light up directories as files are written in them. It watches every \
         directory in the tree, so turn it off for huge or network-mounted trees.",
    ),
    (
        "Details view: git activity",
        "In a git work tree, the Details view shows a calendar of a year's commits for the \
         selected item. Each one runs git log; turn it off for very large repositories.",
    ),
    (
        "External editor",
        "The command F4 edits files with, such as vim or code --wait. Blank uses $VISUAL or \
         $EDITOR. Only used while Use internal editor is off.",
    ),
    (
        "External viewer",
        "The command F3 views files with, such as less or bat. Blank uses $PAGER. Only used \
         while Use internal viewer is off.",
    ),
    (
        "Use internal viewer",
        "F3 opens Rat Commander's own viewer even when an external viewer is set. It is also \
         used whenever no external viewer is configured.",
    ),
    (
        "Use internal editor",
        "F4 opens Rat Commander's own editor even when an external editor is set. It is also \
         used whenever no external editor is configured.",
    ),
    (
        "Command prompt",
        "Show the shell command line below the panels. Without it, typing a letter starts a \
         quick search in the active panel. Ctrl-F5 switches it too.",
    ),
    (
        "Shell (blank = auto-detect)",
        "The program the command line and Ctrl-O run, such as /bin/zsh or pwsh, without \
         arguments. Blank uses $SHELL (on Windows, the shell rc was started from).",
    ),
    (
        "Command history size (0 = off)",
        "How many command lines are remembered across sessions, recalled with Alt-P, Alt-N \
         and Alt-H. 0 turns the history off; lowering it drops the oldest at once.",
    ),
    ("Confirm delete", "Ask before F8 deletes the selection or moves it to the trash."),
    (
        "Confirm overwrite",
        "Ask what to do when a copy or move meets a file that already exists. Off replaces it \
         without asking.",
    ),
    (
        "Confirm execute",
        "Ask before Enter runs a program or opens a file in its default application.",
    ),
    ("Confirm unmount", "Ask before the disk manager unmounts a filesystem."),
    ("Confirm exit", "Ask before F10 quits Rat Commander."),
    (
        "Use trash bin",
        "F8 moves local files to the desktop trash, where they can be restored; Shift-F8 still \
         deletes for good. Off makes F8 delete permanently too.",
    ),
    (
        "Language",
        "The language of menus, dialogs and messages, previewed live as you move through the \
         list. Translations are files in the lang folder of the config directory.",
    ),
    (
        "Reshape RTL text",
        "Shape and reorder Arabic and Persian text for terminals without bidi support. Turn it \
         off on terminals that do their own, such as mlterm or Konsole.",
    ),
    (
        "Graphics",
        "Pixel graphics for progress bars, graphs, thumbnails and dialog buttons. Auto uses \
         Kitty, Sixel or iTerm2 when the terminal has one; Off draws them in text.",
    ),
    (
        "Truecolor (gradients)",
        "Use 24-bit color, which gradients need. Turn it off if colors look wrong: the terminal \
         probably supports only 256 colors.",
    ),
    (
        "Strip trailing spaces on copy",
        "End screen rows with an erase instead of spaces, so text selected with the terminal's \
         mouse copies without trailing blanks. Turn it off if backgrounds break up.",
    ),
];

/// The description shown while the tab strip has focus.
const HELP_TABS: &str = "Ctrl-PgUp and Ctrl-PgDn switch tabs from anywhere in this dialog; ← \
                         and → switch them while the tab row is highlighted.";
/// The descriptions shown while OK or Cancel has focus.
const HELP_OK: &str = "Save the changes on every tab and close the dialog.";
const HELP_CANCEL: &str =
    "Close the dialog without saving anything on any tab, and undo the live previews.";

/// Every description the Settings dialog can show, for the tests that check
/// each one has a field and fits its lines in every language.
#[cfg(test)]
pub(crate) fn settings_help_texts() -> Vec<&'static str> {
    SETTINGS_HELP.iter().map(|(_, h)| *h).chain([HELP_TABS, HELP_OK, HELP_CANCEL]).collect()
}

/// `text` wrapped into at most `rows` lines of `width` cells, the last one cut
/// short with an ellipsis if there was more. Translations are checked to fit, so
/// this only bites on a terminal too narrow for the full box.
fn fit_lines(text: &str, width: usize, rows: usize) -> Vec<String> {
    let mut lines = crate::util::text::wrap(text, width);
    if lines.len() > rows {
        lines.truncate(rows);
        if let Some(last) = lines.last_mut() {
            let (cut, _) = crate::util::text::truncate_width(last, width.saturating_sub(1));
            *last = format!("{cut}…");
        }
    }
    lines
}

/// The editor-options form's groups, in the order [`FormDialog::editor_options`]
/// builds its fields. The counts must sum to the number of fields.
const EDITOR_OPTION_GROUPS: &[Group] =
    &[("Wrap mode", 1, 1), ("Tabulation", 3, 1), ("Other options", 10, 1)];

/// Rows a group of `count` fields occupies when spread over `cols` columns; the
/// last column may be short. Never zero, so an empty group still has an
/// interior to draw.
fn group_row_count(count: usize, cols: usize) -> u16 {
    count.div_ceil(cols.max(1)).max(1) as u16
}

/// Rows a stack of group boxes occupies: each box's rows plus its two borders.
fn groups_height(groups: &[Group]) -> u16 {
    groups.iter().map(|(_, n, cols)| group_row_count(*n, *cols) + 2).sum()
}

/// Fields a stack of group boxes holds.
fn page_field_count(groups: &[Group]) -> usize {
    groups.iter().map(|(_, n, _)| n).sum()
}

// ---------------------------------------------------------------------------
// Form dialog (settings, chmod, chown, symlink)
// ---------------------------------------------------------------------------

/// A single editable field in a [`Form`].
pub enum Field {
    Text {
        label: String,
        value: String,
        cursor: usize,
    },
    Password {
        label: String,
        value: String,
        cursor: usize,
    },
    Check {
        label: String,
        value: bool,
    },
    /// A choice picked from a scrollable dropdown (Enter opens it).
    Choice {
        label: String,
        options: Vec<String>,
        idx: usize,
        /// Whether the dropdown list is currently open.
        open: bool,
        /// Highlighted option while the dropdown is open.
        sel: usize,
        /// First visible option while open (scroll offset); adjusted only when
        /// the highlight would leave the window, so the cursor moves freely.
        top: usize,
    },
}

impl Field {
    pub fn text(label: &str, value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Field::Text { label: label.to_string(), value, cursor }
    }

    pub fn password(label: &str) -> Self {
        Field::Password { label: label.to_string(), value: String::new(), cursor: 0 }
    }

    pub fn check(label: &str, value: bool) -> Self {
        Field::Check { label: label.to_string(), value }
    }

    pub fn choice(label: &str, options: Vec<String>, selected: &str) -> Self {
        let idx = options.iter().position(|o| o == selected).unwrap_or(0);
        Field::Choice { label: label.to_string(), options, idx, open: false, sel: idx, top: 0 }
    }

    fn as_text(&self) -> &str {
        match self {
            Field::Text { value, .. } | Field::Password { value, .. } => value,
            Field::Choice { options, idx, .. } => {
                options.get(*idx).map(|s| s.as_str()).unwrap_or("")
            }
            Field::Check { .. } => "",
        }
    }

    fn as_bool(&self) -> bool {
        matches!(self, Field::Check { value: true, .. })
    }

    fn label(&self) -> &str {
        match self {
            Field::Text { label, .. }
            | Field::Password { label, .. }
            | Field::Check { label, .. }
            | Field::Choice { label, .. } => label,
        }
    }
}

/// A vertical list of editable fields with a single focused row.
pub struct Form {
    fields: Vec<Field>,
    pub(crate) focus: usize,
    /// The field the form opens on is fully marked when it opens pre-filled, so
    /// typing replaces that value rather than appending to it. Dropped by any
    /// focus move or click, so it only ever applies to the field it was set for —
    /// tabbing through a form of existing settings can never wipe one.
    selected: bool,
    /// Whether the form is split into tabbed pages. A tabbed form has one more
    /// focus slot, the tab strip, and only shows the fields of its current page.
    tabbed: bool,
    /// The page showing, on a tabbed form.
    page: usize,
    /// The fields on screen and in the focus ring: every field, unless tabbed.
    visible: Range<usize>,
}

impl Form {
    pub fn new(fields: Vec<Field>) -> Self {
        let selected =
            matches!(fields.first(), Some(Field::Text { value, .. }) if !value.is_empty());
        let visible = 0..fields.len();
        Form { fields, focus: 0, selected, tabbed: false, page: 0, visible }
    }

    /// Number of fields (used to compute the dialog height for click geometry).
    #[allow(dead_code)] // used by form tests
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    // Focus slots: each field's slot is its index, then OK and Cancel follow, so
    // the buttons can be reached and activated with the keyboard. A tabbed form
    // adds the tab strip after those. The numbers never depend on the page, so a
    // slot stays valid across a tab switch.
    fn ok_slot(&self) -> usize {
        self.fields.len()
    }
    fn cancel_slot(&self) -> usize {
        self.fields.len() + 1
    }
    fn strip_slot(&self) -> usize {
        self.fields.len() + 2
    }
    /// Whether focus is on the OK or Cancel button (not a field).
    fn on_button(&self) -> bool {
        self.on_ok() || self.on_cancel()
    }
    fn on_ok(&self) -> bool {
        self.focus == self.ok_slot()
    }
    fn on_cancel(&self) -> bool {
        self.focus == self.cancel_slot()
    }
    /// Whether focus is on a tabbed form's tab strip.
    fn on_strip(&self) -> bool {
        self.tabbed && self.focus == self.strip_slot()
    }

    /// The focus slots in the order Tab visits them: the tab strip (on a tabbed
    /// form), the visible fields, then OK and Cancel.
    fn ring(&self) -> Vec<usize> {
        let mut ring = Vec::with_capacity(self.visible.len() + 3);
        if self.tabbed {
            ring.push(self.strip_slot());
        }
        ring.extend(self.visible.clone());
        ring.extend([self.ok_slot(), self.cancel_slot()]);
        ring
    }

    /// Move focus to a slot, dropping the whole-field mark: a focus move means
    /// the field it was set for is no longer the one being typed into.
    pub(crate) fn focus_at(&mut self, slot: usize) {
        self.focus = slot;
        self.selected = false;
    }

    fn focus_next(&mut self) {
        self.step_focus(true);
    }

    fn focus_prev(&mut self) {
        self.step_focus(false);
    }

    /// Step focus one slot along [`Form::ring`], wrapping at either end. A focus
    /// that isn't in the ring (it can't normally happen) restarts at its head.
    fn step_focus(&mut self, forward: bool) {
        let ring = self.ring();
        let n = ring.len();
        let next = match ring.iter().position(|&s| s == self.focus) {
            Some(i) if forward => ring[(i + 1) % n],
            Some(i) => ring[(i + n - 1) % n],
            None => ring[0],
        };
        self.focus_at(next);
    }

    /// Show page `page` of a tabbed form, whose fields are `visible`. Any open
    /// dropdown closes — it belongs to a field that is about to disappear — and a
    /// focused field hands focus to the new page's first one; focus on the tab
    /// strip or a button stays put, so arrowing along the tabs keeps working.
    pub(crate) fn show_page(&mut self, page: usize, visible: Range<usize>) {
        for field in &mut self.fields {
            if let Field::Choice { open, .. } = field {
                *open = false;
            }
        }
        if self.focus < self.fields.len() {
            self.focus = visible.start;
        }
        self.tabbed = true;
        self.page = page;
        self.visible = visible;
        self.selected = false;
    }

    /// Handle a key for the focused field. Returns true if Enter (submit) was
    /// pressed.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter => return true,
            KeyCode::Tab | KeyCode::Down => self.focus_next(),
            KeyCode::BackTab | KeyCode::Up => self.focus_prev(),
            KeyCode::Char(' ')
                if matches!(self.fields.get(self.focus), Some(Field::Check { .. })) =>
            {
                if let Some(Field::Check { value, .. }) = self.fields.get_mut(self.focus) {
                    *value = !*value;
                }
            }
            // Choice fields are changed via their dropdown (opened with Enter),
            // handled in `FormDialog::handle_key` — arrows just move focus.
            _ => match self.fields.get_mut(self.focus) {
                Some(Field::Text { value, cursor, .. })
                | Some(Field::Password { value, cursor, .. }) => {
                    edit_text_marked(value, cursor, &mut self.selected, key)
                }
                _ => {}
            },
        }
        false
    }
}

/// A dialog title like `"Chmod: file.txt"` for one target or `"Chmod: 3 items"`
/// for several.
fn form_target_title(verb: &str, targets: &[VfsPath]) -> String {
    match targets {
        [one] => format!("{verb}: {}", one.file_name()),
        many => format!("{verb}: {} items", many.len()),
    }
}

/// What a form's values should become on submit.
pub enum FormPurpose {
    Settings,
    /// Change permissions of these targets (recursing into dirs if requested).
    Chmod(Vec<VfsPath>),
    /// Change ownership of these targets (recursing into dirs if requested).
    Chown(Vec<VfsPath>),
    /// Create a symlink inside this directory.
    Symlink(VfsPath),
    /// Open a remote connection of this protocol on the given panel side.
    Connect(Protocol, usize),
    /// Format this device node (disk manager).
    Format(String),
    /// Collect the "Find duplicates" comparison criteria.
    FindDuplicates,
    /// Checksum this file (algorithm + optional comparison digest collected here).
    Checksum(VfsPath),
    /// A guided Git dialog. The form builds its own `git` argv on submit (see
    /// [`GitForm`]), so every git action reaches the app as one `Submit::GitRun`.
    Git(GitForm),
    /// Collect the options for a directory sync; the app then plans it.
    Sync,
    /// The internal editor's Options → General dialog.
    EditorOptions,
    /// Options for sorting the editor's marked block.
    EditorSort,
}

/// The sync-mode choices, in the order they appear in the dropdown. The safest
/// (adds nothing back, removes nothing) comes first, the destructive mirror last.
pub(crate) const SYNC_MODES: [&str; 3] = [
    "One-way: copy new and changed files",
    "One-way mirror: also delete extraneous files",
    "Two-way: newer file wins",
];

/// The [`SyncMode`](crate::ops::sync::SyncMode) a [`SYNC_MODES`] label selects.
pub(crate) fn sync_mode_of(label: &str) -> crate::ops::sync::SyncMode {
    use crate::ops::sync::SyncMode;
    match label {
        l if l == SYNC_MODES[1] => SyncMode::OneWay { delete_extraneous: true },
        l if l == SYNC_MODES[2] => SyncMode::TwoWay,
        _ => SyncMode::OneWay { delete_extraneous: false },
    }
}

/// `git reset` modes, least destructive first, so the dialog opens on the safe
/// one. The label before the space is passed to git as `--<mode>`.
pub(crate) const RESET_MODES: [&str; 3] = [
    "soft   (keep index + working tree)",
    "mixed  (keep working tree)",
    "hard   (discard all changes!)",
];

/// The bare git mode name from a [`RESET_MODES`] label (`"hard  (…)"` → `"hard"`).
pub(crate) fn reset_mode_name(label: &str) -> &str {
    label.split_whitespace().next().unwrap_or("mixed")
}

/// Which guided Git dialog a [`FormPurpose::Git`] form is. Variants carry the
/// repository facts the argv needs but the fields don't hold (e.g. the branch to
/// name on a `push`).
#[derive(Debug, Clone)]
pub enum GitForm {
    Commit,
    Clone,
    Fetch,
    Pull,
    /// `push` names `<remote> <branch>`; the branch comes from the repo, not the user.
    Push {
        branch: String,
    },
    Checkout,
    Reset,
}

/// Connect-form history dropdown state (recent servers, then the hosts
/// `~/.ssh/config` names).
pub(crate) struct ConnectDropdown {
    history: Vec<crate::config::RemoteHistoryEntry>,
    /// What the list shows for each entry.
    labels: Vec<String>,
    pub(crate) open: bool,
    sel: usize,
    /// Click geometry recorded at render time: chevron, plus (rect, index) per
    /// visible dropdown entry.
    chevron: Option<Rect>,
    entries: Vec<(Rect, usize)>,
}

pub struct FormDialog {
    pub title: String,
    pub form: Form,
    pub purpose: FormPurpose,
    /// Present only for connect forms (drives the recent-servers dropdown).
    pub(crate) connect: Option<ConnectDropdown>,
}

impl FormDialog {
    pub fn settings(cfg: &crate::config::Config, truecolor: bool) -> Self {
        use crate::config::{
            AudioDisplay, SAVER_MINUTES, SaverKind, Space3dStyle, ThumbSize, saver_minutes_label,
        };
        // Field order is layout: `SETTINGS_PAGES` slices this list into tabs and
        // group boxes by count. The submit reads fields back by label, so the
        // order can change without touching it.
        let form = Form::new(vec![
            // === Appearance ===
            // --- Display ---
            Field::choice("Theme", crate::ui::theme::palette_names(), &cfg.theme),
            Field::check("Animations", cfg.animation),
            Field::check("Nerd Font symbols", cfg.nerd_font),
            Field::check("System status widget", cfg.system_status),
            // --- Screensaver ---
            Field::choice(
                "Screensaver",
                SAVER_MINUTES.iter().map(|&m| saver_minutes_label(m)).collect(),
                &saver_minutes_label(cfg.screensaver_minutes),
            ),
            Field::choice(
                "Screensaver style",
                SaverKind::ALL.iter().map(|(_, l)| (*l).to_string()).collect(),
                cfg.screensaver.label(),
            ),
            // === Panels ===
            // --- Views ---
            Field::choice(
                "Brief view columns",
                (1..=6).map(|n| n.to_string()).collect(),
                &cfg.brief_columns.to_string(),
            ),
            Field::choice(
                "Thumbnail size",
                ThumbSize::ALL.iter().map(|(_, l)| (*l).to_string()).collect(),
                cfg.thumb_size.label(),
            ),
            Field::choice(
                "3D style",
                Space3dStyle::ALL.iter().map(|(_, l)| (*l).to_string()).collect(),
                cfg.space3d_style.label(),
            ),
            Field::choice(
                "Audio view",
                AudioDisplay::ALL.iter().map(|(_, l)| (*l).to_string()).collect(),
                cfg.audio_display.label(),
            ),
            Field::check("Auto-play audio in the viewer", cfg.audio_autoplay),
            // --- Activity ---
            Field::check("Auto-refresh panels", cfg.auto_refresh),
            Field::check("3D view: show filesystem activity", cfg.space3d_activity),
            Field::check("Details view: git activity", cfg.details_activity),
            // === Programs ===
            // --- Editor and viewer ---
            Field::text("External editor", cfg.editor.clone()),
            Field::text("External viewer", cfg.viewer.clone()),
            Field::check("Use internal viewer", cfg.use_internal_viewer),
            Field::check("Use internal editor", cfg.use_internal_editor),
            // --- Command line ---
            Field::check("Command prompt", cfg.command_prompt),
            Field::text("Shell (blank = auto-detect)", cfg.shell.clone()),
            Field::text("Command history size (0 = off)", cfg.command_history_max.to_string()),
            // === Confirmations ===
            // --- Confirmations ---
            Field::check("Confirm delete", cfg.confirm_delete),
            Field::check("Confirm overwrite", cfg.confirm_overwrite),
            Field::check("Confirm execute", cfg.confirm_execute),
            Field::check("Confirm unmount", cfg.confirm_unmount),
            Field::check("Confirm exit", cfg.confirm_exit),
            // --- Trash ---
            Field::check("Use trash bin", cfg.use_trash),
            // === Language ===
            Field::choice("Language", crate::l10n::available(), &crate::l10n::active_name()),
            Field::check("Reshape RTL text", cfg.reshape_rtl),
            // === Terminal ===
            // --- Capabilities ---
            Field::choice(
                "Graphics",
                vec!["Auto".into(), "Off".into(), "Kitty".into(), "Sixel".into(), "iTerm2".into()],
                graphics_label(&cfg.graphics),
            ),
            Field::check("Truecolor (gradients)", truecolor),
            // --- Mouse selection ---
            Field::check("Strip trailing spaces on copy", cfg.strip_trailing_spaces),
        ]);
        let mut dlg = FormDialog::from_form("Settings", form, FormPurpose::Settings);
        dlg.show_page(0);
        dlg
    }

    /// Open a tabbed form on `tab` instead of its first tab.
    pub fn on_tab(mut self, tab: SettingsTab) -> Self {
        if let Some(page) = SETTINGS_PAGES.iter().position(|p| p.tab == tab) {
            self.show_page(page);
        }
        self
    }

    /// The tab a Settings form is showing, or `None` for any other form.
    pub fn settings_tab(&self) -> Option<SettingsTab> {
        self.pages().and_then(|pages| pages.get(self.form.page)).map(|p| p.tab)
    }

    /// The pages this form is split into, or `None` for an untabbed form.
    fn pages(&self) -> Option<&'static [SettingsPage]> {
        matches!(self.purpose, FormPurpose::Settings).then_some(SETTINGS_PAGES)
    }

    /// Switch a tabbed form to page `page` (see [`Form::show_page`]).
    fn show_page(&mut self, page: usize) {
        let Some(pages) = self.pages() else { return };
        let Some(this) = pages.get(page) else { return };
        let start: usize = pages[..page].iter().map(|p| page_field_count(p.groups)).sum();
        self.form.show_page(page, start..start + page_field_count(this.groups));
    }

    /// Move a tabbed form one tab left or right, wrapping at the ends.
    fn cycle_page(&mut self, forward: bool) {
        let Some(n) = self.pages().map(<[_]>::len) else { return };
        let page = self.form.page;
        self.show_page(if forward { (page + 1) % n } else { (page + n - 1) % n });
    }

    /// Build the internal editor's options form (its Options → General), laid
    /// out in the same three groups mcedit uses.
    pub fn editor_options(opts: &crate::config::EditorOptions) -> Self {
        use crate::config::WrapMode;
        let modes: Vec<String> = WrapMode::ALL.iter().map(|(_, l)| l.to_string()).collect();
        // Field order is load-bearing twice over: `EDITOR_OPTION_GROUPS` slices
        // it into the three boxes, and the submit arm reads it back positionally.
        let form = Form::new(vec![
            // --- Wrap mode ---
            Field::choice("Mode", modes, opts.wrap_mode.label()),
            // --- Tabulation ---
            Field::check("Backspace through tabs", opts.backspace_through_tabs),
            Field::check("Fill tabs with spaces", opts.fill_tabs_with_spaces),
            Field::text("Tab spacing", opts.tab_spacing.to_string()),
            // --- Other options ---
            Field::check("Return does autoindent", opts.return_does_autoindent),
            Field::check("Confirm before saving", opts.confirm_before_saving),
            Field::check("Save file position", opts.save_file_position),
            Field::check("Visible trailing spaces", opts.visible_trailing_spaces),
            Field::check("Visible tabs", opts.visible_tabs),
            Field::check("Syntax highlighting", opts.syntax_highlighting),
            Field::check("Cursor after inserted block", opts.cursor_after_inserted_block),
            Field::check("Persistent selection", opts.persistent_selection),
            Field::check("Group undo", opts.group_undo),
            Field::text("Word wrap line length", opts.word_wrap_line_length.to_string()),
        ]);
        FormDialog::from_form("Editor options", form, FormPurpose::EditorOptions)
    }

    /// Build the editor's block-sort options form (Format → Sort).
    pub fn editor_sort() -> Self {
        FormDialog::from_fields(
            "Sort",
            vec![
                Field::check("Reverse order", false),
                Field::check("Ignore case", false),
                Field::check("Remove duplicate lines", false),
            ],
            FormPurpose::EditorSort,
        )
    }

    /// Build the "Find duplicates" options form. With size/date/content all off,
    /// only file names are compared; name matching is case-sensitive by default.
    pub fn find_duplicates() -> Self {
        let form = Form::new(vec![
            Field::check("Also compare size", false),
            Field::check("Also compare date/time", false),
            Field::check("Also compare content", false),
            Field::check("Case-sensitive names", true),
        ]);
        FormDialog {
            title: "Find duplicates".to_string(),
            form,
            purpose: FormPurpose::FindDuplicates,
            connect: None,
        }
    }

    /// Build the checksum options form for `path`: pick an algorithm and,
    /// optionally, paste a checksum to compare the result against. The file name
    /// is shown in the title.
    pub fn checksum(path: VfsPath) -> Self {
        let form = Form::new(vec![
            Field::choice("Algorithm", ChecksumKind::labels(), ChecksumKind::Sha256.label()),
            Field::text("Compare to (optional)", ""),
        ]);
        FormDialog {
            title: format!("Checksum: {}", path.file_name()),
            form,
            purpose: FormPurpose::Checksum(path),
            connect: None,
        }
    }

    /// Build the disk formatter form for `dev`.
    pub fn format(dev: String) -> Self {
        let fs_options: Vec<String> =
            crate::mount::FsType::ALL.iter().map(|f| f.label().to_string()).collect();
        let form = Form::new(vec![
            Field::choice("Filesystem", fs_options, "FAT32"),
            Field::text("Volume label", ""),
            Field::check("Quick format (NTFS)", false),
            Field::text("Bytes/inode (ext, blank=auto)", ""),
        ]);
        FormDialog {
            title: format!("Format {dev}"),
            form,
            purpose: FormPurpose::Format(dev),
            connect: None,
        }
    }

    /// Build a chmod form for `targets` from the current mode bits. The trailing
    /// "Recurse into directories" checkbox makes the change apply into any
    /// directories in the selection.
    /// Collect the options for mirroring the active panel's directory onto the
    /// other one. `src` / `dst` are the two directories, shown so it is obvious
    /// which way round the sync runs before anything is planned.
    pub fn sync(src: &str, dst: &str) -> Self {
        // The option strings stay in English on purpose: `sync_mode_of` maps the
        // picked value back to a mode by matching them, and a Choice renders its
        // options raw (only the field *label* is translated) — the same deal as
        // the checksum-algorithm and filesystem pickers.
        let modes: Vec<String> = SYNC_MODES.iter().map(|s| s.to_string()).collect();
        FormDialog::from_fields(
            "Synchronize",
            vec![Field::choice("Mode", modes, SYNC_MODES[0])],
            FormPurpose::Sync,
        )
        // The title carries the direction: it is the one thing the user must get
        // right, and it is longer than a field label should be.
        .titled(format!("{}:  {src}  →  {dst}", crate::l10n::trd("Synchronize")))
    }

    /// Replace the dialog title (kept out of `from_fields` so the common case
    /// stays a plain translated key).
    fn titled(mut self, title: String) -> Self {
        self.title = title;
        self
    }

    /// A plain form: a title, its fields, and what to do on submit.
    fn from_fields(title: &str, fields: Vec<Field>, purpose: FormPurpose) -> Self {
        Self::from_form(title, Form::new(fields), purpose)
    }

    /// As [`Self::from_fields`], for a caller that already built the `Form`.
    fn from_form(title: &str, form: Form, purpose: FormPurpose) -> Self {
        FormDialog { title: title.to_string(), form, purpose, connect: None }
    }

    // --- Guided Git dialogs ------------------------------------------------
    //
    // Each collects the options for one git command; `Submit::GitRun` carries the
    // argv they build (see the `FormPurpose::Git` arm in `handle_key`). Field
    // order is load-bearing — the argv builders read them positionally.

    /// Commit the index: message, plus the two flags people reach for most.
    pub fn git_commit() -> Self {
        FormDialog::from_fields(
            "Commit",
            vec![
                Field::text("Message", ""),
                Field::check("Stage all tracked changes (-a)", false),
                Field::check("Amend the last commit", false),
            ],
            FormPurpose::Git(GitForm::Commit),
        )
    }

    /// Clone a URL into the panel's directory. An empty target lets git name it.
    pub fn git_clone() -> Self {
        FormDialog::from_fields(
            "Clone",
            vec![Field::text("Repository URL", ""), Field::text("Into directory (optional)", "")],
            FormPurpose::Git(GitForm::Clone),
        )
    }

    pub fn git_fetch(remotes: Vec<String>) -> Self {
        let all = remotes.len() > 1;
        FormDialog::from_fields(
            "Fetch",
            vec![
                Field::check("All remotes (--all)", all),
                Field::check("Prune deleted branches (--prune)", false),
            ],
            FormPurpose::Git(GitForm::Fetch),
        )
    }

    pub fn git_pull() -> Self {
        FormDialog::from_fields(
            "Pull",
            vec![Field::check("Rebase instead of merge (--rebase)", false)],
            FormPurpose::Git(GitForm::Pull),
        )
    }

    /// Push the current branch. `remotes` populates the dropdown; the force flags
    /// are off by default and `--force-with-lease` is offered above the raw one.
    pub fn git_push(remotes: Vec<String>, branch: String) -> Self {
        let first = remotes.first().cloned().unwrap_or_default();
        FormDialog::from_fields(
            "Push",
            vec![
                Field::choice("Remote", remotes, &first),
                Field::check("Set upstream (--set-upstream)", false),
                Field::check("Force with lease (safer)", false),
                Field::check("Force (overwrites the remote!)", false),
            ],
            FormPurpose::Git(GitForm::Push { branch }),
        )
    }

    /// Switch branches. The dropdown lists local then remote-tracking branches
    /// (current first); filling in the name field creates a new branch instead.
    pub fn git_checkout(branches: Vec<String>) -> Self {
        let first = branches.first().cloned().unwrap_or_default();
        FormDialog::from_fields(
            "Checkout",
            vec![
                Field::choice("Branch", branches, &first),
                Field::text("…or create a new branch named", ""),
            ],
            FormPurpose::Git(GitForm::Checkout),
        )
    }

    /// Reset the current branch. Modes are ordered least- to most-destructive so
    /// the dialog opens on the safe one.
    pub fn git_reset() -> Self {
        let modes: Vec<String> = RESET_MODES.iter().map(|s| s.to_string()).collect();
        FormDialog::from_fields(
            "Reset",
            vec![
                Field::choice("Mode", modes, RESET_MODES[0]),
                Field::text("Target commit", "HEAD"),
            ],
            FormPurpose::Git(GitForm::Reset),
        )
    }

    pub fn chmod(targets: Vec<VfsPath>, mode: u32) -> Self {
        let bit = |m: u32| mode & m != 0;
        let form = Form::new(vec![
            Field::check("Owner read    (400)", bit(0o400)),
            Field::check("Owner write   (200)", bit(0o200)),
            Field::check("Owner exec    (100)", bit(0o100)),
            Field::check("Group read    (040)", bit(0o040)),
            Field::check("Group write   (020)", bit(0o020)),
            Field::check("Group exec    (010)", bit(0o010)),
            Field::check("Other read    (004)", bit(0o004)),
            Field::check("Other write   (002)", bit(0o002)),
            Field::check("Other exec    (001)", bit(0o001)),
            Field::check("Recurse into directories", false),
        ]);
        FormDialog {
            title: form_target_title("Chmod", &targets),
            form,
            purpose: FormPurpose::Chmod(targets),
            connect: None,
        }
    }

    pub fn chown(targets: Vec<VfsPath>, owner: String, group: String) -> Self {
        let form = Form::new(vec![
            Field::text("Owner (name or uid)", owner),
            Field::text("Group (name or gid)", group),
            Field::check("Recurse into directories", false),
        ]);
        FormDialog {
            title: form_target_title("Chown", &targets),
            form,
            purpose: FormPurpose::Chown(targets),
            connect: None,
        }
    }

    pub fn symlink(dir: VfsPath, target: String, name: String) -> Self {
        let form = Form::new(vec![
            Field::text("Points to (target)", target),
            Field::text("Link name", name),
        ]);
        FormDialog {
            title: "Create symlink".to_string(),
            form,
            purpose: FormPurpose::Symlink(dir),
            connect: None,
        }
    }

    /// The currently-selected theme name in the settings form (for live
    /// preview), or `None` if this isn't the settings form.
    pub fn theme_choice(&self) -> Option<&str> {
        self.choice_value("Theme")
    }

    /// The currently-selected language name in the settings form (for live
    /// preview), or `None` if this isn't the settings form.
    pub fn lang_choice(&self) -> Option<&str> {
        self.choice_value("Language")
    }

    /// The currently-selected graphics preference (`auto|off|kitty|sixel|iterm`)
    /// in the settings form (for live preview), or `None` if not the settings form.
    pub fn graphics_choice(&self) -> Option<String> {
        self.choice_value("Graphics").map(graphics_pref)
    }

    /// The currently-selected 3D view style in the settings form (for live
    /// preview), or `None` if not the settings form.
    pub fn space3d_choice(&self) -> Option<crate::config::Space3dStyle> {
        self.choice_value("3D style").map(crate::config::Space3dStyle::from_label)
    }

    /// The value of the settings `Check` field labelled `label_key` (for live
    /// preview), or `None` if this isn't the settings form.
    pub fn check_value(&self, label_key: &str) -> Option<bool> {
        if !matches!(self.purpose, FormPurpose::Settings) {
            return None;
        }
        self.form.fields.iter().find_map(|f| match f {
            Field::Check { label, value } if label == label_key => Some(*value),
            _ => None,
        })
    }

    /// The highlighted option of the settings `Choice` field labelled `label`.
    fn choice_value(&self, label_key: &str) -> Option<&str> {
        if !matches!(self.purpose, FormPurpose::Settings) {
            return None;
        }
        self.form.fields.iter().find_map(|f| match f {
            Field::Choice { label, options, idx, open, sel, .. } if label == label_key => {
                // While the dropdown is open, preview the highlighted option so
                // scrolling shows a live theme/language preview.
                options.get(if *open { *sel } else { *idx }).map(|s| s.as_str())
            }
            _ => None,
        })
    }

    /// The settings field labelled `label`. Panics on a label the form doesn't
    /// have: that is a typo between the constructor and the reader, and the
    /// round-trip test submits every one of them.
    fn setting(&self, label: &str) -> &Field {
        self.form
            .fields
            .iter()
            .find(|f| f.label() == label)
            .unwrap_or_else(|| panic!("the settings form has no field labelled {label:?}"))
    }

    /// Collect the Settings form's values. Read by label rather than by
    /// position, so moving a field to another tab or group can't hand its value
    /// to a neighbour.
    fn settings_values(&self) -> SettingsValues {
        use crate::config::{
            AudioDisplay, SaverKind, Space3dStyle, ThumbSize, saver_minutes_from_label,
        };
        let text = |label| self.setting(label).as_text();
        let on = |label| self.setting(label).as_bool();
        SettingsValues {
            theme: text("Theme").to_string(),
            animation: on("Animations"),
            nerd_font: on("Nerd Font symbols"),
            system_status: on("System status widget"),
            screensaver_minutes: saver_minutes_from_label(text("Screensaver")),
            screensaver: SaverKind::from_label(text("Screensaver style")),
            brief_columns: text("Brief view columns").parse().unwrap_or(2).clamp(1, 6),
            thumb_size: ThumbSize::from_label(text("Thumbnail size")),
            space3d_style: Space3dStyle::from_label(text("3D style")),
            audio_display: AudioDisplay::from_label(text("Audio view")),
            audio_autoplay: on("Auto-play audio in the viewer"),
            auto_refresh: on("Auto-refresh panels"),
            space3d_activity: on("3D view: show filesystem activity"),
            details_activity: on("Details view: git activity"),
            editor: text("External editor").trim().to_string(),
            viewer: text("External viewer").trim().to_string(),
            use_internal_viewer: on("Use internal viewer"),
            use_internal_editor: on("Use internal editor"),
            command_prompt: on("Command prompt"),
            shell: text("Shell (blank = auto-detect)").trim().to_string(),
            // Unreadable input keeps the current size rather than guessing one.
            command_history_max: text("Command history size (0 = off)").trim().parse().ok(),
            confirm_delete: on("Confirm delete"),
            confirm_overwrite: on("Confirm overwrite"),
            confirm_execute: on("Confirm execute"),
            confirm_unmount: on("Confirm unmount"),
            confirm_exit: on("Confirm exit"),
            use_trash: on("Use trash bin"),
            language: text("Language").to_string(),
            reshape_rtl: on("Reshape RTL text"),
            graphics: graphics_pref(text("Graphics")),
            truecolor: on("Truecolor (gradients)"),
            strip_trailing_spaces: on("Strip trailing spaces on copy"),
        }
    }

    pub fn connect(
        protocol: Protocol,
        side: usize,
        history: Vec<crate::config::RemoteHistoryEntry>,
    ) -> Self {
        let cfg = match protocol {
            Protocol::Sftp | Protocol::Scp => crate::vfs::remote::sshconfig::SshConfig::load_user(),
            Protocol::Ftp | Protocol::Ftps => Default::default(),
        };
        Self::connect_with_config(protocol, side, history, &cfg)
    }

    /// [`connect`](Self::connect), offering the hosts `cfg` names.
    pub(crate) fn connect_with_config(
        protocol: Protocol,
        side: usize,
        history: Vec<crate::config::RemoteHistoryEntry>,
        cfg: &crate::vfs::remote::sshconfig::SshConfig,
    ) -> Self {
        let mut fields = vec![
            Field::text("Host", ""),
            Field::text("Port", protocol.default_port().to_string()),
            Field::text("Username", ""),
            Field::password("Password"),
            Field::text("Remote path (blank = home)", ""),
        ];
        // Field 5 is protocol-specific, and the two uses are mutually exclusive:
        // PASV is a plain-FTP concept, while a key file only means anything over
        // SSH. Sharing the index keeps every other field's position fixed.
        if protocol.is_ftp() {
            fields.push(Field::check("Passive mode (PASV)", true));
        } else {
            fields.push(Field::text("Key file (blank = agent / default keys)", ""));
        }
        let form = Form::new(fields);
        // Only this protocol's recent connections, then the config's hosts not
        // among them.
        let mut history: Vec<_> =
            history.into_iter().filter(|e| e.protocol == protocol.scheme_prefix()).collect();
        let mut labels: Vec<String> = history.iter().map(|e| e.label()).collect();
        for (entry, label) in ssh_config_entries(cfg, protocol) {
            if !history.iter().any(|h| h.host == entry.host) {
                history.push(entry);
                labels.push(label);
            }
        }
        FormDialog {
            // The proto prefix stays literal; the word is translated (the title
            // is passed through `trd` again at render, harmlessly, for RTL shaping).
            title: format!(
                "{} {}",
                protocol.scheme_prefix().to_uppercase(),
                crate::l10n::tr("connection")
            ),
            form,
            purpose: FormPurpose::Connect(protocol, side),
            connect: Some(ConnectDropdown {
                history,
                labels,
                open: false,
                sel: 0,
                chevron: None,
                entries: Vec::new(),
            }),
        }
    }

    /// Build a connect form already filled in from a stored remote (its protocol,
    /// host, port, user and path), focused on the password field — for the command
    /// palette's "reconnect to a saved server" entries. `None` if the stored
    /// protocol string is unrecognized.
    pub fn connect_from(entry: &crate::config::RemoteHistoryEntry, side: usize) -> Option<Self> {
        let protocol = Protocol::from_prefix(&entry.protocol)?;
        let mut dlg = FormDialog::connect(protocol, side, vec![entry.clone()]);
        dlg.apply_history(0);
        Some(dlg)
    }

    /// Fill the host/port/user/path fields from history entry `idx` and move the
    /// focus to the password field.
    fn apply_history(&mut self, idx: usize) {
        let entry = match self.connect.as_ref().and_then(|c| c.history.get(idx).cloned()) {
            Some(e) => e,
            None => return,
        };
        if let Some(c) = self.connect.as_mut() {
            c.open = false;
        }
        set_text_field(&mut self.form.fields[0], &entry.host);
        set_text_field(&mut self.form.fields[1], &entry.port.to_string());
        set_text_field(&mut self.form.fields[2], &entry.user);
        if let Some(field) = self.form.fields.get_mut(4) {
            set_text_field(field, &entry.path);
        }
        // Field 5 is the PASV checkbox on FTP forms and the key file on SSH
        // ones; each arm matches its own variant, so the other is left alone.
        match self.form.fields.get_mut(5) {
            Some(Field::Check { value, .. }) => *value = entry.passive,
            Some(field) => set_text_field(field, &entry.key_file),
            None => {}
        }
        // Focus the password, except when a key file was restored — then the
        // password is very likely not the thing that needs typing.
        self.form.focus_at(if entry.key_file.is_empty() { 3 } else { 0 });
    }

    /// Move focus onto the OK (`primary`) or Cancel button slot. Used when the
    /// mouse clicks a button so the synthetic Enter/Esc submits or cancels the
    /// form rather than acting on the field that happened to be focused (e.g.
    /// opening a Choice dropdown). Also closes any open Choice dropdown.
    pub(crate) fn focus_button(&mut self, primary: bool) {
        for field in &mut self.form.fields {
            if let Field::Choice { open, .. } = field {
                *open = false;
            }
        }
        let slot = if primary { self.form.ok_slot() } else { self.form.cancel_slot() };
        self.form.focus_at(slot);
    }

    /// Route a click for the connect dropdown. Returns `Some` if the click hit
    /// the chevron or a dropdown entry (or dismissed an open dropdown).
    pub(crate) fn click_dropdown(&mut self, col: u16, row: u16) -> Option<DialogResult> {
        let hit =
            |r: &Rect| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        let cd = self.connect.as_ref()?;
        if cd.chevron.is_some_and(|r| hit(&r)) {
            let cd = self.connect.as_mut().unwrap();
            cd.open = !cd.open;
            cd.sel = 0;
            return Some(DialogResult::None);
        }
        if !cd.open {
            return None;
        }
        let hidx = cd.entries.iter().find(|(r, _)| hit(r)).map(|&(_, i)| i);
        match hidx {
            Some(i) => self.apply_history(i),
            None => self.connect.as_mut().unwrap().open = false,
        }
        Some(DialogResult::None)
    }

    fn chmod_mode(&self) -> u32 {
        const BITS: [u32; 9] = [0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001];
        // `zip` stops at the 9 permission bits, so the trailing "Recurse"
        // checkbox is ignored here.
        let mut mode = 0;
        for (f, bit) in self.form.fields.iter().zip(BITS) {
            if f.as_bool() {
                mode |= bit;
            }
        }
        mode
    }

    /// Whether the chmod/chown "Recurse into directories" checkbox (always the
    /// last field of those forms) is ticked.
    fn recursive(&self) -> bool {
        self.form.fields.last().map(|f| f.as_bool()).unwrap_or(false)
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        // Ctrl-PgUp/PgDn switch tabs from anywhere in a tabbed form, as they
        // switch a panel's tabs. Checked before the Choice dropdown, which would
        // otherwise take them as a plain page up/down through its options.
        if self.pages().is_some()
            && key.modifiers.contains(ratatui::crossterm::event::KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
        {
            self.cycle_page(key.code == KeyCode::PageDown);
            return DialogResult::None;
        }

        // Connect-form history dropdown: while open it captures navigation keys;
        // closed, pressing ↓ on the Host field opens it.
        let drop_open = self.connect.as_ref().is_some_and(|c| c.open);
        if drop_open {
            match key.code {
                KeyCode::Esc => self.connect.as_mut().unwrap().open = false,
                KeyCode::Up => {
                    let c = self.connect.as_mut().unwrap();
                    c.sel = c.sel.saturating_sub(1);
                }
                KeyCode::Down => {
                    let c = self.connect.as_mut().unwrap();
                    if c.sel + 1 < c.history.len() {
                        c.sel += 1;
                    }
                }
                KeyCode::Enter => {
                    let i = self.connect.as_ref().unwrap().sel;
                    self.apply_history(i);
                }
                _ => {}
            }
            return DialogResult::None;
        }
        if matches!(key.code, KeyCode::Down)
            && self.form.focus == 0
            && self.connect.as_ref().is_some_and(|c| !c.history.is_empty())
        {
            let c = self.connect.as_mut().unwrap();
            c.open = true;
            c.sel = 0;
            return DialogResult::None;
        }

        // A Choice field's scrollable dropdown: Enter on a closed choice opens
        // it; while open, the arrows move the highlight, Enter picks, Esc closes.
        if let Some(Field::Choice { options, idx, open, sel, .. }) =
            self.form.fields.get_mut(self.form.focus)
        {
            if *open {
                let last = options.len().saturating_sub(1);
                match key.code {
                    KeyCode::Esc => *open = false,
                    KeyCode::Up => *sel = sel.saturating_sub(1),
                    KeyCode::Down => *sel = (*sel + 1).min(last),
                    KeyCode::PageUp => *sel = sel.saturating_sub(8),
                    KeyCode::PageDown => *sel = (*sel + 8).min(last),
                    KeyCode::Home => *sel = 0,
                    KeyCode::End => *sel = last,
                    KeyCode::Enter => {
                        *idx = *sel;
                        *open = false;
                    }
                    _ => {}
                }
                return DialogResult::None;
            }
            if key.code == KeyCode::Enter {
                *sel = *idx;
                *open = true;
                return DialogResult::None;
            }
        }

        if let KeyCode::Esc = key.code {
            return DialogResult::Cancel;
        }

        // Focus on the tab strip: Left/Right walk the tabs, Up/Down leave the
        // strip, and Enter submits, as it does from any field.
        if self.form.on_strip() {
            let last = self.pages().map_or(0, |p| p.len().saturating_sub(1));
            match key.code {
                KeyCode::Left => self.cycle_page(false),
                KeyCode::Right => self.cycle_page(true),
                KeyCode::Home => self.show_page(0),
                KeyCode::End => self.show_page(last),
                KeyCode::Down | KeyCode::Tab => self.form.focus_next(),
                KeyCode::Up | KeyCode::BackTab => self.form.focus_prev(),
                _ => {}
            }
            if key.code != KeyCode::Enter {
                return DialogResult::None;
            }
        } else if self.form.on_button() {
            // Focus on the OK / Cancel buttons: arrows move between them and
            // back to the fields; Enter/Space activates.
            match key.code {
                KeyCode::Left | KeyCode::Right => {
                    self.form.focus = if self.form.on_cancel() {
                        self.form.ok_slot()
                    } else {
                        self.form.cancel_slot()
                    };
                    return DialogResult::None;
                }
                KeyCode::Up | KeyCode::BackTab => {
                    self.form.focus_prev();
                    return DialogResult::None;
                }
                KeyCode::Down | KeyCode::Tab => {
                    self.form.focus_next();
                    return DialogResult::None;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if self.form.on_cancel() {
                        return DialogResult::Cancel;
                    }
                    // OK: fall through to build the submit payload below.
                }
                _ => return DialogResult::None,
            }
        } else if !self.form.handle_key(key) {
            return DialogResult::None;
        }
        // Enter (on a field or OK) → build the submit payload.
        let fields = &self.form.fields;
        let submit = match &self.purpose {
            FormPurpose::Settings => Submit::Settings(self.settings_values()),
            FormPurpose::Format(dev) => {
                let fs = crate::mount::FsType::from_label(fields[0].as_text())
                    .unwrap_or(crate::mount::FsType::Fat32);
                Submit::Format(crate::mount::FormatSpec {
                    dev: dev.clone(),
                    fs,
                    label: fields[1].as_text().trim().to_string(),
                    quick: fields[2].as_bool(),
                    inode_bytes: fields[3].as_text().trim().to_string(),
                })
            }
            FormPurpose::EditorOptions => {
                use crate::config::{EditorOptions, WrapMode};
                let num = |i: usize, fallback: usize, lo: usize, hi: usize| {
                    fields[i].as_text().trim().parse::<usize>().unwrap_or(fallback).clamp(lo, hi)
                };
                Submit::EditorOptions(Box::new(EditorOptions {
                    wrap_mode: WrapMode::from_label(fields[0].as_text()),
                    backspace_through_tabs: fields[1].as_bool(),
                    fill_tabs_with_spaces: fields[2].as_bool(),
                    tab_spacing: num(3, 4, 1, 16),
                    return_does_autoindent: fields[4].as_bool(),
                    confirm_before_saving: fields[5].as_bool(),
                    save_file_position: fields[6].as_bool(),
                    visible_trailing_spaces: fields[7].as_bool(),
                    visible_tabs: fields[8].as_bool(),
                    syntax_highlighting: fields[9].as_bool(),
                    cursor_after_inserted_block: fields[10].as_bool(),
                    persistent_selection: fields[11].as_bool(),
                    group_undo: fields[12].as_bool(),
                    word_wrap_line_length: num(13, 72, 20, 1000),
                    // Not in the dialog: kept by the app from the settings in use.
                    ..EditorOptions::default()
                }))
            }
            FormPurpose::EditorSort => Submit::EditorSort {
                reverse: fields[0].as_bool(),
                ignore_case: fields[1].as_bool(),
                unique: fields[2].as_bool(),
            },
            FormPurpose::FindDuplicates => Submit::FindDuplicates(DupCriteria {
                size: fields[0].as_bool(),
                date: fields[1].as_bool(),
                content: fields[2].as_bool(),
                case_sensitive: fields[3].as_bool(),
            }),
            FormPurpose::Checksum(path) => Submit::Checksum {
                path: path.clone(),
                kind: ChecksumKind::from_label(fields[0].as_text()).unwrap_or(ChecksumKind::Sha256),
                expected: fields[1].as_text().trim().to_string(),
            },
            // Each guided git form builds its own argv here, so the app just runs
            // it. Field indices mirror the constructors above.
            FormPurpose::Git(kind) => {
                use crate::git::ops;
                match kind {
                    GitForm::Commit => {
                        let msg = fields[0].as_text().trim().to_string();
                        // Git would reject an empty message anyway; say so here
                        // rather than opening an output box on a certain failure.
                        if msg.is_empty() {
                            return DialogResult::Cancel;
                        }
                        Submit::GitRun {
                            title: "commit".into(),
                            args: ops::commit_args(&msg, fields[1].as_bool(), fields[2].as_bool()),
                        }
                    }
                    GitForm::Clone => {
                        let url = fields[0].as_text().trim().to_string();
                        if url.is_empty() {
                            return DialogResult::Cancel;
                        }
                        Submit::GitRun {
                            title: "clone".into(),
                            args: ops::clone_args(&url, fields[1].as_text()),
                        }
                    }
                    GitForm::Fetch => Submit::GitRun {
                        title: "fetch".into(),
                        args: ops::fetch_args(fields[0].as_bool(), fields[1].as_bool()),
                    },
                    GitForm::Pull => Submit::GitRun {
                        title: "pull".into(),
                        args: ops::pull_args(fields[0].as_bool()),
                    },
                    GitForm::Push { branch } => Submit::GitRun {
                        title: "push".into(),
                        args: ops::push_args(
                            fields[0].as_text(),
                            branch,
                            fields[3].as_bool(),
                            fields[2].as_bool(),
                            fields[1].as_bool(),
                        ),
                    },
                    GitForm::Checkout => {
                        // A typed name creates that branch; otherwise switch to
                        // the one picked from the dropdown.
                        let new = fields[1].as_text().trim().to_string();
                        let (target, create) = if new.is_empty() {
                            (fields[0].as_text().to_string(), false)
                        } else {
                            (new, true)
                        };
                        if target.is_empty() {
                            return DialogResult::Cancel;
                        }
                        Submit::GitRun {
                            title: "checkout".into(),
                            args: ops::checkout_args(&target, create),
                        }
                    }
                    GitForm::Reset => Submit::GitRun {
                        title: "reset".into(),
                        args: ops::reset_args(
                            reset_mode_name(fields[0].as_text()),
                            fields[1].as_text(),
                        ),
                    },
                }
            }
            // The app plans the sync in the background, then previews it.
            FormPurpose::Sync => Submit::SyncPlan(sync_mode_of(fields[0].as_text())),
            FormPurpose::Chmod(paths) => {
                Submit::Chmod(paths.clone(), self.chmod_mode(), self.recursive())
            }
            FormPurpose::Chown(paths) => Submit::Chown(
                paths.clone(),
                fields[0].as_text().trim().to_string(),
                fields[1].as_text().trim().to_string(),
                self.recursive(),
            ),
            FormPurpose::Symlink(dir) => {
                let target = fields[0].as_text().trim().to_string();
                let name = fields[1].as_text().trim().to_string();
                if target.is_empty() || name.is_empty() {
                    return DialogResult::Cancel;
                }
                Submit::Symlink { dir: dir.clone(), target, name }
            }
            FormPurpose::Connect(protocol, side) => {
                let host = fields[0].as_text().trim().to_string();
                if host.is_empty() {
                    return DialogResult::Cancel;
                }
                let port =
                    fields[1].as_text().trim().parse::<u16>().unwrap_or(protocol.default_port());
                Submit::Connect(
                    *side,
                    RemoteCreds {
                        protocol: *protocol,
                        host,
                        port,
                        user: fields[2].as_text().trim().to_string(),
                        password: fields[3].as_text().to_string(),
                        path: fields[4].as_text().trim().to_string(),
                        // Field 5 is the PASV checkbox on FTP forms and the key
                        // file on SSH ones. `as_bool` on a text field is false
                        // and `as_text` on a checkbox is empty, so each protocol
                        // reads its own and gets a harmless default for the other.
                        passive: fields.get(5).map(Field::as_bool).unwrap_or(false),
                        key_file: fields
                            .get(5)
                            .map(|f| f.as_text().trim().to_string())
                            .unwrap_or_default(),
                        key_passphrase: String::new(),
                    },
                )
            }
        };
        DialogResult::Submit(submit)
    }

    /// The dialog's outer box size for the current form. The grouped forms are
    /// wider and taller to fit their bordered group boxes (and, on Settings, the
    /// tab strip); every other form keeps the compact one-row-per-field box.
    fn outer_dims(&self, area: Rect) -> (u16, u16) {
        if let Some(groups) = self.groups() {
            // Each group box = the rows its fields need once spread over its
            // columns, + 2 border rows; plus a spacer and the hint/button row
            // inside, and the outer border. A tabbed form sizes itself for its
            // tallest page, so the box doesn't jump as the tabs change.
            let content = match self.pages() {
                Some(pages) => {
                    let tallest = pages.iter().map(|p| groups_height(p.groups)).max();
                    TAB_STRIP_ROWS + tallest.unwrap_or(0) + 1 /* divider */ + HELP_ROWS as u16
                }
                None => groups_height(groups),
            };
            let height = content + 1 /* spacer */ + 1 /* hint */ + 2 /* border */;
            let w = GROUPED_FORM_WIDTH.min(area.width.saturating_sub(4));
            (w, height)
        } else {
            let height = self.form.fields.len() as u16 + 4;
            let w = 60u16.min(area.width.saturating_sub(4));
            (w, height)
        }
    }

    /// The centered outer box rect (used by `render` and by click hit-testing).
    pub(crate) fn outer_rect(&self, area: Rect) -> Rect {
        let (w, h) = self.outer_dims(area);
        centered(area, w, h)
    }

    /// Total fields the group table claims, for the test that keeps it in step
    /// with the real field list. `None` when this form has no groups.
    ///
    /// The counts do double duty: they also drive the column split in
    /// [`FormDialog::field_rows`], so a table that drifts out of step with the
    /// real field list would mis-place rows as well as mis-size the dialog.
    #[cfg(test)]
    pub(crate) fn group_field_total(&self) -> Option<usize> {
        match self.pages() {
            Some(pages) => Some(pages.iter().map(|p| page_field_count(p.groups)).sum()),
            None => self.groups().map(page_field_count),
        }
    }

    /// The titled groups on screen, or `None` for the flat one-row-per-field
    /// forms. On a tabbed form, those of the page showing.
    fn groups(&self) -> Option<&'static [Group]> {
        match self.purpose {
            FormPurpose::Settings => SETTINGS_PAGES.get(self.form.page).map(|p| p.groups),
            FormPurpose::EditorOptions => Some(EDITOR_OPTION_GROUPS),
            _ => None,
        }
    }

    /// A tabbed form's description block, at the foot of the interior just above
    /// the spacer and button row: the divider row, and the rect its lines go in.
    /// `None` for an untabbed form.
    fn help_area(&self, inner: Rect) -> Option<(u16, Rect)> {
        self.pages()?;
        let rows = HELP_ROWS as u16;
        let top = (inner.y + inner.height).saturating_sub(2 + rows);
        let text =
            Rect { x: inner.x + 1, y: top, width: inner.width.saturating_sub(2), height: rows };
        Some((top.saturating_sub(1), text))
    }

    /// The description of whatever has focus on a tabbed form: the focused
    /// setting, the tab strip, or a button.
    fn help_text(&self) -> Option<&'static str> {
        self.pages()?;
        if self.form.on_strip() {
            return Some(HELP_TABS);
        }
        if self.form.on_ok() {
            return Some(HELP_OK);
        }
        if self.form.on_cancel() {
            return Some(HELP_CANCEL);
        }
        let label = self.form.fields.get(self.form.focus)?.label();
        SETTINGS_HELP.iter().find(|(l, _)| *l == label).map(|(_, help)| *help)
    }

    /// Where a grouped form's boxes go inside the dialog interior: all of it, or
    /// what the tab strip leaves on a tabbed form.
    fn content_area(&self, inner: Rect) -> Rect {
        if self.pages().is_none() {
            return inner;
        }
        let strip = TAB_STRIP_ROWS.min(inner.height);
        Rect { y: inner.y + strip, height: inner.height - strip, ..inner }
    }

    /// The tab strip's cells on the first interior row: each tab's display title
    /// and the rect a click on it hits. Titles keep their natural width with a
    /// space either side while they all fit; otherwise the row is shared evenly
    /// and each title shortened to its share, as a panel's tab strip does.
    fn tab_cells(&self, inner: Rect) -> Vec<(String, Rect)> {
        use unicode_width::UnicodeWidthStr;
        let Some(pages) = self.pages() else { return Vec::new() };
        let titles: Vec<String> = pages.iter().map(|p| crate::l10n::trd(p.title)).collect();
        let natural: usize = titles.iter().map(|t| t.width() + 2).sum();
        let fits = natural <= inner.width as usize;
        let share = (inner.width as usize / titles.len().max(1)).max(1);
        let mut x = inner.x;
        let end = inner.x + inner.width;
        let mut cells = Vec::with_capacity(titles.len());
        for title in titles {
            if x >= end {
                break;
            }
            let (text, w) = if fits {
                let w = title.width() + 2;
                (format!(" {title} "), w)
            } else {
                // Keep a space before the title while there's room for one.
                let body = ellipsize(&title, share.saturating_sub(1));
                (pad_right(&format!(" {body}"), share), share)
            };
            let w = (w as u16).min(end - x);
            cells.push((text, Rect { x, y: inner.y, width: w, height: 1 }));
            x += w;
        }
        cells
    }

    /// A grouped form's boxes (title + rect), laid out vertically inside `inner`.
    fn group_boxes(&self, inner: Rect) -> Vec<(&'static str, Rect)> {
        let groups = self.groups().unwrap_or(&[]);
        let inner = self.content_area(inner);
        let mut boxes = Vec::with_capacity(groups.len());
        let mut y = inner.y;
        for (title, count, cols) in groups {
            let box_h = group_row_count(*count, *cols) + 2;
            boxes.push((*title, Rect { x: inner.x, y, width: inner.width, height: box_h }));
            y += box_h;
        }
        boxes
    }

    /// The on-screen row rect for each field, in field order. Grouped rows sit
    /// inside their group box (inset by the border); other forms stack one
    /// full-width row per field.
    ///
    /// A multi-column group is filled **column-major**, so field order still
    /// walks down one column and then down the next — which is what `Down` and
    /// `Tab` do, since focus movement follows field order. The row count
    /// comes from the group's declared field count rather than from the box
    /// height, so a half-filled last column leaves no phantom rect for a click
    /// to land on.
    ///
    /// Fields on a tab that isn't showing get an empty rect, which no click can
    /// land in.
    fn field_rows(&self, inner: Rect) -> Vec<Rect> {
        let Some(groups) = self.groups() else {
            return (0..self.form.fields.len())
                .map(|i| Rect { y: inner.y + i as u16, height: 1, ..inner })
                .collect();
        };
        let mut rows = vec![Rect::default(); self.form.fields.len()];
        let mut slots = rows.iter_mut().skip(self.form.visible.start);
        for ((_, count, cols), (_, brect)) in groups.iter().zip(self.group_boxes(inner)) {
            let inner_box = Rect {
                x: brect.x + 1,
                y: brect.y + 1,
                width: brect.width.saturating_sub(2),
                height: brect.height.saturating_sub(2),
            };
            let cols = (*cols).max(1) as u16;
            let per_col = group_row_count(*count, cols as usize) as usize;
            let gutter = if cols > 1 { GROUP_COL_GUTTER } else { 0 };
            let col_w = inner_box.width.saturating_sub(gutter * (cols - 1)) / cols;
            for (k, slot) in slots.by_ref().take(*count).enumerate() {
                let (c, r) = (k / per_col, k % per_col);
                *slot = Rect {
                    x: inner_box.x + c as u16 * (col_w + gutter),
                    y: inner_box.y + r as u16,
                    width: col_w,
                    height: 1,
                };
            }
        }
        rows
    }

    pub(crate) fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        gfx: Option<&mut Gfx>,
    ) {
        let rect = self.outer_rect(area);
        draw_shadow(f, rect, theme);
        f.render_widget(Clear, rect);
        // The Settings dialog doubles as the "about" surface: append the program
        // name and version to its title. The version isn't translated.
        let title = if matches!(self.purpose, FormPurpose::Settings) {
            format!(
                "{} — Rat Commander {}",
                crate::l10n::trd(&self.title),
                env!("CARGO_PKG_VERSION")
            )
        } else {
            crate::l10n::trd(&self.title)
        };
        let block = dialog_block(&title, theme);
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let base = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let focus_style = theme.dialog_selection;

        // A tabbed form's strip. The tab showing is marked in the title colour,
        // and takes the selection colour only while the strip itself has focus,
        // so it never looks like a second focused control.
        for (i, (text, cell)) in self.tab_cells(inner).into_iter().enumerate() {
            let style = if i != self.form.page {
                base
            } else if self.form.on_strip() {
                focus_style
            } else {
                base.fg(theme.dialog_title).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            };
            f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), cell);
        }

        // Grouped forms put their fields in titled sub-boxes; other forms are a
        // flat one-row-per-field column. `field_rows` maps each field index to
        // its on-screen row either way.
        if self.groups().is_some() {
            for (title, brect) in self.group_boxes(inner) {
                let gblock = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.dialog_border_fg).bg(theme.dialog_bg))
                    .title(Span::styled(
                        format!(" {} ", crate::l10n::trd(title)),
                        Style::default().fg(theme.dialog_title).bg(theme.dialog_bg),
                    ))
                    .style(base);
                f.render_widget(gblock, brect);
            }
        }
        let rows = self.field_rows(inner);

        // The Host field of a connect form gets a ▼ chevron to open the history.
        let connect_host = self.connect.as_ref().is_some_and(|c| !c.history.is_empty());
        let mut host_chevron: Option<Rect> = None;

        let mut caret: Option<Position> = None;
        for (i, field) in self.form.fields.iter().enumerate() {
            if !self.form.visible.contains(&i) {
                continue;
            }
            let row = rows[i];
            let y = row.y;
            let focused = i == self.form.focus;
            match field {
                Field::Text { label, value, cursor } | Field::Password { label, value, cursor } => {
                    let masked = matches!(field, Field::Password { .. });
                    let label_str = crate::l10n::display(&format!("{}: ", crate::l10n::tr(label)));
                    let lw = (label_str.chars().count() as u16).min(row.width);
                    let style = if focused { focus_style } else { base };
                    f.render_widget(
                        Paragraph::new(Span::styled(label_str, style)),
                        Rect { width: lw, ..row },
                    );
                    let mut field_area =
                        Rect { x: row.x + lw, width: row.width.saturating_sub(lw), ..row };
                    // Reserve room for the chevron on the Host field.
                    if i == 0 && connect_host && field_area.width > 4 {
                        let cx = field_area.x + field_area.width - 2;
                        host_chevron = Some(Rect { x: cx, y, width: 2, height: 1 });
                        field_area.width -= 2;
                    }
                    if let Some(pos) = draw_input_field_ex(
                        f,
                        field_area,
                        value,
                        *cursor,
                        focused,
                        masked,
                        focused && self.form.selected,
                        theme,
                    ) {
                        caret = Some(pos);
                    }
                }
                Field::Check { label, value } => {
                    let mark = if *value { "[x]" } else { "[ ]" };
                    let style = if focused { focus_style } else { base };
                    f.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            crate::l10n::display(&format!("{mark} {}", crate::l10n::tr(label))),
                            style,
                        ))),
                        row,
                    );
                }
                Field::Choice { label, options, idx, .. } => {
                    let style = if focused { focus_style } else { base };
                    let val = options.get(*idx).map(|s| s.as_str()).unwrap_or("");
                    // A ▾ affordance signals the Enter-to-open dropdown.
                    f.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            crate::l10n::display(&format!("{}: {val} ▾", crate::l10n::tr(label))),
                            style,
                        ))),
                        row,
                    );
                }
            }
        }

        // Draw the chevron and (when open) the recent-servers dropdown.
        if let Some(chev) = host_chevron {
            let style = base.add_modifier(Modifier::BOLD);
            f.buffer_mut().set_string(chev.x, chev.y, "▼", style);
        }
        let dropdown_open = self.connect.as_ref().is_some_and(|c| c.open);
        if let Some(c) = self.connect.as_mut() {
            c.chevron = host_chevron;
            c.entries.clear();
        }
        if dropdown_open {
            self.render_dropdown(f, inner, theme);
        }

        // The focused setting's description, under a divider that joins the
        // dialog's border on both sides. Drawn before any open dropdown, which
        // may hang over it.
        if let Some((divider, text)) = self.help_area(inner) {
            let border = Style::default().fg(theme.dialog_border_fg).bg(theme.dialog_border_bg);
            let rule = format!("├{}┤", "─".repeat(inner.width as usize));
            f.buffer_mut().set_string(inner.x.saturating_sub(1), divider, rule, border);
            if let Some(help) = self.help_text() {
                let help = crate::l10n::tr(help);
                let lines = fit_lines(&help, text.width as usize, HELP_ROWS);
                // Shaped per line, in the paragraph's direction (see
                // `display_lines`), so RTL text still reads top to bottom.
                for (i, line) in crate::l10n::display_lines(&help, &lines).into_iter().enumerate() {
                    let row = Rect { y: text.y + i as u16, height: 1, ..text };
                    f.render_widget(Paragraph::new(Span::styled(line, base)), row);
                }
            }
        }

        let choice_open =
            self.form.fields.iter().any(|f| matches!(f, Field::Choice { open: true, .. }));

        let hint = Rect { y: inner.y + inner.height.saturating_sub(1), height: 1, ..inner };
        let extra = match &self.purpose {
            FormPurpose::Chmod(_) => format!("  octal {:03o}", self.chmod_mode()),
            FormPurpose::Settings => "  Ctrl-PgUp/PgDn tabs".to_string(),
            _ => String::new(),
        };
        // OK / Cancel buttons highlight when focused (reachable via ↑↓/Tab).
        let mut gfx = gfx;
        let ok_txt = crate::l10n::tr("OK");
        let cancel_txt = crate::l10n::tr("Cancel");
        // Graphical buttons only when the font can render the labels; otherwise
        // fall back to the text button row (terminal font handles any script).
        // Text buttons, too, while a dropdown is open: the list can hang over
        // the button row, and a graphical button is an image the terminal lays
        // over the cells, so it would stay on top of the list. Text buttons are
        // plain cells the list simply covers.
        if !dropdown_open
            && !choice_open
            && gfx.as_deref().is_some_and(|g| g.buttons_ok())
            && all_renderable(&[&ok_txt, &cancel_txt])
        {
            // Graphical buttons: OK at the left, Cancel at the right, with the
            // navigation hint between them. Left/right halves still hit-test OK/Cancel.
            let ok_w = 10u16.min(hint.width);
            let cancel_w = 12u16.min(hint.width.saturating_sub(ok_w));
            let ok_rect = Rect { x: hint.x, y: hint.y, width: ok_w, height: 1 };
            let cancel_rect =
                Rect { x: hint.x + hint.width - cancel_w, y: hint.y, width: cancel_w, height: 1 };
            let mid_x = ok_rect.x + ok_rect.width + 1;
            let mid_w = cancel_rect.x.saturating_sub(mid_x);
            if mid_w > 0 {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("Tab/↑↓ move  Space toggle{extra}"),
                        base,
                    )))
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(base),
                    Rect { x: mid_x, y: hint.y, width: mid_w, height: 1 },
                );
            }
            gfx_button(
                f,
                gfx.as_deref_mut(),
                Slot::Button(0),
                ok_rect,
                &ok_txt,
                self.form.on_ok(),
                theme,
            );
            gfx_button(
                f,
                gfx,
                Slot::Button(1),
                cancel_rect,
                &cancel_txt,
                self.form.on_cancel(),
                theme,
            );
        } else {
            // Text buttons are laid out exactly like the graphical ones above —
            // OK against the left edge, Cancel against the right, the hint
            // centered in the gap. (Rendering them as one left-aligned line
            // instead would pile all the slack up on the right.)
            let ok_txt = crate::l10n::trd("OK");
            let cancel_txt = crate::l10n::trd("Cancel");
            let label = |text: &str, focused: bool| {
                if focused { format!("[< {text} >]") } else { format!("[  {text}  ]") }
            };
            let ok_label = label(&ok_txt, self.form.on_ok());
            let cancel_label = label(&cancel_txt, self.form.on_cancel());
            // Display width, not chars: a CJK label takes two cells a character,
            // and sizing it by count pushed Cancel half off the right edge.
            use unicode_width::UnicodeWidthStr;
            let ok_w = (ok_label.width() as u16).min(hint.width);
            let cancel_w = (cancel_label.width() as u16).min(hint.width.saturating_sub(ok_w));
            let cancel_x = hint.x + hint.width - cancel_w;
            let styled = |text: String, focused: bool| {
                let style = if focused { theme.button_focused } else { theme.button };
                Paragraph::new(Line::from(Span::styled(text, style))).style(base)
            };
            f.render_widget(
                styled(ok_label, self.form.on_ok()),
                Rect { x: hint.x, y: hint.y, width: ok_w, height: 1 },
            );
            f.render_widget(
                styled(cancel_label, self.form.on_cancel()),
                Rect { x: cancel_x, y: hint.y, width: cancel_w, height: 1 },
            );
            let mid_x = hint.x + ok_w;
            let mid_w = cancel_x.saturating_sub(mid_x);
            if mid_w > 0 {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("Tab/↑↓ move  Space toggle{extra}"),
                        base,
                    )))
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(base),
                    Rect { x: mid_x, y: hint.y, width: mid_w, height: 1 },
                );
            }
        }

        // An open Choice dropdown is drawn last, so it overlays everything it
        // spills across — the button/hint row and the dialog border included.
        // (It is sized against the screen, not the dialog, so a long list can
        // reach well past the box, and it takes its own field's column; see
        // `choice_dropdown_geom`.) The scroll offset
        // `top` is nudged only when the highlight leaves the window, so the cursor
        // moves freely within it.
        let shown = self.form.visible.clone();
        for (i, field) in self.form.fields.iter_mut().enumerate() {
            if !shown.contains(&i) {
                continue;
            }
            if let Field::Choice { options, sel, top, open: true, .. } = field {
                let frect = rows[i];
                let visible = choice_visible_rows(frect, area, options.len());
                *top = crate::util::scroll::scroll_to_visible(*top, *sel, visible);
                render_choice_dropdown(f, frect, area, options, *sel, *top, theme);
            }
        }

        if let Some(pos) = caret
            && !dropdown_open
            && !choice_open
        {
            f.set_cursor_position(pos);
        }
    }

    /// Recompute the dialog's interior rect (mirrors `render`), for click/scroll
    /// hit-testing of the Choice dropdown.
    fn dialog_inner(&self, area: Rect) -> Rect {
        let rect = self.outer_rect(area);
        Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            width: rect.width.saturating_sub(2),
            height: rect.height.saturating_sub(2),
        }
    }

    /// Route a click when a Choice dropdown is (or should be) involved: click a
    /// closed Choice row to open it; click an option to pick it; click elsewhere
    /// (while open) to close. Returns `Some` if the click was consumed.
    pub(crate) fn click_choice(&mut self, area: Rect, col: u16, row: u16) -> Option<DialogResult> {
        let inner = self.dialog_inner(area);
        let rows = self.field_rows(inner);
        // An open dropdown: pick the clicked option, or close on an outside click.
        if let Some(fi) =
            self.form.fields.iter().position(|f| matches!(f, Field::Choice { open: true, .. }))
        {
            let frect = rows[fi];
            if let Some(Field::Choice { options, idx, open, sel, top, .. }) =
                self.form.fields.get_mut(fi)
            {
                let (rect, visible) = choice_dropdown_geom(frect, area, options.len());
                let (list_x, list_y, list_w) =
                    (rect.x + 1, rect.y + 1, rect.width.saturating_sub(2));
                if row >= list_y
                    && row < list_y + visible as u16
                    && col >= list_x
                    && col < list_x + list_w
                {
                    let chosen = *top + (row - list_y) as usize;
                    if chosen < options.len() {
                        *idx = chosen;
                        *sel = chosen;
                    }
                }
                *open = false;
            }
            return Some(DialogResult::None);
        }
        // No dropdown open: a click on a Choice row opens it.
        let hit = self.form.fields.iter().enumerate().find_map(|(i, f)| {
            let r = rows[i];
            let on_row = row == r.y && col >= r.x && col < r.x + r.width;
            (matches!(f, Field::Choice { .. }) && on_row).then_some(i)
        });
        if let Some(i) = hit {
            if let Field::Choice { idx, open, sel, .. } = &mut self.form.fields[i] {
                *sel = *idx;
                *open = true;
            }
            self.form.focus_at(i);
            return Some(DialogResult::None);
        }
        None
    }

    /// Route a click onto a tabbed form's tab strip: show the clicked tab and put
    /// focus on the strip, so the arrow keys carry on from there. Returns `Some`
    /// when a tab was hit.
    pub(crate) fn click_tab(&mut self, area: Rect, col: u16, row: u16) -> Option<DialogResult> {
        let inner = self.dialog_inner(area);
        let page = self
            .tab_cells(inner)
            .iter()
            .position(|(_, r)| row == r.y && col >= r.x && col < r.x + r.width)?;
        self.show_page(page);
        self.form.focus_at(self.form.strip_slot());
        Some(DialogResult::None)
    }

    /// Route a click onto a Text/Password/Check field row: focus a text field and
    /// place its caret under the pointer, or focus and toggle a checkbox. Returns
    /// `Some` when a field row was hit. Choice rows are left to `click_choice`
    /// (which opens their dropdown), and the OK/Cancel row to the button handler.
    pub(crate) fn click_field(&mut self, area: Rect, col: u16, row: u16) -> Option<DialogResult> {
        let inner = self.dialog_inner(area);
        let rows = self.field_rows(inner);
        let i = rows.iter().position(|r| row == r.y && col >= r.x && col < r.x + r.width)?;
        let r = rows[i];
        match self.form.fields.get_mut(i)? {
            Field::Check { value, .. } => {
                *value = !*value;
                self.form.focus_at(i);
                Some(DialogResult::None)
            }
            Field::Text { label, value, cursor } | Field::Password { label, value, cursor } => {
                // Place the caret under the click, mirroring the label width and
                // horizontal scroll used by `render`/`draw_input_field`.
                let label_str = crate::l10n::display(&format!("{}: ", crate::l10n::tr(label)));
                let lw = (label_str.chars().count() as u16).min(r.width);
                let value_x = r.x + lw;
                let char_count = value.chars().count();
                if col >= value_x {
                    let field_w = r.width.saturating_sub(lw) as usize;
                    let inner_w = field_w.saturating_sub(3); // room for the "[^]" affordance
                    let start = cursor.saturating_sub(inner_w.saturating_sub(1));
                    *cursor = (start + (col - value_x) as usize).min(char_count);
                }
                self.form.focus_at(i);
                Some(DialogResult::None)
            }
            // A Choice row opens via `click_choice`, not here.
            Field::Choice { .. } => None,
        }
    }

    /// Test accessor: `(sel, top)` of the currently open Choice dropdown, if any.
    #[cfg(test)]
    pub(crate) fn open_choice_state(&self) -> Option<(usize, usize)> {
        self.form.fields.iter().find_map(|f| match f {
            Field::Choice { sel, top, open: true, .. } => Some((*sel, *top)),
            _ => None,
        })
    }

    /// Move the open Choice dropdown's highlight (mouse wheel); `delta` in rows.
    pub(crate) fn scroll_choice(&mut self, delta: isize) -> bool {
        if let Some(Field::Choice { options, sel, open: true, .. }) =
            self.form.fields.iter_mut().find(|f| matches!(f, Field::Choice { open: true, .. }))
        {
            let last = options.len().saturating_sub(1) as isize;
            *sel = (*sel as isize + delta).clamp(0, last) as usize;
            return true;
        }
        false
    }

    /// Render the recent-servers list under the Host field and record per-entry
    /// click rects. Scrolls so the selection stays visible.
    fn render_dropdown(&mut self, f: &mut Frame, inner: Rect, theme: &Theme) {
        let Some(c) = self.connect.as_mut() else {
            return;
        };
        if c.history.is_empty() {
            return;
        }
        // The list opens just below the Host row, capped to the dialog interior.
        let top = inner.y + 1;
        let avail = (inner.y + inner.height).saturating_sub(top) as usize;
        let visible = c.history.len().min(avail.saturating_sub(2).max(1));
        let rect = Rect { x: inner.x, y: top, width: inner.width, height: (visible + 2) as u16 };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.dialog_title).bg(theme.dialog_bg))
            .title(Span::styled(
                format!(" {} ", crate::l10n::trd("Recent")),
                Style::default().fg(theme.dialog_title).bg(theme.dialog_bg),
            ))
            .style(Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg));
        let list = block.inner(rect);
        f.render_widget(block, rect);

        // Scroll so the selection is on screen.
        let offset = if c.sel >= visible { c.sel + 1 - visible } else { 0 };
        let normal = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let sel_style = theme.dialog_selection;
        for vi in 0..visible {
            let idx = offset + vi;
            let Some(entry) = c.history.get(idx) else {
                break;
            };
            let row = Rect { x: list.x, y: list.y + vi as u16, width: list.width, height: 1 };
            let style = if idx == c.sel { sel_style } else { normal };
            let label = c.labels.get(idx).cloned().unwrap_or_else(|| entry.label());
            let text = crate::util::text::ellipsize(&label, list.width as usize);
            let text = crate::util::text::pad_right(&text, list.width as usize);
            f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), row);
            c.entries.push((row, idx));
        }
    }
}

/// The hosts `~/.ssh/config` names as connect-form entries — the alias as the
/// host, so a connection follows the config (its HostName, jump hosts, keys) —
/// each with what a list shows for it: `db   dba@db.internal:2222 via bastion
/// (ssh config)`.
pub fn ssh_config_entries(
    cfg: &crate::vfs::remote::sshconfig::SshConfig,
    protocol: Protocol,
) -> Vec<(crate::config::RemoteHistoryEntry, String)> {
    use crate::vfs::remote::sshconfig::expand_tokens;
    cfg.aliases()
        .into_iter()
        .map(|alias| {
            let s = cfg.resolve(&alias);
            let port = s.port.unwrap_or(protocol.default_port());
            let user = s.user.clone().unwrap_or_default();
            let target = s
                .hostname
                .as_deref()
                .map_or_else(|| alias.clone(), |h| expand_tokens(h, &alias, &alias, port, &user));
            let at = if user.is_empty() { String::new() } else { format!("{user}@") };
            let via = s
                .proxy_jump
                .as_deref()
                .filter(|j| !j.eq_ignore_ascii_case("none"))
                .map(|j| format!(" via {j}"))
                .unwrap_or_default();
            let label =
                format!("{alias}   {at}{target}:{port}{via}   ({})", crate::l10n::tr("ssh config"));
            let entry = crate::config::RemoteHistoryEntry {
                protocol: protocol.scheme_prefix().to_string(),
                host: alias,
                port,
                user,
                path: String::new(),
                passive: true,
                key_file: String::new(),
            };
            (entry, label)
        })
        .collect()
}

/// Geometry of a Choice field's dropdown box: its rect and how many option rows
/// are visible. The dropdown normally drops *below* the field, but opens *above*
/// it when there isn't enough room below (so the last fields in a tall dialog
/// still show their list on screen).
///
/// It is sized against the whole `screen`, not the dialog interior, so a long
/// list (say, every branch in a repository) is not squeezed into the few rows a
/// small dialog happens to have — it overlays the dialog's border and whatever is
/// behind it. Horizontally it takes `field`'s own x and width, so in a
/// multi-column group it drops under the column it belongs to rather than under
/// the dialog's left edge.
fn choice_dropdown_geom(field: Rect, screen: Rect, options_len: usize) -> (Rect, usize) {
    let below = (screen.y + screen.height).saturating_sub(field.y + 1) as usize; // rows under the field
    let above = field.y.saturating_sub(screen.y) as usize; // rows over the field
    let want = options_len + 2; // options + top/bottom border
    // Prefer dropping down; flip up only when down can't fit and up has more room.
    let open_up = below < want && above > below;
    let room = if open_up { above } else { below };
    let visible = options_len.min(room.saturating_sub(2).max(1)).max(1);
    let box_h = (visible + 2) as u16;
    let y = if open_up { field.y.saturating_sub(box_h) } else { field.y + 1 };
    (Rect { x: field.x, y, width: field.width, height: box_h }, visible)
}

/// Number of option rows visible in the dropdown of the Choice field at `field`.
fn choice_visible_rows(field: Rect, screen: Rect, options_len: usize) -> usize {
    choice_dropdown_geom(field, screen, options_len).1
}

/// Draw a Choice field's scrollable dropdown just below its row (`field`),
/// showing options from `top` with `sel` highlighted.
fn render_choice_dropdown(
    f: &mut Frame,
    field: Rect,
    screen: Rect,
    options: &[String],
    sel: usize,
    top: usize,
    theme: &Theme,
) {
    if options.is_empty() {
        return;
    }
    let (rect, visible) = choice_dropdown_geom(field, screen, options.len());
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.dialog_title).bg(theme.dialog_bg))
        .style(Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg));
    let list = block.inner(rect);
    f.render_widget(block, rect);

    let normal = Style::default().fg(theme.dialog_fg).bg(theme.dialog_bg);
    for vi in 0..visible {
        let idx = top + vi;
        let Some(opt) = options.get(idx) else {
            break;
        };
        let row = Rect { x: list.x, y: list.y + vi as u16, width: list.width, height: 1 };
        let style = if idx == sel { theme.dialog_selection } else { normal };
        let opt = crate::l10n::display(opt);
        let text = crate::util::text::pad_right(
            &crate::util::text::ellipsize(&opt, list.width as usize),
            list.width as usize,
        );
        f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), row);
    }
}

#[cfg(test)]
mod choice_geom_tests {
    use super::*;

    #[test]
    fn dropdown_extends_past_the_dialog_onto_the_screen() {
        // A short dialog (7 rows) near the top of a tall screen, field on row 3.
        let screen = Rect::new(0, 0, 80, 40);
        let inner = Rect::new(10, 2, 40, 5);
        let field = Rect::new(inner.x, 3, inner.width, 1);
        let (rect, visible) = choice_dropdown_geom(field, screen, 30);
        // All 30 options fit below the field on this screen, even though the
        // dialog interior ends at row 7 — the list is not clipped to the box.
        assert_eq!(visible, 30, "the whole list is shown, not just the dialog's rows");
        assert!(
            rect.y + rect.height > inner.y + inner.height,
            "the dropdown overhangs the dialog interior"
        );
        assert!(rect.y + rect.height <= screen.y + screen.height, "but stays on screen");
        // It stays aligned with its field horizontally.
        assert_eq!((rect.x, rect.width), (field.x, field.width));
    }

    #[test]
    fn dropdown_opens_under_the_column_its_field_is_in() {
        // A field in the right-hand column of a two-column group: the list has
        // to drop under *it*, not under the dialog's left edge.
        let screen = Rect::new(0, 0, 80, 24);
        let field = Rect::new(41, 16, 35, 1);
        let (rect, _) = choice_dropdown_geom(field, screen, 2);
        assert_eq!(
            (rect.x, rect.width),
            (field.x, field.width),
            "the dropdown takes its own field's column"
        );
    }

    #[test]
    fn dropdown_flips_above_the_field_when_below_is_cramped() {
        // Field near the bottom of the screen: more room above than below.
        let screen = Rect::new(0, 0, 80, 24);
        let field = Rect::new(5, 21, 40, 1);
        let (rect, visible) = choice_dropdown_geom(field, screen, 20);
        assert!(rect.y < 21, "opens upward");
        assert!(visible >= 1);
        assert!(rect.y >= screen.y, "stays on screen");
    }

    #[test]
    fn dropdown_is_clamped_to_the_screen_not_the_dialog() {
        // A huge list on a short screen: bounded by the screen's rows.
        let screen = Rect::new(0, 0, 80, 12);
        let field = Rect::new(0, 2, 30, 1);
        let (rect, visible) = choice_dropdown_geom(field, screen, 500);
        assert!(visible < 500, "clamped");
        assert!(rect.y + rect.height <= screen.y + screen.height, "never runs off screen");
        assert!(visible >= 1, "always shows at least one option");
    }
}

#[cfg(test)]
mod settings_tab_tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn settings() -> FormDialog {
        FormDialog::settings(&crate::config::Config::default(), true)
    }

    #[test]
    fn ctrl_page_keys_cycle_the_tabs_and_wrap() {
        let mut d = settings();
        assert_eq!(d.settings_tab(), Some(SettingsTab::Appearance));
        d.handle_key(ctrl(KeyCode::PageDown));
        assert_eq!(d.settings_tab(), Some(SettingsTab::Panels));
        // A focused field hands focus to the new tab's first field.
        assert_eq!(d.form.focus, d.form.visible.start);
        d.handle_key(ctrl(KeyCode::PageUp));
        d.handle_key(ctrl(KeyCode::PageUp));
        assert_eq!(d.settings_tab(), Some(SettingsTab::Terminal), "PgUp wraps to the last tab");
        d.handle_key(ctrl(KeyCode::PageDown));
        assert_eq!(d.settings_tab(), Some(SettingsTab::Appearance), "PgDn wraps to the first");
        // Focus on a button stays on it.
        d.form.focus_at(d.form.ok_slot());
        d.handle_key(ctrl(KeyCode::PageDown));
        assert!(d.form.on_ok(), "switching tabs doesn't move focus off OK");
    }

    #[test]
    fn switching_tabs_closes_an_open_dropdown() {
        let mut d = settings();
        d.handle_key(key(KeyCode::Enter)); // open the Theme dropdown
        assert!(d.open_choice_state().is_some());
        // Ctrl-PgDn is a tab switch, not a page down through the theme list.
        d.handle_key(ctrl(KeyCode::PageDown));
        assert_eq!(d.settings_tab(), Some(SettingsTab::Panels));
        assert!(d.open_choice_state().is_none(), "the hidden field's list is closed");
    }

    #[test]
    fn the_tab_strip_is_in_the_focus_ring() {
        let mut d = settings();
        d.handle_key(key(KeyCode::BackTab));
        assert!(d.form.on_strip(), "Shift-Tab from the first field reaches the strip");
        d.handle_key(key(KeyCode::Right));
        assert_eq!(d.settings_tab(), Some(SettingsTab::Panels));
        d.handle_key(key(KeyCode::Left));
        d.handle_key(key(KeyCode::Left));
        assert_eq!(d.settings_tab(), Some(SettingsTab::Terminal), "Left wraps");
        d.handle_key(key(KeyCode::Home));
        assert_eq!(d.settings_tab(), Some(SettingsTab::Appearance));
        d.handle_key(key(KeyCode::End));
        assert_eq!(d.settings_tab(), Some(SettingsTab::Terminal));
        assert!(d.form.on_strip(), "walking the tabs keeps the focus on the strip");

        d.handle_key(key(KeyCode::Down));
        assert_eq!(d.form.focus, d.form.visible.start, "Down enters the page");
        d.handle_key(key(KeyCode::BackTab));
        d.handle_key(key(KeyCode::BackTab));
        assert!(d.form.on_cancel(), "Shift-Tab from the strip wraps to Cancel");
        d.handle_key(key(KeyCode::Tab));
        assert!(d.form.on_strip(), "Tab from Cancel wraps to the strip");
        assert!(
            matches!(d.handle_key(key(KeyCode::Enter)), DialogResult::Submit(Submit::Settings(_))),
            "Enter on the strip submits, as it does from a field"
        );
    }

    #[test]
    fn focus_never_reaches_a_field_on_another_tab() {
        let mut d = settings().on_tab(SettingsTab::Programs);
        let (visible, ok, cancel, strip) =
            (d.form.visible.clone(), d.form.ok_slot(), d.form.cancel_slot(), d.form.strip_slot());
        for _ in 0..(visible.len() + 3) * 2 {
            d.handle_key(key(KeyCode::Down));
            let f = d.form.focus;
            assert!(visible.contains(&f) || [ok, cancel, strip].contains(&f), "focus left the tab");
        }
    }

    #[test]
    fn fields_on_other_tabs_get_empty_rects() {
        let d = settings().on_tab(SettingsTab::Confirmations);
        let inner = d.dialog_inner(Rect::new(0, 0, 80, 24));
        let rows = d.field_rows(inner);
        for (i, r) in rows.iter().enumerate() {
            let shown = d.form.visible.contains(&i);
            assert_eq!(r.width > 0, shown, "field {i} has a rect only if its tab is showing");
        }
    }

    #[test]
    fn untabbed_forms_keep_their_focus_ring() {
        let mut d = FormDialog::chmod(vec![VfsPath::local("/tmp/x")], 0o644);
        // Ten fields, then OK and Cancel, then back round to the first field.
        for expected in (1..12).chain([0, 1]) {
            d.handle_key(key(KeyCode::Tab));
            assert_eq!(d.form.focus, expected);
        }
        assert!(d.settings_tab().is_none() && d.tab_cells(Rect::new(0, 0, 60, 10)).is_empty());
    }

    #[test]
    fn tab_titles_fit_in_english_and_shorten_evenly_when_narrow() {
        let d = settings();
        let inner = d.dialog_inner(Rect::new(0, 0, 80, 24));
        let cells = d.tab_cells(inner);
        assert_eq!(cells.len(), SETTINGS_PAGES.len());
        for ((text, _), page) in cells.iter().zip(SETTINGS_PAGES) {
            assert_eq!(text.trim(), page.title, "English titles are shown whole");
        }
        // Squeezed into 40 cells, every tab still gets a cell of its own, inside
        // the row and not overlapping its neighbour.
        let narrow = Rect { width: 40, ..inner };
        let cells = d.tab_cells(narrow);
        assert_eq!(cells.len(), SETTINGS_PAGES.len());
        for pair in cells.windows(2) {
            assert_eq!(pair[0].1.x + pair[0].1.width, pair[1].1.x, "cells abut without overlap");
        }
        let last = cells.last().unwrap().1;
        assert!(last.x + last.width <= narrow.x + narrow.width);
        assert!(cells.iter().any(|(t, _)| t.contains('~')), "long titles are ellipsized");
    }

    /// Every Settings field has a description, and every description belongs to
    /// a field — a renamed label would otherwise silently lose its help text.
    #[test]
    fn every_settings_field_has_a_description() {
        let mut d = settings();
        let mut described = 0;
        for page in 0..SETTINGS_PAGES.len() {
            d.show_page(page);
            for field in d.form.visible.clone() {
                d.form.focus_at(field);
                assert!(d.help_text().is_some(), "field {field} on page {page} has no description");
                described += 1;
            }
        }
        assert_eq!(described, d.form.fields.len(), "every field was visited");
        for (label, _) in SETTINGS_HELP {
            assert!(
                d.form.fields.iter().any(|f| f.label() == *label),
                "stale description: {label}"
            );
        }
    }

    #[test]
    fn clicking_a_tab_shows_it_and_focuses_the_strip() {
        let area = Rect::new(0, 0, 80, 24);
        let mut d = settings();
        let inner = d.dialog_inner(area);
        let (_, cell) = d.tab_cells(inner)[3].clone();
        assert!(d.click_tab(area, cell.x + 1, cell.y).is_some());
        assert_eq!(d.settings_tab(), Some(SettingsTab::Confirmations));
        assert!(d.form.on_strip());
        // A click beside the strip is not a tab.
        assert!(d.click_tab(area, cell.x, cell.y + 1).is_none());
    }
}
