//! The map's colours, taken from the theme so the map belongs to whatever
//! theme is active — no colours of its own for a theme to define.

use crate::ui::graphics::raster::{Rgb, over, rgb, shade};
use crate::ui::theme::Theme;
use ratatui::style::Color;

/// Colours for the pixel map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MapPalette {
    pub ocean: Rgb,
    pub land: Rgb,
    pub coast: Rgb,
    pub border: Rgb,
    pub river: Rgb,
    pub city: Rgb,
    pub label: Rgb,
    /// The GeoJSON: its lines and points, and its areas' fill.
    pub feature: Rgb,
    /// GeoJSON other than the object chosen in the list.
    pub feature_dim: Rgb,
    /// The feature picked out.
    pub selected: Rgb,
}

impl MapPalette {
    pub fn from_theme(theme: &Theme) -> Self {
        let bg = rgb(theme.panel_bg);
        let text = rgb(theme.text_fg);
        let land = over(bg, text, 0.16);
        let feature = rgb(theme.marked_fg);
        MapPalette {
            // The sea a shade darker than the panel, the land a shade toward
            // the text colour: apart on any theme, dark or light.
            ocean: shade(bg, 0.82),
            land,
            coast: over(bg, text, 0.55),
            border: over(land, text, 0.38),
            river: over(land, rgb(theme.symlink_fg), 0.55),
            city: over(bg, text, 0.75),
            label: text,
            feature,
            feature_dim: over(bg, feature, 0.45),
            selected: distinct(
                feature,
                [rgb(theme.hotkey_fg), rgb(theme.error_fg), rgb(theme.exec_fg)],
            ),
        }
    }
}

/// The first of `candidates` that stands apart from `from`.
fn distinct(from: Rgb, candidates: [Rgb; 3]) -> Rgb {
    let dist = |a: Rgb, b: Rgb| {
        let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
        d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
    };
    candidates.into_iter().find(|&c| dist(c, from) > 90 * 90).unwrap_or(candidates[1])
}

/// Colours for the map drawn in character cells.
#[derive(Debug, Clone, Copy)]
pub struct CellPalette {
    pub ocean: Color,
    pub land: Color,
    pub feature_fill: Color,
    pub coast: Color,
    pub border: Color,
    pub river: Color,
    pub feature: Color,
    pub feature_dim: Color,
    pub selected: Color,
    pub label: Color,
}

impl CellPalette {
    /// The pixel palette's colours on a truecolor terminal; otherwise the
    /// theme's own, which a 16-colour terminal can show.
    pub fn from_theme(theme: &Theme) -> Self {
        if theme.truecolor {
            let p = MapPalette::from_theme(theme);
            let c = |x: Rgb| Color::Rgb(x.0, x.1, x.2);
            return CellPalette {
                ocean: c(p.ocean),
                land: c(p.land),
                feature_fill: c(over(p.land, p.feature, 0.3)),
                coast: c(p.coast),
                border: c(p.border),
                river: c(p.river),
                feature: c(p.feature),
                feature_dim: c(p.feature_dim),
                selected: c(p.selected),
                label: c(p.label),
            };
        }
        CellPalette {
            ocean: theme.panel_bg,
            land: theme.cursor_inactive.bg.unwrap_or(theme.dialog_bg),
            feature_fill: theme.cursor_inactive.bg.unwrap_or(theme.dialog_bg),
            coast: theme.text_fg,
            border: theme.panel_border,
            river: theme.symlink_fg,
            feature: theme.marked_fg,
            feature_dim: theme.panel_fg,
            selected: theme.hotkey_fg,
            label: theme.text_fg,
        }
    }
}
