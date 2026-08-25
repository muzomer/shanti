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
//! theme = "catppuccin-mocha"
//!
//! # Runs after every space, in every repository.
//! [hooks]
//! copy = [".envrc"]
//! run = ["direnv allow"]
//!
//! # Runs after those, only for the repository keyed here.
//! [repos.shanti.hooks]
//! copy = [".env"]
//! run = ["cargo fetch"]
//! ```
//!
//! Reading is the bulk of this module. It is the weakest of the three
//! configuration layers, so the file's values are handed to [`crate::cli`],
//! which decides whether they win and then normalises whatever did.
//!
//! [`persist_theme`] is the one exception, and the only writer in the whole
//! codebase. The rule it obeys: **the file belongs to the user**. Shanti owns
//! exactly one key in it — `theme`, the scheme the picker changed — and edits
//! that key in place with `toml_edit`, leaving every comment, blank line,
//! ordering choice and key shanti does not model byte-identical. Serialising a
//! [`Config`] back out would be shorter and would quietly delete all of it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{self, Context};
use serde::{Deserialize, Serialize};
use toml_edit::{value, DocumentMut};
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
    /// Colour scheme, named from the catalogue in [`crate::theme::scheme`].
    ///
    /// Kept as a plain string rather than an enum: the catalogue is the one
    /// list of valid names, and mirroring it into a serde enum here would give
    /// a typo a parser error that names neither the schemes nor this file.
    /// [`Config::validate`] checks it instead, once, on load.
    pub theme: Option<String>,
    /// Hooks run after *every* space is created, whatever its repository.
    pub hooks: Hooks,
    /// Per-repository settings, keyed by the repository's name or its absolute
    /// path — see [`RepoConfig`].
    pub repos: BTreeMap<String, RepoConfig>,
}

/// Settings that apply to one repository only.
///
/// Keyed in the file by the repository's directory name (`[repos.shanti]`) or,
/// when two checkouts share a name, by its absolute path
/// (`[repos."/home/me/src/shanti"]`). Both forms may match; the name-keyed entry
/// is the general one and runs first.
///
/// This lives in the *user's own* configuration file on purpose. shanti never
/// reads a hook out of a repository's working tree, so cloning a hostile
/// repository can never make shanti run its code — see [`crate::hooks`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RepoConfig {
    /// Hooks run after a space of this repository is created.
    pub hooks: Hooks,
}

/// What to do once a new space exists on disk.
///
/// A fresh space is a bare checkout: everything a project needs but does not
/// version — `.env`, `.envrc`, editor settings, installed dependencies — is
/// missing, and putting it there by hand is the manual step this removes.
///
/// Two lists, because the two needs are different in kind. `copy` carries
/// ignored files over from the source repository and cannot fail in an
/// interesting way; `run` is arbitrary shell, and is the part that can take
/// minutes and go wrong.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Hooks {
    /// Files to copy from the repository root into the new space, as paths
    /// relative to that root. A path that does not exist is skipped, not an
    /// error: a `.env` no one has written yet is the normal case.
    pub copy: Vec<PathBuf>,
    /// Shell commands run in the new space, in order, after the copies.
    pub run: Vec<String>,
}

impl Hooks {
    /// Whether there is nothing to do — so callers can skip the machinery
    /// entirely rather than submit an empty job.
    pub fn is_empty(&self) -> bool {
        self.copy.is_empty() && self.run.is_empty()
    }
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

        config.validate(path)?;
        debug!("Loaded configuration from {}: {:?}", path.display(), config);
        Ok(config)
    }

    /// Rejects values that parse as their type but name nothing real.
    ///
    /// Serde can only say "this is a string"; whether that string is a scheme
    /// is a question only the catalogue can answer. Doing it here means a bad
    /// name is caught when the file is read — at startup, with the file named —
    /// rather than at the first frame that needs a colour.
    fn validate(&self, path: &Path) -> eyre::Result<()> {
        if let Some(name) = &self.theme {
            crate::theme::scheme::find(name).map_err(|error| {
                eyre::eyre!(error).wrap_err(format!(
                    "Invalid configuration file {}: key `theme`",
                    path.display()
                ))
            })?;
        }
        Ok(())
    }
}

