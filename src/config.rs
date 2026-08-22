//! Shanti's configuration file.
//!
//! Shanti was configured entirely through environment variables and CLI flags.
//! This module adds a persistent file so the settings a user rarely changes
//! (where their repositories live, which VCS backend to prefer, which editor to
//! open) do not have to be repeated on every invocation.
//!
//! Format: **TOML**. It is the de-facto configuration format of the Rust
//! ecosystem — users already edit `Cargo.toml`, the parser is a single small
//! dependency, and its parse errors carry a line/column span, which is what
//! lets us report the offending key instead of panicking.
//!
//! Location: `<config dir>/config.toml`, where the config directory is the
//! platform one (`ProjectDirs::config_local_dir`) unless `SHANTI_CONFIG`
//! overrides it — the same shape as `SHANTI_DATA`. A missing file is not an
//! error: it means "use the defaults".
//!
//! ```toml
//! repos_dirs = ["~/src", "~/work"]
//! worktrees_dir = "~/worktrees"
//! run_fetch = true
//! backend = "jujutsu"
//! editor = "nvim"
//! ```
//!
//! This module only *reads* the file. It is the weakest of the three
//! configuration layers, so the file's values are handed to [`crate::cli`],
//! which decides whether they win and then normalises whatever did.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, Context};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// File name looked up inside the configuration directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Version control backend used when creating a workspace for a repository
/// that supports more than one.
///
/// Kept as a plain enum local to the config layer so the file format does not
/// move whenever the VCS abstraction does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// Plain git worktrees.
    #[default]
    Git,
    /// Jujutsu workspaces.
    #[serde(alias = "jj")]
    Jujutsu,
}

/// The settings shanti reads from disk.
///
/// Every field is optional in the file; anything absent falls back to
/// [`Default`]. Unknown keys are rejected so a typo is reported rather than
/// silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Directories scanned for git repositories.
    ///
    /// Kept exactly as written: `~` expansion and canonicalisation happen in
    /// `cli::resolve`, once, for whichever layer wins the precedence contest.
    pub repos_dirs: Vec<PathBuf>,
    /// Directory under which worktrees/workspaces are created. Also normalised
    /// by `cli::resolve` rather than here.
    pub worktrees_dir: Option<PathBuf>,
    /// Whether to fetch every repository at startup.
    pub run_fetch: bool,
    /// Backend preferred when creating a workspace in a new repository.
    pub backend: Backend,
    /// Command used to open a worktree in an editor, e.g. `nvim` or `code`.
    pub editor: Option<String>,
}

impl Config {
    /// Load the configuration from the resolved config directory.
    ///
    /// A missing file yields [`Config::default`]. A malformed file is an error
    /// naming the file and the offending key.
    pub fn load() -> eyre::Result<Self> {
        let path = Self::path()?;
        Self::load_from(&path)
    }

    /// Path of the configuration file: `<config dir>/config.toml`.
    pub fn path() -> eyre::Result<PathBuf> {
        Ok(crate::dirs::get_config_dir()?.join(CONFIG_FILE_NAME))
    }

    /// Load the configuration from an explicit path.
    ///
    /// Separated from [`Config::load`] so it can be tested without touching the
    /// user's real config directory.
    pub fn load_from(path: &Path) -> eyre::Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            // Not having a config file is the normal case, not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                debug!(
                    "No configuration file at {}, using defaults",
                    path.display()
                );
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).wrap_err_with(|| {
                    format!("Could not read the configuration file {}", path.display())
                });
            }
        };

        // toml's Display already renders the offending line, column and key, so
        // the wrapper only has to add which file it came from.
        let config: Self = toml::from_str(&contents)
            .wrap_err_with(|| format!("Invalid configuration file {}", path.display()))?;

        debug!("Loaded configuration from {}: {:?}", path.display(), config);
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("could not create a temporary directory");
        let path = dir.path().join(CONFIG_FILE_NAME);
        std::fs::write(&path, contents).expect("could not write the configuration file");
        (dir, path)
    }

    #[test]
    fn config_missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_from(&dir.path().join(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.backend, Backend::Git);
        assert!(!config.run_fetch);
    }

    #[test]
    fn config_empty_file_yields_defaults() {
        let (_dir, path) = write_config("");
        assert_eq!(Config::load_from(&path).unwrap(), Config::default());
    }

    #[test]
    fn config_reads_every_setting() {
        let (_dir, path) = write_config(
            r#"
            repos_dirs = ["/src", "/work"]
            worktrees_dir = "/worktrees"
            run_fetch = true
            backend = "jujutsu"
            editor = "nvim"
            "#,
        );
        let config = Config::load_from(&path).unwrap();
        assert_eq!(
            config.repos_dirs,
            vec![PathBuf::from("/src"), PathBuf::from("/work")]
        );
        assert_eq!(config.worktrees_dir, Some(PathBuf::from("/worktrees")));
        assert!(config.run_fetch);
        assert_eq!(config.backend, Backend::Jujutsu);
        assert_eq!(config.editor.as_deref(), Some("nvim"));
    }

    #[test]
    fn config_partial_file_keeps_defaults_for_the_rest() {
        let (_dir, path) = write_config("run_fetch = true\n");
        let config = Config::load_from(&path).unwrap();
        assert!(config.run_fetch);
        assert_eq!(config.backend, Backend::Git);
        assert!(config.repos_dirs.is_empty());
        assert_eq!(config.worktrees_dir, None);
    }

    #[test]
    fn config_accepts_jj_as_a_backend_alias() {
        let (_dir, path) = write_config(r#"backend = "jj""#);
        assert_eq!(Config::load_from(&path).unwrap().backend, Backend::Jujutsu);
    }

    /// Paths are handed over verbatim: expanding them here as well would put a
    /// second copy of the normalisation rules in the codebase, and the loader
    /// has no way to know whether this layer is the one that wins.
    #[test]
    fn config_keeps_paths_verbatim_for_the_cli_to_normalise() {
        let (_dir, path) = write_config(
            r#"
            repos_dirs = ["~/src"]
            worktrees_dir = "~/worktrees"
            "#,
        );
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.repos_dirs, vec![PathBuf::from("~/src")]);
        assert_eq!(config.worktrees_dir, Some(PathBuf::from("~/worktrees")));
    }

    #[test]
    fn config_wrong_type_reports_the_file_and_the_key() {
        let (_dir, path) = write_config("run_fetch = \"yes\"\n");
        let error = format!("{:?}", Config::load_from(&path).unwrap_err());
        assert!(error.contains("Invalid configuration file"), "{error}");
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(error.contains("run_fetch"), "{error}");
    }

    #[test]
    fn config_unknown_key_is_reported_not_ignored() {
        let (_dir, path) = write_config("run_fecth = true\n");
        let error = format!("{:?}", Config::load_from(&path).unwrap_err());
        assert!(error.contains("run_fecth"), "{error}");
    }

    /// `SHANTI_CONFIG` is the documented escape hatch, so the path resolution
    /// is worth pinning even though it has to touch the process environment.
    #[test]
    fn config_path_honours_the_override_env_var() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SHANTI_CONFIG", dir.path());
        let path = Config::path();
        std::env::remove_var("SHANTI_CONFIG");
        assert_eq!(path.unwrap(), dir.path().join(CONFIG_FILE_NAME));
    }

    #[test]
    fn config_unknown_backend_is_reported() {
        let (_dir, path) = write_config(r#"backend = "mercurial""#);
        let error = format!("{:?}", Config::load_from(&path).unwrap_err());
        assert!(error.contains("backend"), "{error}");
    }
}
