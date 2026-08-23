use color_eyre::eyre::{self};
use directories::BaseDirs;
use std::path::{Path, PathBuf};
/// Directory holding the data shanti writes for itself — today, just the log.
///
/// `SHANTI_DATA` overrides the location, mirroring how `SHANTI_CONFIG` overrides
/// the config directory.
///
/// Deliberately **not** `ProjectDirs::data_local_dir`, which resolves on macOS
/// to `~/Library/Application Support/com.muzomer.shanti` — the directory nobody
/// thinks to look in, and the log is exactly what someone goes hunting for when
/// shanti misbehaves. And deliberately `state`, not `share`: the XDG spec puts
/// logs and other persistent-but-not-precious data under state, while share is
/// for data whose loss would matter. `XDG_STATE_HOME` is honoured first so the
/// choice stays the user's, then `~/.local/state/shanti`.
///
/// Decided in shanti-1xq.5, after [`get_config_dir`] moved in shanti-1xq.4.
pub fn get_data_dir() -> eyre::Result<PathBuf> {
    let base =
        BaseDirs::new().ok_or_else(|| eyre::eyre!("Unable to find a home directory for shanti"))?;
    Ok(state_dir_from(
        env_value("SHANTI_DATA"),
        env_value("XDG_STATE_HOME"),
        base.home_dir(),
    ))
}

/// The precedence rule on its own, so it can be tested without touching the
/// process environment — which every other test in this binary shares.
fn state_dir_from(
    shanti_data: Option<String>,
    xdg_state_home: Option<String>,
    home: &Path,
) -> PathBuf {
    if let Some(explicit) = shanti_data {
        return PathBuf::from(explicit);
    }
    if let Some(xdg) = xdg_state_home {
        return PathBuf::from(xdg).join("shanti");
    }
    home.join(".local").join("state").join("shanti")
}

/// Directory holding shanti's configuration file.
///
/// `SHANTI_CONFIG` overrides the location, mirroring how `SHANTI_DATA` overrides
/// the data directory. It names a *directory*, not a file, so the same override
/// can host future config assets next to `config.toml`.
///
/// Deliberately **not** `ProjectDirs::config_local_dir`, which resolves on macOS
/// to `~/Library/Application Support/com.muzomer.shanti`. shanti is a terminal
/// tool whose users keep their dotfiles under `~/.config` and expect to edit
/// this file by hand; a path inside `Application Support` is one nobody thinks
/// to look in. `XDG_CONFIG_HOME` is honoured first so the choice stays the
/// user's, then `~/.config/shanti`.
///
/// Note this diverges from [`get_data_dir`], which still follows the platform
/// convention — see shanti-1xq.5.
pub fn get_config_dir() -> eyre::Result<PathBuf> {
    let base =
        BaseDirs::new().ok_or_else(|| eyre::eyre!("Unable to find a home directory for shanti"))?;
    Ok(config_dir_from(
        env_value("SHANTI_CONFIG"),
        env_value("XDG_CONFIG_HOME"),
        base.home_dir(),
    ))
}

/// Reads a variable, treating an empty value as unset the way the XDG spec does.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

/// The precedence rule on its own, so it can be tested without touching the
/// process environment — which every other test in this binary shares.
fn config_dir_from(
    shanti_config: Option<String>,
    xdg_config_home: Option<String>,
    home: &Path,
) -> PathBuf {
    if let Some(explicit) = shanti_config {
        return PathBuf::from(explicit);
    }
    if let Some(xdg) = xdg_config_home {
        return PathBuf::from(xdg).join("shanti");
    }
    home.join(".config").join("shanti")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shanti_config_wins_over_everything() {
        let dir = config_dir_from(
            Some("/explicit".into()),
            Some("/xdg".into()),
            Path::new("/home/someone"),
        );
        assert_eq!(dir, PathBuf::from("/explicit"));
    }

    #[test]
    fn xdg_config_home_is_honoured_before_the_default() {
        let dir = config_dir_from(None, Some("/xdg".into()), Path::new("/home/someone"));
        assert_eq!(dir, PathBuf::from("/xdg/shanti"));
    }

    #[test]
    fn shanti_data_wins_over_everything() {
        let dir = state_dir_from(
            Some("/explicit".into()),
            Some("/xdg".into()),
            Path::new("/home/someone"),
        );
        assert_eq!(dir, PathBuf::from("/explicit"));
    }

    #[test]
    fn xdg_state_home_is_honoured_before_the_default() {
        let dir = state_dir_from(None, Some("/xdg".into()), Path::new("/home/someone"));
        assert_eq!(dir, PathBuf::from("/xdg/shanti"));
    }

    /// The decision recorded in shanti-1xq.5: state, not share — the log is
    /// persistent but not precious — and not `~/Library/Application Support`,
    /// which is where nobody looks for the file they need when shanti misbehaves.
    #[test]
    fn the_data_default_is_the_xdg_state_directory() {
        let dir = state_dir_from(None, None, Path::new("/home/someone"));
        assert_eq!(dir, PathBuf::from("/home/someone/.local/state/shanti"));
    }

    /// The decision recorded in shanti-1xq.4: a dotfile directory, not
    /// `~/Library/Application Support`, because shanti is a terminal tool whose
    /// users edit this file by hand.
    #[test]
    fn the_default_is_a_dotfile_directory() {
        let dir = config_dir_from(None, None, Path::new("/home/someone"));
        assert_eq!(dir, PathBuf::from("/home/someone/.config/shanti"));
    }
}
