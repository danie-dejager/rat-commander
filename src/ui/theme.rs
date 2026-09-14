//! Color themes.
//!
//! A [`Palette`] is a classic 16-ANSI-color terminal scheme (plus bg/fg). The
//! [`Theme`] is built from a palette via [`Theme::from_palette`], mapping the
//! palette onto every UI element. A curated set of well-known schemes from
//! terminalcolors.com is provided in [`PALETTES`]; more can be added by
//! appending palette literals.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{LazyLock, RwLock};

const fn rgb(h: u32) -> Color {
    Color::Rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
}

/// The signature Rat/Midnight Commander teal used for the selection bar and
/// menu / function-key bars (matching the real program).
#[allow(dead_code)] // referenced by theme tests
const MC_TEAL: Color = rgb(0x00a3a3);

/// A 16-color terminal palette plus background/foreground.
#[derive(Clone, Copy)]
// The full 16-color ANSI model; the current styles don't read every slot.
#[allow(dead_code)]
pub struct Palette {
    pub name: &'static str,
    pub bg: Color,
    pub fg: Color,
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
}

// ---------------------------------------------------------------------------
// Per-element gradients
// ---------------------------------------------------------------------------

/// The axis a gradient runs along inside the element it paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GradientDir {
    /// Left edge to right edge.
    #[default]
    Horizontal,
    /// Top edge to bottom edge.
    Vertical,
    /// Top-left corner to bottom-right corner.
    Diagonal,
    /// Center outwards to the corners.
    Radial,
}

impl GradientDir {
    pub const ALL: [GradientDir; 4] =
        [Self::Horizontal, Self::Vertical, Self::Diagonal, Self::Radial];

    /// A one-character marker for the theme editor's item list. Kept to arrows
    /// and a circle from the common blocks, so a plain terminal font has them.
    pub fn glyph(self) -> char {
        match self {
            Self::Horizontal => '↔',
            Self::Vertical => '↕',
            Self::Diagonal => '↘',
            Self::Radial => '◎',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "Horizontal",
            Self::Vertical => "Vertical",
            Self::Diagonal => "Diagonal",
            Self::Radial => "Radial",
        }
    }

    /// The next direction, for cycling through them in the theme editor.
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|d| *d == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// Where the cell at `(x, y)` sits along the gradient, in `[0, 1]`. The
    /// coordinates are absolute; `r` is the region the ramp spans.
    pub fn t(self, x: u16, y: u16, r: Rect) -> f64 {
        let fx = frac(x.saturating_sub(r.x), r.width);
        let fy = frac(y.saturating_sub(r.y), r.height);
        match self {
            Self::Horizontal => fx,
            Self::Vertical => fy,
            Self::Diagonal => (fx + fy) / 2.0,
            Self::Radial => {
                // Normalized offset from the center, so the ramp fills whatever
                // shape the region has (a wide box gets a wide ellipse).
                let (dx, dy) = ((fx - 0.5) * 2.0, (fy - 0.5) * 2.0);
                ((dx * dx + dy * dy).sqrt() / std::f64::consts::SQRT_2).clamp(0.0, 1.0)
            }
        }
    }
}

/// `d`'s position within a `span`-wide axis, in `[0, 1]`.
fn frac(d: u16, span: u16) -> f64 {
    if span <= 1 { 0.0 } else { (d as f64 / (span - 1) as f64).clamp(0.0, 1.0) }
}

/// A gradient attached to one UI element in `themes.toml`:
///
/// ```toml
/// [theme.gradients.panel_bg]
/// to = "#001a80"
/// direction = "vertical"
/// animated = false
/// ```
///
/// `from` defaults to the element's own color (`panel_bg` above), so a gradient
/// usually only has to name its second endpoint. `animated` is off by default —
/// a moving background is distracting, while a moving cursor or bar is not — and
/// is additionally gated on the global animation setting.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GradientSpec {
    /// First endpoint; the element's own flat color when absent.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_hex_color")]
    pub from: Option<Color>,
    /// Second endpoint.
    #[serde(with = "hex_color")]
    pub to: Color,
    #[serde(default)]
    pub direction: GradientDir,
    #[serde(default)]
    pub animated: bool,
}

impl GradientSpec {
    /// A still, horizontal ramp from the element's own color to `to`.
    pub fn new(to: Color) -> Self {
        GradientSpec { from: None, to, direction: GradientDir::Horizontal, animated: false }
    }

    /// The ramp the theme editor switches an element on with: towards white on a
    /// dark color, towards black on a light one, so the gradient is visible the
    /// moment it is enabled and can then be tuned.
    pub fn default_for(base: Color) -> Self {
        let (r, g, b) = to_rgb(base);
        let luma = 0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64;
        let target = if luma > 140.0 { rgb(0x000000) } else { rgb(0xffffff) };
        Self::new(mix(base, target, 0.35))
    }
}

/// Whether a gradient repaints a cell's background or its foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradPaint {
    Bg,
    Fg,
}

/// The part of the screen an element owns. The bars are the only chrome drawn
/// over a full row of their own, so keeping them in their own zone stops a body
/// gradient from bleeding into a bar that happens to share its color (the stock
/// themes give the cursor, the menu bar and the F-key bar one and the same
/// teal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradZone {
    Body,
    Menubar,
    Fkeys,
}

/// Generate the gradient-carrying elements from one ordered list: the role enum,
/// the serialized `[theme.gradients.*]` table, and the lookup from a role to the
/// flat color it ramps from. Roles are listed most-specific first — that is the
/// order [`crate::ui::gradient`] resolves a cell's color in.
macro_rules! gradient_roles {
    ( $( $variant:ident, $field:ident, $paint:ident, $zone:ident ; )* ) => {
        /// A UI element that can carry its own gradient.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum GradRole { $( $variant, )* }

        /// How many elements can carry a gradient.
        pub const GRAD_ROLES: usize = [ $( stringify!($variant), )* ].len();

        impl GradRole {
            /// Every role, most-specific first.
            pub const ALL: [GradRole; GRAD_ROLES] = [ $( GradRole::$variant, )* ];

            /// This role's slot in a theme's resolved gradient table.
            pub fn index(self) -> usize {
                self as usize
            }

            pub fn paint(self) -> GradPaint {
                match self { $( GradRole::$variant => GradPaint::$paint, )* }
            }

            pub fn zone(self) -> GradZone {
                match self { $( GradRole::$variant => GradZone::$zone, )* }
            }
        }

        /// A theme's per-element gradients. Every element is optional; without
        /// one it keeps painting its flat color.
        #[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
        pub struct Gradients {
            $( #[serde(default, skip_serializing_if = "Option::is_none")]
               pub $field: Option<GradientSpec>, )*
        }

        impl Gradients {
            pub fn get(&self, role: GradRole) -> Option<&GradientSpec> {
                match role { $( GradRole::$variant => self.$field.as_ref(), )* }
            }

            /// The (possibly absent) gradient of `role`, for switching it on or off.
            pub fn slot(&mut self, role: GradRole) -> &mut Option<GradientSpec> {
                match role { $( GradRole::$variant => &mut self.$field, )* }
            }

            /// Whether the theme defines no gradients at all.
            pub fn is_empty(&self) -> bool {
                $( self.$field.is_none() && )* true
            }
        }

        impl ThemeSpec {
            /// The flat color `role` paints without a gradient — the ramp's
            /// default first endpoint, and what the screen repaint matches
            /// already-drawn cells against.
            pub fn gradient_base(&self, role: GradRole) -> Color {
                match role { $( GradRole::$variant => self.$field, )* }
            }
        }
    };
}

gradient_roles! {
    ButtonFocusedBg,   button_focused_bg,   Bg, Body;
    ButtonBg,          button_bg,           Bg, Body;
    InputBg,           input_bg,            Bg, Body;
    CursorBg,          cursor_bg,           Bg, Body;
    CursorInactiveBg,  cursor_inactive_bg,  Bg, Body;
    DialogSelectionBg, dialog_selection_bg, Bg, Body;
    MenuSelectionBg,   menu_selection_bg,   Bg, Body;
    MenuBg,            menu_bg,             Bg, Body;
    DialogBg,          dialog_bg,           Bg, Body;
    PanelBg,           panel_bg,            Bg, Body;
    MenubarBg,         menubar_bg,          Bg, Menubar;
    FkeyLabelBg,       fkey_label_bg,       Bg, Fkeys;
    PanelBorderActive, panel_border_active, Fg, Body;
    PanelBorder,       panel_border,        Fg, Body;
    DialogBorderFg,    dialog_border_fg,    Fg, Body;
}

/// A theme's gradient for one element, resolved to RGB endpoints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grad {
    /// The flat color the element has without the gradient.
    pub base: Color,
    pub from: (u8, u8, u8),
    pub to: (u8, u8, u8),
    pub dir: GradientDir,
    pub animated: bool,
}

// ---------------------------------------------------------------------------
// User-editable themes (themes.toml)
// ---------------------------------------------------------------------------

/// A theme stored in `themes.toml`: an explicit color for every UI element
/// (background/foreground pairs where applicable). This is the form edited by
/// the user and used at runtime — colors map straight onto the [`Theme`] with no
/// hue mixing. The built-in [`PALETTES`] seed it (their well-known schemes are
/// derived once into these component colors) and are the fallback if the file is
/// missing or invalid. Colors are `#rrggbb` hex.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeSpec {
    pub name: String,

    // -- Panels --
    #[serde(with = "hex_color")]
    pub panel_bg: Color,
    #[serde(with = "hex_color")]
    pub panel_fg: Color,
    /// Body text in the editor/viewer (usually higher contrast than `panel_fg`).
    #[serde(with = "hex_color")]
    pub text_fg: Color,
    #[serde(with = "hex_color")]
    pub panel_border: Color,
    #[serde(with = "hex_color")]
    pub panel_border_active: Color,
    /// Column headers (Name/Size/…).
    #[serde(with = "hex_color")]
    pub header_fg: Color,

    // -- Cursor (the selection bar over the focused file) --
    #[serde(with = "hex_color")]
    pub cursor_bg: Color,
    #[serde(with = "hex_color")]
    pub cursor_fg: Color,
    /// Cursor on the inactive panel.
    #[serde(with = "hex_color")]
    pub cursor_inactive_bg: Color,
    #[serde(with = "hex_color")]
    pub cursor_inactive_fg: Color,

    // -- File-type name colors --
    #[serde(with = "hex_color")]
    pub marked_fg: Color,
    #[serde(with = "hex_color")]
    pub dir_fg: Color,
    /// Regular files with no special type. Defaulted for `themes.toml` files
    /// written before this field existed (so they keep loading).
    #[serde(default = "default_file_fg", with = "hex_color")]
    pub file_fg: Color,
    #[serde(with = "hex_color")]
    pub exec_fg: Color,
    #[serde(with = "hex_color")]
    pub symlink_fg: Color,
    #[serde(with = "hex_color")]
    pub archive_fg: Color,
    #[serde(with = "hex_color")]
    pub doc_fg: Color,
    #[serde(with = "hex_color")]
    pub image_fg: Color,
    #[serde(with = "hex_color")]
    pub media_fg: Color,
    /// 3D models and CAD files. Defaulted for `themes.toml` files written before
    /// this field existed (so they keep loading).
    #[serde(default = "default_model_fg", with = "hex_color")]
    pub model_fg: Color,

    // -- Top menu bar + bottom F-key bar --
    #[serde(with = "hex_color")]
    pub menubar_bg: Color,
    #[serde(with = "hex_color")]
    pub menubar_fg: Color,
    #[serde(with = "hex_color")]
    pub fkey_label_bg: Color,
    #[serde(with = "hex_color")]
    pub fkey_label_fg: Color,
    #[serde(with = "hex_color")]
    pub fkey_num_bg: Color,
    #[serde(with = "hex_color")]
    pub fkey_num_fg: Color,

    // -- Dialogs --
    #[serde(with = "hex_color")]
    pub dialog_bg: Color,
    #[serde(with = "hex_color")]
    pub dialog_fg: Color,
    #[serde(with = "hex_color")]
    pub dialog_title: Color,
    #[serde(with = "hex_color")]
    pub dialog_border_fg: Color,
    #[serde(with = "hex_color")]
    pub dialog_border_bg: Color,
    /// Focused control / selected row inside a dialog.
    #[serde(with = "hex_color")]
    pub dialog_selection_bg: Color,
    #[serde(with = "hex_color")]
    pub dialog_selection_fg: Color,

    // -- Pulldown menus --
    #[serde(with = "hex_color")]
    pub menu_bg: Color,
    #[serde(with = "hex_color")]
    pub menu_fg: Color,
    #[serde(with = "hex_color")]
    pub menu_selection_bg: Color,
    #[serde(with = "hex_color")]
    pub menu_selection_fg: Color,
    /// Underlined accelerator letters in menus.
    #[serde(with = "hex_color")]
    pub hotkey_fg: Color,

    // -- Text inputs + buttons --
    #[serde(with = "hex_color")]
    pub input_bg: Color,
    #[serde(with = "hex_color")]
    pub input_fg: Color,
    #[serde(with = "hex_color")]
    pub button_bg: Color,
    #[serde(with = "hex_color")]
    pub button_fg: Color,
    #[serde(with = "hex_color")]
    pub button_focused_bg: Color,
    #[serde(with = "hex_color")]
    pub button_focused_fg: Color,

    // -- Misc --
    #[serde(with = "hex_color")]
    pub error_fg: Color,
    /// Text drawn over animated gradient bars.
    #[serde(with = "hex_color")]
    pub bar_fg: Color,
    /// Accent gradient endpoints: the ramp the progress bars, graphs and disk
    /// treemap fade through, and the default look of the bars and the cursor on
    /// truecolor terminals (until those elements get a gradient of their own).
    #[serde(with = "hex_color")]
    pub gradient_from: Color,
    #[serde(with = "hex_color")]
    pub gradient_to: Color,

    /// Per-element gradients (`[theme.gradients.*]`). Serialized last, since
    /// TOML sub-tables have to follow the plain keys of their parent table.
    #[serde(default, skip_serializing_if = "Gradients::is_empty")]
    pub gradients: Gradients,
}

/// Which live-preview surface best exercises a given color, so the visual theme
/// editor can show the relevant chrome while that color is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    /// The two file panels plus the menu bar, function-key bar and pulldown menu.
    Panels,
    /// A demo dialog with a text input and buttons.
    Dialog,
    /// A small editor / viewer with body text.
    Editor,
}

/// One editable row in a [`ThemeSpec`]: a human label and the preview surface it
/// drives. Entries are shown, in order, in the theme editor's item list. A row
/// with a `role` edits that element's *gradient* (its second endpoint) rather
/// than a flat color, and follows the row of the color it ramps from.
pub struct ThemeFieldMeta {
    pub group: &'static str,
    pub label: &'static str,
    pub preview: PreviewKind,
    pub role: Option<GradRole>,
}

/// Generate the editable-field table and indexed color accessors from a single
/// ordered list, so [`THEME_FIELDS`] and [`ThemeSpec::color_at`] can never drift
/// out of sync. Each entry names the flat color it edits; a gradient row names
/// the same field plus the [`GradRole`] whose ramp it edits.
macro_rules! theme_fields {
    ( $( $group:literal, $label:literal, $preview:ident, $field:ident, $role:expr ; )* ) => {
        /// Every editable row, in item-list display order. The index into this
        /// table matches [`ThemeSpec::color_at`] / [`ThemeSpec::set_color_at`].
        pub static THEME_FIELDS: &[ThemeFieldMeta] = &[
            $( ThemeFieldMeta {
                group: $group,
                label: $label,
                preview: PreviewKind::$preview,
                role: $role,
            }, )*
        ];
        impl ThemeSpec {
            /// The color of the editable field at display index `i` (falls back to
            /// the panel background for an out-of-range index). A gradient row
            /// yields its second endpoint, or — while the gradient is off — the
            /// flat color it would ramp from.
            pub fn color_at(&self, i: usize) -> Color {
                let mut n = 0usize;
                $( if n == i {
                    return match $role {
                        Some(r) => self.gradients.get(r).map_or(self.$field, |g| g.to),
                        None => self.$field,
                    };
                } n += 1; )*
                let _ = n;
                self.panel_bg
            }
            /// Replace the color of the editable field at display index `i`.
            /// Setting a gradient row's color switches that gradient on.
            pub fn set_color_at(&mut self, i: usize, c: Color) {
                let mut n = 0usize;
                $( if n == i {
                    match $role {
                        Some(r) => match self.gradients.slot(r) {
                            Some(g) => g.to = c,
                            slot => *slot = Some(GradientSpec::new(c)),
                        },
                        None => self.$field = c,
                    }
                    return;
                } n += 1; )*
                let _ = n;
            }
        }
    };
}

