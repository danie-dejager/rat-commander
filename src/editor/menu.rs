//! The editor's F9 pulldown menu (mcedit's layout, minus the entries this
//! editor has no equivalent for).
//!
//! Only actions the editor can actually carry out are listed — mcedit items
//! that would need a tags database (find declaration), a macro recorder, a
//! spell checker, an encoding converter or a window manager are simply absent
//! rather than present-but-dead.

use crate::ui::pulldown::{self, Action, Menu, MenuItem, PulldownState};

/// An action one of the editor's menu items triggers. Most are carried out by
/// the editor itself; the handful that need a dialog or the file manager's help
/// leave through an [`EditorSignal`](super::EditorSignal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    Separator,

    // -- File --
    /// Open another file in the editor (browser).
    OpenFile,
    /// Start a fresh, unnamed buffer.
    NewFile,
    Save,
    SaveAs,
    /// Insert a file's contents at the cursor (browser).
    InsertFile,
    /// Write the marked block (or the whole buffer) to a file (browser).
    CopyToFile,
    About,
    Quit,

    // -- Edit --
    Undo,
    Redo,
    /// Switch between insert and overwrite typing.
    ToggleInsert,
    /// Start / end a block mark (F3).
    ToggleMark,
    /// Mark the whole buffer.
    MarkAll,
    /// Drop the current mark.
    Unmark,
    /// Copy the marked block to the cursor (F5).
    CopyBlock,
    /// Move the marked block to the cursor (F6).
    MoveBlock,
    /// Delete the marked block (F8).
    DeleteBlock,
    ClipCopy,
    ClipCut,
    ClipPaste,
    /// Jump to the start of the buffer.
    DocStart,
    /// Jump to the end of the buffer.
    DocEnd,

    // -- Search --
    Search,
    /// Repeat the last search (F7's remembered pattern).
    SearchAgain,
    Replace,
    BookmarkToggle,
    BookmarkNext,
    BookmarkPrev,
    BookmarkFlush,

    // -- Command --
    GotoLine,
    /// Jump to the bracket matching the one at the cursor.
    MatchBracket,
    ToggleSyntax,
    ToggleWrap,
    ToggleHex,
    /// Repaint the whole screen (after a stray write from another program).
    RefreshScreen,

    // -- Format --
    InsertDateTime,
    /// Re-wrap the paragraph around the cursor to the wrap column.
    FormatParagraph,
    /// Sort the marked block's lines (options collected in a dialog).
    SortBlock,
    /// Run a shell command and insert its output at the cursor.
    PasteOutput,

    // -- Options --
    /// The editor options dialog (mcedit's Options → General).
    Options,
    /// Write the current editor options to the config file.
    SaveSetup,
}

impl Action for EditorAction {
    fn separator() -> Self {
        EditorAction::Separator
    }

    fn is_separator(self) -> bool {
        matches!(self, EditorAction::Separator)
    }
}

/// The editor's menu bar.
pub type EditorMenu = PulldownState<EditorAction>;

type Item = MenuItem<EditorAction>;

fn item(label: &str, action: EditorAction) -> Item {
    pulldown::item(label, action)
}
fn item_key(label: &str, shortcut: &'static str, action: EditorAction) -> Item {
    pulldown::item_key(label, shortcut, action)
}
fn sep() -> Item {
    pulldown::sep()
}

/// The menu-bar titles, in order. Like the file manager's bar, a title's
/// accelerator is its first letter; "File" and "Format" share one, so typing
/// `f` repeatedly steps between them.
pub const TITLES: [&str; 6] = ["File", "Edit", "Search", "Command", "Format", "Options"];

/// The titles in the active language.
pub fn titles() -> [String; 6] {
    TITLES.map(crate::l10n::tr)
}

