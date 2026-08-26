//! The one place a colour is chosen.
//!
//! # The palette is one of several
//!
//! The hues themselves live in the scheme modules beside this one
//! (`tokyo_night`, `catppuccin`, `gruvbox`, `ansi`), each a [`Theme`]
//! constructor and nothing else, catalogued in [`scheme`]. Tokyo Night
//! ("night") is the default and the reason the others are shaped the way they
//! are: the smallest colour target in this UI is a status glyph one character
//! wide, and Tokyo Night keeps the severity hues far apart at that size — its
//! warning orange (`#ff9e64`) and its danger red (`#f7768e`) sit roughly 40°
//! apart with clearly different saturation, so `↑ ahead` never reads as
//! `✘ gone`. Every scheme added later has to earn the same separation, by
//! whatever means its own family allows; `scheme`'s tests hold it to that.
//!
//! # Semantic names only
//!
//! Every name below says what the colour *means*, never what it *is*. Call
//! sites ask for [`accent()`] or [`danger()`]; none of them may name a hue. That
//! is what makes the palette swappable — a light variant, or a move to Mocha, is
//! a different [`Theme`] value and nothing else.
//!
//! Status glyph colours resolve here too, via [`tone`]. The domain model
//! (`vcs::status`) decides *which* glyph and *how severe* it is; this module is
//! the only place that severity becomes a colour.
//!
//! # Why the palette is a value, not a set of constants
//!
//! A scheme the user can pick has to be able to change while the process runs,
//! so the palette is a [`Theme`] struct held in one process-global slot and read
//! through the accessors below. The alternative — borrowing a `&Theme` through
//! every `draw` — would have to be carried by the `Modal` trait and every
//! component signature, for a value that changes at human speed and is read by
//! exactly one thread. A `RwLock` inside a `OnceLock` buys that flexibility with
//! no new dependency and an uncontended read on the render path.
//!
//! Only the *base* tokens are stored. Every composed style ([`title()`],
//! [`selected_row()`], the `border_*` family, …) is derived from them on each
//! call, so a scheme author cannot set a colour in one place and forget its
//! twin in another.

use std::sync::{OnceLock, RwLock};

use ratatui::style::{Color, Modifier, Style};

use crate::vcs::Tone;

mod ansi;
mod catppuccin;
mod gruvbox;
pub mod scheme;
mod tokyo_night;

pub use scheme::{Appearance, Scheme, UnknownScheme};

/// A complete palette: the base tokens every style in the UI is built from.
///
/// The fields are the *meanings*, not the hues. A new scheme is a new value of
/// this type; nothing else in the codebase changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    // --- Surfaces ---
    /// The application's base canvas.
    pub background: Color,
    /// A raised surface: popup fills and anything that should sit above the
    /// canvas.
    pub surface: Color,
    /// The selection band. Deliberately the loudest surface in the palette —
    /// the selected row has to be findable without reading it.
    pub surface_selected: Color,

    // --- Accent ---
    /// The colour of attention: active headers, focused borders, key hints.
    pub accent: Color,
    /// The accent at rest: separators, scrollbar tracks, structural rules —
    /// chrome that has to be visible without competing with content.
    pub accent_dim: Color,

    // --- Text ---
    /// The thing the user is actually reading.
    pub text_primary: Color,
    /// Supporting detail that belongs to the primary text (a repository name
    /// next to a space name).
    pub text_secondary: Color,
    /// Present but deliberately quiet: counts, hints, disabled states, borders
    /// at rest.
    pub text_muted: Color,

    // --- Notification severities ---
    /// Everything is as it should be.
    pub success: Color,
    /// Neutral news that needs no action from the user.
    pub info: Color,
    /// Something needs attention but nothing is broken.
    pub warning: Color,
    /// Something is wrong.
    pub danger: Color,

    // --- Destruction ---
    /// Reserved for actions that destroy something the user cannot get back.
    ///
    /// Deeper than [`Theme::danger`] on purpose: a red *report* and a red
    /// *button* must not look alike, or a confirmation dialog stops reading as
    /// a decision point.
    pub destructive: Color,
    /// The background behind a destructive choice.
    pub destructive_surface: Color,
}