theme_fields! {
    // -- Panels & chrome (previewed on the two-panel view) --
    "Panel", "Background", Panels, panel_bg, None;
    "Panel", "Background gradient", Panels, panel_bg, Some(GradRole::PanelBg);
    "Panel", "Foreground", Panels, panel_fg, None;
    "Panel", "Border", Panels, panel_border, None;
    "Panel", "Border gradient", Panels, panel_border, Some(GradRole::PanelBorder);
    "Panel", "Active border", Panels, panel_border_active, None;
    "Panel", "Active border gradient", Panels, panel_border_active, Some(GradRole::PanelBorderActive);
    "Panel", "Column header", Panels, header_fg, None;
    "Cursor", "Background", Panels, cursor_bg, None;
    "Cursor", "Background gradient", Panels, cursor_bg, Some(GradRole::CursorBg);
    "Cursor", "Foreground", Panels, cursor_fg, None;
    "Cursor", "Inactive background", Panels, cursor_inactive_bg, None;
    "Cursor", "Inactive background gradient", Panels, cursor_inactive_bg, Some(GradRole::CursorInactiveBg);
    "Cursor", "Inactive foreground", Panels, cursor_inactive_fg, None;
    "File types", "Marked", Panels, marked_fg, None;
    "File types", "Directory", Panels, dir_fg, None;
    "File types", "File", Panels, file_fg, None;
    "File types", "Executable", Panels, exec_fg, None;
    "File types", "Symlink", Panels, symlink_fg, None;
    "File types", "Archive", Panels, archive_fg, None;
    "File types", "Document", Panels, doc_fg, None;
    "File types", "Image", Panels, image_fg, None;
    "File types", "Media", Panels, media_fg, None;
    "File types", "3D model", Panels, model_fg, None;
    "Menu bar", "Background", Panels, menubar_bg, None;
    "Menu bar", "Background gradient", Panels, menubar_bg, Some(GradRole::MenubarBg);
    "Menu bar", "Foreground", Panels, menubar_fg, None;
    "Function keys", "Label background", Panels, fkey_label_bg, None;
    "Function keys", "Label background gradient", Panels, fkey_label_bg, Some(GradRole::FkeyLabelBg);
    "Function keys", "Label foreground", Panels, fkey_label_fg, None;
    "Function keys", "Number background", Panels, fkey_num_bg, None;
    "Function keys", "Number foreground", Panels, fkey_num_fg, None;
    "Function keys", "Gradient text", Panels, bar_fg, None;
    "Accent", "Gradient from", Panels, gradient_from, None;
    "Accent", "Gradient to", Panels, gradient_to, None;
    "Pulldown menu", "Background", Panels, menu_bg, None;
    "Pulldown menu", "Background gradient", Panels, menu_bg, Some(GradRole::MenuBg);
    "Pulldown menu", "Foreground", Panels, menu_fg, None;
    "Pulldown menu", "Selection background", Panels, menu_selection_bg, None;
    "Pulldown menu", "Selection background gradient", Panels, menu_selection_bg, Some(GradRole::MenuSelectionBg);
    "Pulldown menu", "Selection foreground", Panels, menu_selection_fg, None;
    "Pulldown menu", "Hotkey letter", Panels, hotkey_fg, None;
    // -- Dialogs, inputs & buttons (previewed on the demo dialog) --
    "Dialog", "Background", Dialog, dialog_bg, None;
    "Dialog", "Background gradient", Dialog, dialog_bg, Some(GradRole::DialogBg);
    "Dialog", "Foreground", Dialog, dialog_fg, None;
    "Dialog", "Title", Dialog, dialog_title, None;
    "Dialog", "Border", Dialog, dialog_border_fg, None;
    "Dialog", "Border gradient", Dialog, dialog_border_fg, Some(GradRole::DialogBorderFg);
    "Dialog", "Border background", Dialog, dialog_border_bg, None;
    "Dialog", "Selection background", Dialog, dialog_selection_bg, None;
    "Dialog", "Selection background gradient", Dialog, dialog_selection_bg, Some(GradRole::DialogSelectionBg);
    "Dialog", "Selection foreground", Dialog, dialog_selection_fg, None;
    "Dialog", "Error text", Dialog, error_fg, None;
    "Input", "Background", Dialog, input_bg, None;
    "Input", "Background gradient", Dialog, input_bg, Some(GradRole::InputBg);
    "Input", "Foreground", Dialog, input_fg, None;
    "Button", "Background", Dialog, button_bg, None;
    "Button", "Background gradient", Dialog, button_bg, Some(GradRole::ButtonBg);
    "Button", "Foreground", Dialog, button_fg, None;
    "Button", "Focused background", Dialog, button_focused_bg, None;
    "Button", "Focused background gradient", Dialog, button_focused_bg, Some(GradRole::ButtonFocusedBg);
    "Button", "Focused foreground", Dialog, button_focused_fg, None;
    // -- Editor / viewer --
    "Editor / Viewer", "Body text", Editor, text_fg, None;
}

/// A clone of every active theme spec, in file order — the editable source for
/// the visual theme editor's picker.
pub fn active_specs() -> Vec<ThemeSpec> {
    ACTIVE.read().unwrap().clone()
}

/// Insert or replace `spec` (matched by name) in the active set and persist the
/// whole set to `themes.toml`. The in-memory set is updated even if the file
/// write fails; the error string is for surfacing to the user.
pub fn save_spec(spec: ThemeSpec) -> Result<(), String> {
    {
        let mut active = ACTIVE.write().unwrap();
        let key = norm_name(&spec.name);
        match active.iter_mut().find(|p| norm_name(&p.name) == key) {
            Some(slot) => *slot = spec,
            None => active.push(spec),
        }
    }
    let specs = active_specs();
    let path = crate::config::paths::themes_file().ok_or("no config directory available")?;
    write_themes(&path, &specs).map_err(|e| e.to_string())
}

/// Extract the per-component colors from a (derived) [`Theme`] into a [`ThemeSpec`]
/// — how the built-in schemes become editable component colors in `themes.toml`.
fn theme_to_spec(t: &Theme) -> ThemeSpec {
    let fg = |s: &Style| s.fg.unwrap_or(t.panel_fg);
    let bg = |s: &Style| s.bg.unwrap_or(t.panel_bg);
    ThemeSpec {
        name: t.name.clone(),
        panel_bg: t.panel_bg,
        panel_fg: t.panel_fg,
        text_fg: t.text_fg,
        panel_border: t.panel_border,
        panel_border_active: t.panel_border_active,
        header_fg: t.header_fg,
        cursor_bg: bg(&t.cursor),
        cursor_fg: fg(&t.cursor),
        cursor_inactive_bg: bg(&t.cursor_inactive),
        cursor_inactive_fg: fg(&t.cursor_inactive),
        marked_fg: t.marked_fg,
        dir_fg: t.dir_fg,
        file_fg: t.file_fg,
        exec_fg: t.exec_fg,
        symlink_fg: t.symlink_fg,
        archive_fg: t.archive_fg,
        doc_fg: t.doc_fg,
        image_fg: t.image_fg,
        media_fg: t.media_fg,
        model_fg: t.model_fg,
        menubar_bg: bg(&t.menubar),
        menubar_fg: fg(&t.menubar),
        fkey_label_bg: bg(&t.fkey_label),
        fkey_label_fg: fg(&t.fkey_label),
        fkey_num_bg: bg(&t.fkey_num),
        fkey_num_fg: fg(&t.fkey_num),
        dialog_bg: t.dialog_bg,
        dialog_fg: t.dialog_fg,
        dialog_title: t.dialog_title,
        dialog_border_fg: t.dialog_border_fg,
        dialog_border_bg: t.dialog_border_bg,
        dialog_selection_bg: bg(&t.dialog_selection),
        dialog_selection_fg: fg(&t.dialog_selection),
        menu_bg: t.menu_bg,
        menu_fg: t.menu_fg,
        menu_selection_bg: bg(&t.menu_selection),
        menu_selection_fg: fg(&t.menu_selection),
        hotkey_fg: t.hotkey_fg,
        input_bg: t.input_bg,
        input_fg: t.input_fg,
        button_bg: bg(&t.button),
        button_fg: fg(&t.button),
        button_focused_bg: bg(&t.button_focused),
        button_focused_fg: fg(&t.button_focused),
        error_fg: t.error_fg,
        bar_fg: t.bar_fg,
        gradient_from: Color::Rgb(t.grad_a.0, t.grad_a.1, t.grad_a.2),
        gradient_to: Color::Rgb(t.grad_b.0, t.grad_b.1, t.grad_b.2),
        // The derived ANSI schemes keep their flat elements; gradients are
        // something a theme opts into (in the editor or in `themes.toml`).
        gradients: Gradients::default(),
    }
}

/// TOML wrapper: `[[theme]]` array-of-tables.
#[derive(Default, Serialize, Deserialize)]
struct ThemesFile {
    /// Every preset this file has already been offered — so a preset the user
    /// deleted stays deleted, while presets added in a later release are still
    /// put in front of them. Declared (and so written) before the themes: a
    /// plain key after an array of tables would belong to the last table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    known_presets: Vec<String>,
    #[serde(default, rename = "theme")]
    theme: Vec<ThemeSpec>,
}

/// (De)serialize a [`Color`] as a `#rrggbb` hex string.
mod hex_color {
    use ratatui::style::Color;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(c: &Color, s: S) -> Result<S::Ok, S::Error> {
        let (r, g, b) = super::to_rgb(*c);
        s.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color, D::Error> {
        let s = String::deserialize(d)?;
        super::parse_hex(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("expected #rrggbb color, got {s:?}")))
    }
}