/// Record `name` as the `theme` key of the configuration file at `path`.
///
/// This is the only place shanti writes to the user's configuration, so it is
/// deliberately narrow: it parses the existing document, replaces or appends
/// the single top-level `theme` key, and writes the document back. Nothing
/// else in the file moves — a round-trip through [`Config`] would drop the
/// user's comments, their formatting and any key this struct does not model.
///
/// The name is not validated here. The caller already holds a
/// [`crate::theme::scheme::Scheme`] it took from the catalogue, so the only way
/// to reach this function is with a name that exists.
///
/// Failure is reported, never fatal: the scheme is already applied in memory,
/// and a config file that could not be written costs the user the choice at
/// the *next* start and nothing now.
pub fn persist_theme(path: &Path, name: &str) -> eyre::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        // No file yet is the normal first-run case: start from an empty
        // document and let the atomic write create it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).wrap_err_with(|| {
                format!("Could not read the configuration file {}", path.display())
            });
        }
    };

    let mut document: DocumentMut = existing.parse().wrap_err_with(|| {
        format!(
            "Could not update the configuration file {}: it is not valid TOML",
            path.display()
        )
    })?;

    // Indexing assigns in place when the key is already there, and appends to
    // the root table when it is not — toml_edit renders root key/values ahead
    // of any `[table]`, so appending can never land the key inside `[hooks]`.
    document["theme"] = value(name);

    write_atomically(path, &document.to_string())
        .wrap_err_with(|| format!("Could not write the configuration file {}", path.display()))
}

