//! Gruvbox, dark variant.
//!
//! Gruvbox's signature hue is its bright orange, but orange cannot be the
//! accent here: it is only ~20° from the bright red, and the one-character
//! glyph rule (see `tokyo_night`) needs the warm severity slot to be
//! unmistakable. So warning takes the family's **yellow** (`#fabd2f`), danger
//! the bright red (`#fb4934`), and the accent falls back to the muted blue
//! (`#83a598`) — which also keeps the accent from competing with a warning
//! glyph on the same row.

use ratatui::style::Color;

use super::Theme;

impl Theme {
    /// Gruvbox dark, medium contrast.
    pub const fn gruvbox_dark() -> Self {
        Self {
            background: Color::Rgb(0x28, 0x28, 0x28),
            surface: Color::Rgb(0x3c, 0x38, 0x36),
            surface_selected: Color::Rgb(0x50, 0x49, 0x45),

            accent: Color::Rgb(0x83, 0xa5, 0x98),
            accent_dim: Color::Rgb(0x66, 0x5c, 0x54),

            text_primary: Color::Rgb(0xeb, 0xdb, 0xb2),
            text_secondary: Color::Rgb(0xd5, 0xc4, 0xa1),
            text_muted: Color::Rgb(0x92, 0x83, 0x74),

            success: Color::Rgb(0xb8, 0xbb, 0x26),
            info: Color::Rgb(0x8e, 0xc0, 0x7c),
            warning: Color::Rgb(0xfa, 0xbd, 0x2f),
            danger: Color::Rgb(0xfb, 0x49, 0x34),

            destructive: Color::Rgb(0xcc, 0x24, 0x1d),
            destructive_surface: Color::Rgb(0x3c, 0x28, 0x28),
        }
    }
}