impl Theme {
    /// Where a status [`Tone`] becomes a colour — the only such mapping.
    ///
    /// The domain model says how severe a glyph is; the severity tokens above
    /// say what that severity looks like. Neither side knows about the other.
    pub const fn tone(&self, tone: Tone) -> Color {
        match tone {
            Tone::Muted => self.text_muted,
            Tone::Ok => self.success,
            Tone::Info => self.info,
            Tone::Warn => self.warning,
            Tone::Danger => self.danger,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::tokyo_night()
    }
}

/// The active palette.
///
/// One slot for the whole process, because "what the application looks like" is
/// a property of the application and not of any one widget. Written only by
/// [`set`].
fn slot() -> &'static RwLock<Theme> {
    static SLOT: OnceLock<RwLock<Theme>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Theme::default()))
}

/// The palette in force right now.
///
/// `Theme` is `Copy`, so the lock is released before the caller does anything
/// with it and a panicking draw can never leave it poisoned for the next frame.
pub fn current() -> Theme {
    // A poisoned lock would mean a panic while swapping schemes. Colours are
    // never worth taking the process down for, so fall back to the palette the
    // writer was mid-way through installing.
    slot().read().map_or_else(|e| *e.into_inner(), |t| *t)
}

/// Serialises the tests that install a palette.
///
/// The palette is process-global by design (see the module docs), so two tests
/// swapping it in parallel would read each other's colours. Every test that
/// calls [`set`] takes this first, so the globality that is a feature at
/// runtime is not a source of flakes in the suite.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    // A panicking test must not take every later one down with it: the data
    // behind the lock is `()`, so there is no invariant left broken.
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Install a palette. The single mutation point for the whole process.
pub fn set(theme: Theme) {
    match slot().write() {
        Ok(mut slot) => *slot = theme,
        Err(e) => *e.into_inner() = theme,
    }
}

// --- Base tokens -------------------------------------------------------------
//
// The palette's raw hues, and the layer everything below is built from. They are
// **private on purpose**, which is what turns the module rule into something the
// compiler enforces rather than something a reviewer has to catch:
//
// > No component names a raw `Color`, and none names a constant either: it calls
// > the accessor for the _meaning_.
//
// A component asking for `title()` or `selected_row()` is asking for a role, and
// the role is free to change hue when a scheme changes. A component that could
// ask for `accent()` would be naming a hue, and no scheme could then move it.
// Keeping these crate-private means that mistake cannot be written.
//
// A new *semantic* accessor belongs in this module, next to the ones below, and
// may use these freely.

/// The application's base canvas.
fn background() -> Color {
    current().background
}
/// A raised surface: popup fills and anything above the canvas.
fn surface() -> Color {
    current().surface
}
/// The selection band.
fn surface_selected() -> Color {
    current().surface_selected
}
/// The colour of attention.
fn accent() -> Color {
    current().accent
}
/// The accent at rest.
fn accent_dim() -> Color {
    current().accent_dim
}
/// The thing the user is actually reading.
fn text_primary() -> Color {
    current().text_primary
}
/// Supporting detail beside the primary text.
fn text_secondary() -> Color {
    current().text_secondary
}
/// Present but deliberately quiet.
fn text_muted() -> Color {
    current().text_muted
}
/// Everything is as it should be.
pub fn success() -> Color {
    current().success
}
/// Neutral news that needs no action.
pub fn info() -> Color {
    current().info
}
/// Something needs attention but nothing is broken.
pub fn warning() -> Color {
    current().warning
}
/// Something is wrong.
pub fn danger() -> Color {
    current().danger
}
/// Reserved for actions that destroy something unrecoverable.
pub fn destructive() -> Color {
    current().destructive
}
/// The background behind a destructive choice.
pub fn destructive_surface() -> Color {
    current().destructive_surface
}