/// Write `contents` to `path` so that `path` is never seen half-written.
///
/// A neighbouring temporary file plus a rename, because rename within one
/// directory is atomic: a crash or a full disk leaves either the old file or
/// the new one, never a truncated config the next start would refuse to parse.
fn write_atomically(path: &Path, contents: &str) -> eyre::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir)
            .wrap_err_with(|| format!("Could not create the directory {}", dir.display()))?;
    }

    // Same directory as the target: a rename across filesystems is not atomic
    // and would fail outright, which is exactly what a system temp dir risks.
    let file_name = path
        .file_name()
        .unwrap_or_else(|| CONFIG_FILE_NAME.as_ref());
    let mut temp = path.to_path_buf();
    temp.set_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));

    std::fs::write(&temp, contents)?;
    if let Err(error) = std::fs::rename(&temp, path) {
        // Leaving a stray dotfile next to the config would be a second, quieter
        // failure, so clean up before reporting the first.
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
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
            theme = "catppuccin-mocha"
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
        assert_eq!(config.theme.as_deref(), Some("catppuccin-mocha"));
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
    fn config_reads_global_and_per_repository_hooks() {
        let (_dir, path) = write_config(
            r#"
            [hooks]
            copy = [".envrc"]
            run = ["direnv allow"]

            [repos.shanti.hooks]
            copy = [".env"]
            run = ["cargo fetch", "cargo build"]
            "#,
        );
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.hooks.copy, vec![PathBuf::from(".envrc")]);
        assert_eq!(config.hooks.run, vec!["direnv allow".to_string()]);
        let repo = config.repos.get("shanti").expect("the repo entry is read");
        assert_eq!(repo.hooks.copy, vec![PathBuf::from(".env")]);
        assert_eq!(repo.hooks.run.len(), 2);
    }

    /// A path-keyed entry is how two checkouts sharing a directory name are
    /// told apart, so quoting a path as a key has to work.
    #[test]
    fn config_repo_entries_may_be_keyed_by_path() {
        let (_dir, path) = write_config(
            r#"
            [repos."/home/me/src/shanti".hooks]
            run = ["true"]
            "#,
        );
        let config = Config::load_from(&path).unwrap();
        assert!(config.repos.contains_key("/home/me/src/shanti"));
    }

    #[test]
    fn config_without_hooks_has_none() {
        let (_dir, path) = write_config("run_fetch = true\n");
        let config = Config::load_from(&path).unwrap();
        assert!(config.hooks.is_empty());
        assert!(config.repos.is_empty());
    }

    #[test]
    fn config_unknown_hook_key_is_reported() {
        let (_dir, path) = write_config("[hooks]\ncopyy = [\".env\"]\n");
        let error = format!("{:?}", Config::load_from(&path).unwrap_err());
        assert!(error.contains("copyy"), "{error}");
    }

    /// A theme is only a string to serde, so the load has to be what rejects a
    /// name that is not in the catalogue — and the message has to teach the
    /// user the names they could have written instead.
    #[test]
    fn config_unknown_theme_lists_the_valid_schemes() {
        let (_dir, path) = write_config(r#"theme = "dracula""#);
        let error = format!("{:?}", Config::load_from(&path).unwrap_err());
        assert!(error.contains("theme"), "{error}");
        assert!(error.contains("dracula"), "{error}");
        for scheme in crate::theme::scheme::ALL {
            assert!(error.contains(scheme.name), "{error}");
        }
    }

    /// Names come out of a file a human typed, so the leniency the catalogue
    /// promises has to survive the load.
    #[test]
    fn config_theme_name_may_differ_in_case() {
        let (_dir, path) = write_config(r#"theme = "Catppuccin-Latte""#);
        assert_eq!(
            Config::load_from(&path).unwrap().theme.as_deref(),
            Some("Catppuccin-Latte")
        );
    }

    /// The whole point of `toml_edit`: a user's file is theirs. Comments, blank
    /// lines, ordering and tables shanti does not model must come back out
    /// byte-identical, with only the one owned key changed.
    #[test]
    fn persist_theme_preserves_comments_blank_lines_and_tables() {
        let original = "\
# Where my repositories live.
repos_dirs = [\"/src\"]

run_fetch = true # fetch on start

# Runs after every space.
[hooks]
copy = [\".envrc\"]
run = [\"direnv allow\"]
";
        let (_dir, path) = write_config(original);
        persist_theme(&path, "catppuccin-mocha").unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        // Every original line survives, in its original order.
        assert!(
            written.contains("# Where my repositories live."),
            "{written}"
        );
        assert!(
            written.contains("run_fetch = true # fetch on start"),
            "{written}"
        );
        assert!(written.contains("# Runs after every space."), "{written}");
        assert!(written.contains("\n\n"), "blank lines were lost: {written}");
        assert!(written.contains("[hooks]"), "{written}");
        // The only difference is the added key.
        assert!(
            written.contains("theme = \"catppuccin-mocha\""),
            "{written}"
        );
        // A root key appended after a `[table]` would otherwise land inside it.
        let theme_at = written.find("theme =").unwrap();
        assert!(theme_at < written.find("[hooks]").unwrap(), "{written}");
        // And the file still parses as the config it was.
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.theme.as_deref(), Some("catppuccin-mocha"));
        assert_eq!(config.hooks.copy, vec![PathBuf::from(".envrc")]);
    }

    #[test]
    fn persist_theme_creates_a_missing_file_and_directory() {
        let dir = tempfile::tempdir().unwrap();
        // Two levels deep: neither the file nor its directory exists yet.
        let path = dir
            .path()
            .join("nested")
            .join("shanti")
            .join(CONFIG_FILE_NAME);
        persist_theme(&path, "gruvbox-dark").unwrap();

        assert_eq!(
            Config::load_from(&path).unwrap().theme.as_deref(),
            Some("gruvbox-dark")
        );
    }

    #[test]
    fn persist_theme_replaces_an_existing_key_in_place() {
        let (_dir, path) =
            write_config("run_fetch = true\ntheme = \"tokyo-night\"\neditor = \"nvim\"\n");
        persist_theme(&path, "catppuccin-latte").unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written, "run_fetch = true\ntheme = \"catppuccin-latte\"\neditor = \"nvim\"\n",
            "the key moved instead of being replaced where it stood"
        );
        assert_eq!(written.matches("theme").count(), 1, "{written}");
    }

    /// Writing can fail for reasons shanti does not control — a read-only home,
    /// a full disk. The scheme is already applied in memory, so the failure has
    /// to come back as a value the caller can show, never a panic or an exit.
    #[test]
    fn persist_theme_reports_a_write_failure_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the config *directory* should be: creating the
        // directory fails, and with it the write.
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, "").unwrap();
        let path = blocker.join(CONFIG_FILE_NAME);

        let error = format!("{:?}", persist_theme(&path, "tokyo-night").unwrap_err());
        assert!(error.contains(&path.display().to_string()), "{error}");
        // Nothing was left behind next to the blocker.
        assert!(blocker.is_file());
    }

    /// A file shanti cannot parse must not be flattened into a one-key file:
    /// refusing keeps whatever the user was mid-way through editing.
    #[test]
    fn persist_theme_refuses_to_overwrite_an_unparseable_file() {
        let (_dir, path) = write_config("this is not = = toml\n");
        assert!(persist_theme(&path, "tokyo-night").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "this is not = = toml\n"
        );
    }

    /// The temporary file is an implementation detail: it must never survive a
    /// successful write and be picked up as clutter in the config directory.
    #[test]
    fn persist_theme_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        persist_theme(&path, "tokyo-night").unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from(CONFIG_FILE_NAME)]);
    }

    #[test]
    fn config_unknown_backend_is_reported() {
        let (_dir, path) = write_config(r#"backend = "mercurial""#);
        let error = format!("{:?}", Config::load_from(&path).unwrap_err());
        assert!(error.contains("backend"), "{error}");
    }
}
