//! The Catppuccin family: `mocha` (dark) and `latte` (light).
//!
//! Catppuccin is a low-chroma pastel family, which is exactly where the
//! one-character-glyph rule bites: upstream's peach (`#fab387`) and red
//! (`#f38ba8`) are both washed-out warm tones and blur together at one
//! character. So warning is taken from the family's **yellow** rather than its
//! peach — the widest warm/red separation Catppuccin offers — and the
//! destructive red is the deeper, more saturated red of the *other* flavour, so
//! a red report and a red button still do not look alike.

use ratatui::style::Color;

use super::Theme;

impl Theme {
    /// Catppuccin Mocha — the family's darkest flavour.
    pub const fn catppuccin_mocha() -> Self {
        Self {
            background: Color::Rgb(0x1e, 0x1e, 0x2e),
            surface: Color::Rgb(0x31, 0x32, 0x44),
            surface_selected: Color::Rgb(0x45, 0x47, 0x5a),

            accent: Color::Rgb(0x89, 0xb4, 0xfa),
            accent_dim: Color::Rgb(0x58, 0x5b, 0x70),

            text_primary: Color::Rgb(0xcd, 0xd6, 0xf4),
            text_secondary: Color::Rgb(0xba, 0xc2, 0xde),
            text_muted: Color::Rgb(0x7f, 0x84, 0x9c),

            success: Color::Rgb(0xa6, 0xe3, 0xa1),
            info: Color::Rgb(0x89, 0xdc, 0xeb),
            // Yellow, not peach: see the module note.
            warning: Color::Rgb(0xf9, 0xe2, 0xaf),
            danger: Color::Rgb(0xf3, 0x8b, 0xa8),

            destructive: Color::Rgb(0xd2, 0x0f, 0x39),
            destructive_surface: Color::Rgb(0x3a, 0x24, 0x34),
        }
    }

    /// Catppuccin Latte — the family's light flavour.
    ///
    /// The pastels are replaced by their Latte counterparts, which are darker
    /// and more saturated precisely because they have to sit on `#eff1f5`.
    pub const fn catppuccin_latte() -> Self {
        Self {
            background: Color::Rgb(0xef, 0xf1, 0xf5),
            surface: Color::Rgb(0xcc, 0xd0, 0xda),
            surface_selected: Color::Rgb(0xbc, 0xc0, 0xcc),

            accent: Color::Rgb(0x1e, 0x66, 0xd5),
            accent_dim: Color::Rgb(0x9c, 0xa0, 0xb0),

            text_primary: Color::Rgb(0x4c, 0x4f, 0x69),
            text_secondary: Color::Rgb(0x6c, 0x6f, 0x85),
            text_muted: Color::Rgb(0x8c, 0x8f, 0xa1),

            success: Color::Rgb(0x40, 0xa0, 0x2b),
            info: Color::Rgb(0x04, 0xa5, 0xe5),
            warning: Color::Rgb(0xdf, 0x8e, 0x1d),
            danger: Color::Rgb(0xd2, 0x0f, 0x39),

            destructive: Color::Rgb(0x9e, 0x0b, 0x2c),
            destructive_surface: Color::Rgb(0xf2, 0xd3, 0xda),
        }
    }
}
