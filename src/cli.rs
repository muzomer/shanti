use std::path::{Path, PathBuf};

use clap::Parser;
use color_eyre::eyre::{eyre, Result, WrapErr};

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Directory where the new git worktrees will be stored
    #[arg(
        short = 'd',
        long = "worktrees-dir",
        value_name = "DIR",
        env = "SHANTI_WORKTREES_DIR"
    )]
    // TODO: list worktrees from the repositories directly instead of getting the worktrees_dir from user
    pub worktrees_dir: String,
    /// Directory of the git repositories (colon-separated for multiple)
    #[arg(
        short = 'r',
        long = "repos-dir",
        value_name = "DIR",
        env = "SHANTI_REPOS_DIR",
        num_args = 1..,
        value_delimiter = ':'
    )]
    pub repos_dirs: Vec<String>,

    /// Whether to run git fetch for each repo. Default: false
    #[arg(
        short = 'f',
        long = "run-fetch",
        value_name = "BOOLEAN",
        default_value_t = false
    )]
    pub run_fetch: bool,
}

impl Args {
    /// Parses the command line and resolves every directory to an absolute path.
    ///
    /// Prefer this over [`Args::new`]: a bad path is a user mistake, not a bug,
    /// so it belongs in the error channel that `main` already routes to stderr.
    pub fn try_new() -> Result<Self> {
        Self::parse().resolve()
    }

    /// Same as [`Args::try_new`], but reports the error itself and exits.
    ///
    /// Only exists because `App::new` cannot yet propagate an error. It runs
    /// before the alternate screen is entered, so writing to stderr here is
    /// safe. Remove it once `App::new` returns a `Result` and can call
    /// [`Args::try_new`] directly.
    pub fn new() -> Self {
        match Self::try_new() {
            Ok(args) => args,
            Err(error) => {
                // `{:#}` keeps the whole chain on a single line, so the user sees
                // both what we were doing and why the OS refused.
                eprintln!("shanti: {error:#}");
                std::process::exit(1);
            }
        }
    }

    /// Turns the raw string arguments into absolute, existing directories.
    fn resolve(mut self) -> Result<Self> {
        // Splitting on ':' can yield nothing at all (an empty environment
        // variable, for instance), and an empty list would silently show a UI
        // with no repositories instead of explaining what is missing.
        if self.repos_dirs.is_empty() {
            return Err(eyre!(
                "--repos-dir: no repository directory given (set it or SHANTI_REPOS_DIR)"
            ));
        }

        self.repos_dirs = self
            .repos_dirs
            .iter()
            .map(|dir| resolve_existing_dir(dir, "--repos-dir"))
            .collect::<Result<Vec<_>>>()?;

        // The worktrees directory is an output location, so create it rather
        // than making the user run `mkdir` before their first worktree.
        let worktrees_dir = expand(&self.worktrees_dir, "--worktrees-dir")?;
        std::fs::create_dir_all(&worktrees_dir).wrap_err_with(|| {
            format!(
                "--worktrees-dir: could not create '{}'",
                worktrees_dir.display()
            )
        })?;
        self.worktrees_dir = resolve_existing_dir(&self.worktrees_dir, "--worktrees-dir")?;

        Ok(self)
    }
}

/// Expands a leading `~`, failing with the flag name so the user knows which
/// argument to fix.
fn expand(dir: &str, flag: &str) -> Result<PathBuf> {
    expand_tilde::expand_tilde_owned(dir)
        .wrap_err_with(|| format!("{flag}: could not expand '~' in '{dir}'"))
}

/// Resolves `dir` to an absolute path, requiring it to exist.
fn resolve_existing_dir(dir: &str, flag: &str) -> Result<String> {
    let expanded = expand(dir, flag)?;
    let canonical = std::fs::canonicalize(&expanded)
        .wrap_err_with(|| format!("{flag}: could not open directory '{}'", expanded.display()))?;

    if !canonical.is_dir() {
        return Err(eyre!(
            "{flag}: '{}' is not a directory",
            canonical.display()
        ));
    }

    into_utf8(canonical, flag)
}

/// The rest of the program stores directories as `String`, so a path the OS
/// accepts but Rust cannot represent as UTF-8 has to be rejected here.
fn into_utf8(path: PathBuf, flag: &str) -> Result<String> {
    path.into_os_string().into_string().map_err(|raw| {
        eyre!(
            "{flag}: path is not valid UTF-8: '{}'",
            Path::new(&raw).display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(repos_dirs: Vec<String>, worktrees_dir: String) -> Args {
        Args {
            worktrees_dir,
            repos_dirs,
            run_fetch: false,
        }
    }

    #[test]
    fn missing_repos_dir_reports_the_path_and_the_flag() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("nope");

        let error = args(
            vec![missing.to_str().unwrap().to_string()],
            temp.path().to_str().unwrap().to_string(),
        )
        .resolve()
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("--repos-dir"), "{message}");
        assert!(message.contains("nope"), "{message}");
    }

    #[test]
    fn empty_repos_dirs_is_rejected() {
        let temp = tempfile::tempdir().unwrap();

        let error = args(vec![], temp.path().to_str().unwrap().to_string())
            .resolve()
            .unwrap_err();

        assert!(format!("{error:#}").contains("--repos-dir"));
    }

    #[test]
    fn missing_worktrees_dir_is_created() {
        let temp = tempfile::tempdir().unwrap();
        let worktrees = temp.path().join("worktrees").join("nested");

        let resolved = args(
            vec![temp.path().to_str().unwrap().to_string()],
            worktrees.to_str().unwrap().to_string(),
        )
        .resolve()
        .unwrap();

        assert!(worktrees.is_dir());
        assert!(resolved.worktrees_dir.ends_with("nested"));
    }

    #[test]
    fn a_file_where_a_repos_dir_is_expected_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("not-a-dir");
        std::fs::write(&file, b"").unwrap();

        let error = args(
            vec![file.to_str().unwrap().to_string()],
            temp.path().to_str().unwrap().to_string(),
        )
        .resolve()
        .unwrap_err();

        assert!(format!("{error:#}").contains("is not a directory"));
    }

    #[test]
    fn resolved_paths_are_absolute() {
        let temp = tempfile::tempdir().unwrap();

        let resolved = args(
            vec![temp.path().to_str().unwrap().to_string()],
            temp.path().to_str().unwrap().to_string(),
        )
        .resolve()
        .unwrap();

        assert!(Path::new(&resolved.repos_dirs[0]).is_absolute());
        assert!(Path::new(&resolved.worktrees_dir).is_absolute());
    }
}
