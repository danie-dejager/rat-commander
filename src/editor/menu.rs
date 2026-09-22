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
    /// Move to the next JSON syntax error.
    NextError,
    /// Move to the previous JSON syntax error.
    PrevError,

    // -- Command --
    GotoLine,
    /// Jump to the bracket matching the one at the cursor.
    MatchBracket,
    ToggleSyntax,
    ToggleWrap,
    ToggleHex,
    /// Switch between the spreadsheet grid and the text.
    ToggleSheet,
    /// Draw the file's GeoJSON on a map.
    GeoMap,
    /// Decode the JSON Web Token under the cursor.
    DecodeJwt,
    /// Repaint the whole screen (after a stray write from another program).
    RefreshScreen,
    /// The binary-templates submenu's parent item.
    TemplateMenu,
    /// Pick the binary template for the hex view.
    ChooseTemplate,
    /// Run the template again.
    RerunTemplate,
    /// Select the template variable under the byte cursor.
    JumpToVariable,
    /// Open the template in the text editor.
    EditTemplate,
    /// Start a new template for this kind of file.
    NewTemplate,
    /// Stop using a template for this file.
    CloseTemplate,
    /// The JSON submenu's parent item.
    JsonMenu,
    /// Pretty-print the JSON document.
    JsonPretty,
    /// Write the JSON document on one line.
    JsonMinify,
    /// Sort every object's keys.
    JsonSortKeys,
    /// Show or hide the hex editor's data inspector.
    ToggleInspector,

    // -- Format --
    InsertDateTime,
    /// Re-wrap the paragraph around the cursor to the wrap column.
    FormatParagraph,
    /// Sort the marked block's lines (options collected in a dialog).
    SortBlock,
    /// Run a shell command and insert its output at the cursor.
    PasteOutput,
    /// Spreadsheet grid: an empty record above the cursor's.
    SheetInsertRow,
    /// Spreadsheet grid: remove the cursor's record.
    SheetDeleteRow,
    /// Spreadsheet grid: an empty column left of the cursor's.
    SheetInsertCol,
    /// Spreadsheet grid: remove the cursor's column.
    SheetDeleteCol,
    /// Spreadsheet grid: whether the first record is the header row.
    SheetHeader,

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
fn item_sub(label: &str, shortcut: &'static str, action: EditorAction, sub: Vec<Item>) -> Item {
    pulldown::item_sub(label, shortcut, action, sub)
}

/// The menu-bar titles, in order. Like the file manager's bar, a title's
/// accelerator is its first letter; "File" and "Format" share one, so typing
/// `f` repeatedly steps between them.
pub const TITLES: [&str; 6] = ["File", "Edit", "Search", "Command", "Format", "Options"];

/// The titles in the active language.
pub fn titles() -> [String; 6] {
    TITLES.map(crate::l10n::tr)
}

/// What the editor is showing, which decides the menu items that can act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuMode {
    Text,
    Hex,
    /// The spreadsheet grid over a CSV or TSV file.
    Sheet,
    /// The tag view over an audio file.
    Tags,
}