/// Build the editor's menu bar, opened on menu `active` (0 = File).
///
/// `hex` is whether the editor is in hex mode: that mode supports only a
/// fraction of these actions, so the rest are greyed out rather than silently
/// doing nothing.
pub fn editor_menu(active: usize, hex: bool) -> EditorMenu {
    // In hex mode the text buffer isn't the thing being edited, so everything
    // that reads or writes it is unavailable.
    let text_only = |i: Item| i.disabled(hex);

    let file = Menu {
        items: vec![
            item("&Open file...", EditorAction::OpenFile),
            item_key("&New", "Ctrl-N", EditorAction::NewFile),
            sep(),
            item_key("&Save", "F2", EditorAction::Save),
            text_only(item_key("Save &as...", "Shift-F2", EditorAction::SaveAs)),
            sep(),
            text_only(item_key("&Insert file...", "Shift-F5", EditorAction::InsertFile)),
            text_only(item_key("&Copy to file...", "Ctrl-F", EditorAction::CopyToFile)),
            sep(),
            item("A&bout...", EditorAction::About),
            sep(),
            item_key("&Quit", "F10", EditorAction::Quit),
        ],
    };

    let edit = Menu {
        items: vec![
            text_only(item_key("&Undo", "Ctrl-Z", EditorAction::Undo)),
            text_only(item_key("&Redo", "Ctrl-Y", EditorAction::Redo)),
            sep(),
            text_only(item_key("Toggle &ins/overwrite", "Ins", EditorAction::ToggleInsert)),
            sep(),
            text_only(item_key("Toggle mar&k", "F3", EditorAction::ToggleMark)),
            text_only(item_key("Mark &all", "Ctrl-A", EditorAction::MarkAll)),
            text_only(item("U&nmark", EditorAction::Unmark)),
            sep(),
            text_only(item_key("&Copy", "F5", EditorAction::CopyBlock)),
            text_only(item_key("&Move", "F6", EditorAction::MoveBlock)),
            text_only(item_key("&Delete", "F8", EditorAction::DeleteBlock)),
            sep(),
            text_only(item_key("Copy to clip&board", "Ctrl-C", EditorAction::ClipCopy)),
            text_only(item_key("Cu&t to clipboard", "Ctrl-X", EditorAction::ClipCut)),
            text_only(item_key("&Paste from clipboard", "Ctrl-V", EditorAction::ClipPaste)),
            sep(),
            item_key("Be&ginning", "Ctrl-Home", EditorAction::DocStart),
            item_key("&End", "Ctrl-End", EditorAction::DocEnd),
        ],
    };

    let search = Menu {
        items: vec![
            item_key("&Search...", "F7", EditorAction::Search),
            item_key("Search a&gain", "Shift-F7", EditorAction::SearchAgain),
            item_key("&Replace...", "F4", EditorAction::Replace),
            sep(),
            text_only(item_key("&Toggle bookmark", "Alt-K", EditorAction::BookmarkToggle)),
            text_only(item_key("&Next bookmark", "Alt-J", EditorAction::BookmarkNext)),
            text_only(item_key("&Prev bookmark", "Alt-I", EditorAction::BookmarkPrev)),
            text_only(item_key("&Flush bookmarks", "Alt-O", EditorAction::BookmarkFlush)),
        ],
    };

    let command = Menu {
        items: vec![
            text_only(item_key("&Go to line...", "Alt-L", EditorAction::GotoLine)),
            text_only(item_key("Go to matching &bracket", "Alt-B", EditorAction::MatchBracket)),
            sep(),
            text_only(item_key("Toggle &syntax highlighting", "Ctrl-S", EditorAction::ToggleSyntax)),
            text_only(item_key("Toggle &word wrap", "Shift-F9", EditorAction::ToggleWrap)),
            item_key("Toggle &hex editor", "Ctrl-F9", EditorAction::ToggleHex),
            sep(),
            item_key("&Refresh screen", "Ctrl-L", EditorAction::RefreshScreen),
        ],
    };

    let format = Menu {
        items: vec![
            text_only(item("Insert &date/time", EditorAction::InsertDateTime)),
            sep(),
            text_only(item_key("&Format paragraph", "Alt-P", EditorAction::FormatParagraph)),
            text_only(item_key("&Sort...", "Alt-T", EditorAction::SortBlock)),
            text_only(item_key("&Paste output of...", "Alt-U", EditorAction::PasteOutput)),
        ],
    };

    let options = Menu {
        items: vec![
            item("&General...", EditorAction::Options),
            sep(),
            item("&Save setup", EditorAction::SaveSetup),
        ],
    };

    EditorMenu::build(
        titles().to_vec(),
        vec![file, edit, search, command, format, options],
        active,
    )
}

/// Every menu's label keys, in bar order — the single source shared by the menu
/// itself and the l10n accelerator test, so the two can't drift apart.
#[allow(dead_code)] // read by the l10n and editor-menu tests
pub const MENU_KEYS: &[&[&str]] = &[
    &[
        "&Open file...", "&New", "&Save", "Save &as...", "&Insert file...",
        "&Copy to file...", "A&bout...", "&Quit",
    ],
    &[
        "&Undo", "&Redo", "Toggle &ins/overwrite", "Toggle mar&k", "Mark &all", "U&nmark",
        "&Copy", "&Move", "&Delete", "Copy to clip&board", "Cu&t to clipboard",
        "&Paste from clipboard", "Be&ginning", "&End",
    ],
    &[
        "&Search...", "Search a&gain", "&Replace...", "&Toggle bookmark", "&Next bookmark",
        "&Prev bookmark", "&Flush bookmarks",
    ],
    &[
        "&Go to line...", "Go to matching &bracket", "Toggle &syntax highlighting",
        "Toggle &word wrap", "Toggle &hex editor", "&Refresh screen",
    ],
    &["Insert &date/time", "&Format paragraph", "&Sort...", "&Paste output of..."],
    &["&General...", "&Save setup"],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators_are_unique_within_each_menu() {
        let m = editor_menu(0, false);
        for (mi, menu) in m.menus().iter().enumerate() {
            let mut seen = Vec::new();
            for it in &menu.items {
                if let Some(hk) = it.hotkey() {
                    assert!(!seen.contains(&hk), "duplicate accelerator {hk:?} in editor menu {mi}");
                    seen.push(hk);
                }
            }
        }
    }

    #[test]
    fn menu_keys_mirror_the_built_menus() {
        // The l10n accelerator test reads MENU_KEYS; if a menu gains an item and
        // the list isn't updated, that test would quietly stop covering it.
        let m = editor_menu(0, false);
        assert_eq!(m.menus().len(), MENU_KEYS.len());
        for (mi, menu) in m.menus().iter().enumerate() {
            let built = menu.items.iter().filter(|it| !it.action.is_separator()).count();
            assert_eq!(built, MENU_KEYS[mi].len(), "editor menu {mi} and MENU_KEYS disagree");
        }
    }

    #[test]
    fn hex_mode_greys_out_the_text_only_actions() {
        let m = editor_menu(0, true);
        let find = |a: EditorAction| {
            m.menus()
                .iter()
                .flat_map(|menu| menu.items.iter())
                .find(|it| it.action == a)
                .unwrap_or_else(|| panic!("{a:?} is in the menu"))
        };
        assert!(!find(EditorAction::Undo).selectable(), "undo needs the text buffer");
        assert!(!find(EditorAction::FormatParagraph).selectable());
        // Saving, quitting and switching back to text mode still work in hex mode.
        assert!(find(EditorAction::Save).selectable());
        assert!(find(EditorAction::Quit).selectable());
        assert!(find(EditorAction::ToggleHex).selectable());
    }
}