/// (De)serialize an optional [`Color`]: a `#rrggbb` string, or absent.
mod opt_hex_color {
    use ratatui::style::Color;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(c: &Option<Color>, s: S) -> Result<S::Ok, S::Error> {
        match c {
            Some(c) => super::hex_color::serialize(c, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Color>, D::Error> {
        super::hex_color::deserialize(d).map(Some)
    }
}

/// Default for [`ThemeSpec::file_fg`] (added after the initial release) so older
/// `themes.toml` files without the field still deserialize; a neutral light gray
/// like most themes' normal-file text. Regenerated presets set a per-theme value.
fn default_file_fg() -> Color {
    rgb(0xc6c6c6)
}

/// `themes.toml` files without the field still deserialize. Orange: the one
/// warm hue none of the other file-type accents had taken.
fn default_model_fg() -> Color {
    rgb(0xff9944)
}

/// Parse `#rrggbb` / `rrggbb` / `0xrrggbb` into an RGB [`Color`].
fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim();
    let s = s.strip_prefix('#').or_else(|| s.strip_prefix("0x")).unwrap_or(s);
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(rgb(n))
}

const THEMES_HEADER: &str = "\
# Rat Commander themes. Each [[theme]] sets an explicit #rrggbb color for every
# UI element (e.g. menu_bg, dialog_bg, dialog_border_fg, input_bg, cursor_bg).
# Edit any preset, add your own [[theme]] blocks, then pick one in Options →
# Settings (the Theme field). Saving applies the change at once. Delete this file
# to regenerate the presets.
#
# `known_presets` records which presets you have already been offered: a preset
# you delete from this file stays deleted, while presets added in a later release
# are appended on the next start. Take a name off that list to be offered its
# preset again.
#
# Panels, dialogs, menus, inputs, buttons, cursors, the two bars and the frames
# can each fade between two colors instead of painting one. Add a table per
# element at the end of its [[theme]] (truecolor terminals only):
#
#   [theme.gradients.panel_bg]
#   to = \"#001a80\"        # second endpoint; `from` defaults to panel_bg itself
#   direction = \"vertical\" # horizontal | vertical | diagonal | radial
#   animated = false       # off by default; drifts when on (with animations on)
#
# The elements that take one are panel_bg, panel_border, panel_border_active,
# cursor_bg, cursor_inactive_bg, menubar_bg, fkey_label_bg, menu_bg,
# menu_selection_bg, dialog_bg, dialog_border_fg, dialog_selection_bg, input_bg,
# button_bg and button_focused_bg. Two elements set to the same flat color can
# not be told apart on screen and so share a gradient — give them distinct
# colors to ramp them separately. See the `Rat Commander Neon` preset.\n\n";

/// The signature Rat Commander theme (the default): a deep-blue two-panel look
/// with a teal selection bar and light "paper" dialogs. Defined with explicit
/// component colors rather than derived from an ANSI palette.
fn rat_commander_spec() -> ThemeSpec {
    ThemeSpec {
        name: "Rat Commander".to_string(),
        panel_bg: rgb(0x0000cd),
        panel_fg: rgb(0xc6c6c6),
        text_fg: rgb(0xd7d7d7),
        // Doubles as the dim text color on the blue panels, the grey dialogs and
        // the cyan menus alike, so it sits at a brightness all three can read; a
        // greyed lavender reads more clearly on the blue than a saturated one.
        panel_border: rgb(0x7474b2),
        panel_border_active: rgb(0x55ffff),
        header_fg: rgb(0xffff55),
        cursor_bg: rgb(0x00a3a3),
        cursor_fg: rgb(0x000000),
        cursor_inactive_bg: rgb(0x3333dd),
        cursor_inactive_fg: rgb(0xc6c6c6),
        marked_fg: rgb(0xffff55),
        dir_fg: rgb(0xc6c6c6),
        file_fg: rgb(0xc6c6c6),
        exec_fg: rgb(0x55ff55),
        symlink_fg: rgb(0x55ffff),
        archive_fg: rgb(0xff55ff),
        doc_fg: rgb(0xd7a700),
        image_fg: rgb(0x55ffff),
        media_fg: rgb(0x55ff55),
        model_fg: rgb(0xff9944),
        menubar_bg: rgb(0x00a3a3),
        menubar_fg: rgb(0x0000cd),
        fkey_label_bg: rgb(0x00a3a3),
        fkey_label_fg: rgb(0x0000cd),
        fkey_num_bg: rgb(0x000000),
        fkey_num_fg: rgb(0xffffff),
        dialog_bg: rgb(0xc6c6c6),
        dialog_fg: rgb(0x000000),
        dialog_title: rgb(0x0000cc),
        dialog_border_fg: rgb(0x0000cc),
        dialog_border_bg: rgb(0xc6c6c6),
        dialog_selection_bg: rgb(0x0dcdcd),
        dialog_selection_fg: rgb(0x000000),
        menu_bg: rgb(0x0dcdcd),
        menu_fg: rgb(0xffffff),
        menu_selection_bg: rgb(0x000000),
        menu_selection_fg: rgb(0xffffff),
        hotkey_fg: rgb(0xffff00),
        input_bg: rgb(0x0dcdcd),
        input_fg: rgb(0x000000),
        button_bg: rgb(0xc6c6c6),
        button_fg: rgb(0x000000),
        button_focused_bg: rgb(0x0dcdcd),
        button_focused_fg: rgb(0x000000),
        error_fg: rgb(0xff5555),
        bar_fg: rgb(0x000000),
        gradient_from: rgb(0x009c9c),
        gradient_to: rgb(0x12baba),
        gradients: Gradients::default(),
    }
}

/// The classic Midnight Commander look: a lighter blue panel with white text and
/// a bright cyan menu/status bar.
fn midnight_commander_spec() -> ThemeSpec {
    ThemeSpec {
        name: "Midnight Commander".to_string(),
        panel_bg: rgb(0x0d73cc),
        panel_fg: rgb(0xffffff),
        text_fg: rgb(0xd7d7d7),
        panel_border: rgb(0xffffff),
        panel_border_active: rgb(0xffffff),
        header_fg: rgb(0xffff55),
        cursor_bg: rgb(0x0dcdcd),
        cursor_fg: rgb(0x000000),
        cursor_inactive_bg: rgb(0x0d73cc),
        cursor_inactive_fg: rgb(0xffffff),
        marked_fg: rgb(0xffff55),
        dir_fg: rgb(0xffffff),
        file_fg: rgb(0xd2d2d2),
        exec_fg: rgb(0x55ff55),
        symlink_fg: rgb(0x55ffff),
        archive_fg: rgb(0xff55ff),
        doc_fg: rgb(0xe61000),
        image_fg: rgb(0x55ffff),
        media_fg: rgb(0x55ff55),
        model_fg: rgb(0xff9944),
        menubar_bg: rgb(0x0dcdcd),
        menubar_fg: rgb(0x000000),
        fkey_label_bg: rgb(0x0dcdcd),
        fkey_label_fg: rgb(0x0000cd),
        fkey_num_bg: rgb(0x000000),
        fkey_num_fg: rgb(0xffffff),
        dialog_bg: rgb(0xc6c6c6),
        dialog_fg: rgb(0x000000),
        dialog_title: rgb(0x0d73cc),
        dialog_border_fg: rgb(0x000000),
        dialog_border_bg: rgb(0xc6c6c6),
        dialog_selection_bg: rgb(0x0dcdcd),
        dialog_selection_fg: rgb(0x000000),
        menu_bg: rgb(0x0dcdcd),
        menu_fg: rgb(0xffffff),
        menu_selection_bg: rgb(0x000000),
        menu_selection_fg: rgb(0xffffff),
        hotkey_fg: rgb(0xffff00),
        input_bg: rgb(0x0dcdcd),
        input_fg: rgb(0x000000),
        button_bg: rgb(0xc6c6c6),
        button_fg: rgb(0x000000),
        button_focused_bg: rgb(0x0dcdcd),
        button_focused_fg: rgb(0x000000),
        error_fg: rgb(0xff5555),
        bar_fg: rgb(0x000000),
        gradient_from: rgb(0x0dcdcd),
        gradient_to: rgb(0x0dcdcd),
        gradients: Gradients::default(),
    }
}

/// A darker Midnight Commander variant: deep indigo panels with a teal accent.
fn midnight_commander_dark_spec() -> ThemeSpec {
    ThemeSpec {
        name: "Midnight Commander Dark".to_string(),
        panel_bg: rgb(0x1818d4),
        panel_fg: rgb(0xe8e8e8),
        text_fg: rgb(0xefefef),
        panel_border: rgb(0x7676dd),
        panel_border_active: rgb(0x4cffff),
        header_fg: rgb(0xffff44),
        cursor_bg: rgb(0x00a3a3),
        cursor_fg: rgb(0x000000),
        cursor_inactive_bg: rgb(0x3131d6),
        cursor_inactive_fg: rgb(0xe8e8e8),
        marked_fg: rgb(0xffff44),
        dir_fg: rgb(0x4cffff),
        file_fg: rgb(0xe8e8e8),
        exec_fg: rgb(0x4cff4c),
        symlink_fg: rgb(0x4cffff),
        archive_fg: rgb(0xff55ff),
        doc_fg: rgb(0xe8e8e8),
        image_fg: rgb(0x4cffff),
        media_fg: rgb(0x4cff4c),
        model_fg: rgb(0xff9944),
        menubar_bg: rgb(0x00a3a3),
        menubar_fg: rgb(0x1818d4),
        fkey_label_bg: rgb(0x00a3a3),
        fkey_label_fg: rgb(0x1818d4),
        fkey_num_bg: rgb(0x1818d4),
        fkey_num_fg: rgb(0xffffff),
        dialog_bg: rgb(0x3131d6),
        dialog_fg: rgb(0xe8e8e8),
        dialog_title: rgb(0x4cffff),
        dialog_border_fg: rgb(0x4cffff),
        dialog_border_bg: rgb(0x3131d6),
        dialog_selection_bg: rgb(0x4cffff),
        dialog_selection_fg: rgb(0x1818d4),
        menu_bg: rgb(0x0e0ed1),
        menu_fg: rgb(0xffffff),
        menu_selection_bg: rgb(0x6c6cff),
        menu_selection_fg: rgb(0xffffff),
        hotkey_fg: rgb(0xffff44),
        input_bg: rgb(0x0000cc),
        input_fg: rgb(0xffffff),
        button_bg: rgb(0x3131d6),
        button_fg: rgb(0xe8e8e8),
        button_focused_bg: rgb(0x4cffff),
        button_focused_fg: rgb(0x1818d4),
        error_fg: rgb(0xff6464),
        bar_fg: rgb(0x000000),
        gradient_from: rgb(0x009c9c),
        gradient_to: rgb(0x12baba),
        gradients: Gradients::default(),
    }
}

/// A gradient showcase built on the Rat Commander colors: still ramps on the
/// panels, dialogs and frames, and moving ones on the cursor and the two bars.
/// Kept as a preset so the per-element gradients are visible (and editable)
/// without hand-writing `themes.toml`.
fn rat_commander_neon_spec() -> ThemeSpec {
    let grad = |to: u32, direction: GradientDir, animated: bool| {
        Some(GradientSpec { from: None, to: rgb(to), direction, animated })
    };
    ThemeSpec {
        name: "Rat Commander Neon".to_string(),
        panel_bg: rgb(0x14003c),
        panel_border: rgb(0x7a3cff),
        panel_border_active: rgb(0x22e0ff),
        cursor_bg: rgb(0x7a1fff),
        cursor_inactive_bg: rgb(0x2a0a5e),
        menubar_bg: rgb(0x7a1fff),
        menubar_fg: rgb(0xffffff),
        fkey_label_bg: rgb(0x7a1fff),
        fkey_label_fg: rgb(0xffffff),
        fkey_num_bg: rgb(0x14003c),
        dialog_bg: rgb(0x1d0a4e),
        dialog_fg: rgb(0xe8e8ff),
        dialog_title: rgb(0x22e0ff),
        dialog_border_fg: rgb(0x7a3cff),
        dialog_border_bg: rgb(0x1d0a4e),
        dialog_selection_bg: rgb(0x7a1fff),
        dialog_selection_fg: rgb(0xffffff),
        menu_bg: rgb(0x1d0a4e),
        menu_selection_bg: rgb(0x7a1fff),
        input_bg: rgb(0x2a0a5e),
        input_fg: rgb(0xe8e8ff),
        button_bg: rgb(0x2a0a5e),
        button_fg: rgb(0xe8e8ff),
        button_focused_bg: rgb(0x7a1fff),
        button_focused_fg: rgb(0xffffff),
        bar_fg: rgb(0xffffff),
        gradient_from: rgb(0x7a1fff),
        gradient_to: rgb(0x22e0ff),
        gradients: Gradients {
            // Backgrounds stay still — a drifting panel is distracting.
            panel_bg: grad(0x3d0a6e, GradientDir::Vertical, false),
            dialog_bg: grad(0x2f1470, GradientDir::Diagonal, false),
            menu_bg: grad(0x2f1470, GradientDir::Vertical, false),
            input_bg: grad(0x3d0a6e, GradientDir::Horizontal, false),
            panel_border: grad(0x22e0ff, GradientDir::Vertical, false),
            panel_border_active: grad(0xff3caa, GradientDir::Vertical, false),
            dialog_border_fg: grad(0x22e0ff, GradientDir::Horizontal, false),
            // …while the chrome that marks *where you are* moves.
            cursor_bg: grad(0x22e0ff, GradientDir::Horizontal, true),
            cursor_inactive_bg: grad(0x3d0a6e, GradientDir::Horizontal, false),
            menubar_bg: grad(0x22e0ff, GradientDir::Horizontal, true),
            fkey_label_bg: grad(0x22e0ff, GradientDir::Horizontal, true),
            button_bg: grad(0x3d0a6e, GradientDir::Radial, false),
            button_focused_bg: grad(0x22e0ff, GradientDir::Radial, true),
            dialog_selection_bg: grad(0x22e0ff, GradientDir::Horizontal, false),
            menu_selection_bg: grad(0x22e0ff, GradientDir::Horizontal, false),
        },
        ..rat_commander_spec()
    }
}

/// The presets that stay flat: the CRT themes imitate a single-phosphor screen,
/// and a ramp across the picture would give the illusion away. Matched with
/// [`norm_name`], like every other theme lookup.
const FLAT_PRESETS: [&str; 2] = ["Amber CRT", "Green CRT"];

/// A visibly different shade of `c` for a gradient's far end: lighter on a dark
/// color, darker on a light one, so the ramp shows on any theme.
fn shade(c: Color, amount: f64) -> Color {
    let target = if luma(c) > 140.0 { rgb(0x000000) } else { rgb(0xffffff) };
    mix(c, target, amount)
}

/// How far apart two colors are, summed over the channels. Anything under ~24
/// reads as the same color on screen — and a gradient between two of those is
/// just a flat element with extra lines in `themes.toml`.
fn spread(a: Color, b: Color) -> u32 {
    let (a, b) = (to_rgb(a), to_rgb(b));
    a.0.abs_diff(b.0) as u32 + a.1.abs_diff(b.1) as u32 + a.2.abs_diff(b.2) as u32
}

/// A far end for `base` that is actually distinguishable: `toward` when the two
/// colors differ enough to read as a ramp, else a shade of `base` itself. (Some
/// themes give an element the very color of the accent it would fade towards —
/// the Commander cursor, menu bar and F-key bar are all the accent teal.)
fn far_end(base: Color, toward: Color, amount: f64) -> Color {
    if spread(base, toward) >= 60 { toward } else { shade(base, amount) }
}

/// A far end for a large surface: `base` tinted `amount` of the way towards the
/// theme's accent, falling back to a plain shade when the surface already *is*
/// that accent and the tint would be invisible.
fn tint(base: Color, accent: Color, amount: f64, fallback: f64) -> Color {
    let tinted = mix(base, accent, amount);
    if spread(base, tinted) >= 24 { tinted } else { shade(base, fallback) }
}

/// A preset built *around* its backdrop. [`derive_gradients`] gives every theme
/// a hint of its own accent behind the panels; these fade to a color chosen for
/// them instead — the Tron grid glowing at the horizon, the Synthwave dusk
/// turning magenta, Coral Reef warming from deep water to reef sand — so the
/// background is part of the design rather than a suggestion of depth. Only the
/// surfaces are replaced; the cursor, bars and frames still follow the theme's
/// own accent.
struct Backdrop {
    name: &'static str,
    /// What the panel background fades down to.
    panels: u32,
    /// What the dialog and menu surfaces fade across to.
    dialogs: u32,
}

const SHOWCASE_BACKDROPS: [Backdrop; 5] = [
    Backdrop { name: "Tron", panels: 0x06334d, dialogs: 0x0a3d5a },
    Backdrop { name: "Graphite", panels: 0x2c313a, dialogs: 0x191c21 },
    Backdrop { name: "Synthwave", panels: 0x4a1046, dialogs: 0x4a1f6b },
    Backdrop { name: "Aurora", panels: 0x0f4a40, dialogs: 0x123f4a },
    Backdrop { name: "Coral Reef", panels: 0x56303a, dialogs: 0x3c3040 },
];

/// The presets built on a pair of opposing hues frame their dialogs in one hue
/// and wash the inside towards a deep shade of the other — Anaglyph's cyan frame
/// around a red wash, Fire and Ice's ice around embers — so every dialog carries
/// the pair. Each wash stays about as dark as the surface it leaves, so the text
/// keeps its contrast across the whole dialog. Only the dialog ramp's far end is
/// replaced; its direction and everything else stay derived.
const CONTRAST_DIALOGS: [(&str, u32); 5] = [
    ("Anaglyph", 0x561424),
    ("Fire and Ice", 0x50240c),
    ("Acid", 0x520f40),
    ("Regalia", 0x4a1450),
    ("Patina", 0x0a3a37),
];

/// The gradients a preset carries by default, derived from its own colors.
///
/// The chrome that marks *where you are* — the cursor and the two bars — sweeps
/// towards the theme's accent and drifts, exactly as it did when one accent
/// gradient drove all three. The large surfaces underneath take a still, lightly
/// accent-tinted ramp for depth, and the focused frame and button get one to set
/// them off. Everything else (the inactive frame, the selections, the unfocused
/// buttons) stays flat, so the ramps mark something instead of coating the whole
/// UI. A theme can of course say otherwise: this only fills in the presets.
fn derive_gradients(s: &ThemeSpec) -> Gradients {
    let accent = mix(s.gradient_from, s.gradient_to, 0.5);
    let still = |to: Color, direction: GradientDir| {
        Some(GradientSpec { from: None, to, direction, animated: false })
    };
    let sweep = |base: Color| {
        Some(GradientSpec {
            from: None,
            to: far_end(base, s.gradient_to, 0.30),
            direction: GradientDir::Horizontal,
            animated: true,
        })
    };
    Gradients {
        // Surfaces: a tint of the theme's own accent, held still.
        panel_bg: still(tint(s.panel_bg, accent, 0.14, 0.10), GradientDir::Vertical),
        dialog_bg: still(tint(s.dialog_bg, accent, 0.10, 0.08), GradientDir::Diagonal),
        menu_bg: still(tint(s.menu_bg, accent, 0.12, 0.10), GradientDir::Vertical),
        input_bg: still(shade(s.input_bg, 0.18), GradientDir::Horizontal),
        // The focused panel's frame fades towards the quieter border color, so
        // it reads as lit from the top rather than as a second flat outline.
        panel_border_active: still(
            far_end(s.panel_border_active, s.panel_border, 0.35),
            GradientDir::Vertical,
        ),
        dialog_border_fg: still(far_end(s.dialog_border_fg, accent, 0.35), GradientDir::Horizontal),
        // Focus chrome sweeps and drifts.
        cursor_bg: sweep(s.cursor_bg),
        menubar_bg: sweep(s.menubar_bg),
        fkey_label_bg: sweep(s.fkey_label_bg),
        // A soft pill highlight on the button that has the keyboard.
        button_focused_bg: still(shade(s.button_focused_bg, 0.32), GradientDir::Radial),
        ..Gradients::default()
    }
}

/// The built-in presets as component specs. The three Rat/Midnight Commander
/// themes are defined explicitly (above); every other well-known scheme is
/// derived once from its ANSI [`Palette`] via [`Theme::from_ansi`]. These seed
/// `themes.toml` and serve as the fallback set. `Rat Commander` is first, so it
/// is the default ([`Theme::mc`], [`BUILTIN`]`[0]`).
fn builtin_specs() -> Vec<ThemeSpec> {
    let mut specs = vec![
        rat_commander_spec(),
        midnight_commander_spec(),
        midnight_commander_dark_spec(),
        rat_commander_neon_spec(),
    ];
    specs.extend(PALETTES.iter().map(|p| theme_to_spec(&Theme::from_ansi(p, true))));
    // Give every preset its gradients, leaving the hand-written showcase with
    // the ones it defines and the CRT themes deliberately flat.
    for spec in specs.iter_mut() {
        let flat = FLAT_PRESETS.iter().any(|n| norm_name(n) == norm_name(&spec.name));
        if spec.gradients.is_empty() && !flat {
            spec.gradients = derive_gradients(spec);
        }
        // The showcase themes swap the derived surface tints for their own
        // backdrop; everything else about their gradients stays derived.
        if let Some(b) =
            SHOWCASE_BACKDROPS.iter().find(|b| norm_name(b.name) == norm_name(&spec.name))
        {
            for (slot, to) in [
                (&mut spec.gradients.panel_bg, b.panels),
                (&mut spec.gradients.dialog_bg, b.dialogs),
                (&mut spec.gradients.menu_bg, b.dialogs),
            ] {
                if let Some(g) = slot.as_mut() {
                    g.to = rgb(to);
                }
            }
        }
        if let Some((_, to)) =
            CONTRAST_DIALOGS.iter().find(|(n, _)| norm_name(n) == norm_name(&spec.name))
            && let Some(g) = spec.gradients.dialog_bg.as_mut()
        {
            g.to = rgb(*to);
        }
    }
    specs
}

/// The flat colors of presets as an earlier release shipped them, before they
/// were retouched. Only the colors matter: the gradients a preset carried have
/// changed from release to release on their own.
fn retired_presets() -> Vec<ThemeSpec> {
    // Rat Commander before its low-contrast colors were lifted: the dim text
    // (and inactive frame), the inactive cursor bar and the document color. The
    // Neon showcase inherited the document color.
    let old_doc = rgb(0xaa5500);
    vec![
        ThemeSpec {
            panel_border: rgb(0x5959ca),
            cursor_inactive_bg: rgb(0x1818cc),
            doc_fg: old_doc,
            ..rat_commander_spec()
        },
        ThemeSpec { doc_fg: old_doc, ..rat_commander_neon_spec() },
    ]
}

static BUILTIN: LazyLock<Vec<ThemeSpec>> = LazyLock::new(builtin_specs);
/// The themes currently in effect (built-ins until `themes.toml` is loaded).
static ACTIVE: LazyLock<RwLock<Vec<ThemeSpec>>> = LazyLock::new(|| RwLock::new(builtin_specs()));

/// Replace the active theme set (ignored if empty).
fn set_palettes(specs: Vec<ThemeSpec>) {
    if !specs.is_empty() {
        *ACTIVE.write().unwrap() = specs;
    }
}

/// Add fields introduced after a user's `themes.toml` was first written, so an
/// older file keeps working and gains sensible values on upgrade. Currently the
/// only such field is `file_fg` (the regular-file color): it defaults to each
/// theme's own `panel_fg` — exactly what normal files rendered as before it
/// became themable — so both the presets and any user-made themes look unchanged.
/// Returns the migrated TOML when anything was added, else `None`.
fn migrate_theme_toml(text: &str) -> Option<String> {
    let mut doc = toml::from_str::<toml::Table>(text).ok()?;
    let themes = doc.get_mut("theme")?.as_array_mut()?;
    let mut changed = false;
    for entry in themes.iter_mut() {
        let Some(tbl) = entry.as_table_mut() else { continue };
        if !tbl.contains_key("file_fg") {
            let fallback = tbl
                .get("panel_fg")
                .cloned()
                .unwrap_or_else(|| toml::Value::String("#c6c6c6".to_string()));
            tbl.insert("file_fg".to_string(), fallback);
            changed = true;
        }
    }
    changed.then(|| toml::to_string(&doc).ok()).flatten()
}

/// Give the stock presets in an older `themes.toml` the gradients they now ship
/// with. A theme is only upgraded when it still matches the built-in of the same
/// name color for color, so a preset the user has retouched — and any theme they
/// wrote themselves — is left exactly as it is. Returns whether anything changed.
fn adopt_preset_gradients(specs: &mut [ThemeSpec]) -> bool {
    let mut changed = false;
    for spec in specs.iter_mut() {
        if !spec.gradients.is_empty() {
            continue;
        }
        let key = norm_name(&spec.name);
        let Some(builtin) = BUILTIN.iter().find(|b| norm_name(&b.name) == key) else {
            continue;
        };
        if builtin.gradients.is_empty() {
            continue;
        }
        // Compare against the preset under the user's own spelling of the name,
        // with the gradients stripped: equal means untouched.
        let mut upgraded = builtin.clone();
        upgraded.name = spec.name.clone();
        let flat = ThemeSpec { gradients: Gradients::default(), ..upgraded.clone() };
        if flat == *spec {
            *spec = upgraded;
            changed = true;
        }
    }
    changed
}

/// Move the stock presets in an older `themes.toml` onto the colors they have
/// since been retouched to. As with [`adopt_preset_gradients`], only a theme
/// whose colors still match an earlier release's preset one for one is upgraded,
/// so a preset the user has recolored keeps their colors. Its gradients stay as
/// they are, except that a ramp running to a retired color runs to the color
/// that replaced it. Returns whether anything changed.
fn adopt_retouched_presets(specs: &mut [ThemeSpec]) -> bool {
    let colors = |s: &ThemeSpec| ThemeSpec {
        name: String::new(),
        gradients: Gradients::default(),
        ..s.clone()
    };
    let retired = retired_presets();
    let mut changed = false;
    for spec in specs.iter_mut() {
        let key = norm_name(&spec.name);
        let Some(builtin) = BUILTIN.iter().find(|b| norm_name(&b.name) == key) else {
            continue;
        };
        let Some(old) =
            retired.iter().find(|r| norm_name(&r.name) == key && colors(r) == colors(spec))
        else {
            continue;
        };
        let mut gradients = std::mem::take(&mut spec.gradients);
        for (i, field) in THEME_FIELDS.iter().enumerate() {
            let (was, now) = (old.color_at(i), builtin.color_at(i));
            if field.role.is_some() || was == now {
                continue;
            }
            for role in GradRole::ALL {
                if let Some(g) = gradients.slot(role) {
                    if g.to == was {
                        g.to = now;
                    }
                    if g.from == Some(was) {
                        g.from = Some(now);
                    }
                }
            }
        }
        *spec = ThemeSpec { name: spec.name.clone(), gradients, ..builtin.clone() };
        changed = true;
    }
    changed
}

/// The names of every shipped preset, in file order.
fn preset_names() -> Vec<String> {
    BUILTIN.iter().map(|s| s.name.clone()).collect()
}

/// Put the presets a `themes.toml` has never been offered — the ones added in a
/// release later than the file — at the end of it, so an existing install still
/// gets new themes. A preset the user deleted is not resurrected: once a file
/// records a preset in `known_presets`, it is never added again.
///
/// A file written before `known_presets` existed has no record at all, so it is
/// offered every preset it is missing, once; from then on its deletions stick.
/// Returns whether anything was added.
fn add_new_presets(tf: &mut ThemesFile) -> bool {
    let mut added = false;
    for preset in BUILTIN.iter() {
        let key = norm_name(&preset.name);
        let present = tf.theme.iter().any(|t| norm_name(&t.name) == key);
        let offered = tf.known_presets.iter().any(|n| norm_name(n) == key);
        if !present && !offered {
            tf.theme.push(preset.clone());
            added = true;
        }
    }
    added
}

/// Load `themes.toml` (generating it from the presets if absent) and make those
/// palettes active. Call once at startup, before deriving the initial theme.
pub fn load_user_themes() {
    let Some(path) = crate::config::paths::themes_file() else {
        return;
    };
    if !path.exists() {
        let _ = write_themes(&path, &builtin_specs());
        return; // built-ins are already active by default
    }
    if let Some(specs) = upgrade_themes_file(&path) {
        set_palettes(specs);
    }
}

/// Read `themes.toml` and bring it up to date with this release: newly-added
/// color fields, retouched preset colors, the gradients the presets now ship
/// with, and any preset the file has never been offered. The file is written
/// back when any of that changed. Returns the themes to make active, or `None`
/// when it can't be read or parsed — in which case the built-ins stay in effect
/// rather than the user's file being clobbered.
fn upgrade_themes_file(path: &Path) -> Option<Vec<ThemeSpec>> {
    let text = std::fs::read_to_string(path).ok()?;
    // Upgrade an older file in place: add any newly-introduced color fields with
    // appearance-preserving values, then parse.
    let migrated = migrate_theme_toml(&text);
    let src = migrated.as_deref().unwrap_or(&text);
    let mut tf: ThemesFile = toml::from_str(src).ok()?;
    if tf.theme.is_empty() {
        return None;
    }
    let retouched = adopt_retouched_presets(&mut tf.theme);
    let adopted = adopt_preset_gradients(&mut tf.theme);
    let added = add_new_presets(&mut tf);
    // Record the offer even when nothing was added, so a file written before
    // `known_presets` existed starts keeping its deletions from now on.
    let recorded = tf.known_presets != preset_names();
    if migrated.is_some() || retouched || adopted || added || recorded {
        let _ = write_themes(path, &tf.theme);
    }
    Some(tf.theme)
}

/// Re-read `themes.toml` and make it active (after the user edits it). Returns
/// the number of themes, or an error message for a malformed file.
pub fn reload_user_themes() -> Result<usize, String> {
    let path = crate::config::paths::themes_file().ok_or("no config directory available")?;
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let tf: ThemesFile = toml::from_str(&text).map_err(|e| e.to_string())?;
    if tf.theme.is_empty() {
        return Err("themes.toml has no [[theme]] entries".to_string());
    }
    let n = tf.theme.len();
    set_palettes(tf.theme);
    Ok(n)
}

fn write_themes(path: &Path, specs: &[ThemeSpec]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Whatever else is being written, every preset has been offered by now —
    // either it is in `specs` or the user removed it on purpose.
    let tf = ThemesFile { known_presets: preset_names(), theme: specs.to_vec() };
    let body = toml::to_string_pretty(&tf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, format!("{THEMES_HEADER}{body}"))
}

/// Centralized styles for every UI element, derived from a palette.
#[derive(Clone)]
pub struct Theme {
    pub name: String,
    pub truecolor: bool,
    pub panel_bg: Color,
    pub panel_fg: Color,
    /// Higher-contrast foreground for dense text views (editor/viewer), pushed
    /// away from the background so body text reads crisply.
    pub text_fg: Color,
    pub panel_border: Color,
    pub panel_border_active: Color,
    pub header_fg: Color,
    pub cursor: Style,
    pub cursor_inactive: Style,
    pub cursor_fg: Color,
    pub marked_fg: Color,
    pub dir_fg: Color,
    pub file_fg: Color,
    pub exec_fg: Color,
    pub symlink_fg: Color,
    /// File-type accent colors (by extension): archives, documents, images, and
    /// audio/video media.
    pub archive_fg: Color,
    pub doc_fg: Color,
    pub image_fg: Color,
    pub media_fg: Color,
    pub model_fg: Color,
    pub menubar: Style,
    pub fkey_label: Style,
    pub fkey_num: Style,
    pub dialog_bg: Color,
    pub dialog_fg: Color,
    pub dialog_title: Color,
    /// The dialog's border (frame) — foreground and background, separate from the
    /// interior so a theme can outline dialogs distinctly.
    pub dialog_border_fg: Color,
    pub dialog_border_bg: Color,
    /// Highlight style for a focused control / selected row inside a dialog.
    pub dialog_selection: Style,
    /// Background/foreground of pulldown menu dropdowns (kept distinct from
    /// dialogs so a theme can dress them differently).
    pub menu_bg: Color,
    pub menu_fg: Color,
    /// Highlight style for the selected item in a pulldown menu.
    pub menu_selection: Style,
    /// Foreground for menu/menu-bar **hotkey** letters (the underlined accelerator
    /// char), drawn over both the bar and the dropdown.
    pub hotkey_fg: Color,
    pub input_bg: Color,
    pub input_fg: Color,
    pub button: Style,
    pub button_focused: Style,
    pub error_fg: Color,
    /// Readable foreground for text drawn over a gradient bar.
    pub bar_fg: Color,
    /// Animation frame (set per-frame by the renderer).
    pub anim: usize,
    /// Whether gradients should animate (slide) this frame.
    pub animated: bool,
    /// Accent gradient endpoints (RGB) used for bars when `truecolor` is set.
    grad_a: (u8, u8, u8),
    grad_b: (u8, u8, u8),
    /// Per-element gradients, resolved from the spec and indexed by
    /// [`GradRole::index`]. Empty on a theme that defines none.
    grads: [Option<Grad>; GRAD_ROLES],
}

impl Theme {
    /// The default theme (classic Midnight Commander blue).
    pub fn mc() -> Self {
        Theme::from_spec(&BUILTIN[0], true)
    }

    /// Build a [`Theme`] from explicit per-component colors (the `themes.toml`
    /// form). `truecolor` only governs gradient animation; the colors are used
    /// as-is. Structural emphasis (bold cursor/selection/buttons) is applied here.
    pub fn from_spec(s: &ThemeSpec, truecolor: bool) -> Self {
        let bg_fg = |bg: Color, fg: Color| Style::default().bg(bg).fg(fg);
        let bold = |bg: Color, fg: Color| bg_fg(bg, fg).add_modifier(Modifier::BOLD);
        // Resolve each element's gradient once: endpoints to RGB, with `from`
        // defaulting to the flat color the element would otherwise paint.
        let mut grads = [None; GRAD_ROLES];
        for role in GradRole::ALL {
            if let Some(g) = s.gradients.get(role) {
                let base = s.gradient_base(role);
                grads[role.index()] = Some(Grad {
                    base,
                    from: to_rgb(g.from.unwrap_or(base)),
                    to: to_rgb(g.to),
                    dir: g.direction,
                    animated: g.animated,
                });
            }
        }
        Theme {
            name: s.name.clone(),
            truecolor,
            panel_bg: s.panel_bg,
            panel_fg: s.panel_fg,
            text_fg: s.text_fg,
            panel_border: s.panel_border,
            panel_border_active: s.panel_border_active,
            header_fg: s.header_fg,
            cursor: bold(s.cursor_bg, s.cursor_fg),
            cursor_inactive: bg_fg(s.cursor_inactive_bg, s.cursor_inactive_fg),
            cursor_fg: s.cursor_fg,
            marked_fg: s.marked_fg,
            dir_fg: s.dir_fg,
            file_fg: s.file_fg,
            exec_fg: s.exec_fg,
            symlink_fg: s.symlink_fg,
            archive_fg: s.archive_fg,
            doc_fg: s.doc_fg,
            image_fg: s.image_fg,
            media_fg: s.media_fg,
            model_fg: s.model_fg,
            menubar: bg_fg(s.menubar_bg, s.menubar_fg),
            fkey_label: bg_fg(s.fkey_label_bg, s.fkey_label_fg),
            fkey_num: bold(s.fkey_num_bg, s.fkey_num_fg),
            dialog_bg: s.dialog_bg,
            dialog_fg: s.dialog_fg,
            dialog_title: s.dialog_title,
            dialog_border_fg: s.dialog_border_fg,
            dialog_border_bg: s.dialog_border_bg,
            dialog_selection: bold(s.dialog_selection_bg, s.dialog_selection_fg),
            menu_bg: s.menu_bg,
            menu_fg: s.menu_fg,
            menu_selection: bold(s.menu_selection_bg, s.menu_selection_fg),
            hotkey_fg: s.hotkey_fg,
            input_bg: s.input_bg,
            input_fg: s.input_fg,
            button: bg_fg(s.button_bg, s.button_fg),
            button_focused: bold(s.button_focused_bg, s.button_focused_fg),
            error_fg: s.error_fg,
            bar_fg: s.bar_fg,
            anim: 0,
            animated: false,
            grad_a: to_rgb(s.gradient_from),
            grad_b: to_rgb(s.gradient_to),
            grads,
        }
    }

    /// Derive the default component colors for a built-in ANSI scheme. Used only
    /// to seed the editable [`ThemeSpec`]s; the runtime builds themes via
    /// [`from_spec`](Self::from_spec).
    fn from_ansi(p: &Palette, truecolor: bool) -> Self {
        let surface = if truecolor { mix(p.bg, p.fg, 0.12) } else { p.bright_black };
        // Derived themes use a gradient-friendly bright-blue cursor. (The teal
        // Commander cursor lives in the explicit specs, not here.)
        let (cursor_bg, cursor_fg) =
            (p.bright_blue, best_contrast(p.bright_blue, p.bg, p.bright_white));
        // Borders/column separators must contrast with the panel background on
        // every theme (e.g. MC's blue border would vanish on its blue bg), so
        // derive them from a bg↔fg mix rather than a palette hue.
        let border = mix(p.bg, p.fg, 0.45);

        // Dialogs sit on a neutral, slightly elevated surface; menus get a
        // clearly distinct blue-tinted panel so the two read as different chrome
        // on every theme. (The MC theme overrides both further down.)
        let dialog_surface = surface;
        let menu_surface = mix(p.bg, p.blue, 0.40);

        // The top menu bar and bottom F-key bar are drawn from the middle of the
        // theme's own accent gradient (the same colour the truecolor bars fade
        // through) so the chrome matches the theme instead of a stock cyan.
        let (bar_bg, bar_bg_fg) = {
            let mid = mix(p.bright_blue, p.bright_magenta, 0.5);
            (mid, best_contrast(mid, p.black, p.bright_white))
        };

        let mut theme = Theme {
            name: p.name.to_string(),
            truecolor,
            panel_bg: p.bg,
            panel_fg: p.fg,
            text_fg: contrast_text(p.fg, p.bg),
            panel_border: border,
            panel_border_active: p.bright_cyan,
            header_fg: p.bright_yellow,
            cursor: Style::default().bg(cursor_bg).fg(cursor_fg).add_modifier(Modifier::BOLD),
            cursor_inactive: Style::default().bg(surface).fg(p.fg),
            cursor_fg,
            marked_fg: p.bright_yellow,
            dir_fg: p.bright_blue,
            // Regular files match the normal panel text by default (what they
            // rendered as before this became its own themable color).
            file_fg: p.fg,
            exec_fg: p.bright_green,
            symlink_fg: p.bright_cyan,
            // Archives = purple, documents = (dark) yellow, images = cyan,
            // audio/video = green — matching Midnight Commander's scheme.
            archive_fg: p.bright_magenta,
            doc_fg: p.yellow,
            image_fg: p.bright_cyan,
            media_fg: p.bright_green,
            // Models = orange, the one warm hue still free.
            model_fg: p.bright_red,
            menubar: Style::default().bg(bar_bg).fg(bar_bg_fg),
            fkey_label: Style::default().bg(bar_bg).fg(bar_bg_fg),
            // Function-key numbers sit on a solid, contrasting "key cap" so they
            // stand out from the colored label cells.
            fkey_num: Style::default()
                .bg(p.bg)
                .fg(best_contrast(p.bg, p.black, p.bright_white))
                .add_modifier(Modifier::BOLD),
            // Dialogs use a neutral surface with cyan title/selection accents…
            dialog_bg: dialog_surface,
            dialog_fg: p.fg,
            dialog_title: p.bright_cyan,
            // The frame matches the title/interior by default (set after the MC
            // override below, so it tracks any per-theme dialog adjustments).
            dialog_border_fg: p.bright_cyan,
            dialog_border_bg: dialog_surface,
            dialog_selection: Style::default()
                .bg(p.bright_cyan)
                .fg(best_contrast(p.bright_cyan, p.bg, p.bright_white))
                .add_modifier(Modifier::BOLD),
            // …while menus get a distinct blue-tinted panel with a blue
            // selection bar, so the two kinds of chrome read differently.
            menu_bg: menu_surface,
            menu_fg: best_contrast(menu_surface, p.black, p.bright_white),
            menu_selection: Style::default()
                .bg(p.bright_blue)
                .fg(best_contrast(p.bright_blue, p.bg, p.bright_white))
                .add_modifier(Modifier::BOLD),
            hotkey_fg: p.bright_yellow,
            input_bg: p.blue,
            input_fg: best_contrast(p.blue, p.bg, p.bright_white),
            button: Style::default().bg(surface).fg(p.fg),
            button_focused: Style::default()
                .bg(p.bright_cyan)
                .fg(p.bg)
                .add_modifier(Modifier::BOLD),
            error_fg: p.bright_red,
            // Derived themes use a vivid blue→magenta gradient; the text over the
            // bars picks whichever of black/white contrasts with its midpoint.
            bar_fg: best_contrast(
                mix(p.bright_blue, p.bright_magenta, 0.5),
                p.black,
                p.bright_white,
            ),
            anim: 0,
            animated: false,
            grad_a: to_rgb(p.bright_blue),
            grad_b: to_rgb(p.bright_magenta),
            grads: [None; GRAD_ROLES],
        };

        // The dialog frame matches the title/interior.
        theme.dialog_border_fg = theme.dialog_title;
        theme.dialog_border_bg = theme.dialog_bg;
        theme
    }

    /// Look up an active theme by name (case-insensitive, ignoring spaces and
    /// dashes), falling back to the default (mc) theme.
    pub fn by_name(name: &str, truecolor: bool) -> Self {
        let key = norm_name(name);
        let spec = ACTIVE
            .read()
            .unwrap()
            .iter()
            .find(|p| norm_name(&p.name) == key)
            .cloned()
            .unwrap_or_else(|| BUILTIN[0].clone());
        Theme::from_spec(&spec, truecolor)
    }

    /// Base style for panel content (background + default foreground).
    pub fn panel_base(&self) -> Style {
        Style::default().bg(self.panel_bg).fg(self.panel_fg)
    }

    /// The gradient color at column `i` of `width` cells. Falls back to a solid
    /// accent color when truecolor is unavailable.
    pub fn gradient_at(&self, i: usize, width: usize) -> Color {
        if !self.truecolor {
            return Color::Rgb(self.grad_a.0, self.grad_a.1, self.grad_a.2);
        }
        let base = if width <= 1 { 0.0 } else { i as f64 / (width - 1) as f64 };
        // When animated, slide a triangle wave so the gradient bounces a→b→a
        // and shifts over time; otherwise a static linear a→b ramp.
        let t = if self.animated { triangle(base * 1.5 + self.anim as f64 * 0.04) } else { base };
        let r = lerp(self.grad_a.0, self.grad_b.0, t);
        let g = lerp(self.grad_a.1, self.grad_b.1, t);
        let b = lerp(self.grad_a.2, self.grad_b.2, t);
        Color::Rgb(r, g, b)
    }

    // -- Per-element gradients ------------------------------------------------

    /// The gradient painting `role`, if this theme defines one. Gradients need
    /// truecolor, so a 16/256-color terminal always gets `None` and the flat
    /// element color it already had.
    pub fn grad(&self, role: GradRole) -> Option<&Grad> {
        if !self.truecolor {
            return None;
        }
        self.grads[role.index()].as_ref()
    }

    /// Whether any element carries a gradient — the cheap check that lets the
    /// screen-wide repaint skip a theme that has none.
    pub fn has_gradients(&self) -> bool {
        self.truecolor && self.grads.iter().any(Option::is_some)
    }

    /// `role`'s gradient color for the cell at `(x, y)` of the region `r` the
    /// ramp spans.
    pub fn grad_color_in(&self, role: GradRole, x: u16, y: u16, r: Rect) -> Option<Color> {
        let g = self.grad(role)?;
        Some(self.grad_color_of(g, g.dir.t(x, y, r)))
    }

    /// `role`'s gradient color at column `i` of a `width`-cell row — for the
    /// one-row bars and the cursor bar, which paint themselves cell by cell.
    pub fn grad_color_at(&self, role: GradRole, i: usize, width: usize) -> Option<Color> {
        let w = width.min(u16::MAX as usize) as u16;
        self.grad_color_in(role, i.min(u16::MAX as usize) as u16, 0, Rect::new(0, 0, w, 1))
    }

    /// The background for one cell of a gradient bar (menu bar, F-key labels,
    /// cursor bar): the element's own gradient, else — on truecolor — the
    /// theme's accent gradient, else `None` for "keep the flat style".
    pub fn bar_bg(&self, role: GradRole, i: usize, width: usize) -> Option<Color> {
        self.grad_color_at(role, i, width)
            .or_else(|| self.truecolor.then(|| self.gradient_at(i, width)))
    }

    /// Interpolate one resolved gradient, sliding it when both the gradient and
    /// the app's animation setting ask for it.
    fn grad_color_of(&self, g: &Grad, t: f64) -> Color {
        let t = if g.animated && self.animated {
            triangle(t * 1.5 + self.anim as f64 * 0.04)
        } else {
            t.clamp(0.0, 1.0)
        };
        Color::Rgb(lerp(g.from.0, g.to.0, t), lerp(g.from.1, g.to.1, t), lerp(g.from.2, g.to.2, t))
    }

    /// The gradient color (full RGB) at normalized position `t` in `[0, 1]`,
    /// honoring the animation slide but **not** the `truecolor` gate — the
    /// pixel-graphics raster always has full color available (even on a sixel
    /// terminal that doesn't advertise truecolor cells), so it draws the real
    /// gradient rather than falling back to a solid accent.
    pub fn gradient_rgb(&self, t: f64) -> (u8, u8, u8) {
        let tt = if self.animated {
            triangle(t * 1.5 + self.anim as f64 * 0.04)
        } else {
            t.clamp(0.0, 1.0)
        };
        (
            lerp(self.grad_a.0, self.grad_b.0, tt),
            lerp(self.grad_a.1, self.grad_b.1, tt),
            lerp(self.grad_a.2, self.grad_b.2, tt),
        )
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::mc()
    }
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t).round().clamp(0.0, 255.0) as u8
}

/// Triangle wave over period 1: 0 → 1 → 0.
fn triangle(x: f64) -> f64 {
    let f = x - x.floor();
    if f < 0.5 { f * 2.0 } else { 2.0 * (1.0 - f) }
}

fn to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (128, 128, 128),
    }
}

