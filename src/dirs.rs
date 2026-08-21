use color_eyre::eyre::{self};
use directories::ProjectDirs;
use std::path::PathBuf;
pub fn get_data_dir() -> eyre::Result<PathBuf> {
    let directory = if let Ok(s) = std::env::var("SHANTI_DATA") {
        PathBuf::from(s)
    } else if let Some(proj_dirs) = ProjectDirs::from("com", "muzomer", "shanti") {
        proj_dirs.data_local_dir().to_path_buf()
    } else {
        return Err(eyre::eyre!("Unable to find data directory for shanti"));
    };
    Ok(directory)
}

/// Directory holding shanti's configuration file.
///
/// `SHANTI_CONFIG` overrides the platform location, mirroring how `SHANTI_DATA`
/// overrides the data directory. It names a *directory*, not a file, so the same
/// override can host future config assets next to `config.toml`.
pub fn get_config_dir() -> eyre::Result<PathBuf> {
    let directory = if let Ok(s) = std::env::var("SHANTI_CONFIG") {
        PathBuf::from(s)
    } else if let Some(proj_dirs) = ProjectDirs::from("com", "muzomer", "shanti") {
        proj_dirs.config_local_dir().to_path_buf()
    } else {
        return Err(eyre::eyre!("Unable to find config directory for shanti"));
    };
    Ok(directory)
}