/// Where a status [`Tone`] becomes a colour — the only such mapping.
pub fn tone(tone: Tone) -> Color {
    current().tone(tone)
}

// --- Composed styles ---------------------------------------------------------

/// Text carrying a status [`Tone`], for the places that spell a status out in
/// words rather than in a glyph. Goes through [`tone`] like everything else, so
/// there is still exactly one tone-to-colour mapping.
pub fn tone_text(tone_of: Tone) -> Style {
    Style::new().fg(tone(tone_of))
}

/// The application canvas behind the main panel.
pub fn canvas() -> Style {
    Style::new().bg(background())
}
/// The fill of a popup, so it reads as sitting above the canvas.
pub fn popup_surface() -> Style {
    Style::new().bg(surface()).fg(text_primary())
}

/// The selected row in any list.
pub fn selected_row() -> Style {
    Style::new()
        .bg(surface_selected())
        .fg(text_primary())
        .add_modifier(Modifier::BOLD)
}

/// A panel border at rest.
pub fn border() -> Style {
    Style::new().fg(text_muted())
}
/// The border of whatever currently owns the keyboard.
pub fn border_focused() -> Style {
    Style::new().fg(accent())
}
/// The border of an input inside a focused popup — one step louder than the
/// popup around it, so the caret's home is obvious.
pub fn border_input_focused() -> Style {
    Style::new().fg(accent()).add_modifier(Modifier::BOLD)
}
/// The border of a dialog that is about to destroy something.
pub fn border_destructive() -> Style {
    Style::new().fg(destructive()).add_modifier(Modifier::BOLD)
}

/// An active header.
pub fn title() -> Style {
    Style::new().fg(accent()).add_modifier(Modifier::BOLD)
}
/// Body text.
pub fn text() -> Style {
    Style::new().fg(text_primary())
}
/// Supporting detail.
pub fn secondary() -> Style {
    Style::new().fg(text_secondary())
}
/// Quiet chrome: hints, counts, footer labels.
pub fn muted() -> Style {
    Style::new().fg(text_muted())
}
/// Structural rules — separators, scrollbars.
pub fn rule() -> Style {
    Style::new().fg(accent_dim())
}

/// A key name in the footer, e.g. `[Enter]`.
pub fn key() -> Style {
    Style::new().fg(accent()).add_modifier(Modifier::BOLD)
}
/// A key that carries out a destructive action.
pub fn key_destructive() -> Style {
    Style::new().fg(destructive()).add_modifier(Modifier::BOLD)
}
/// A key that backs safely out.
pub fn key_safe() -> Style {
    Style::new().fg(success()).add_modifier(Modifier::BOLD)
}

/// A success message.
pub fn success_text() -> Style {
    Style::new().fg(success()).add_modifier(Modifier::BOLD)
}
/// An informational message: news, not a problem. The only one of the four
/// that is not bold — an announcement that shouts reads as a failure, which is
/// the confusion `shanti-nbt.3` exists to end.
pub fn info_text() -> Style {
    Style::new().fg(info())
}
/// A warning message.
pub fn warning_text() -> Style {
    Style::new().fg(warning()).add_modifier(Modifier::BOLD)
}
/// An error message.
pub fn danger_text() -> Style {
    Style::new().fg(danger()).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default palette is what the application shipped with; a scheme
    /// added later must not change what a user who picked nothing sees.
    #[test]
    fn default_is_tokyo_night() {
        assert_eq!(Theme::default(), Theme::tokyo_night());
    }

    /// Composed styles must follow the installed palette, not a copy of the
    /// colours taken when the module was written.
    #[test]
    fn styles_follow_the_installed_theme() {
        let _guard = test_lock();
        let swapped = Theme {
            accent: Color::Rgb(1, 2, 3),
            ..Theme::tokyo_night()
        };
        set(swapped);
        assert_eq!(title().fg, Some(Color::Rgb(1, 2, 3)));

        set(Theme::tokyo_night());
        assert_eq!(title().fg, Some(Theme::tokyo_night().accent));
    }
}
