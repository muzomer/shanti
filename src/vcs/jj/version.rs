//! The jj version shanti is talking to, and the floor it insists on.

use std::fmt;
use std::str::FromStr;

use color_eyre::eyre::{self, eyre, WrapErr};

/// Oldest jj release shanti is known to work with.
///
/// Why a floor at all: shanti reads jj through the template language, and both
/// template keywords and workspace flags have moved between jj releases. Without
/// this check an older jj fails deep inside a parse — an unreadable "unknown
/// keyword" error, or a record with the wrong number of fields — instead of
/// saying the one thing the user can act on: upgrade jj.
///
/// Raise this deliberately, together with the templates that need the newer
/// syntax; never as a side effect.
pub const MINIMUM_JJ_VERSION: JjVersion = JjVersion::new(0, 28, 0);

/// A `major.minor.patch` jj version.
///
/// Pre-release and build suffixes (`0.41.0-a1b2c3d`) are parsed and discarded:
/// they identify a build, not a feature set, and ordering them correctly would
/// buy nothing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct JjVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl JjVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Read a version out of `jj --version` output, which is `jj 0.40.0` plus an
    /// optional suffix. Tolerant of surrounding whitespace and of the leading
    /// `jj ` being absent, so a future banner change does not break it.
    pub fn parse_version_output(output: &str) -> eyre::Result<Self> {
        let token = output
            .split_whitespace()
            .find(|token| token.starts_with(|c: char| c.is_ascii_digit()))
            .ok_or_else(|| eyre!("could not find a version number in jj output: {output:?}"))?;
        token.parse()
    }

    /// Whether this version satisfies shanti's floor.
    pub fn is_supported(self) -> bool {
        self >= MINIMUM_JJ_VERSION
    }
}

impl FromStr for JjVersion {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Cut the suffix first, so `0.41.0-a1b2c3d` parses as 0.41.0.
        let core = s.split(['-', '+']).next().unwrap_or(s);
        let mut parts = core.split('.');
        let mut number = |what: &str| -> eyre::Result<u32> {
            parts
                .next()
                .ok_or_else(|| eyre!("jj version {s:?} has no {what} component"))?
                .parse()
                .wrap_err_with(|| format!("jj version {s:?} has a non-numeric {what} component"))
        };

        let major = number("major")?;
        let minor = number("minor")?;
        // jj has always shipped a patch component, but treating it as optional
        // costs nothing and keeps a hypothetical `0.42` from being fatal.
        let patch = match parts.next() {
            Some(patch) => patch
                .parse()
                .wrap_err_with(|| format!("jj version {s:?} has a non-numeric patch component"))?,
            None => 0,
        };

        Ok(Self::new(major, minor, patch))
    }
}

impl fmt::Display for JjVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_version_banner() {
        let version = JjVersion::parse_version_output("jj 0.40.0\n").unwrap();
        assert_eq!(version, JjVersion::new(0, 40, 0));
    }

    #[test]
    fn discards_build_suffixes() {
        assert_eq!(
            JjVersion::parse_version_output("jj 0.41.0-a1b2c3d4").unwrap(),
            JjVersion::new(0, 41, 0)
        );
    }

    #[test]
    fn treats_a_missing_patch_as_zero() {
        assert_eq!(
            "0.42".parse::<JjVersion>().unwrap(),
            JjVersion::new(0, 42, 0)
        );
    }

    #[test]
    fn rejects_output_without_a_version() {
        assert!(JjVersion::parse_version_output("command not found").is_err());
    }

    #[test]
    fn orders_by_component() {
        assert!(JjVersion::new(0, 40, 0) > JjVersion::new(0, 9, 9));
        assert!(JjVersion::new(1, 0, 0) > JjVersion::new(0, 99, 0));
        assert!(JjVersion::new(0, 28, 1) > JjVersion::new(0, 28, 0));
    }

    #[test]
    fn the_floor_accepts_itself_and_rejects_anything_older() {
        assert!(MINIMUM_JJ_VERSION.is_supported());
        assert!(!JjVersion::new(0, 27, 9).is_supported());
    }
}