/// Build the editor's menu bar, opened on menu `active` (0 = File).
///
/// `mode` is what the editor is showing: hex mode supports only a fraction of
/// these actions and the spreadsheet grid has no use for the ones that act on
/// lines and marked blocks, so those are greyed out rather than silently doing
/// nothing — and the grid's own row and column actions are greyed out
/// everywhere else. `checked` is whether the file's syntax is checked (JSON,
/// TOML, YAML, XML), which is what gives the error items something to move
/// between; `template` whether hex mode is showing a binary template's
/// variables; `json` whether the file is JSON, for the JSON tools.
pub fn editor_menu(
    active: usize,
    mode: MenuMode,
    checked: bool,
    template: bool,
    json: bool,
) -> EditorMenu {
    let hex = mode == MenuMode::Hex;
    // In hex mode the text buffer isn't the thing being edited, so everything
    // that reads or writes it is unavailable.
    let text_only = |i: Item| i.disabled(hex);
    // Actions on lines, marks and the typing position, which the grid replaces
    // with cells.
    let not_grid = |i: Item| i.disabled(mode != MenuMode::Text);
    let grid_only = |i: Item| i.disabled(mode != MenuMode::Sheet);

    let file = Menu {
        items: vec![
            item("&Open file...", EditorAction::OpenFile),
            item_key("&New", "Ctrl-N", EditorAction::NewFile),
            sep(),
            item_key("&Save", "F2", EditorAction::Save),
            text_only(item_key("Save &as...", "Shift-F2", EditorAction::SaveAs)),
            sep(),
            not_grid(item_key("&Insert file...", "Shift-F5", EditorAction::InsertFile)),
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
            not_grid(item_key("Toggle &ins/overwrite", "Ins", EditorAction::ToggleInsert)),
            sep(),
            not_grid(item_key("Toggle mar&k", "F3", EditorAction::ToggleMark)),
            not_grid(item_key("Mark &all", "Ctrl-A", EditorAction::MarkAll)),
            not_grid(item("U&nmark", EditorAction::Unmark)),
            sep(),
            not_grid(item_key("&Copy", "F5", EditorAction::CopyBlock)),
            not_grid(item_key("&Move", "F6", EditorAction::MoveBlock)),
            not_grid(item_key("&Delete", "F8", EditorAction::DeleteBlock)),
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
            not_grid(item_key("&Toggle bookmark", "Alt-K", EditorAction::BookmarkToggle)),
            not_grid(item_key("&Next bookmark", "Alt-J", EditorAction::BookmarkNext)),
            not_grid(item_key("&Prev bookmark", "Alt-I", EditorAction::BookmarkPrev)),
            not_grid(item_key("&Flush bookmarks", "Alt-O", EditorAction::BookmarkFlush)),
            sep(),
            item_key("Next &error", "Alt-E", EditorAction::NextError).disabled(!checked),
            item_key("Pre&vious error", "Alt-Shift-E", EditorAction::PrevError).disabled(!checked),
        ],
    };

    let command = Menu {
        items: vec![
            text_only(item_key("&Go to line...", "Alt-L", EditorAction::GotoLine)),
            not_grid(item_key("Go to matching &bracket", "Alt-B", EditorAction::MatchBracket)),
            sep(),
            text_only(item_key(
                "Toggle &syntax highlighting",
                "Ctrl-S",
                EditorAction::ToggleSyntax,
            )),
            not_grid(item_key("Toggle &word wrap", "Shift-F9", EditorAction::ToggleWrap)),
            item_key("Toggle &hex editor", "Ctrl-F9", EditorAction::ToggleHex),
            text_only(item_key("Toggle sprea&dsheet", "Alt-G", EditorAction::ToggleSheet)),
            sep(),
            text_only(item_key("Show GeoJSON &map...", "Alt-M", EditorAction::GeoMap)),
            text_only(item("D&ecode JWT at cursor", EditorAction::DecodeJwt)),
            item_sub(
                "Binary &templates",
                "▶",
                EditorAction::TemplateMenu,
                template_items(template),
            )
            .disabled(!hex),
            item_key("Data &inspector", "F8", EditorAction::ToggleInspector).disabled(!hex),
            sep(),
            item_key("&Refresh screen", "Ctrl-L", EditorAction::RefreshScreen),
        ],
    };

    let format = Menu {
        items: vec![
            not_grid(item("Insert &date/time", EditorAction::InsertDateTime)),
            sep(),
            not_grid(item_key("&Format paragraph", "Alt-P", EditorAction::FormatParagraph)),
            not_grid(item_key("&Sort...", "Alt-T", EditorAction::SortBlock)),
            not_grid(item_key("&Paste output of...", "Alt-U", EditorAction::PasteOutput)),
            item_sub("&JSON", "▶", EditorAction::JsonMenu, json_items())
                .disabled(!json || mode != MenuMode::Text),
            sep(),
            grid_only(item_key("Insert &row", "F5", EditorAction::SheetInsertRow)),
            grid_only(item_key("Delete ro&w", "F8", EditorAction::SheetDeleteRow)),
            grid_only(item_key("Insert &column", "F6", EditorAction::SheetInsertCol)),
            grid_only(item_key("Delete co&lumn", "Shift-F8", EditorAction::SheetDeleteCol)),
            grid_only(item_key("First row is &header", "F3", EditorAction::SheetHeader)),
        ],
    };

    let options = Menu {
        items: vec![
            item("&General...", EditorAction::Options),
            sep(),
            item("&Save setup", EditorAction::SaveSetup),
        ],
    };

    EditorMenu::build(titles().to_vec(), vec![file, edit, search, command, format, options], active)
}