/// Mix two colors: `t`=0 → a, `t`=1 → b.
fn mix(a: Color, b: Color, t: f64) -> Color {
    let (ar, ag, ab) = to_rgb(a);
    let (br, bg, bb) = to_rgb(b);
    Color::Rgb(lerp(ar, br, t), lerp(ag, bg, t), lerp(ab, bb, t))
}

/// Pick whichever of `dark`/`light` contrasts better against `bg`.
fn best_contrast(bg: Color, dark: Color, light: Color) -> Color {
    if luma(bg) > 140.0 { dark } else { light }
}

/// Rec. 601 luma (0..=255) of an RGB color.
fn luma(c: Color) -> f64 {
    let (r, g, b) = to_rgb(c);
    0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64
}

/// Ensure `fg` is legible on `bg`. When their luma already differs enough the
/// color is returned unchanged (so accent hues survive on dark surfaces); only
/// when contrast is too low is `fg` blended toward black or white — whichever
/// the background is farther from — until it stands out. Used to keep the
/// per-level heading colors readable on bright dialog backgrounds.
pub(crate) fn readable_on(fg: Color, bg: Color) -> Color {
    const MIN_DIFF: f64 = 96.0;
    let bg_luma = luma(bg);
    let target = if bg_luma < 128.0 { Color::Rgb(255, 255, 255) } else { Color::Rgb(0, 0, 0) };
    let mut out = fg;
    let mut t = 0.0;
    while (luma(out) - bg_luma).abs() < MIN_DIFF && t < 1.0 {
        t += 0.2;
        out = mix(fg, target, t);
    }
    out
}

