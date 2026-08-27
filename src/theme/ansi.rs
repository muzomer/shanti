//! The terminal's own 16 colours.
//!
//! For users who already theme their terminal: every token here is an ANSI
//! index, so the palette *is* whatever the terminal is configured with and
//! shanti stops having an opinion. That is the whole point, and also the whole
//! limitation — this scheme cannot promise contrast, because it does not know
//! the hues it is asking for.
//!
//! Two consequences shape the choices below. The canvas and the text body use
//! [`Color::Reset`] (the terminal's default background and foreground) rather
//! than black and white, so the scheme works on a light and a dark terminal
//! alike. And the one-character glyph rule (see `tokyo_night`) is honoured the
//! only way it can be at this level of control: warning takes ANSI yellow and
//! danger ANSI bright red — slots that are far apart in every terminal palette
//! anyone actually ships, unlike yellow-versus-orange, which ANSI has no name
//! for.

use ratatui::style::Color;

use super::Theme;

impl Theme {
    /// The terminal's own 16 colours.
    pub const fn ansi() -> Self {
        Self {
            background: Color::Reset,
            // A popup is told apart from the canvas by its border, not its
            // fill: any concrete fill we picked would be wrong on half the
            // terminals this scheme exists to serve.
            surface: Color::Reset,
            // The one place a concrete colour is unavoidable — a selection band
            // has to differ from the canvas. Blue is the safest bet: it is the
            // conventional selection colour and is dark enough for a light
            // terminal's default foreground and light enough for a dark one's.
            surface_selected: Color::Blue,

            accent: Color::LightBlue,
            accent_dim: Color::Blue,

            text_primary: Color::Reset,
            text_secondary: Color::White,
            text_muted: Color::DarkGray,

            success: Color::Green,
            info: Color::Cyan,
            warning: Color::Yellow,
            danger: Color::LightRed,

            destructive: Color::Red,
            // Same reasoning as `surface`: the destructive border and key
            // colours carry the warning, so the band stays the terminal's own.
            destructive_surface: Color::Reset,
        }
    }
}