/// The binary-templates submenu (Command → Binary templates, hex mode only).
/// [`TEMPLATE_MENU_KEYS`] mirrors its label keys.
fn template_items(template: bool) -> Vec<Item> {
    let shown = |i: Item| i.disabled(!template);
    vec![
        item_key("&Choose template...", "F5", EditorAction::ChooseTemplate),
        item_key("&Run again", "Shift-F5", EditorAction::RerunTemplate),
        shown(item_key("&Jump to variable", "F6", EditorAction::JumpToVariable)),
        sep(),
        shown(item("&Edit template...", EditorAction::EditTemplate)),
        item("&New template...", EditorAction::NewTemplate),
        sep(),
        shown(item("C&lose template", EditorAction::CloseTemplate)),
    ]
}

/// The JSON submenu (Format → JSON). [`JSON_MENU_KEYS`] mirrors its label keys.
fn json_items() -> Vec<Item> {
    vec![
        item_key("&Pretty-print", "Alt-F", EditorAction::JsonPretty),
        item("&Minify", EditorAction::JsonMinify),
        item("&Sort keys", EditorAction::JsonSortKeys),
    ]
}

/// The JSON submenu's label keys, for the l10n accelerator test.
#[allow(dead_code)] // read by the l10n and editor-menu tests
pub const JSON_MENU_KEYS: &[&str] = &["&Pretty-print", "&Minify", "&Sort keys"];

/// The binary-templates submenu's label keys, for the l10n accelerator test.
#[allow(dead_code)] // read by the l10n and editor-menu tests
pub const TEMPLATE_MENU_KEYS: &[&str] = &[
    "&Choose template...",
    "&Run again",
    "&Jump to variable",
    "&Edit template...",
    "&New template...",
    "C&lose template",
];

