//! The one place a colour is chosen.
//!
//! # Palette: Tokyo Night (the "night" variant)
//!
//! `CLAUDE.md` asks for a dark palette from the Tokyo Night or Catppuccin Mocha
//! families. Tokyo Night wins here for one concrete reason: the smallest colour
//! target in this UI is a single status glyph — one character wide — and Tokyo
//! Night keeps the severity hues far apart at that size. Its warning orange
//! (`#ff9e64`) and its danger red (`#f7768e`) sit roughly 40° apart in hue with
//! clearly different saturation, so `↑ ahead` never reads as `✘ gone` at a
//! glance. Catppuccin Mocha's peach and red are both low-chroma pastels and blur
//! into each other at one character. Tokyo Night's near-black base (`#1a1b26`)
//! also leaves room for a genuinely high-contrast selection band, which the
//! pastel family does not.
//!
//! # Semantic names only
//!
//! Every name below says what the colour *means*, never what it *is*. Call
//! sites ask for [`ACCENT`] or [`DANGER`]; none of them may name a hue. That is
//! what makes the palette swappable — a light variant, or a move to Mocha, is
//! an edit to this file and nothing else.
//!
//! Status glyph colours resolve here too, via [`tone`]. The domain model
//! (`vcs::status`) decides *which* glyph and *how severe* it is; this module is
//! the only place that severity becomes a colour.

use ratatui::style::{Color, Modifier, Style};

use crate::vcs::Tone;

// --- Surfaces ---------------------------------------------------------------

/// The application's base canvas.
pub const BACKGROUND: Color = Color::Rgb(0x1a, 0x1b, 0x26);
/// A raised surface: popup fills and anything that should sit above the canvas.
pub const SURFACE: Color = Color::Rgb(0x29, 0x2e, 0x42);
/// The selection band. Deliberately the loudest surface in the palette — the
/// selected row has to be findable without reading it.
pub const SURFACE_SELECTED: Color = Color::Rgb(0x3d, 0x59, 0xa1);

// --- Accent -----------------------------------------------------------------

/// The colour of attention: active headers, focused borders, key hints.
pub const ACCENT: Color = Color::Rgb(0x7a, 0xa2, 0xf7);
/// The accent at rest: separators, scrollbar tracks, structural rules — chrome
/// that has to be visible without competing with content.
pub const ACCENT_DIM: Color = Color::Rgb(0x39, 0x4b, 0x70);

// --- Text -------------------------------------------------------------------

/// The thing the user is actually reading.
pub const TEXT_PRIMARY: Color = Color::Rgb(0xc0, 0xca, 0xf5);
/// Supporting detail that belongs to the primary text (a repository name next
/// to a space name).
pub const TEXT_SECONDARY: Color = Color::Rgb(0xa9, 0xb1, 0xd6);
/// Present but deliberately quiet: counts, hints, disabled states, borders at
/// rest.
pub const TEXT_MUTED: Color = Color::Rgb(0x56, 0x5f, 0x89);

// --- Notification severities ------------------------------------------------

/// Everything is as it should be.
pub const SUCCESS: Color = Color::Rgb(0x9e, 0xce, 0x6a);
/// Neutral news that needs no action from the user.
pub const INFO: Color = Color::Rgb(0x7d, 0xcf, 0xff);
/// Something needs attention but nothing is broken.
pub const WARNING: Color = Color::Rgb(0xff, 0x9e, 0x64);
/// Something is wrong.
pub const DANGER: Color = Color::Rgb(0xf7, 0x76, 0x8e);

// --- Destruction ------------------------------------------------------------

/// Reserved for actions that destroy something the user cannot get back.
///
/// Deeper than [`DANGER`] on purpose: a red *report* and a red *button* must not
/// look alike, or a confirmation dialog stops reading as a decision point.
pub const DESTRUCTIVE: Color = Color::Rgb(0xdb, 0x4b, 0x4b);
/// The background behind a destructive choice.
pub const DESTRUCTIVE_SURFACE: Color = Color::Rgb(0x37, 0x22, 0x2c);

// --- Composed styles --------------------------------------------------------

/// The application canvas behind the main panel.
pub const CANVAS: Style = Style::new().bg(BACKGROUND);
/// The fill of a popup, so it reads as sitting above the canvas.
pub const POPUP_SURFACE: Style = Style::new().bg(SURFACE).fg(TEXT_PRIMARY);

/// The selected row in any list.
pub const SELECTED_ROW: Style = Style::new()
    .bg(SURFACE_SELECTED)
    .fg(TEXT_PRIMARY)
    .add_modifier(Modifier::BOLD);

/// A panel border at rest.
pub const BORDER: Style = Style::new().fg(TEXT_MUTED);
/// The border of whatever currently owns the keyboard.
pub const BORDER_FOCUSED: Style = Style::new().fg(ACCENT);
/// The border of an input inside a focused popup — one step louder than the
/// popup around it, so the caret's home is obvious.
pub const BORDER_INPUT_FOCUSED: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
/// The border of a dialog that is about to destroy something.
pub const BORDER_DESTRUCTIVE: Style = Style::new().fg(DESTRUCTIVE).add_modifier(Modifier::BOLD);

/// An active header.
pub const TITLE: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
/// Body text.
pub const TEXT: Style = Style::new().fg(TEXT_PRIMARY);
/// Supporting detail.
pub const SECONDARY: Style = Style::new().fg(TEXT_SECONDARY);
/// Quiet chrome: hints, counts, footer labels.
pub const MUTED: Style = Style::new().fg(TEXT_MUTED);
/// Structural rules — separators, scrollbars.
pub const RULE: Style = Style::new().fg(ACCENT_DIM);

/// A key name in the footer, e.g. `[Enter]`.
pub const KEY: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
/// A key that carries out a destructive action.
pub const KEY_DESTRUCTIVE: Style = Style::new().fg(DESTRUCTIVE).add_modifier(Modifier::BOLD);
/// A key that backs safely out.
pub const KEY_SAFE: Style = Style::new().fg(SUCCESS).add_modifier(Modifier::BOLD);

/// A success message.
pub const SUCCESS_TEXT: Style = Style::new().fg(SUCCESS).add_modifier(Modifier::BOLD);
/// A warning message.
pub const WARNING_TEXT: Style = Style::new().fg(WARNING).add_modifier(Modifier::BOLD);
/// An error message.
pub const DANGER_TEXT: Style = Style::new().fg(DANGER).add_modifier(Modifier::BOLD);

/// Where a status [`Tone`] becomes a colour — the only such mapping.
///
/// The domain model says how severe a glyph is; the severity tokens above say
/// what that severity looks like. Neither side knows about the other.
pub const fn tone(tone: Tone) -> Color {
    match tone {
        Tone::Muted => TEXT_MUTED,
        Tone::Ok => SUCCESS,
        Tone::Info => INFO,
        Tone::Warn => WARNING,
        Tone::Danger => DANGER,
    }
}