/// A higher-contrast version of `fg` for dense text: nudge it away from the
/// background — brighter on dark backgrounds, darker on light ones — so body
/// text in the editor/viewer reads crisply (it's softer by default for chrome).
fn contrast_text(fg: Color, bg: Color) -> Color {
    let target = if luma(bg) < 128.0 { Color::Rgb(255, 255, 255) } else { Color::Rgb(0, 0, 0) };
    mix(fg, target, 0.3)
}

/// Normalize a theme name for matching (lower-case, no spaces/dashes).
fn norm_name(name: &str) -> String {
    name.to_ascii_lowercase().replace([' ', '-', '_'], "")
}

/// All active theme names, in file order (built-ins until `themes.toml` loads).
pub fn palette_names() -> Vec<String> {
    ACTIVE.read().unwrap().iter().map(|p| p.name.clone()).collect()
}

/// Whether an active palette matches `name` (fuzzy, like [`Theme::by_name`]).
#[cfg(test)]
fn has_palette(name: &str) -> bool {
    let key = norm_name(name);
    ACTIVE.read().unwrap().iter().any(|p| norm_name(&p.name) == key)
}

/// Curated terminal color schemes (a subset of terminalcolors.com). Each is a
/// standard 16-ANSI palette; the list is data-driven so more can be appended.
// The Rat/Midnight Commander themes are defined explicitly (see
// `rat_commander_spec` and friends), not derived from an ANSI palette, so they
// are intentionally absent from this list.
pub static PALETTES: &[Palette] = &[
    Palette {
        name: "Dracula",
        bg: rgb(0x282a36),
        fg: rgb(0xf8f8f2),
        black: rgb(0x21222c),
        red: rgb(0xff5555),
        green: rgb(0x50fa7b),
        yellow: rgb(0xf1fa8c),
        blue: rgb(0xbd93f9),
        magenta: rgb(0xff79c6),
        cyan: rgb(0x8be9fd),
        white: rgb(0xf8f8f2),
        bright_black: rgb(0x6272a4),
        bright_red: rgb(0xff6e6e),
        bright_green: rgb(0x69ff94),
        bright_yellow: rgb(0xffffa5),
        bright_blue: rgb(0xd6acff),
        bright_magenta: rgb(0xff92df),
        bright_cyan: rgb(0xa4ffff),
        bright_white: rgb(0xffffff),
    },
    Palette {
        name: "Nord",
        bg: rgb(0x2e3440),
        fg: rgb(0xd8dee9),
        black: rgb(0x3b4252),
        red: rgb(0xbf616a),
        green: rgb(0xa3be8c),
        yellow: rgb(0xebcb8b),
        blue: rgb(0x81a1c1),
        magenta: rgb(0xb48ead),
        cyan: rgb(0x88c0d0),
        white: rgb(0xe5e9f0),
        bright_black: rgb(0x4c566a),
        bright_red: rgb(0xbf616a),
        bright_green: rgb(0xa3be8c),
        bright_yellow: rgb(0xebcb8b),
        bright_blue: rgb(0x81a1c1),
        bright_magenta: rgb(0xb48ead),
        bright_cyan: rgb(0x8fbcbb),
        bright_white: rgb(0xeceff4),
    },
    Palette {
        name: "Gruvbox Dark",
        bg: rgb(0x282828),
        fg: rgb(0xebdbb2),
        black: rgb(0x282828),
        red: rgb(0xcc241d),
        green: rgb(0x98971a),
        yellow: rgb(0xd79921),
        blue: rgb(0x458588),
        magenta: rgb(0xb16286),
        cyan: rgb(0x689d6a),
        white: rgb(0xa89984),
        bright_black: rgb(0x928374),
        bright_red: rgb(0xfb4934),
        bright_green: rgb(0xb8bb26),
        bright_yellow: rgb(0xfabd2f),
        bright_blue: rgb(0x83a598),
        bright_magenta: rgb(0xd3869b),
        bright_cyan: rgb(0x8ec07c),
        bright_white: rgb(0xebdbb2),
    },
    Palette {
        name: "Gruvbox Light",
        bg: rgb(0xfbf1c7),
        fg: rgb(0x3c3836),
        black: rgb(0xfbf1c7),
        red: rgb(0xcc241d),
        green: rgb(0x98971a),
        yellow: rgb(0xd79921),
        blue: rgb(0x458588),
        magenta: rgb(0xb16286),
        cyan: rgb(0x689d6a),
        white: rgb(0x7c6f64),
        bright_black: rgb(0x928374),
        bright_red: rgb(0x9d0006),
        bright_green: rgb(0x79740e),
        bright_yellow: rgb(0xb57614),
        bright_blue: rgb(0x076678),
        bright_magenta: rgb(0x8f3f71),
        bright_cyan: rgb(0x427b58),
        bright_white: rgb(0x3c3836),
    },
    Palette {
        name: "Solarized Dark",
        bg: rgb(0x002b36),
        fg: rgb(0x839496),
        black: rgb(0x073642),
        red: rgb(0xdc322f),
        green: rgb(0x859900),
        yellow: rgb(0xb58900),
        blue: rgb(0x268bd2),
        magenta: rgb(0xd33682),
        cyan: rgb(0x2aa198),
        white: rgb(0xeee8d5),
        bright_black: rgb(0x586e75),
        bright_red: rgb(0xcb4b16),
        bright_green: rgb(0x586e75),
        bright_yellow: rgb(0x657b83),
        bright_blue: rgb(0x839496),
        bright_magenta: rgb(0x6c71c4),
        bright_cyan: rgb(0x93a1a1),
        bright_white: rgb(0xfdf6e3),
    },
    Palette {
        name: "Solarized Light",
        bg: rgb(0xfdf6e3),
        fg: rgb(0x657b83),
        black: rgb(0x073642),
        red: rgb(0xdc322f),
        green: rgb(0x859900),
        yellow: rgb(0xb58900),
        blue: rgb(0x268bd2),
        magenta: rgb(0xd33682),
        cyan: rgb(0x2aa198),
        white: rgb(0xeee8d5),
        bright_black: rgb(0x002b36),
        bright_red: rgb(0xcb4b16),
        bright_green: rgb(0x586e75),
        bright_yellow: rgb(0x657b83),
        bright_blue: rgb(0x268bd2),
        bright_magenta: rgb(0x6c71c4),
        bright_cyan: rgb(0x2aa198),
        bright_white: rgb(0x002b36),
    },
    Palette {
        name: "Tokyo Night",
        bg: rgb(0x1a1b26),
        fg: rgb(0xc0caf5),
        black: rgb(0x15161e),
        red: rgb(0xf7768e),
        green: rgb(0x9ece6a),
        yellow: rgb(0xe0af68),
        blue: rgb(0x7aa2f7),
        magenta: rgb(0xbb9af7),
        cyan: rgb(0x7dcfff),
        white: rgb(0xa9b1d6),
        bright_black: rgb(0x414868),
        bright_red: rgb(0xf7768e),
        bright_green: rgb(0x9ece6a),
        bright_yellow: rgb(0xe0af68),
        bright_blue: rgb(0x7aa2f7),
        bright_magenta: rgb(0xbb9af7),
        bright_cyan: rgb(0x7dcfff),
        bright_white: rgb(0xc0caf5),
    },
    Palette {
        name: "Catppuccin Mocha",
        bg: rgb(0x1e1e2e),
        fg: rgb(0xcdd6f4),
        black: rgb(0x45475a),
        red: rgb(0xf38ba8),
        green: rgb(0xa6e3a1),
        yellow: rgb(0xf9e2af),
        blue: rgb(0x89b4fa),
        magenta: rgb(0xf5c2e7),
        cyan: rgb(0x94e2d5),
        white: rgb(0xbac2de),
        bright_black: rgb(0x585b70),
        bright_red: rgb(0xf38ba8),
        bright_green: rgb(0xa6e3a1),
        bright_yellow: rgb(0xf9e2af),
        bright_blue: rgb(0x89b4fa),
        bright_magenta: rgb(0xf5c2e7),
        bright_cyan: rgb(0x94e2d5),
        bright_white: rgb(0xa6adc8),
    },
    Palette {
        name: "One Dark",
        bg: rgb(0x282c34),
        fg: rgb(0xabb2bf),
        black: rgb(0x282c34),
        red: rgb(0xe06c75),
        green: rgb(0x98c379),
        yellow: rgb(0xe5c07b),
        blue: rgb(0x61afef),
        magenta: rgb(0xc678dd),
        cyan: rgb(0x56b6c2),
        white: rgb(0xabb2bf),
        bright_black: rgb(0x5c6370),
        bright_red: rgb(0xe06c75),
        bright_green: rgb(0x98c379),
        bright_yellow: rgb(0xe5c07b),
        bright_blue: rgb(0x61afef),
        bright_magenta: rgb(0xc678dd),
        bright_cyan: rgb(0x56b6c2),
        bright_white: rgb(0xffffff),
    },
    Palette {
        name: "Tomorrow Night",
        bg: rgb(0x1d1f21),
        fg: rgb(0xc5c8c6),
        black: rgb(0x1d1f21),
        red: rgb(0xcc6666),
        green: rgb(0xb5bd68),
        yellow: rgb(0xf0c674),
        blue: rgb(0x81a2be),
        magenta: rgb(0xb294bb),
        cyan: rgb(0x8abeb7),
        white: rgb(0xc5c8c6),
        bright_black: rgb(0x969896),
        bright_red: rgb(0xcc6666),
        bright_green: rgb(0xb5bd68),
        bright_yellow: rgb(0xf0c674),
        bright_blue: rgb(0x81a2be),
        bright_magenta: rgb(0xb294bb),
        bright_cyan: rgb(0x8abeb7),
        bright_white: rgb(0xffffff),
    },
    Palette {
        name: "Cobalt2",
        bg: rgb(0x122738),
        fg: rgb(0xffffff),
        black: rgb(0x000000),
        red: rgb(0xff0000),
        green: rgb(0x38de21),
        yellow: rgb(0xffe50a),
        blue: rgb(0x1460d2),
        magenta: rgb(0xff005d),
        cyan: rgb(0x00bbbb),
        white: rgb(0xbbbbbb),
        bright_black: rgb(0x555555),
        bright_red: rgb(0xf40e17),
        bright_green: rgb(0x3bd01d),
        bright_yellow: rgb(0xedc809),
        bright_blue: rgb(0x5555ff),
        bright_magenta: rgb(0xff55ff),
        bright_cyan: rgb(0x6ae3fa),
        bright_white: rgb(0xffffff),
    },
    Palette {
        name: "Everforest",
        bg: rgb(0x2d353b),
        fg: rgb(0xd3c6aa),
        black: rgb(0x475258),
        red: rgb(0xe67e80),
        green: rgb(0xa7c080),
        yellow: rgb(0xdbbc7f),
        blue: rgb(0x7fbbb3),
        magenta: rgb(0xd699b6),
        cyan: rgb(0x83c092),
        white: rgb(0xd3c6aa),
        bright_black: rgb(0x475258),
        bright_red: rgb(0xe67e80),
        bright_green: rgb(0xa7c080),
        bright_yellow: rgb(0xdbbc7f),
        bright_blue: rgb(0x7fbbb3),
        bright_magenta: rgb(0xd699b6),
        bright_cyan: rgb(0x83c092),
        bright_white: rgb(0xd3c6aa),
    },
    Palette {
        name: "Ayu",
        bg: rgb(0x0a0e14),
        fg: rgb(0xb3b1ad),
        black: rgb(0x01060e),
        red: rgb(0xea6c73),
        green: rgb(0x91b362),
        yellow: rgb(0xf9af4f),
        blue: rgb(0x53bdfa),
        magenta: rgb(0xfae994),
        cyan: rgb(0x90e1c6),
        white: rgb(0xc7c7c7),
        bright_black: rgb(0x686868),
        bright_red: rgb(0xf07178),
        bright_green: rgb(0xc2d94c),
        bright_yellow: rgb(0xffb454),
        bright_blue: rgb(0x59c2ff),
        bright_magenta: rgb(0xffee99),
        bright_cyan: rgb(0x95e6cb),
        bright_white: rgb(0xffffff),
    },
    Palette {
        name: "Nightfox",
        bg: rgb(0x192330),
        fg: rgb(0xcdcecf),
        black: rgb(0x393b44),
        red: rgb(0xc94f6d),
        green: rgb(0x81b29a),
        yellow: rgb(0xdbc074),
        blue: rgb(0x719cd6),
        magenta: rgb(0x9d79d6),
        cyan: rgb(0x63cdcf),
        white: rgb(0xdfdfe0),
        bright_black: rgb(0x575860),
        bright_red: rgb(0xd16983),
        bright_green: rgb(0x8ebaa4),
        bright_yellow: rgb(0xe0c989),
        bright_blue: rgb(0x86abdc),
        bright_magenta: rgb(0xbaa1e2),
        bright_cyan: rgb(0x7ad5d6),
        bright_white: rgb(0xe4e4e5),
    },
    Palette {
        name: "Rose Pine",
        bg: rgb(0x191724),
        fg: rgb(0xe0def4),
        black: rgb(0x26233a),
        red: rgb(0xeb6f92),
        green: rgb(0x31748f),
        yellow: rgb(0xf6c177),
        blue: rgb(0x9ccfd8),
        magenta: rgb(0xc4a7e7),
        cyan: rgb(0xebbcba),
        white: rgb(0xe0def4),
        bright_black: rgb(0x6e6a86),
        bright_red: rgb(0xeb6f92),
        bright_green: rgb(0x31748f),
        bright_yellow: rgb(0xf6c177),
        bright_blue: rgb(0x9ccfd8),
        bright_magenta: rgb(0xc4a7e7),
        bright_cyan: rgb(0xebbcba),
        bright_white: rgb(0xe0def4),
    },
    Palette {
        name: "GitHub Light",
        bg: rgb(0xffffff),
        fg: rgb(0x24292e),
        black: rgb(0x24292e),
        red: rgb(0xd73a49),
        green: rgb(0x28a745),
        yellow: rgb(0xdbab09),
        blue: rgb(0x0366d6),
        magenta: rgb(0x5a32a3),
        cyan: rgb(0x0598bc),
        white: rgb(0x6a737d),
        bright_black: rgb(0x959da5),
        bright_red: rgb(0xcb2431),
        bright_green: rgb(0x22863a),
        bright_yellow: rgb(0xb08800),
        bright_blue: rgb(0x005cc5),
        bright_magenta: rgb(0x5a32a3),
        bright_cyan: rgb(0x3192aa),
        bright_white: rgb(0xd1d5da),
    },
    // Single-hue themes: every color is within the hue family so the whole UI
    // (cursor, bars, gradient) stays monochrome / amber / green.
    Palette {
        name: "Monochrome",
        bg: rgb(0x000000),
        fg: rgb(0xc6c6c6),
        black: rgb(0x000000),
        red: rgb(0x5f5f5f),
        green: rgb(0x8a8a8a),
        yellow: rgb(0xa8a8a8),
        blue: rgb(0x6c6c6c),
        magenta: rgb(0x949494),
        cyan: rgb(0xb0b0b0),
        white: rgb(0xc6c6c6),
        bright_black: rgb(0x3a3a3a),
        bright_red: rgb(0x8a8a8a),
        bright_green: rgb(0xb0b0b0),
        bright_yellow: rgb(0xffffff),
        bright_blue: rgb(0xbdbdbd),
        bright_magenta: rgb(0xf0f0f0),
        bright_cyan: rgb(0xe0e0e0),
        bright_white: rgb(0xffffff),
    },
    Palette {
        name: "Amber CRT",
        bg: rgb(0x160d00),
        fg: rgb(0xffb000),
        black: rgb(0x160d00),
        red: rgb(0xcc7000),
        green: rgb(0xd98a00),
        yellow: rgb(0xe0a000),
        blue: rgb(0xb36b00),
        magenta: rgb(0xc98200),
        cyan: rgb(0xe0a040),
        white: rgb(0xffb000),
        bright_black: rgb(0x5a3c00),
        bright_red: rgb(0xff9030),
        bright_green: rgb(0xffc060),
        bright_yellow: rgb(0xffd000),
        bright_blue: rgb(0xffb000),
        bright_magenta: rgb(0xff8000),
        bright_cyan: rgb(0xffe0a0),
        bright_white: rgb(0xfff0d0),
    },
    Palette {
        name: "Green CRT",
        bg: rgb(0x001000),
        fg: rgb(0x33ff33),
        black: rgb(0x001000),
        red: rgb(0x00aa00),
        green: rgb(0x11cc11),
        yellow: rgb(0x66dd33),
        blue: rgb(0x009900),
        magenta: rgb(0x22bb22),
        cyan: rgb(0x55dd55),
        white: rgb(0x33ff33),
        bright_black: rgb(0x004d00),
        bright_red: rgb(0x55ff55),
        bright_green: rgb(0x88ff88),
        bright_yellow: rgb(0xaaffaa),
        bright_blue: rgb(0x55ff55),
        bright_magenta: rgb(0x00bb00),
        bright_cyan: rgb(0xaaffcc),
        bright_white: rgb(0xccffcc),
    },
    // Rainbow: every ANSI slot is a different hue of the spectrum (red → orange
    // → yellow → green → blue → indigo → violet) over a deep indigo backdrop, so
    // the file list and gradient bars cycle through the full rainbow.
    Palette {
        name: "Rainbow",
        bg: rgb(0x1a1a2e),
        fg: rgb(0xf0f0f0),
        black: rgb(0x1a1a2e),
        red: rgb(0xff3b30),
        green: rgb(0x34c759),
        yellow: rgb(0xffcc00),
        blue: rgb(0x007aff),
        magenta: rgb(0xaf52de),
        cyan: rgb(0x00c7be),
        white: rgb(0xf0f0f0),
        bright_black: rgb(0x4a4a6a),
        bright_red: rgb(0xff6b5e),
        bright_green: rgb(0x5ee87a),
        bright_yellow: rgb(0xffe14d),
        bright_blue: rgb(0x4d9fff),
        bright_magenta: rgb(0xd16bff),
        bright_cyan: rgb(0x4de1d8),
        bright_white: rgb(0xffffff),
    },
    // Candy: a light, pastel sweet-shop palette — mint greens, caramel yellows,
    // peach oranges and grape purples on a pale candy-pink background. The
    // "bright" tints stay medium-saturated so accents read on the light bg.
    Palette {
        name: "Candy",
        bg: rgb(0xfdeef7),
        fg: rgb(0x5d4470),
        black: rgb(0x3a2a4a),
        red: rgb(0xe85d9a),
        green: rgb(0x3fa86a),
        yellow: rgb(0xc8881f),
        blue: rgb(0x7b5fd0),
        magenta: rgb(0xb24fc4),
        cyan: rgb(0x2fa896),
        white: rgb(0x5d4470),
        bright_black: rgb(0xa98fc0),
        bright_red: rgb(0xf26faa),
        bright_green: rgb(0x4fc47e),
        bright_yellow: rgb(0xd99a1f),
        bright_blue: rgb(0x8a6fe0),
        bright_magenta: rgb(0xc45fd6),
        bright_cyan: rgb(0x3fc0a8),
        bright_white: rgb(0x3a2a4a),
    },
    // Neon: saturated electric blues, cyans, reds and greens glowing against a
    // near-black backdrop.
    Palette {
        name: "Neon",
        bg: rgb(0x0a0a12),
        fg: rgb(0xe6f7ff),
        black: rgb(0x0a0a12),
        red: rgb(0xff2d6f),
        green: rgb(0x39ff14),
        yellow: rgb(0xffe93b),
        blue: rgb(0x2d9bff),
        magenta: rgb(0xc724ff),
        cyan: rgb(0x18f0ff),
        white: rgb(0xe6f7ff),
        bright_black: rgb(0x2a2a3a),
        bright_red: rgb(0xff5c8a),
        bright_green: rgb(0x6dff5c),
        bright_yellow: rgb(0xfff45c),
        bright_blue: rgb(0x5cb8ff),
        bright_magenta: rgb(0xe05cff),
        bright_cyan: rgb(0x5cf7ff),
        bright_white: rgb(0xffffff),
    },
    // Forest: earthy browns and a spread of dark-to-light greens (bark, moss,
    // leaf, sage) over a deep woodland backdrop.
    Palette {
        name: "Forest",
        bg: rgb(0x1a2417),
        fg: rgb(0xd8e0c8),
        black: rgb(0x14180f),
        red: rgb(0xb5532e),
        green: rgb(0x5a8c3a),
        yellow: rgb(0xb08540),
        blue: rgb(0x4a7d6a),
        magenta: rgb(0x8a6d4a),
        cyan: rgb(0x6fa86b),
        white: rgb(0xd8e0c8),
        bright_black: rgb(0x4a5a3a),
        bright_red: rgb(0xd57a4a),
        bright_green: rgb(0x8fc46a),
        bright_yellow: rgb(0xd4a85a),
        bright_blue: rgb(0x6fa88c),
        bright_magenta: rgb(0xb08d63),
        bright_cyan: rgb(0x9fd49a),
        bright_white: rgb(0xeef0e0),
    },
    // Freedom: mostly blues and golds over a deep-navy field, with just a touch
    // of red.
    Palette {
        name: "Freedom",
        bg: rgb(0x0a1a3f),
        fg: rgb(0xf0f4ff),
        black: rgb(0x081230),
        red: rgb(0xd83a4a),
        green: rgb(0x4a9d6a),
        yellow: rgb(0xffd23f),
        blue: rgb(0x2b6cff),
        magenta: rgb(0x6d7de0),
        cyan: rgb(0x3fb0e0),
        white: rgb(0xf0f4ff),
        bright_black: rgb(0x3a4a6f),
        bright_red: rgb(0xff5c6a),
        bright_green: rgb(0x6fc78a),
        bright_yellow: rgb(0xffe066),
        bright_blue: rgb(0x5c9bff),
        bright_magenta: rgb(0x8a9bf0),
        bright_cyan: rgb(0x6fd0ff),
        bright_white: rgb(0xffffff),
    },
    // Movienight: the cinematic teal-and-orange grade — deep orange and cyan
    // playing off each other against a dark theatre backdrop.
    Palette {
        name: "Movienight",
        bg: rgb(0x0d1417),
        fg: rgb(0xdfe8ea),
        black: rgb(0x0a0f11),
        red: rgb(0xff6a2b),
        green: rgb(0x3fa890),
        yellow: rgb(0xffa033),
        blue: rgb(0x1f9bb3),
        magenta: rgb(0xe0843f),
        cyan: rgb(0x22c8d8),
        white: rgb(0xdfe8ea),
        bright_black: rgb(0x2a3a3f),
        bright_red: rgb(0xff8c4d),
        bright_green: rgb(0x4fd0b0),
        bright_yellow: rgb(0xffb84d),
        bright_blue: rgb(0x33c0d8),
        bright_magenta: rgb(0xff9a4d),
        bright_cyan: rgb(0x4fe0ee),
        bright_white: rgb(0xf0f8fa),
    },
    // Themes built around their backdrop: each fades the panels and dialogs to a
    // color of its own (see [`SHOWCASE_BACKDROPS`]) instead of the hint of accent
    // every other preset gets.
    //
    // Tron: an icy-blue grid over a black-blue night, with the film's warm red
    // kept for what wants attention — errors, marked files, the column header.
    Palette {
        name: "Tron",
        bg: rgb(0x000b14),
        fg: rgb(0xbfe6f5),
        black: rgb(0x000b14),
        red: rgb(0xff2d20),
        green: rgb(0x2fd6bd),
        yellow: rgb(0xff5a2b),
        blue: rgb(0x0d4d6b),
        magenta: rgb(0x2f7fd6),
        cyan: rgb(0x22a8cc),
        white: rgb(0xbfe6f5),
        bright_black: rgb(0x0d3348),
        bright_red: rgb(0xff4030),
        bright_green: rgb(0x5ff0d8),
        bright_yellow: rgb(0xff6b3d),
        bright_blue: rgb(0x7fe3ff),
        bright_magenta: rgb(0x35a7e8),
        bright_cyan: rgb(0xa9f3ff),
        bright_white: rgb(0xffffff),
    },
    // Graphite: monochrome — no hue anywhere, so the UI is carried entirely by
    // brightness and by the slow fade down the panels.
    Palette {
        name: "Graphite",
        bg: rgb(0x16181c),
        fg: rgb(0xc9ced6),
        black: rgb(0x0d0f12),
        red: rgb(0x8b9198),
        green: rgb(0x9aa1a9),
        yellow: rgb(0xb2b9c1),
        blue: rgb(0x333941),
        magenta: rgb(0x8f959d),
        cyan: rgb(0xa7aeb6),
        white: rgb(0xc9ced6),
        bright_black: rgb(0x2b3037),
        bright_red: rgb(0xa9b0b8),
        bright_green: rgb(0xb9c0c8),
        bright_yellow: rgb(0xffffff),
        bright_blue: rgb(0xaab1b9),
        bright_magenta: rgb(0xd7dce3),
        bright_cyan: rgb(0xeef1f5),
        bright_white: rgb(0xffffff),
    },
    // Synthwave: a violet dusk deepening to magenta down the panels, with the
    // cursor and bars sweeping cyan into hot pink.
    Palette {
        name: "Synthwave",
        bg: rgb(0x1b0b2e),
        fg: rgb(0xf0e6ff),
        black: rgb(0x120720),
        red: rgb(0xff3b6b),
        green: rgb(0x2de2c6),
        yellow: rgb(0xff9f1c),
        blue: rgb(0x3a1f6b),
        magenta: rgb(0xc724b1),
        cyan: rgb(0x00d9ff),
        white: rgb(0xf0e6ff),
        bright_black: rgb(0x3a2158),
        bright_red: rgb(0xff5c7a),
        bright_green: rgb(0x5ff5dc),
        bright_yellow: rgb(0xffd166),
        bright_blue: rgb(0x00e5ff),
        bright_magenta: rgb(0xff2e97),
        bright_cyan: rgb(0x8be9fd),
        bright_white: rgb(0xffffff),
    },
    // Aurora: polar-night blue lit from below by a green curtain, the cursor and
    // bars sweeping sky blue through violet.
    Palette {
        name: "Aurora",
        bg: rgb(0x0a1626),
        fg: rgb(0xd8e6f0),
        black: rgb(0x071120),
        red: rgb(0xff6b81),
        green: rgb(0x3ddc97),
        yellow: rgb(0xffd479),
        blue: rgb(0x1b3a5c),
        magenta: rgb(0xa06bff),
        cyan: rgb(0x37c8d8),
        white: rgb(0xd8e6f0),
        bright_black: rgb(0x16324d),
        bright_red: rgb(0xff8095),
        bright_green: rgb(0x5cf2a8),
        bright_yellow: rgb(0xffe08a),
        bright_blue: rgb(0x6ea8ff),
        bright_magenta: rgb(0xb388ff),
        bright_cyan: rgb(0x7ce9ff),
        bright_white: rgb(0xffffff),
    },
    // Coral Reef: deep water at the top warming to reef dusk at the bottom, with
    // turquoise chrome and coral accents.
    Palette {
        name: "Coral Reef",
        bg: rgb(0x06232e),
        fg: rgb(0xe8f4f2),
        black: rgb(0x041a23),
        red: rgb(0xff6f59),
        green: rgb(0x34d399),
        yellow: rgb(0xffc857),
        blue: rgb(0x0d4a5c),
        magenta: rgb(0xff7eb6),
        cyan: rgb(0x2ec4b6),
        white: rgb(0xe8f4f2),
        bright_black: rgb(0x0e3d4d),
        bright_red: rgb(0xff8a70),
        bright_green: rgb(0x5eead4),
        bright_yellow: rgb(0xffd98a),
        bright_blue: rgb(0x22d3ee),
        bright_magenta: rgb(0xff8fab),
        bright_cyan: rgb(0x7ee8dd),
        bright_white: rgb(0xffffff),
    },
    // Themes built on a pair of opposing hues: one carries the cursor, the bars
    // and the directories, the other the frames, the dialogs and whatever is
    // marked, so the two kinds of chrome never blur into each other. Inside, each
    // dialog washes back towards the first hue (see [`CONTRAST_DIALOGS`]), so the
    // text fields take a color well away from that wash.
    //
    // Anaglyph: the red and cyan of 3D glasses over a near-black screen — a red
    // cursor sweeping into violet, cyan frames around red-washed dialogs, and
    // ice-cyan highlights.
    Palette {
        name: "Anaglyph",
        bg: rgb(0x0b0d11),
        fg: rgb(0xe4eaee),
        black: rgb(0x0b0d11),
        red: rgb(0xd01a30),
        green: rgb(0x2fbf8f),
        yellow: rgb(0xff9aa8),
        blue: rgb(0x0c4a57),
        magenta: rgb(0x7a2fd0),
        cyan: rgb(0x14a8b8),
        white: rgb(0xe4eaee),
        bright_black: rgb(0x28303a),
        bright_red: rgb(0xff7a3d),
        bright_green: rgb(0x9dffb0),
        bright_yellow: rgb(0xa8f4ff),
        bright_blue: rgb(0xf02a40),
        bright_magenta: rgb(0x9a4dff),
        bright_cyan: rgb(0x22d8ec),
        bright_white: rgb(0xffffff),
    },
    // Fire and Ice: a deep-navy night with a cursor burning orange into crimson,
    // and frames, highlights and text fields in cold ice blue around dialogs
    // glowing with embers.
    Palette {
        name: "Fire and Ice",
        bg: rgb(0x0b1426),
        fg: rgb(0xe6eef7),
        black: rgb(0x080f1d),
        red: rgb(0xc8321a),
        green: rgb(0x5fb8e8),
        yellow: rgb(0xb3b8ff),
        blue: rgb(0x163a66),
        magenta: rgb(0xb81e50),
        cyan: rgb(0x6cc6ff),
        white: rgb(0xe6eef7),
        bright_black: rgb(0x2a3a55),
        bright_red: rgb(0xff6b6b),
        bright_green: rgb(0xffc857),
        bright_yellow: rgb(0xc8ecff),
        bright_blue: rgb(0xdc500a),
        bright_magenta: rgb(0xe62a70),
        bright_cyan: rgb(0x7fd4ff),
        bright_white: rgb(0xffffff),
    },
    // Acid: hot magenta against lime on black — a magenta cursor sweeping into
    // violet, lime frames around magenta-washed dialogs, green menus and text
    // fields.
    Palette {
        name: "Acid",
        bg: rgb(0x0c0a0f),
        fg: rgb(0xeef0e6),
        black: rgb(0x0c0a0f),
        red: rgb(0xff2e7e),
        green: rgb(0x9cff2e),
        yellow: rgb(0xff9a3d),
        blue: rgb(0x1f5a0a),
        magenta: rgb(0xb0189a),
        cyan: rgb(0x7cff6a),
        white: rgb(0xeef0e6),
        bright_black: rgb(0x2e2836),
        bright_red: rgb(0xff4f6d),
        bright_green: rgb(0x5cffd0),
        bright_yellow: rgb(0xf4ff6a),
        bright_blue: rgb(0xe0209f),
        bright_magenta: rgb(0xa040ff),
        bright_cyan: rgb(0xa6ff2e),
        bright_white: rgb(0xffffff),
    },
    // Regalia: violet and gold on deep aubergine — a violet cursor sweeping into
    // orchid, gold frames and highlights, dialogs washed with plum.
    Palette {
        name: "Regalia",
        bg: rgb(0x170f26),
        fg: rgb(0xece4f5),
        black: rgb(0x110a1c),
        red: rgb(0xe0457b),
        green: rgb(0x9ccf6a),
        yellow: rgb(0xe8b04a),
        blue: rgb(0x4f2f9a),
        magenta: rgb(0x9a2e88),
        cyan: rgb(0xd9b86a),
        white: rgb(0xece4f5),
        bright_black: rgb(0x3a2e52),
        bright_red: rgb(0xff5c8a),
        bright_green: rgb(0xb8e986),
        bright_yellow: rgb(0xffe27a),
        bright_blue: rgb(0x8a5cff),
        bright_magenta: rgb(0xcc38aa),
        bright_cyan: rgb(0xf2c14e),
        bright_white: rgb(0xffffff),
    },
    // Patina: verdigris and copper on dark bronze — a teal cursor sweeping into
    // blue, copper frames, menus and text fields around verdigris-washed
    // dialogs, pale apricot highlights.
    Palette {
        name: "Patina",
        bg: rgb(0x191410),
        fg: rgb(0xeee4d8),
        black: rgb(0x15100d),
        red: rgb(0xc8553a),
        green: rgb(0x5fa87a),
        yellow: rgb(0xe8b86a),
        blue: rgb(0x5a3218),
        magenta: rgb(0x2a5cc0),
        cyan: rgb(0x2aa89c),
        white: rgb(0xeee4d8),
        bright_black: rgb(0x4a3c30),
        bright_red: rgb(0xff6b5a),
        bright_green: rgb(0x9fe0a0),
        bright_yellow: rgb(0xffdcba),
        bright_blue: rgb(0x118e88),
        bright_magenta: rgb(0x3a78f0),
        bright_cyan: rgb(0xf5a86a),
        bright_white: rgb(0xffffff),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    // -- Per-element gradients ------------------------------------------------

    /// A preset that ships flat, for tests that add their own gradients.
    fn flat_preset() -> ThemeSpec {
        BUILTIN
            .iter()
            .find(|s| s.gradients.is_empty())
            .cloned()
            .expect("the CRT presets ship without gradients")
    }

    /// A spec with `panel_bg` ramping black → white, for the gradient tests.
    fn ramp_spec() -> ThemeSpec {
        let mut spec = flat_preset();
        spec.panel_bg = rgb(0x000000);
        spec.gradients.panel_bg = Some(GradientSpec::new(rgb(0xffffff)));
        spec
    }

    #[test]
    fn a_themes_file_gains_presets_it_has_never_been_offered() {
        // An older file: two themes, and no record of what it has been shown.
        let mine = ThemeSpec { name: "Mine".to_string(), ..BUILTIN[0].clone() };
        let mut tf =
            ThemesFile { known_presets: Vec::new(), theme: vec![mine.clone(), BUILTIN[1].clone()] };

        assert!(add_new_presets(&mut tf), "the presets it is missing are added");
        assert_eq!(tf.theme[0], mine, "the user's own theme keeps its place");
        assert_eq!(tf.theme[1], BUILTIN[1], "so does the preset it already had");
        for preset in BUILTIN.iter() {
            assert!(
                tf.theme.iter().any(|t| t.name == preset.name),
                "{} should now be on offer",
                preset.name
            );
        }
        assert_eq!(tf.theme.len(), BUILTIN.len() + 1, "and nothing is duplicated");

        // Nothing left to add on the next start.
        assert!(!add_new_presets(&mut tf));
    }

    #[test]
    fn a_preset_the_user_deleted_is_not_put_back() {
        // A file that has been offered everything, with two presets removed.
        let mut tf = ThemesFile { known_presets: preset_names(), theme: vec![BUILTIN[0].clone()] };
        assert!(!add_new_presets(&mut tf), "deletions stick");
        assert_eq!(tf.theme.len(), 1);

        // Taking a name off the list asks for that preset back — and only it.
        let wanted = BUILTIN[2].name.clone();
        tf.known_presets.retain(|n| *n != wanted);
        assert!(add_new_presets(&mut tf));
        assert_eq!(tf.theme.len(), 2);
        assert_eq!(tf.theme[1].name, wanted);
    }

    #[test]
    fn an_older_themes_file_is_brought_up_to_date_on_load() {
        // A file as an earlier release left it: one stock preset with no
        // gradients, one theme of the user's own, and no record of what it has
        // been offered.
        let stock = ThemeSpec { gradients: Gradients::default(), ..BUILTIN[0].clone() };
        let mine = ThemeSpec { name: "Mine".to_string(), ..stock.clone() };
        let path =
            std::env::temp_dir().join(format!("rc_upgrade_test_{}.toml", std::process::id()));
        let old = ThemesFile { known_presets: Vec::new(), theme: vec![stock, mine.clone()] };
        std::fs::write(&path, toml::to_string_pretty(&old).unwrap()).unwrap();

        let specs = upgrade_themes_file(&path).expect("the file loads");
        assert!(specs.contains(&mine), "the user's own theme is untouched");
        assert_eq!(
            specs.iter().find(|s| s.name == BUILTIN[0].name),
            Some(&BUILTIN[0]),
            "the preset it already had picks up its gradients"
        );
        for preset in BUILTIN.iter() {
            assert!(
                specs.iter().any(|s| s.name == preset.name),
                "{} should now be on offer",
                preset.name
            );
        }

        // It was written back, record and all, and a second start changes nothing.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("known_presets = ["), "the offer is recorded:\n{text:.400}");
        assert_eq!(upgrade_themes_file(&path).as_ref(), Some(&specs), "idempotent");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn the_offer_record_is_written_before_the_themes() {
        // A plain key after an array of tables would be read as part of the last
        // table, so `known_presets` has to come first — and survive a round trip.
        let specs = builtin_specs();
        let path = std::env::temp_dir().join(format!("rc_offer_test_{}.toml", std::process::id()));
        write_themes(&path, &specs).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        // Match the keys themselves — the header comment mentions both by name.
        let (record, first_theme) = (text.find("\nknown_presets = "), text.find("\n[[theme]]\n"));
        assert!(record.is_some() && record < first_theme, "the record comes first");
        let back: ThemesFile = toml::from_str(&text).unwrap();
        assert_eq!(back.known_presets, preset_names());
        assert_eq!(back.theme, specs);
    }

    #[test]
    fn the_showcase_backdrops_name_real_presets_and_reach_them() {
        let specs = builtin_specs();
        for b in &SHOWCASE_BACKDROPS {
            let spec = specs
                .iter()
                .find(|s| norm_name(&s.name) == norm_name(b.name))
                .unwrap_or_else(|| panic!("{} is not a preset", b.name));
            assert_eq!(spec.gradients.panel_bg.expect("a panel ramp").to, rgb(b.panels));
            assert_eq!(spec.gradients.dialog_bg.expect("a dialog ramp").to, rgb(b.dialogs));
            assert_eq!(spec.gradients.menu_bg.expect("a menu ramp").to, rgb(b.dialogs));
        }
    }

    #[test]
    fn the_contrast_presets_wash_their_dialogs_and_nothing_else() {
        let specs = builtin_specs();
        for (name, wash) in CONTRAST_DIALOGS {
            let spec = specs
                .iter()
                .find(|s| norm_name(&s.name) == norm_name(name))
                .unwrap_or_else(|| panic!("{name} is not a preset"));
            let ramp = spec.gradients.dialog_bg.expect("a dialog ramp");
            assert_eq!(ramp.to, rgb(wash), "{name}: the dialog washes to its own color");
            // The panels and menus keep the ramps every other preset derives.
            let derived = derive_gradients(spec);
            assert_eq!(spec.gradients.panel_bg, derived.panel_bg, "{name}: panels stay derived");
            assert_eq!(spec.gradients.menu_bg, derived.menu_bg, "{name}: menus stay derived");
            // A text field sitting where the wash is deepest must still read as
            // a field rather than a hole in the dialog.
            assert!(
                spread(spec.input_bg, ramp.to) >= 60,
                "{name}: the text fields {:?} vanish into the wash {:?}",
                spec.input_bg,
                ramp.to
            );
        }
    }

    /// The worst contrast `fg` hits anywhere along a `from`→`to` ramp — zero
    /// where the text's brightness falls between the two ends, which is a
    /// background that swallows its own text partway down.
    fn worst_contrast(fg: Color, from: Color, to: Color) -> f64 {
        let (f, a, b) = (luma(fg), luma(from), luma(to));
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        if (lo..=hi).contains(&f) { 0.0 } else { (f - lo).abs().min((f - hi).abs()) }
    }

    #[test]
    fn a_background_ramp_never_swallows_the_text_over_it() {
        // A ramp repaints the whole surface, so text has to stay legible along
        // all of it — not just against the flat color the theme lists. Themes
        // that start out with thin contrast are their own business; what a ramp
        // must not do is take much of what was there away.
        for spec in builtin_specs() {
            for (surface, base, ramp, over) in [
                (
                    "panel",
                    spec.panel_bg,
                    spec.gradients.panel_bg,
                    [spec.panel_fg, spec.file_fg, spec.dir_fg],
                ),
                ("dialog", spec.dialog_bg, spec.gradients.dialog_bg, [spec.dialog_fg; 3]),
                ("menu", spec.menu_bg, spec.gradients.menu_bg, [spec.menu_fg; 3]),
            ] {
                let Some(g) = ramp else { continue };
                let from = g.from.unwrap_or(base);
                for fg in over {
                    // Roughly where terminal text stops being crisp — or all of
                    // the little the theme had to begin with, since a ramp need
                    // not improve on a flat color, only leave it alone.
                    let need = (luma(fg) - luma(base)).abs().min(60.0) - 6.0;
                    let worst = worst_contrast(fg, from, g.to);
                    assert!(
                        worst >= need,
                        "{}: {surface} text {fg:?} is lost along the ramp to {:?} \
                         ({worst:.0}, wanted {need:.0})",
                        spec.name,
                        g.to
                    );
                }
            }
        }
    }

    #[test]
    fn every_preset_but_the_crt_ones_ships_gradients() {
        let specs = builtin_specs();
        for spec in &specs {
            let flat = FLAT_PRESETS.iter().any(|n| norm_name(n) == norm_name(&spec.name));
            assert_eq!(
                spec.gradients.is_empty(),
                flat,
                "{} should{} ship gradients",
                spec.name,
                if flat { " not" } else { "" }
            );
        }
        // The CRT themes are the flat ones, and they are still in the set.
        for name in FLAT_PRESETS {
            assert!(specs.iter().any(|s| s.name == name), "{name} is still a preset");
        }
    }

    #[test]
    fn every_derived_gradient_is_actually_visible() {
        // A ramp whose ends land on the same color is just a flat element with
        // extra lines in themes.toml — every preset's must read as a gradient.
        for spec in builtin_specs() {
            for role in GradRole::ALL {
                let Some(g) = spec.gradients.get(role) else { continue };
                let from = g.from.unwrap_or_else(|| spec.gradient_base(role));
                assert!(
                    spread(from, g.to) >= 24,
                    "{} {role:?}: {from:?} → {:?} is too close to see",
                    spec.name,
                    g.to
                );
            }
        }
    }

    #[test]
    fn an_older_themes_file_gains_the_preset_gradients_but_keeps_edits() {
        // Three themes as an older file held them: an untouched preset, one the
        // user recolored, and one of their own.
        let untouched = ThemeSpec { gradients: Gradients::default(), ..BUILTIN[0].clone() };
        let mut tweaked = untouched.clone();
        tweaked.panel_bg = rgb(0x123456);
        tweaked.name = BUILTIN[1].name.clone();
        let mine = ThemeSpec { name: "Mine".to_string(), ..untouched.clone() };
        let mut specs = vec![untouched, tweaked.clone(), mine.clone()];

        assert!(adopt_preset_gradients(&mut specs), "the untouched preset is upgraded");
        assert_eq!(specs[0].gradients, BUILTIN[0].gradients, "it gains the shipped ramps");
        assert_eq!(specs[0].panel_bg, BUILTIN[0].panel_bg, "and nothing else moves");
        assert_eq!(specs[1], tweaked, "a recolored preset is left alone");
        assert_eq!(specs[2], mine, "so is a theme of the user's own");

        // Running again finds nothing left to do, so the file is not rewritten.
        assert!(!adopt_preset_gradients(&mut specs));
    }

    #[test]
    fn an_older_themes_file_gains_retouched_preset_colors_but_keeps_edits() {
        let retired = retired_presets();
        let old = |name: &str| retired.iter().find(|r| r.name == name).unwrap().clone();
        let builtin = |name: &str| BUILTIN.iter().find(|b| b.name == name).unwrap().clone();

        // Rat Commander as the last release wrote it: the old colors, and ramps
        // derived from them — the active frame fading to the old dim color.
        let rat = old("Rat Commander");
        let shipped = ThemeSpec { gradients: derive_gradients(&rat), ..rat.clone() };
        assert_eq!(shipped.gradients.panel_border_active.unwrap().to, rat.panel_border);
        // The first release with gradients derived a darker menu ramp.
        let mut first = shipped.clone();
        first.gradients.menu_bg.as_mut().unwrap().to = rgb(0x0bb4b4);
        // One from before gradients, under the user's own spelling of the name.
        let flat = ThemeSpec { name: "rat commander".to_string(), ..rat.clone() };
        // A recolored preset, and a theme of the user's own made from the old one.
        let mut tweaked = shipped.clone();
        tweaked.panel_bg = rgb(0x123456);
        let mine = ThemeSpec { name: "Mine".to_string(), ..shipped.clone() };
        let mut specs =
            vec![shipped, first, flat, old("Rat Commander Neon"), tweaked.clone(), mine.clone()];

        assert!(adopt_retouched_presets(&mut specs), "the untouched presets are upgraded");
        assert!(adopt_preset_gradients(&mut specs), "and the flat one gains its ramps");
        let now = builtin("Rat Commander");
        assert_eq!(specs[0], now, "the shipped copy takes the new colors, frame ramp and all");
        assert_eq!(specs[1].gradients.menu_bg.unwrap().to, rgb(0x0bb4b4), "its ramps stay");
        assert_eq!(specs[1], ThemeSpec { gradients: specs[1].gradients.clone(), ..now.clone() });
        assert_eq!(
            specs[2],
            ThemeSpec { name: "rat commander".to_string(), ..now },
            "one from before gradients ends up as the preset, keeping its name"
        );
        assert_eq!(specs[3], builtin("Rat Commander Neon"), "Neon follows what it inherited");
        assert_eq!(specs[4], tweaked, "a recolored preset is left alone");
        assert_eq!(specs[5], mine, "so is a theme of the user's own");

        // Running again finds nothing left to do, so the file is not rewritten.
        assert!(!adopt_retouched_presets(&mut specs));
        assert!(!adopt_preset_gradients(&mut specs));
    }

    #[test]
    fn rat_commander_keeps_its_quiet_colors_legible() {
        let s = BUILTIN.iter().find(|b| b.name == "Rat Commander").unwrap();
        let apart = |a: Color, b: Color| (luma(a) - luma(b)).abs();
        // The dim text is drawn on the panels, the dialogs and the menus, so it
        // has to hold up on every one of them — most of all on the panels.
        assert!(apart(s.panel_border, s.panel_bg) >= 90.0, "dim text on the panel");
        assert!(apart(s.panel_border, s.dialog_bg) >= 60.0, "dim text on a dialog");
        assert!(apart(s.panel_border, s.menu_bg) >= 20.0, "a disabled menu item");
        // …while staying quieter than the text it sits beside.
        assert!(apart(s.panel_border, s.panel_bg) < apart(s.panel_fg, s.panel_bg));
        // The inactive cursor bar — also what marks the viewer's and editor's
        // "Find all" lines — has to show against the panel at all.
        assert!(spread(s.cursor_inactive_bg, s.panel_bg) >= 90, "inactive cursor bar");
        assert!(apart(s.cursor_inactive_fg, s.cursor_inactive_bg) >= 90.0, "its text");
        assert!(apart(s.doc_fg, s.panel_bg) >= 120.0, "documents");
    }

    #[test]
    fn a_gradient_runs_from_the_element_color_to_its_second_endpoint() {
        let t = Theme::from_spec(&ramp_spec(), true);
        let r = Rect::new(0, 0, 10, 1);
        assert_eq!(t.grad_color_in(GradRole::PanelBg, 0, 0, r), Some(rgb(0x000000)));
        assert_eq!(t.grad_color_in(GradRole::PanelBg, 9, 0, r), Some(rgb(0xffffff)));
        // …and interpolates in between.
        let mid = t.grad_color_in(GradRole::PanelBg, 5, 0, r).unwrap();
        assert!(matches!(mid, Color::Rgb(v, _, _) if (100..=180).contains(&v)), "{mid:?}");
        // An element the theme gives no gradient has none.
        assert!(t.grad(GradRole::ButtonBg).is_none());
    }

    #[test]
    fn an_explicit_from_overrides_the_element_color() {
        let mut spec = ramp_spec();
        spec.gradients.panel_bg =
            Some(GradientSpec { from: Some(rgb(0xff0000)), ..GradientSpec::new(rgb(0x00ff00)) });
        let t = Theme::from_spec(&spec, true);
        let r = Rect::new(0, 0, 4, 1);
        assert_eq!(t.grad_color_in(GradRole::PanelBg, 0, 0, r), Some(rgb(0xff0000)));
        assert_eq!(t.grad_color_in(GradRole::PanelBg, 3, 0, r), Some(rgb(0x00ff00)));
    }

    #[test]
    fn gradients_need_truecolor() {
        let t = Theme::from_spec(&ramp_spec(), false);
        assert!(!t.has_gradients(), "no ramps on a 16/256-color terminal");
        assert!(t.grad(GradRole::PanelBg).is_none());
    }

    #[test]
    fn only_an_animated_gradient_moves_with_the_phase() {
        let r = Rect::new(0, 0, 10, 4);
        let still = Theme::from_spec(&ramp_spec(), true);
        let mut moving = {
            let mut spec = ramp_spec();
            spec.gradients.panel_bg.as_mut().unwrap().animated = true;
            Theme::from_spec(&spec, true)
        };
        let mut still = still;
        for t in [&mut still, &mut moving] {
            t.animated = true; // the app-wide animation setting is on
        }
        let sample = |t: &Theme, phase: usize| {
            let mut t = t.clone();
            t.anim = phase;
            t.grad_color_in(GradRole::PanelBg, 3, 1, r)
        };
        assert_eq!(sample(&still, 0), sample(&still, 9), "a still gradient ignores the phase");
        assert_ne!(sample(&moving, 0), sample(&moving, 9), "an animated one drifts");
        // …and stands still again when the app's animations are switched off.
        moving.animated = false;
        assert_eq!(sample(&moving, 0), sample(&moving, 9));
    }

    #[test]
    fn a_direction_picks_the_axis_the_ramp_runs_along() {
        let r = Rect::new(0, 0, 8, 8);
        let of = |dir: GradientDir, x, y| {
            let mut spec = ramp_spec();
            spec.gradients.panel_bg.as_mut().unwrap().direction = dir;
            Theme::from_spec(&spec, true).grad_color_in(GradRole::PanelBg, x, y, r).unwrap()
        };
        // Horizontal varies across x and not down y; vertical is the other way.
        assert_ne!(of(GradientDir::Horizontal, 0, 0), of(GradientDir::Horizontal, 7, 0));
        assert_eq!(of(GradientDir::Horizontal, 3, 0), of(GradientDir::Horizontal, 3, 7));
        assert_ne!(of(GradientDir::Vertical, 0, 0), of(GradientDir::Vertical, 0, 7));
        assert_eq!(of(GradientDir::Vertical, 0, 3), of(GradientDir::Vertical, 7, 3));
        // Diagonal reaches both endpoints only in the corners.
        assert_eq!(of(GradientDir::Diagonal, 0, 0), rgb(0x000000));
        assert_eq!(of(GradientDir::Diagonal, 7, 7), rgb(0xffffff));
        // Radial starts in the middle and brightens outwards.
        assert_eq!(of(GradientDir::Radial, 3, 3), of(GradientDir::Radial, 4, 4));
        assert!(luma(of(GradientDir::Radial, 0, 0)) > luma(of(GradientDir::Radial, 4, 4)));
    }

    #[test]
    fn gradients_round_trip_through_themes_toml() {
        let mut spec = ramp_spec();
        spec.gradients.cursor_bg = Some(GradientSpec {
            from: Some(rgb(0x102030)),
            to: rgb(0x405060),
            direction: GradientDir::Radial,
            animated: true,
        });
        let text = toml::to_string_pretty(&ThemesFile {
            known_presets: Vec::new(),
            theme: vec![spec.clone()],
        })
        .unwrap();
        assert!(
            text.contains("[theme.gradients.panel_bg]"),
            "gradients get their own table:\n{text}"
        );
        assert!(text.contains("direction = \"radial\""), "the direction is a plain word:\n{text}");
        let back: ThemesFile = toml::from_str(&text).unwrap();
        assert_eq!(back.theme[0], spec);
        // An absent `from` stays absent rather than being written out.
        assert_eq!(text.matches("\nfrom =").count(), 1, "only cursor_bg names a `from`:\n{text}");
    }

    #[test]
    fn a_theme_without_gradients_writes_no_table_and_still_loads() {
        let spec = flat_preset(); // the CRT themes ship without gradients
        assert!(spec.gradients.is_empty());
        let text = toml::to_string_pretty(&ThemesFile {
            known_presets: Vec::new(),
            theme: vec![spec.clone()],
        })
        .unwrap();
        assert!(!text.contains("gradients"), "nothing is written for a flat theme:\n{text}");
        let back: ThemesFile = toml::from_str(&text).unwrap();
        assert_eq!(back.theme[0], spec);
    }

    #[test]
    fn a_bar_falls_back_to_the_accent_gradient_until_it_has_its_own() {
        // A theme with no gradient of its own keeps the accent ramp bars always had.
        let stock = Theme::from_spec(&flat_preset(), true);
        assert_eq!(stock.bar_bg(GradRole::MenubarBg, 0, 10), Some(stock.gradient_at(0, 10)));
        // Given its own gradient, the bar uses that instead.
        let mut spec = flat_preset();
        spec.menubar_bg = rgb(0x000000);
        spec.gradients.menubar_bg = Some(GradientSpec::new(rgb(0xffffff)));
        let own = Theme::from_spec(&spec, true);
        assert_eq!(own.bar_bg(GradRole::MenubarBg, 0, 10), Some(rgb(0x000000)));
        assert_eq!(own.bar_bg(GradRole::MenubarBg, 9, 10), Some(rgb(0xffffff)));
        // Without truecolor there is no ramp at all, own gradient or not.
        assert_eq!(Theme::from_spec(&spec, false).bar_bg(GradRole::MenubarBg, 0, 10), None);
    }

    #[test]
    fn switching_a_gradient_on_starts_from_a_visible_ramp() {
        // Dark elements ramp towards white, light ones towards black, so the
        // theme editor shows something the moment the ramp is switched on.
        assert!(luma(GradientSpec::default_for(rgb(0x101010)).to) > luma(rgb(0x101010)));
        assert!(luma(GradientSpec::default_for(rgb(0xf0f0f0)).to) < luma(rgb(0xf0f0f0)));
    }

    #[test]
    fn the_neon_preset_ships_still_backgrounds_and_moving_chrome() {
        let neon = BUILTIN.iter().find(|s| s.name == "Rat Commander Neon").expect("preset present");
        let g = &neon.gradients;
        for still in [&g.panel_bg, &g.dialog_bg, &g.menu_bg] {
            assert!(!still.expect("background ramps").animated, "backgrounds stay still");
        }
        assert!(g.cursor_bg.unwrap().animated, "the cursor drifts");
        assert!(g.menubar_bg.unwrap().animated, "so do the bars");
    }

    #[test]
    fn readable_on_only_adjusts_low_contrast_colors() {
        // A bright accent on a dark background already contrasts — left as-is.
        let accent = rgb(0xffd75f); // light yellow
        assert_eq!(readable_on(accent, rgb(0x101010)), accent, "kept on a dark bg");

        // The same bright accent on a bright dialog background is illegible, so it
        // is darkened until it stands out.
        let bright_bg = rgb(0xf5f5f5);
        let fixed = readable_on(accent, bright_bg);
        assert_ne!(fixed, accent, "adjusted on a bright bg");
        assert!(
            (luma(fixed) - luma(bright_bg)).abs() >= 96.0,
            "the result has adequate contrast with the background"
        );
    }

    #[test]
    fn editing_one_component_changes_only_that_element() {
        let mut spec = builtin_specs()[0].clone(); // Rat Commander (the default)
        let base = Theme::from_spec(&spec, true);
        // Give the dialog a completely different background — directly, no mixing.
        spec.dialog_bg = rgb(0x123456);
        let edited = Theme::from_spec(&spec, true);
        assert_eq!(edited.dialog_bg, rgb(0x123456), "dialog bg follows the spec verbatim");
        // Unrelated elements are untouched.
        assert_eq!(edited.panel_bg, base.panel_bg);
        assert_eq!(edited.menu_bg, base.menu_bg);
        assert_eq!(edited.cursor.bg, base.cursor.bg);
        assert_eq!(edited.input_bg, base.input_bg);
    }

    #[test]
    fn migration_fills_missing_file_fg_from_panel_fg() {
        // Simulate a pre-upgrade file by stripping the `file_fg` lines.
        let spec = builtin_specs()[0].clone();
        let full = toml::to_string_pretty(&ThemesFile {
            known_presets: Vec::new(),
            theme: vec![spec.clone()],
        })
        .unwrap();
        let old: String = full
            .lines()
            .filter(|l| !l.trim_start().starts_with("file_fg"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!old.contains("file_fg"), "precondition: field removed");

        let migrated = migrate_theme_toml(&old).expect("an old file should migrate");
        let back: ThemesFile = toml::from_str(&migrated).unwrap();
        assert_eq!(
            back.theme[0].file_fg, spec.panel_fg,
            "file_fg is migrated to the theme's own panel_fg"
        );
        // A file that already has the field is left untouched.
        assert!(migrate_theme_toml(&full).is_none(), "no-op when nothing is missing");
    }

    #[test]
    fn builtin_themes_serialize_and_reparse() {
        let specs = builtin_specs();
        assert!(specs.len() >= 10, "expected the full preset set");
        let body =
            toml::to_string_pretty(&ThemesFile { known_presets: Vec::new(), theme: specs.clone() })
                .unwrap();
        let back: ThemesFile = toml::from_str(&body).unwrap();
        assert_eq!(back.theme.len(), specs.len());
        for (a, b) in specs.iter().zip(&back.theme) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.panel_bg, b.panel_bg, "{} panel_bg", a.name);
            assert_eq!(a.menu_bg, b.menu_bg, "{} menu_bg", a.name);
            assert_eq!(a.dialog_border_fg, b.dialog_border_fg, "{} dialog_border_fg", a.name);
            assert_eq!(a.cursor_bg, b.cursor_bg, "{} cursor_bg", a.name);
            assert_eq!(a, b, "{} round-trips whole, gradients included", a.name);
        }
        // A gradient theme writes `[theme.gradients.*]` sub-tables, which have to
        // sit after its own plain keys and must not swallow the presets after it.
        let with_gradients = specs.iter().position(|s| !s.gradients.is_empty());
        assert!(
            with_gradients.is_some_and(|i| i < specs.len() - 1),
            "a gradient preset is followed by more themes"
        );
    }

    #[test]
    fn generated_file_has_header_and_reparses() {
        let path = std::env::temp_dir().join(format!("rc_themes_test_{}.toml", std::process::id()));
        write_themes(&path, &builtin_specs()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Rat Commander themes"), "has a header comment");
        assert!(text.contains("[[theme]]") && text.contains("dialog_bg = \"#"));
        let tf: ThemesFile = toml::from_str(&text).unwrap();
        assert_eq!(tf.theme.len(), builtin_specs().len());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn hex_parsing_accepts_common_forms() {
        assert_eq!(parse_hex("#00a3a3"), Some(rgb(0x00a3a3)));
        assert_eq!(parse_hex("00a3a3"), Some(rgb(0x00a3a3)));
        assert_eq!(parse_hex("0x00A3A3"), Some(rgb(0x00a3a3)));
        assert_eq!(parse_hex("  #ffffff "), Some(rgb(0xffffff)));
        assert_eq!(parse_hex("#fff"), None, "3-digit not supported");
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn palette_lookup_is_fuzzy() {
        assert!(has_palette("Dracula"));
        assert!(has_palette("tokyo night"));
        assert!(has_palette("rose-pine"));
        assert!(!has_palette("nonsense"));
    }

    #[test]
    fn gradient_interpolates_endpoints() {
        let t = Theme::by_name("Dracula", true);
        let a = t.gradient_at(0, 10);
        let b = t.gradient_at(9, 10);
        assert!(matches!(a, Color::Rgb(..)));
        assert_ne!(a, b, "gradient should vary across the width");
    }

    #[test]
    fn no_truecolor_means_solid_bar() {
        let t = Theme::by_name("Nord", false);
        assert_eq!(t.gradient_at(0, 10), t.gradient_at(9, 10));
    }

    #[test]
    fn non_mc_menu_bar_follows_the_accent_gradient_not_raw_cyan() {
        // Non-cyan themes no longer paint the menu/F-key bar with their raw
        // `cyan` palette slot; it sits at the middle of the theme's accent
        // gradient (the colour the truecolor bars fade through).
        for name in ["Dracula", "Nord", "Gruvbox Dark", "Tokyo Night"] {
            let t = Theme::by_name(name, true);
            let p = PALETTES.iter().find(|p| p.name == name).unwrap();
            assert_ne!(
                t.menubar.bg,
                Some(p.cyan),
                "{name} menu bar should not use the raw cyan slot"
            );
            // It sits at the middle of the theme's accent gradient, so the F9 bar
            // reads like the rest of the theme's chrome — and matches the F-key bar.
            assert_eq!(t.menubar.bg, t.fkey_label.bg, "{name} menu and F-key bars match");
            assert_eq!(
                t.menubar.bg,
                Some(mix(p.bright_blue, p.bright_magenta, 0.5)),
                "{name} bar = accent-gradient midpoint",
            );
        }
    }

    #[test]
    fn both_mc_themes_use_signature_teal() {
        for name in ["Rat Commander", "Midnight Commander Dark"] {
            let t = Theme::by_name(name, true);
            assert_eq!(t.cursor.bg, Some(MC_TEAL), "{name} cursor bg");
            assert_eq!(t.cursor.fg, Some(rgb(0x000000)), "{name} cursor fg");
            assert_eq!(t.menubar.bg, Some(MC_TEAL), "{name} menubar bg");
            assert_eq!(t.fkey_label.bg, Some(MC_TEAL), "{name} fkey bar bg");
            // In truecolor the bars/cursor are drawn via the gradient. It should
            // still shift (some gradient) but stay in the teal family (g ≈ b,
            // red kept low) so it reads as cyan, not blue→magenta.
            let (a, b) = (t.gradient_at(0, 20), t.gradient_at(19, 20));
            assert_ne!(a, b, "{name} gradient should still vary");
            for c in [a, b] {
                if let Color::Rgb(r, g, bl) = c {
                    assert!(r < g && r < bl, "{name} gradient stop {c:?} not teal");
                    assert!(g.abs_diff(bl) < 40, "{name} gradient stop {c:?} not cyan-ish");
                }
            }
        }
    }

    #[test]
    fn mc_theme_uses_classic_two_tone_chrome() {
        let t = Theme::by_name("Midnight Commander", true);
        let cyan = rgb(0x0dcdcd);
        let black = rgb(0x000000);
        // Dialogs: light "paper" background, black text, blue titles.
        assert_eq!(t.dialog_bg, rgb(0xc6c6c6));
        assert_eq!(t.dialog_fg, black);
        assert_eq!(t.dialog_title, rgb(0x0d73cc));
        // Teal selection bars / input fields inside dialogs.
        assert_eq!(t.dialog_selection.bg, Some(cyan));
        assert_eq!(t.button_focused.bg, Some(cyan));
        assert_eq!(t.input_bg, cyan);
        assert_eq!(t.input_fg, black);
        // Menus stay bright cyan with white text and a black selection bar.
        assert_eq!(t.menu_bg, cyan);
        assert_eq!(t.menu_fg, rgb(0xffffff));
        assert_eq!(t.menu_selection.bg, Some(black));
    }

    #[test]
    fn text_fg_is_more_contrasty_than_panel_fg() {
        // For both dark and light themes, the editor/viewer text color should be
        // further (in luma) from the background than the default panel foreground.
        for name in ["Dracula", "Nord", "Gruvbox Dark", "Gruvbox Light", "Solarized Light"] {
            let t = Theme::by_name(name, true);
            let d_text = (luma(t.text_fg) - luma(t.panel_bg)).abs();
            let d_panel = (luma(t.panel_fg) - luma(t.panel_bg)).abs();
            assert!(
                d_text >= d_panel,
                "{name}: text_fg ({d_text}) should contrast at least as much as panel_fg ({d_panel})"
            );
            assert_ne!(t.text_fg, t.panel_fg, "{name}: text_fg should differ from panel_fg");
        }
    }

    #[test]
    fn non_mc_themes_distinguish_menus_from_dialogs() {
        for name in ["Dracula", "Nord", "Gruvbox Dark", "Gruvbox Light", "Tokyo Night", "Ayu"] {
            let t = Theme::by_name(name, true);
            assert_ne!(t.menu_bg, t.dialog_bg, "{name} menu/dialog bg identical");
            assert_ne!(
                t.menu_selection.bg, t.dialog_selection.bg,
                "{name} menu/dialog selection identical"
            );
        }
    }

    #[test]
    fn new_themes_are_registered_and_build() {
        for name in ["Rainbow", "Candy", "Neon", "Forest", "Freedom", "Movienight"] {
            assert!(has_palette(name), "{name} palette missing");
            let t = Theme::by_name(name, true);
            assert_eq!(t.name, name);
            // Sanity: distinct bg/fg and a non-trivial gradient.
            assert_ne!(t.panel_bg, t.panel_fg, "{name} bg == fg");
            assert_ne!(t.gradient_at(0, 10), t.gradient_at(9, 10), "{name} flat gradient");
        }
    }

    #[test]
    fn rat_commander_is_default_and_commander_themes_are_registered() {
        // All three adopted themes are present and build, and the old no-space
        // "MidnightCommander Classic" is gone (replaced by Rat Commander).
        for name in ["Rat Commander", "Midnight Commander", "Midnight Commander Dark"] {
            assert!(has_palette(name), "{name} missing");
            assert_eq!(Theme::by_name(name, true).name, name);
        }
        assert!(!has_palette("MidnightCommander Classic"), "old classic theme should be gone");

        // Rat Commander is the first built-in, hence the default.
        assert_eq!(builtin_specs()[0].name, "Rat Commander");
        assert_eq!(Theme::mc().name, "Rat Commander");
        assert_eq!(crate::config::Config::default().theme, "Rat Commander");

        // Its signature colors: deep-blue panels, teal selection bar, light dialogs.
        let t = Theme::by_name("Rat Commander", true);
        assert_eq!(t.panel_bg, rgb(0x0000cd));
        assert_eq!(t.cursor.bg, Some(MC_TEAL));
        assert_eq!(t.cursor.fg, Some(rgb(0x000000)));
        assert_eq!(t.dialog_bg, rgb(0xc6c6c6));
        assert_eq!(t.archive_fg, rgb(0xff55ff));
    }
}