/// Every menu's label keys, in bar order — the single source shared by the menu
/// itself and the l10n accelerator test, so the two can't drift apart.
#[allow(dead_code)] // read by the l10n and editor-menu tests
pub const MENU_KEYS: &[&[&str]] = &[
    &[
        "&Open file...",
        "&New",
        "&Save",
        "Save &as...",
        "&Insert file...",
        "&Copy to file...",
        "A&bout...",
        "&Quit",
    ],
    &[
        "&Undo",
        "&Redo",
        "Toggle &ins/overwrite",
        "Toggle mar&k",
        "Mark &all",
        "U&nmark",
        "&Copy",
        "&Move",
        "&Delete",
        "Copy to clip&board",
        "Cu&t to clipboard",
        "&Paste from clipboard",
        "Be&ginning",
        "&End",
    ],
    &[
        "&Search...",
        "Search a&gain",
        "&Replace...",
        "&Toggle bookmark",
        "&Next bookmark",
        "&Prev bookmark",
        "&Flush bookmarks",
        "Next &error",
        "Pre&vious error",
    ],
    &[
        "&Go to line...",
        "Go to matching &bracket",
        "Toggle &syntax highlighting",
        "Toggle &word wrap",
        "Toggle &hex editor",
        "Toggle sprea&dsheet",
        "Show GeoJSON &map...",
        "D&ecode JWT at cursor",
        "Binary &templates",
        "Data &inspector",
        "&Refresh screen",
    ],
    &[
        "Insert &date/time",
        "&Format paragraph",
        "&Sort...",
        "&Paste output of...",
        "&JSON",
        "Insert &row",
        "Delete ro&w",
        "Insert &column",
        "Delete co&lumn",
        "First row is &header",
    ],
    &["&General...", "&Save setup"],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators_are_unique_within_each_menu() {
        let m = editor_menu(0, MenuMode::Text, false, false, false);
        for (mi, menu) in m.menus().iter().enumerate() {
            let mut seen = Vec::new();
            for it in &menu.items {
                if let Some(hk) = it.hotkey() {
                    assert!(
                        !seen.contains(&hk),
                        "duplicate accelerator {hk:?} in editor menu {mi}"
                    );
                    seen.push(hk);
                }
            }
        }
    }

    #[test]
    fn menu_keys_mirror_the_built_menus() {
        // The l10n accelerator test reads MENU_KEYS; if a menu gains an item and
        // the list isn't updated, that test would quietly stop covering it.
        let m = editor_menu(0, MenuMode::Text, false, false, false);
        assert_eq!(m.menus().len(), MENU_KEYS.len());
        for (mi, menu) in m.menus().iter().enumerate() {
            let built = menu.items.iter().filter(|it| !it.action.is_separator()).count();
            assert_eq!(built, MENU_KEYS[mi].len(), "editor menu {mi} and MENU_KEYS disagree");
        }
    }

    #[test]
    fn hex_mode_greys_out_the_text_only_actions() {
        let m = editor_menu(0, MenuMode::Hex, false, false, false);
        let find = |a: EditorAction| find_item(&m, a);
        assert!(!find(EditorAction::Undo).selectable(), "undo needs the text buffer");
        assert!(!find(EditorAction::FormatParagraph).selectable());
        // Saving, quitting and switching back to text mode still work in hex mode.
        assert!(find(EditorAction::Save).selectable());
        assert!(find(EditorAction::Quit).selectable());
        assert!(find(EditorAction::ToggleHex).selectable());
        assert!(!find(EditorAction::SheetInsertRow).selectable());
    }

    #[test]
    fn the_json_submenu_is_for_json_text_and_mirrors_its_keys() {
        let plain = editor_menu(0, MenuMode::Text, false, false, false);
        assert!(!find_item(&plain, EditorAction::JsonMenu).selectable());
        let json = editor_menu(0, MenuMode::Text, true, false, true);
        let parent = find_item(&json, EditorAction::JsonMenu);
        assert!(parent.selectable());
        assert_eq!(parent.submenu.len(), JSON_MENU_KEYS.len());
        let hex = editor_menu(0, MenuMode::Hex, false, false, true);
        assert!(!find_item(&hex, EditorAction::JsonMenu).selectable());
    }

    #[test]
    fn the_template_submenu_is_for_hex_mode_and_mirrors_its_keys() {
        let text = editor_menu(0, MenuMode::Text, false, false, false);
        let hex = editor_menu(0, MenuMode::Hex, false, true, false);
        assert!(!find_item(&text, EditorAction::TemplateMenu).selectable());
        let parent = find_item(&hex, EditorAction::TemplateMenu);
        assert!(parent.selectable());
        let leaves: Vec<_> = parent.submenu.iter().filter(|i| !i.action.is_separator()).collect();
        assert_eq!(leaves.len(), TEMPLATE_MENU_KEYS.len());
        let mut seen = Vec::new();
        for it in &parent.submenu {
            if let Some(hk) = it.hotkey() {
                assert!(!seen.contains(&hk), "duplicate accelerator {hk:?} in the template menu");
                seen.push(hk);
            }
        }
        // Without a template running, only choosing, rerunning and making one work.
        let bare = editor_menu(0, MenuMode::Hex, false, false, false);
        let sub = &find_item(&bare, EditorAction::TemplateMenu).submenu;
        let usable = |a: EditorAction| sub.iter().find(|i| i.action == a).unwrap().selectable();
        assert!(usable(EditorAction::ChooseTemplate) && usable(EditorAction::NewTemplate));
        assert!(!usable(EditorAction::EditTemplate) && !usable(EditorAction::CloseTemplate));
    }

    #[test]
    fn the_error_items_need_a_json_file() {
        let plain = editor_menu(0, MenuMode::Text, false, false, false);
        let json = editor_menu(0, MenuMode::Text, true, false, false);
        assert!(!find_item(&plain, EditorAction::NextError).selectable());
        assert!(find_item(&json, EditorAction::NextError).selectable());
        assert!(find_item(&json, EditorAction::PrevError).selectable());
    }

    fn find_item(m: &EditorMenu, a: EditorAction) -> &MenuItem<EditorAction> {
        m.menus()
            .iter()
            .flat_map(|menu| menu.items.iter())
            .find(|it| it.action == a)
            .unwrap_or_else(|| panic!("{a:?} is in the menu"))
    }

    #[test]
    fn the_grid_greys_out_line_actions_and_offers_its_own() {
        let grid = editor_menu(0, MenuMode::Sheet, false, false, false);
        let text = editor_menu(0, MenuMode::Text, false, false, false);
        for a in
            [EditorAction::SheetInsertRow, EditorAction::SheetDeleteCol, EditorAction::SheetHeader]
        {
            assert!(find_item(&grid, a).selectable(), "{a:?} works in the grid");
            assert!(!find_item(&text, a).selectable(), "{a:?} has no rows to act on in the text");
        }
        for a in [EditorAction::ToggleMark, EditorAction::FormatParagraph, EditorAction::ToggleWrap]
        {
            assert!(!find_item(&grid, a).selectable(), "{a:?} acts on lines");
        }
        for a in [
            EditorAction::Undo,
            EditorAction::Save,
            EditorAction::ToggleSheet,
            EditorAction::Search,
        ] {
            assert!(find_item(&grid, a).selectable(), "{a:?} still works in the grid");
        }
    }
}
