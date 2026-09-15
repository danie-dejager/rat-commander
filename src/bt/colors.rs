//! Template colours on screen. 010 Editor colours are `0xBBGGRR` pastels made
//! for a white background, so they are laid over the theme's background as a
//! tint — strong enough to tell structures apart, faint enough to keep the
//! bytes readable on a dark theme.

use super::tree::NO_COLOR;
use crate::ui::theme::Theme;
use ratatui::style::{Color, Style};

/// The colours of the template styles (`style=sHeading1`, …), as 010 Editor's
/// light theme draws them: `(background, foreground)`.
const STYLE_COLORS: &[(u32, u32)] = &[
    (NO_COLOR, NO_COLOR), // sNone
    (0xF5D3B8, NO_COLOR), // sHeading1
    (0xE8A970, NO_COLOR), // sHeading1Accent
    (0xB8E6C8, NO_COLOR), // sHeading2
    (0x7FCF98, NO_COLOR), // sHeading2Accent
    (0xB8D9F5, NO_COLOR), // sHeading3
    (0x70B4EB, NO_COLOR), // sHeading3Accent
    (0xE6B8E6, NO_COLOR), // sHeading4
    (0xCF7FCF, NO_COLOR), // sHeading4Accent
    (0xF0E0C8, NO_COLOR), // sSection1
    (0xDCC09A, NO_COLOR), // sSection1Accent
    (0xD2F0E1, NO_COLOR), // sSection2
    (0xA8DCC0, NO_COLOR), // sSection2Accent
    (0xC8E1F0, NO_COLOR), // sSection3
    (0x9AC4DC, NO_COLOR), // sSection3Accent
    (0xE6D2F0, NO_COLOR), // sSection4
    (0xCCA8DC, NO_COLOR), // sSection4Accent
    (0x9EF5F5, NO_COLOR), // sMarker
    (0x30D8F0, NO_COLOR), // sMarkerAccent
    (0xE6E6E6, NO_COLOR), // sData
    (0xC8C8C8, NO_COLOR), // sDataAccent
];

fn rgb(c: u32) -> Color {
    Color::Rgb((c & 0xff) as u8, ((c >> 8) & 0xff) as u8, ((c >> 16) & 0xff) as u8)
}

fn luma(c: Color) -> f32 {
    match c {
        Color::Rgb(r, g, b) => 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32,
        _ => 0.0,
    }
}

/// The style for bytes a template coloured `fg` / `bg` (either may be
/// [`NO_COLOR`]) or gave template style `style`; `None` when it didn't colour
/// them at all.
pub fn byte_style(theme: &Theme, fg: u32, bg: u32, style: u8) -> Option<Style> {
    let (fg, bg) = if fg == NO_COLOR && bg == NO_COLOR {
        STYLE_COLORS.get(style as usize).copied().unwrap_or((NO_COLOR, NO_COLOR))
    } else {
        (fg, bg)
    };
    if fg == NO_COLOR && bg == NO_COLOR {
        return None;
    }
    let base = theme.panel_bg;
    let dark = luma(base) < 128.0;
    let back = if bg == NO_COLOR {
        base
    } else {
        crate::ui::dialog::widgets::mix_rgb(base, rgb(bg), if dark { 0.38 } else { 0.7 })
    };
    let fore = if fg == NO_COLOR { theme.text_fg } else { rgb(fg) };
    Some(Style::default().fg(crate::ui::theme::readable_on(fore, back)).bg(back))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_bgr_and_tinted_toward_the_background() {
        assert_eq!(rgb(0x0000ff), Color::Rgb(255, 0, 0));
        let theme = Theme::by_name("Default", true);
        assert!(byte_style(&theme, NO_COLOR, NO_COLOR, 0).is_none());
        let s = byte_style(&theme, NO_COLOR, 0x0000ff, 0).unwrap();
        assert_ne!(s.bg, Some(Color::Rgb(255, 0, 0)), "a tint, not the raw colour");
        assert!(byte_style(&theme, NO_COLOR, NO_COLOR, 1).is_some(), "styles have colours");
    }
}
