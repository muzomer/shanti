//! The catalogue: every built-in scheme, addressable by a stable name.
//!
//! This module is the single list of schemes the rest of the program may know
//! about. Configuration, the command line and the picker all read [`ALL`], so a
//! scheme added to the array below appears everywhere at once and cannot be
//! offered in one place and rejected in another.
//!
//! The [`Scheme::name`] strings are **API**: they are written into a user's
//! configuration file and must keep working across releases. They are
//! kebab-case, ASCII, and never renamed — a nicer label belongs in
//! [`Scheme::label`], which nothing persists.

use std::fmt;

use super::Theme;

/// How light the scheme's canvas is — the one property a user needs before
/// picking, because a dark scheme on a light terminal is unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    /// Light text on a dark canvas.
    Dark,
    /// Dark text on a light canvas.
    Light,
    /// Neither: the scheme defers to the terminal's own palette, so whether it
    /// ends up light or dark is the user's own configuration. Modelled
    /// explicitly rather than guessed, so a picker can say "follows terminal"
    /// instead of lying in one direction.
    Terminal,
}

impl Appearance {
    /// A short word for a list or an error message.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Terminal => "follows terminal",
        }
    }
}

/// One entry in the catalogue.
///
/// The palette is reached through a function pointer rather than stored inline,
/// so a scheme stays a *constructor* — data produced in one place — and this
/// array stays a table of contents.
#[derive(Debug, Clone, Copy)]
pub struct Scheme {
    /// The stable, kebab-case identifier a user writes down.
    pub name: &'static str,
    /// A short human label for a picker row.
    pub label: &'static str,
    /// Light, dark, or the terminal's own.
    pub appearance: Appearance,
    build: fn() -> Theme,
}

impl Scheme {
    /// The palette this scheme names.
    pub fn theme(&self) -> Theme {
        (self.build)()
    }
}

/// Every built-in scheme, in the order a picker should show them: the default
/// first, then dark before light within each family.
pub const ALL: &[Scheme] = &[
    Scheme {
        name: "tokyo-night",
        label: "Tokyo Night",
        appearance: Appearance::Dark,
        build: Theme::tokyo_night,
    },
    Scheme {
        name: "tokyo-night-storm",
        label: "Tokyo Night Storm",
        appearance: Appearance::Dark,
        build: Theme::tokyo_night_storm,
    },
    Scheme {
        name: "tokyo-night-day",
        label: "Tokyo Night Day",
        appearance: Appearance::Light,
        build: Theme::tokyo_night_day,
    },
    Scheme {
        name: "catppuccin-mocha",
        label: "Catppuccin Mocha",
        appearance: Appearance::Dark,
        build: Theme::catppuccin_mocha,
    },
    Scheme {
        name: "catppuccin-latte",
        label: "Catppuccin Latte",
        appearance: Appearance::Light,
        build: Theme::catppuccin_latte,
    },
    Scheme {
        name: "gruvbox-dark",
        label: "Gruvbox Dark",
        appearance: Appearance::Dark,
        build: Theme::gruvbox_dark,
    },
    Scheme {
        name: "ansi",
        label: "Terminal (ANSI 16)",
        appearance: Appearance::Terminal,
        build: Theme::ansi,
    },
];

/// The scheme the application starts with, and falls back to.
pub const DEFAULT: &str = "tokyo-night";

/// A name that is not in the catalogue.
///
/// It carries the name that was asked for so every caller — the configuration
/// file, an environment variable, a flag — can report *where* the bad name came
/// from while this type supplies the part that is always the same: what the
/// user could have written instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownScheme {
    /// What the user asked for.
    pub name: String,
}

impl fmt::Display for UnknownScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown colour scheme `{}`; valid schemes are: ",
            self.name
        )?;
        for (i, scheme) in ALL.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            f.write_str(scheme.name)?;
        }
        Ok(())
    }
}

impl std::error::Error for UnknownScheme {}

/// Look a scheme up by name.
///
/// Matching ignores case and surrounding whitespace: a name typed into a
/// configuration file is prose, and rejecting `Tokyo-Night` would be a puzzle
/// with no lesson in it. Everything else must match exactly, so the names stay
/// a closed set rather than a fuzzy search.
pub fn find(name: &str) -> Result<&'static Scheme, UnknownScheme> {
    let wanted = name.trim();
    ALL.iter()
        .find(|scheme| scheme.name.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| UnknownScheme {
            name: name.to_string(),
        })
}

/// The palette a name refers to.
pub fn theme(name: &str) -> Result<Theme, UnknownScheme> {
    find(name).map(Scheme::theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance criterion the one-character glyph rule exists for: in
    /// every scheme, a warning and an error must not be the same colour. Every
    /// scheme module documents *how* it keeps them apart; this is the guard
    /// that a later edit cannot quietly undo it.
    #[test]
    fn warn_and_danger_differ_in_every_scheme() {
        for scheme in ALL {
            let theme = scheme.theme();
            assert_ne!(
                theme.warning, theme.danger,
                "{} uses one colour for warning and danger",
                scheme.name
            );
        }
    }

    /// A red *report* and a red *button* must stay distinguishable too — the
    /// reason `destructive` is a separate token from `danger`.
    #[test]
    fn destructive_differs_from_danger_in_every_scheme() {
        for scheme in ALL {
            let theme = scheme.theme();
            assert_ne!(
                theme.destructive, theme.danger,
                "{} uses one colour for danger and destruction",
                scheme.name
            );
        }
    }

    /// The catalogue is the whole promise of the feature: enough schemes to be
    /// worth choosing between, and at least two that work on a light terminal.
    #[test]
    fn the_catalogue_covers_light_and_dark() {
        assert!(ALL.len() >= 6, "expected at least six schemes");
        let light = ALL
            .iter()
            .filter(|s| s.appearance == Appearance::Light)
            .count();
        assert!(
            light >= 2,
            "expected at least two light schemes, got {light}"
        );
    }

    /// Names are persisted, so they must be unique and stay in the kebab-case
    /// shape a user can type without guessing.
    #[test]
    fn names_are_unique_and_kebab_case() {
        for (i, scheme) in ALL.iter().enumerate() {
            assert!(
                scheme
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{} is not kebab-case",
                scheme.name
            );
            assert!(
                !ALL[..i].iter().any(|other| other.name == scheme.name),
                "{} appears twice",
                scheme.name
            );
        }
    }

    #[test]
    fn lookup_is_forgiving_about_case_and_spacing() {
        assert_eq!(theme("  Tokyo-Night "), Ok(Theme::tokyo_night()));
        assert_eq!(theme("catppuccin-latte"), Ok(Theme::catppuccin_latte()));
    }

    /// An unknown name has to teach the user what they could have written —
    /// the message is the only help a configuration file gets.
    #[test]
    fn an_unknown_name_lists_the_valid_ones() {
        let err = theme("dracula").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("dracula"), "{message}");
        for scheme in ALL {
            assert!(message.contains(scheme.name), "{message}");
        }
    }

    /// The default name must resolve, and to the palette the app ships with.
    #[test]
    fn the_default_name_is_the_default_theme() {
        assert_eq!(theme(DEFAULT), Ok(Theme::default()));
    }
}
