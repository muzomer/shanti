//! The Tokyo Night family: `night` (the default), `storm` and `day`.
//!
//! One rule governs every constructor here, and every other scheme module: the
//! smallest colour target in this UI is a status glyph one character wide, so
//! [`Theme::warning`] and [`Theme::danger`] must stay far enough apart in hue
//! *and* saturation to be told apart at that size. Tokyo Night is the family
//! this rule was written from — its orange (`#ff9e64`) and its pink-red
//! (`#f7768e`) sit roughly 40° apart with clearly different chroma, so `↑ ahead`
//! never reads as `✘ gone`.
//!
//! The three variants differ only in how light the surfaces are; the severity
//! hues are shared, because a user who switches from `night` to `storm` is
//! changing the room's lighting, not re-learning what orange means.

use ratatui::style::Color;

use super::Theme;

impl Theme {
    /// Tokyo Night, the "night" variant — the darkest surfaces of the family,
    /// and what the application looks like out of the box.
    pub const fn tokyo_night() -> Self {
        Self {
            background: Color::Rgb(0x1a, 0x1b, 0x26),
            surface: Color::Rgb(0x29, 0x2e, 0x42),
            surface_selected: Color::Rgb(0x3d, 0x59, 0xa1),

            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            accent_dim: Color::Rgb(0x39, 0x4b, 0x70),

            text_primary: Color::Rgb(0xc0, 0xca, 0xf5),
            text_secondary: Color::Rgb(0xa9, 0xb1, 0xd6),
            text_muted: Color::Rgb(0x56, 0x5f, 0x89),

            success: Color::Rgb(0x9e, 0xce, 0x6a),
            info: Color::Rgb(0x7d, 0xcf, 0xff),
            warning: Color::Rgb(0xff, 0x9e, 0x64),
            danger: Color::Rgb(0xf7, 0x76, 0x8e),

            destructive: Color::Rgb(0xdb, 0x4b, 0x4b),
            destructive_surface: Color::Rgb(0x37, 0x22, 0x2c),
        }
    }

    /// Tokyo Night "storm" — the same palette on a lighter, bluer canvas, for
    /// terminals where the near-black of `night` looks like a hole in the
    /// screen.
    pub const fn tokyo_night_storm() -> Self {
        Self {
            background: Color::Rgb(0x24, 0x28, 0x3b),
            surface: Color::Rgb(0x2f, 0x33, 0x4d),
            surface_selected: Color::Rgb(0x3d, 0x59, 0xa1),

            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            accent_dim: Color::Rgb(0x3b, 0x42, 0x61),

            ..Self::tokyo_night()
        }
    }

    /// Tokyo Night "day" — the light variant.
    ///
    /// On a light canvas the severity hues have to be *darkened*, not reused: a
    /// pastel that glows on near-black disappears on paper white. So warning
    /// becomes a burnt amber (`#b15c00`) and danger a saturated crimson
    /// (`#f52a65`) — still the warm/red split the one-character rule needs, but
    /// with enough contrast against `#e1e2e7` to be seen at all.
    pub const fn tokyo_night_day() -> Self {
        Self {
            background: Color::Rgb(0xe1, 0xe2, 0xe7),
            surface: Color::Rgb(0xd0, 0xd5, 0xe3),
            surface_selected: Color::Rgb(0xb6, 0xbf, 0xe2),

            accent: Color::Rgb(0x2e, 0x7d, 0xe9),
            accent_dim: Color::Rgb(0xa1, 0xa6, 0xc5),

            text_primary: Color::Rgb(0x37, 0x60, 0xbf),
            text_secondary: Color::Rgb(0x61, 0x72, 0xb0),
            text_muted: Color::Rgb(0x84, 0x8c, 0xb5),

            success: Color::Rgb(0x58, 0x75, 0x39),
            info: Color::Rgb(0x00, 0x71, 0x97),
            warning: Color::Rgb(0xb1, 0x5c, 0x00),
            danger: Color::Rgb(0xf5, 0x2a, 0x65),

            destructive: Color::Rgb(0xc6, 0x43, 0x43),
            destructive_surface: Color::Rgb(0xec, 0xd4, 0xdc),
        }
    }
}
