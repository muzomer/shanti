//! Command line parsing and the single place where configuration precedence is
//! decided.
//!
//! Shanti takes its settings from three places. They are layered, lowest to
//! highest:
//!
//! 1. built-in defaults,
//! 2. the configuration file (see [`crate::config`]),
//! 3. environment variables,
//! 4. command line flags.
//!
//! Clap already reads the environment for us, which is convenient but hides the
//! distinction we need: by the time we look at the parsed struct, a value that
//! came from `SHANTI_REPOS_DIR`, a value the user typed, and a value clap
//! invented from a default all look identical. `ArgMatches::value_source` is
//! what tells them apart, so the merge below reads the *source* of every
//! setting rather than only its value. Without that, a config file entry would
//! lose to clap's default and appear to be ignored.
//!
//! Path normalisation (tilde expansion, canonicalisation, the "is it really a
//! directory" checks) happens once, in [`resolve`], *after* the winner of each
//! setting is known. That way a `~` in the config file is expanded exactly like
//! a `~` on the command line, and the logic exists in one copy.
//!
//! The repository list is deliberately the lenient one: a missing entry is
//! skipped with a warning and only an empty result is fatal. See
//! [`resolve_repos_dirs`].

use std::fmt;
use std::path::{Path, PathBuf};

use clap::{parser::ValueSource, ArgMatches, CommandFactory, FromArgMatches, Parser};
use color_eyre::eyre::{eyre, Result, WrapErr};
use tracing::{debug, warn};

use crate::config::{Backend, Config};
use crate::hooks::HookSettings;
use crate::theme::{scheme, Scheme};

/// The raw command line, before any merging or path resolution.
///
/// Every setting is optional here: "absent" is what lets a lower layer win.
/// `run_fetch` is a plain `bool` because clap's `SetTrue` action always yields
/// one; its *source* is what says whether the user asked for it.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Directory where the new git worktrees will be stored
    #[arg(
        short = 'd',
        long = "worktrees-dir",
        value_name = "DIR",
        env = "SHANTI_WORKTREES_DIR"
    )]
    // TODO: list worktrees from the repositories directly instead of getting the worktrees_dir from user
    worktrees_dir: Option<String>,

    /// Directory of the git repositories (colon-separated for multiple)
    #[arg(
        short = 'r',
        long = "repos-dir",
        value_name = "DIR",
        env = "SHANTI_REPOS_DIR",
        num_args = 1..,
        value_delimiter = ':'
    )]
    repos_dirs: Vec<String>,

    /// Whether to run git fetch for each repo. Default: false
    #[arg(short = 'f', long = "run-fetch", env = "SHANTI_RUN_FETCH")]
    run_fetch: bool,

    /// Colour scheme to use, e.g. `tokyo-night` or `catppuccin-latte`
    #[arg(long = "theme", value_name = "NAME", env = "SHANTI_THEME")]
    theme: Option<String>,

    /// Configuration file to read instead of the default location
    #[arg(long = "config", value_name = "FILE")]
    config: Option<PathBuf>,

    /// Print the effective configuration, with the origin of each value, and exit
    #[arg(long = "show-config")]
    show_config: bool,

    /// Skip every post-create hook for this run
    ///
    /// The flag half of `SHANTI_NO_HOOKS`. Deliberately *not* wired to clap's
    /// `env`: the variable is a switch that any non-empty value sets — which is
    /// what a user typing `SHANTI_NO_HOOKS=1` means — while clap would insist
    /// on `true` or `false` and reject the rest. `HookSettings::from_config`
    /// reads it instead, so the two spellings mean the same thing and the
    /// variable has exactly one reader.
    #[arg(long = "no-hooks")]
    no_hooks: bool,
}

/// Where a setting's final value came from.
///
/// Ordered lowest to highest so the variants themselves document the
/// precedence, and so a test can assert the order rather than prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// Nothing set it; this is shanti's built-in value.
    Default,
    /// Read from the configuration file.
    ConfigFile,
    /// Read from an environment variable by clap.
    Environment,
    /// Typed on the command line.
    CommandLine,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Origin::Default => "built-in default",
            Origin::ConfigFile => "config file",
            Origin::Environment => "environment",
            Origin::CommandLine => "command line",
        };
        f.write_str(name)
    }
}

/// The origin of every resolved setting, kept so `--show-config` can answer
/// "why is it using that directory?" without anyone reading the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origins {
    pub worktrees_dir: Origin,
    pub repos_dirs: Origin,
    pub run_fetch: Origin,
    pub backend: Origin,
    pub editor: Origin,
    pub theme: Origin,
    /// Where the *on/off* of hooks came from: the command line or the
    /// environment when they were switched off, the file otherwise. The lists
    /// themselves have only ever one source, and it is the file.
    pub hooks: Origin,
}

/// Which layer clap took each of its own values from.
///
/// Only clap can answer this, so it is captured next to the parse and then
/// passed into the merge as plain data, which keeps [`resolve`] testable
/// without building an `ArgMatches`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Sources {
    worktrees_dir: Option<Origin>,
    repos_dirs: Option<Origin>,
    run_fetch: Option<Origin>,
    theme: Option<Origin>,
}

impl Sources {
    fn from_matches(matches: &ArgMatches) -> Self {
        Self {
            worktrees_dir: clap_origin(matches, "worktrees_dir"),
            repos_dirs: clap_origin(matches, "repos_dirs"),
            run_fetch: clap_origin(matches, "run_fetch"),
            theme: clap_origin(matches, "theme"),
        }
    }
}

/// Translates clap's value source into an [`Origin`], collapsing "clap made
/// this up" to `None` so the config file can still win.
fn clap_origin(matches: &ArgMatches, id: &str) -> Option<Origin> {
    match matches.value_source(id) {
        Some(ValueSource::CommandLine) => Some(Origin::CommandLine),
        Some(ValueSource::EnvVariable) => Some(Origin::Environment),
        // `DefaultValue` means the user said nothing, so it must not outrank
        // the configuration file. Anything clap adds later is treated the same.
        _ => None,
    }
}

/// The effective configuration, with every directory already resolved to an
/// absolute, existing path.
#[derive(Debug, Clone)]
pub struct Args {
    /// Directory where new worktrees/workspaces are created.
    pub worktrees_dir: String,
    /// Directories scanned for repositories.
    pub repos_dirs: Vec<String>,
    /// Whether to fetch every repository at startup.
    pub run_fetch: bool,
    /// Backend preferred when creating a workspace in a new repository.
    pub backend: Backend,
    /// Command used to open a worktree in an editor.
    pub editor: Option<String>,
    /// The colour scheme in force for this run.
    ///
    /// Resolved to a catalogue entry rather than kept as the string the user
    /// wrote: validation belongs to the layer that decides the winner, so no
    /// later reader — the startup `theme::set`, the picker, `--show-config` —
    /// can be handed a name that turns out not to exist.
    pub theme: &'static Scheme,
    /// Where each of the above came from.
    pub origins: Origins,
    /// Configuration file consulted, whether or not it existed.
    pub config_path: PathBuf,
    /// The user asked for the configuration to be printed instead of the UI.
    pub show_config: bool,
    /// What to run after a space is created, already resolved.
    ///
    /// Hooks ride on `Args` rather than being loaded again inside `App` because
    /// they are a *setting*, and this is the one place a setting is resolved:
    /// `--no-hooks` is a command line layer over `SHANTI_NO_HOOKS` over the
    /// configuration file, which is exactly the precedence machinery here. It
    /// also keeps `App::with_args` honest — construction still reads neither
    /// argv, the environment nor the configuration file, so a test points an
    /// `App` at its own hooks the same way it points it at its own directories.
    pub hooks: HookSettings,
}

impl Args {
    /// Parses the command line, merges it with the configuration file, and
    /// resolves every directory to an absolute path.
    ///
    /// A bad path is a user mistake, not a bug, so it is returned rather than
    /// reported here: `main` already routes the error channel to stderr, and it
    /// is the only place allowed to end the process.
    pub fn try_new() -> Result<Self> {
        Self::from_matches(&Cli::command().get_matches())
    }

    /// Builds the effective configuration from directories the caller already
    /// resolved, consulting neither the command line, the environment, nor the
    /// configuration file.
    ///
    /// This is the seam that lets an `App` be built without touching anything
    /// process-global: tests hand in their own temp directories instead of
    /// exporting `SHANTI_*` variables and serialising on a lock. Every origin is
    /// reported as [`Origin::Default`] because no configuration layer was
    /// consulted — the caller *is* the source.
    ///
    /// The paths are taken as given: the caller owns them, so there is nothing
    /// here to expand or validate.
    pub fn for_dirs(worktrees_dir: impl Into<String>, repos_dirs: Vec<String>) -> Self {
        Self {
            worktrees_dir: worktrees_dir.into(),
            repos_dirs,
            run_fetch: false,
            backend: Backend::default(),
            editor: None,
            theme: default_scheme(),
            origins: Origins {
                worktrees_dir: Origin::Default,
                repos_dirs: Origin::Default,
                run_fetch: Origin::Default,
                backend: Origin::Default,
                editor: Origin::Default,
                theme: Origin::Default,
                hooks: Origin::Default,
            },
            config_path: PathBuf::new(),
            show_config: false,
            // No configuration layer was consulted, so there is nothing to run.
            // `disabled()` rather than `from_config(Config::default())`: the
            // latter would read `SHANTI_NO_HOOKS`, and this seam exists
            // precisely so that nothing process-global is touched.
            hooks: HookSettings::disabled(),
        }
    }

    /// The same configuration with hooks of the caller's choosing.
    ///
    /// The companion to [`Args::for_dirs`]: a test that wants to prove a hook
    /// ran needs to hand one in without a configuration file on disk.
    pub fn with_hooks(mut self, hooks: HookSettings) -> Self {
        self.hooks = hooks;
        self
    }

    /// Loads the configuration file named by the parsed command line and merges
    /// everything into the effective configuration.
    fn from_matches(matches: &ArgMatches) -> Result<Self> {
        let cli = Cli::from_arg_matches(matches)?;

        let config_path = match &cli.config {
            Some(path) => expand(path).wrap_err("--config")?,
            None => Config::path()?,
        };
        let config = Config::load_from(&config_path)?;

        resolve(cli, Sources::from_matches(matches), config, config_path)
    }

    /// Renders the effective configuration and the origin of each value.
    ///
    /// Returned as a `String` rather than printed so it can be asserted on in
    /// tests.
    pub fn report(&self) -> String {
        let state = if self.config_path.is_file() {
            "loaded"
        } else {
            "not found, using defaults"
        };
        let mut out = format!("config file: {} ({state})\n\n", self.config_path.display());

        let origins = &self.origins;
        out.push_str(&setting(
            "worktrees_dir",
            &self.worktrees_dir,
            origins.worktrees_dir,
        ));

        // A multi-valued setting still has a single origin: the layer that won.
        let first = self.repos_dirs.first().map(String::as_str).unwrap_or("");
        out.push_str(&setting("repos_dirs", first, origins.repos_dirs));
        for dir in self.repos_dirs.iter().skip(1) {
            out.push_str(&format!("{:<14}   {dir}\n", ""));
        }

        out.push_str(&setting(
            "run_fetch",
            &self.run_fetch.to_string(),
            origins.run_fetch,
        ));
        out.push_str(&setting(
            "backend",
            &format!("{:?}", self.backend).to_lowercase(),
            origins.backend,
        ));
        out.push_str(&setting(
            "editor",
            self.editor.as_deref().unwrap_or("<unset>"),
            origins.editor,
        ));
        out.push_str(&setting("theme", self.theme.name, origins.theme));
        // Counts, not the commands themselves: the report answers "is anything
        // going to run?", and a user who wants to know *what* has the file open.
        out.push_str(&setting("hooks", &self.hooks_summary(), origins.hooks));
        out
    }

    /// The one-line answer to "what will run after a space is created?".
    fn hooks_summary(&self) -> String {
        let counts = self.hooks.counts();
        let configured = if counts.is_empty() {
            "none configured".to_string()
        } else {
            format!(
                "{} file(s) copied, {} command(s), {} repo(s) with their own",
                counts.copies, counts.commands, counts.repos
            )
        };
        if self.hooks.enabled() {
            configured
        } else {
            format!("disabled ({configured})")
        }
    }
}

fn setting(name: &str, value: &str, origin: Origin) -> String {
    format!("{name:<14} = {value}  ({origin})\n")
}

/// Merges the three layers and normalises every path that survives.
///
/// Split out of [`Args::from_matches`] so the precedence rules can be tested
/// without a real configuration file on disk.
fn resolve(cli: Cli, sources: Sources, config: Config, config_path: PathBuf) -> Result<Args> {
    // An empty environment variable reaches clap as an empty value rather than
    // as an absence, and an empty path can never be resolved. Dropping the
    // blanks here lets `SHANTI_REPOS_DIR=` fall through to the next layer
    // instead of failing on a path that was never really given.
    let cli_worktrees_dir = cli.worktrees_dir.filter(|dir| !dir.trim().is_empty());
    let cli_repos_dirs: Vec<String> = cli
        .repos_dirs
        .into_iter()
        .filter(|dir| !dir.trim().is_empty())
        .collect();

    // --- hooks -------------------------------------------------------------
    // Resolved from the whole configuration rather than field by field: what
    // runs after a space is created is the `[hooks]` table *and* every
    // `[repos.<name>.hooks]` table, and which of them applies is not known
    // until there is a repository in hand. `--no-hooks` (and the
    // `SHANTI_NO_HOOKS` clap reads into it) short-circuits all of it.
    let hooks = HookSettings::from_config(config.clone());
    let hooks = if cli.no_hooks {
        hooks.switched_off()
    } else {
        hooks
    };
    // Which layer turned them off, or the file that listed them. `from_config`
    // is the only reader of `SHANTI_NO_HOOKS`, so "off without the flag" is
    // precisely "off by the environment".
    let hooks_origin = match (cli.no_hooks, hooks.enabled()) {
        (true, _) => Origin::CommandLine,
        (false, false) => Origin::Environment,
        (false, true) => Origin::ConfigFile,
    };

    // --- worktrees_dir -----------------------------------------------------
    let (worktrees_dir, worktrees_origin) = match (sources.worktrees_dir, cli_worktrees_dir) {
        (Some(origin), Some(dir)) => (Some(PathBuf::from(dir)), origin),
        _ => match config.worktrees_dir {
            Some(dir) => (Some(dir), Origin::ConfigFile),
            None => (None, Origin::Default),
        },
    };
    let worktrees_dir = worktrees_dir.ok_or_else(|| {
        eyre!(
            "no worktrees directory given: pass --worktrees-dir, set SHANTI_WORKTREES_DIR, \
             or add worktrees_dir to {}",
            config_path.display()
        )
    })?;

    // --- repos_dirs --------------------------------------------------------
    let (repos_dirs, repos_origin) = match sources.repos_dirs {
        // Splitting on ':' can still yield nothing (an empty environment
        // variable, for instance). An empty layer carries no information, so it
        // falls through to the next one instead of shadowing it.
        Some(origin) if !cli_repos_dirs.is_empty() => (
            cli_repos_dirs.into_iter().map(PathBuf::from).collect(),
            origin,
        ),
        _ if !config.repos_dirs.is_empty() => (config.repos_dirs, Origin::ConfigFile),
        _ => (Vec::new(), Origin::Default),
    };
    if repos_dirs.is_empty() {
        return Err(eyre!(
            "no repository directory given: pass --repos-dir, set SHANTI_REPOS_DIR, \
             or add repos_dirs to {}",
            config_path.display()
        ));
    }

    // --- run_fetch ---------------------------------------------------------
    let (run_fetch, fetch_origin) = match sources.run_fetch {
        Some(origin) => (cli.run_fetch, origin),
        // `run_fetch = false` in the file is indistinguishable from the default
        // and is reported as such; the resulting value is the same either way.
        None if config.run_fetch => (true, Origin::ConfigFile),
        None => (false, Origin::Default),
    };

    // --- theme -------------------------------------------------------------
    // An empty value carries no choice, so it falls through to the next layer
    // exactly like an empty `SHANTI_REPOS_DIR` does.
    let cli_theme = cli.theme.filter(|name| !name.trim().is_empty());
    let (theme_name, theme_origin) = match (sources.theme, cli_theme) {
        (Some(origin), Some(name)) => (Some(name), origin),
        _ => match config.theme.clone() {
            Some(name) => (Some(name), Origin::ConfigFile),
            None => (None, Origin::Default),
        },
    };
    // The file was already checked on load; this catches the flag and the
    // environment, and labels the error with the layer the name came from.
    let theme = match &theme_name {
        Some(name) => scheme::find(name)
            .map_err(|error| eyre!("{}: {error}", label("--theme", theme_origin)))?,
        None => default_scheme(),
    };

    // --- settings the command line does not expose yet ---------------------
    let backend_origin = if config.backend == Backend::default() {
        Origin::Default
    } else {
        Origin::ConfigFile
    };
    let editor_origin = if config.editor.is_some() {
        Origin::ConfigFile
    } else {
        Origin::Default
    };

    let origins = Origins {
        worktrees_dir: worktrees_origin,
        repos_dirs: repos_origin,
        run_fetch: fetch_origin,
        backend: backend_origin,
        editor: editor_origin,
        theme: theme_origin,
        hooks: hooks_origin,
    };
    debug!(?origins, "Resolved the configuration sources");

    // Normalisation happens once, here, so it applies to whichever layer won.
    let repos_dirs = resolve_repos_dirs(&repos_dirs, &label("--repos-dir", repos_origin))?;

    // The worktrees directory is an output location, so create it rather than
    // making the user run `mkdir` before their first worktree.
    let worktrees_label = label("--worktrees-dir", worktrees_origin);
    let expanded = expand(&worktrees_dir).wrap_err_with(|| worktrees_label.clone())?;
    std::fs::create_dir_all(&expanded).wrap_err_with(|| {
        format!(
            "{worktrees_label}: could not create '{}'",
            expanded.display()
        )
    })?;
    let worktrees_dir =
        resolve_existing_dir(&worktrees_dir).wrap_err_with(|| worktrees_label.clone())?;

    Ok(Args {
        worktrees_dir,
        repos_dirs,
        run_fetch,
        backend: config.backend,
        editor: config.editor,
        theme,
        origins,
        config_path,
        show_config: cli.show_config,
        hooks,
    })
}

/// The scheme shanti starts with when no layer names one.
///
/// `expect` rather than a fallback: [`scheme::DEFAULT`] naming something the
/// catalogue does not contain is a bug in the catalogue, and one its own tests
/// already forbid.
fn default_scheme() -> &'static Scheme {
    scheme::find(scheme::DEFAULT).expect("the default scheme is in the catalogue")
}

/// Names the setting the way the user wrote it, so an error points at the file
/// they have to edit rather than at a flag they never typed.
fn label(flag: &str, origin: Origin) -> String {
    match origin {
        Origin::ConfigFile => format!("{flag} (from the config file)"),
        Origin::Environment => format!("{flag} (from the environment)"),
        _ => flag.to_string(),
    }
}

/// Resolves every entry of the repository list, tolerating the ones that are
/// gone.
///
/// The list names places to *look*, not places that must all be there: a stale
/// entry left over from an old machine should not veto the directories that are
/// still perfectly good, so each failure is skipped with a warning. Only an
/// empty result is fatal, and that error repeats every entry with its own
/// reason, because the user of a TUI that never starts has no log to consult.
///
/// This is also what keeps a single directory typed after `--repos-dir` a hard
/// error: it is the only candidate, so nothing survives it and the fatal branch
/// is taken. The same holds for a single-entry environment variable or config
/// file value, which is why the errors are labelled with their origin.
fn resolve_repos_dirs(dirs: &[PathBuf], label: &str) -> Result<Vec<String>> {
    let mut resolved = Vec::with_capacity(dirs.len());
    let mut skipped = Vec::new();

    for dir in dirs {
        match resolve_existing_dir(dir) {
            Ok(path) => resolved.push(path),
            // The reason is kept as text rather than as an error so it can be
            // reported once per entry, either as a warning or inside the
            // combined failure below.
            Err(error) => skipped.push((dir, format!("{error:#}"))),
        }
    }

    if resolved.is_empty() {
        // With a single entry there is no list to summarise, so the reason is
        // reported on its own — this is the `--repos-dir /gone` case, and it
        // reads exactly as it did before the list became tolerant.
        if let [(_, reason)] = skipped.as_slice() {
            return Err(eyre!("{label}: {reason}"));
        }

        // Every reason already names the entry it is about, so the entry is
        // not repeated in front of it.
        let reasons = skipped
            .iter()
            .map(|(_, reason)| format!("\n  {reason}"))
            .collect::<String>();
        return Err(eyre!(
            "{label}: none of the repository directories could be opened:{reasons}"
        ));
    }

    for (dir, reason) in skipped {
        warn!(
            setting = %label,
            directory = %dir.display(),
            %reason,
            "Skipping a repository directory that could not be opened"
        );
    }

    Ok(resolved)
}

/// Expands a leading `~`.
///
/// The setting name is not part of the message: callers add it with
/// `wrap_err`, which lets [`resolve_repos_dirs`] reuse the bare reason for one
/// entry of a list.
fn expand(dir: &Path) -> Result<PathBuf> {
    expand_tilde::expand_tilde(dir)
        .map(|expanded| expanded.into_owned())
        .wrap_err_with(|| format!("could not expand '~' in '{}'", dir.display()))
}

/// Resolves `dir` to an absolute path, requiring it to exist.
fn resolve_existing_dir(dir: &Path) -> Result<String> {
    let expanded = expand(dir)?;
    let canonical = std::fs::canonicalize(&expanded)
        .wrap_err_with(|| format!("could not open directory '{}'", expanded.display()))?;

    if !canonical.is_dir() {
        return Err(eyre!("'{}' is not a directory", canonical.display()));
    }

    into_utf8(canonical)
}

/// The rest of the program stores directories as `String`, so a path the OS
/// accepts but Rust cannot represent as UTF-8 has to be rejected here.
fn into_utf8(path: PathBuf) -> Result<String> {
    path.into_os_string()
        .into_string()
        .map_err(|raw| eyre!("path is not valid UTF-8: '{}'", Path::new(&raw).display()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    /// Clap reads the real process environment, so the tests that exercise the
    /// environment layer have to run one at a time.
    static ENV: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        // A panicking test must not disable the remaining ones.
        ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Parses `argv` (plus whatever is in the environment) the way `try_new`
    /// does, then merges it with an explicit configuration.
    ///
    /// Every test goes through the lock, not only the ones that set variables:
    /// a `SHANTI_*` leaked by a neighbouring test would otherwise change the
    /// answer here.
    fn resolve_with(argv: &[&str], config: Config) -> Result<Args> {
        let _guard = env_lock();
        clear_env();
        parse_and_resolve(argv, config)
    }

    /// The developer running the tests may well have `SHANTI_*` exported in
    /// their own shell, which would silently outrank the layer under test.
    fn clear_env() {
        for name in [
            "SHANTI_REPOS_DIR",
            "SHANTI_WORKTREES_DIR",
            "SHANTI_RUN_FETCH",
            "SHANTI_THEME",
        ] {
            std::env::remove_var(name);
        }
    }

    /// Same, with the given environment variables set for the duration of the
    /// parse. Clap reads the real process environment, so this is the only way
    /// to exercise the environment layer.
    fn resolve_with_env(vars: &[(&str, &str)], argv: &[&str], config: Config) -> Result<Args> {
        let _guard = env_lock();
        clear_env();
        for (name, value) in vars {
            std::env::set_var(name, value);
        }
        let resolved = parse_and_resolve(argv, config);
        clear_env();
        resolved
    }

    fn parse_and_resolve(argv: &[&str], config: Config) -> Result<Args> {
        let matches = Cli::command().get_matches_from(argv);
        let cli = Cli::from_arg_matches(&matches).unwrap();
        let sources = Sources::from_matches(&matches);
        resolve(
            cli,
            sources,
            config,
            PathBuf::from("/nonexistent/config.toml"),
        )
    }

    fn config_with(repos: &[&Path], worktrees: &Path) -> Config {
        Config {
            repos_dirs: repos.iter().map(|dir| dir.to_path_buf()).collect(),
            worktrees_dir: Some(worktrees.to_path_buf()),
            ..Config::default()
        }
    }

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("could not create a temporary directory")
    }

    /// Runs `body` with a scoped tracing subscriber and returns what it logged.
    ///
    /// Skipping an entry is only visible as a warning, so the test has to read
    /// the log to check that the user is told about it.
    fn capturing_logs<T>(body: impl FnOnce() -> T) -> (T, String) {
        let buffer = Logs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();

        let value = tracing::subscriber::with_default(subscriber, body);
        let written = buffer.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (value, String::from_utf8_lossy(&written).into_owned())
    }

    /// A `MakeWriter` that keeps everything in memory.
    #[derive(Clone, Default)]
    struct Logs(std::sync::Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for Logs {
        type Writer = Self;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    fn canonical(path: &Path) -> String {
        std::fs::canonicalize(path)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    }

    // --- precedence, one pair per test -------------------------------------

    #[test]
    fn config_file_beats_the_built_in_default() {
        let dir = temp();
        let args = resolve_with(&["shanti"], config_with(&[dir.path()], dir.path())).unwrap();

        assert_eq!(args.origins.repos_dirs, Origin::ConfigFile);
        assert_eq!(args.origins.worktrees_dir, Origin::ConfigFile);
        assert_eq!(args.repos_dirs, vec![canonical(dir.path())]);
    }

    #[test]
    fn a_flag_beats_the_config_file() {
        let from_config = temp();
        let from_flag = temp();

        let args = resolve_with(
            &[
                "shanti",
                "--repos-dir",
                from_flag.path().to_str().unwrap(),
                "--worktrees-dir",
                from_flag.path().to_str().unwrap(),
            ],
            config_with(&[from_config.path()], from_config.path()),
        )
        .unwrap();

        assert_eq!(args.origins.repos_dirs, Origin::CommandLine);
        assert_eq!(args.repos_dirs, vec![canonical(from_flag.path())]);
        assert_eq!(args.worktrees_dir, canonical(from_flag.path()));
    }

    #[test]
    fn the_environment_beats_the_config_file() {
        let from_config = temp();
        let from_env = temp();
        let env = from_env.path().to_str().unwrap();

        let args = resolve_with_env(
            &[("SHANTI_REPOS_DIR", env), ("SHANTI_WORKTREES_DIR", env)],
            &["shanti"],
            config_with(&[from_config.path()], from_config.path()),
        )
        .unwrap();

        assert_eq!(args.origins.repos_dirs, Origin::Environment);
        assert_eq!(args.repos_dirs, vec![canonical(from_env.path())]);
    }

    #[test]
    fn a_flag_beats_the_environment() {
        let from_env = temp();
        let from_flag = temp();
        let env = from_env.path().to_str().unwrap();

        let args = resolve_with_env(
            &[("SHANTI_REPOS_DIR", env), ("SHANTI_WORKTREES_DIR", env)],
            &["shanti", "--repos-dir", from_flag.path().to_str().unwrap()],
            Config::default(),
        )
        .unwrap();

        assert_eq!(args.origins.repos_dirs, Origin::CommandLine);
        assert_eq!(args.repos_dirs, vec![canonical(from_flag.path())]);
        // The setting the flag did not cover still comes from the environment.
        assert_eq!(args.origins.worktrees_dir, Origin::Environment);
    }

    /// The regression this issue exists to prevent: clap always produces a
    /// value for a flag, so a naive merge would let its default silently beat
    /// the configuration file.
    #[test]
    fn a_clap_default_does_not_beat_the_config_file() {
        let dir = temp();
        let config = Config {
            run_fetch: true,
            ..config_with(&[dir.path()], dir.path())
        };

        let args = resolve_with(&["shanti"], config).unwrap();

        assert!(args.run_fetch);
        assert_eq!(args.origins.run_fetch, Origin::ConfigFile);
    }

    #[test]
    fn the_run_fetch_flag_beats_the_config_file() {
        let dir = temp();
        let args = resolve_with(
            &["shanti", "--run-fetch"],
            config_with(&[dir.path()], dir.path()),
        )
        .unwrap();

        assert!(args.run_fetch);
        assert_eq!(args.origins.run_fetch, Origin::CommandLine);
    }

    #[test]
    fn an_empty_environment_variable_falls_through_to_the_config_file() {
        let dir = temp();

        let args = resolve_with_env(
            &[("SHANTI_REPOS_DIR", ""), ("SHANTI_WORKTREES_DIR", "")],
            &["shanti"],
            config_with(&[dir.path()], dir.path()),
        )
        .unwrap();

        assert_eq!(args.origins.repos_dirs, Origin::ConfigFile);
        assert_eq!(args.origins.worktrees_dir, Origin::ConfigFile);
    }

    // --- theme, the same four layers ---------------------------------------

    #[test]
    fn the_theme_defaults_to_the_catalogue_default() {
        let dir = temp();
        let args = resolve_with(&["shanti"], config_with(&[dir.path()], dir.path())).unwrap();

        assert_eq!(args.theme.name, scheme::DEFAULT);
        assert_eq!(args.origins.theme, Origin::Default);
    }

    #[test]
    fn the_theme_in_the_config_file_beats_the_default() {
        let dir = temp();
        let config = Config {
            theme: Some("gruvbox-dark".to_string()),
            ..config_with(&[dir.path()], dir.path())
        };

        let args = resolve_with(&["shanti"], config).unwrap();

        assert_eq!(args.theme.name, "gruvbox-dark");
        assert_eq!(args.origins.theme, Origin::ConfigFile);
    }

    #[test]
    fn the_theme_environment_variable_beats_the_config_file() {
        let dir = temp();
        let config = Config {
            theme: Some("gruvbox-dark".to_string()),
            ..config_with(&[dir.path()], dir.path())
        };

        let args =
            resolve_with_env(&[("SHANTI_THEME", "catppuccin-latte")], &["shanti"], config).unwrap();

        assert_eq!(args.theme.name, "catppuccin-latte");
        assert_eq!(args.origins.theme, Origin::Environment);
    }

    #[test]
    fn the_theme_flag_beats_the_environment() {
        let dir = temp();
        let config = Config {
            theme: Some("gruvbox-dark".to_string()),
            ..config_with(&[dir.path()], dir.path())
        };

        let args = resolve_with_env(
            &[("SHANTI_THEME", "catppuccin-latte")],
            &["shanti", "--theme", "ansi"],
            config,
        )
        .unwrap();

        assert_eq!(args.theme.name, "ansi");
        assert_eq!(args.origins.theme, Origin::CommandLine);
    }

    /// An empty variable is not a choice, so it must not shadow the file — the
    /// same rule the directory settings follow.
    #[test]
    fn an_empty_theme_variable_falls_through_to_the_config_file() {
        let dir = temp();
        let config = Config {
            theme: Some("gruvbox-dark".to_string()),
            ..config_with(&[dir.path()], dir.path())
        };

        let args = resolve_with_env(&[("SHANTI_THEME", "")], &["shanti"], config).unwrap();

        assert_eq!(args.theme.name, "gruvbox-dark");
        assert_eq!(args.origins.theme, Origin::ConfigFile);
    }

    /// A name typed on the command line has to fail at startup, naming the
    /// layer it came from and every name that would have worked.
    #[test]
    fn an_unknown_theme_is_a_startup_error_listing_the_valid_ones() {
        let dir = temp();
        let error = resolve_with(
            &["shanti", "--theme", "dracula"],
            config_with(&[dir.path()], dir.path()),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("--theme"), "{message}");
        assert!(message.contains("dracula"), "{message}");
        for entry in scheme::ALL {
            assert!(message.contains(entry.name), "{message}");
        }
    }

    /// The error has to point at the layer the user must edit, not at a flag
    /// they never typed.
    #[test]
    fn an_unknown_theme_in_the_environment_names_the_environment() {
        let dir = temp();
        let error = resolve_with_env(
            &[("SHANTI_THEME", "dracula")],
            &["shanti"],
            config_with(&[dir.path()], dir.path()),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("from the environment"), "{message}");
    }

    // --- normalisation applies to every layer ------------------------------

    #[test]
    fn a_tilde_in_the_config_file_is_expanded() {
        let dir = temp();
        let config = Config {
            repos_dirs: vec![PathBuf::from("~")],
            ..config_with(&[], dir.path())
        };

        let args = resolve_with(&["shanti"], config).unwrap();

        let home = expand_tilde::expand_tilde(Path::new("~"))
            .unwrap()
            .into_owned();
        assert_eq!(args.repos_dirs, vec![canonical(&home)]);
    }

    #[test]
    fn a_config_file_path_is_canonicalised() {
        let dir = temp();
        let nested = dir.path().join("repos");
        std::fs::create_dir(&nested).unwrap();
        // A path the OS accepts but which is not the shortest spelling.
        let indirect = nested.join("..").join("repos");

        let args = resolve_with(&["shanti"], config_with(&[&indirect], dir.path())).unwrap();

        assert_eq!(args.repos_dirs, vec![canonical(&nested)]);
    }

    #[test]
    fn a_worktrees_dir_from_the_config_file_is_created() {
        let dir = temp();
        let worktrees = dir.path().join("worktrees").join("nested");

        let args = resolve_with(&["shanti"], config_with(&[dir.path()], &worktrees)).unwrap();

        assert!(worktrees.is_dir());
        assert!(args.worktrees_dir.ends_with("nested"));
        assert!(Path::new(&args.worktrees_dir).is_absolute());
    }

    // --- errors ------------------------------------------------------------

    #[test]
    fn a_missing_repos_dir_reports_the_path_and_the_source() {
        let dir = temp();
        let missing = dir.path().join("nope");

        let error = resolve_with(&["shanti"], config_with(&[&missing], dir.path())).unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("--repos-dir"), "{message}");
        assert!(message.contains("config file"), "{message}");
        assert!(message.contains("nope"), "{message}");
    }

    #[test]
    fn a_missing_repos_dir_flag_is_reported_as_a_flag() {
        let dir = temp();
        let missing = dir.path().join("nope");

        let error = resolve_with(
            &[
                "shanti",
                "--repos-dir",
                missing.to_str().unwrap(),
                "--worktrees-dir",
                dir.path().to_str().unwrap(),
            ],
            Config::default(),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("--repos-dir"), "{message}");
        assert!(!message.contains("config file"), "{message}");
    }

    #[test]
    fn no_repos_dir_in_any_layer_is_rejected() {
        let dir = temp();
        let config = Config {
            worktrees_dir: Some(dir.path().to_path_buf()),
            ..Config::default()
        };

        let message = format!("{:#}", resolve_with(&["shanti"], config).unwrap_err());
        assert!(message.contains("--repos-dir"), "{message}");
        assert!(message.contains("SHANTI_REPOS_DIR"), "{message}");
        assert!(message.contains("repos_dirs"), "{message}");
    }

    #[test]
    fn no_worktrees_dir_in_any_layer_is_rejected() {
        let dir = temp();
        let config = Config {
            repos_dirs: vec![dir.path().to_path_buf()],
            ..Config::default()
        };

        let message = format!("{:#}", resolve_with(&["shanti"], config).unwrap_err());
        assert!(message.contains("--worktrees-dir"), "{message}");
        assert!(message.contains("worktrees_dir"), "{message}");
    }

    /// A stale entry must not veto the entries that are still there: the
    /// maintainer's own `SHANTI_REPOS_DIR` holds one of each.
    #[test]
    fn a_missing_entry_in_the_repos_dir_list_is_skipped() {
        let dir = temp();
        let missing = dir.path().join("gone");
        let list = format!("{}:{}", missing.display(), dir.path().display());

        let (result, logs) = capturing_logs(|| {
            resolve_with_env(
                &[
                    ("SHANTI_REPOS_DIR", list.as_str()),
                    ("SHANTI_WORKTREES_DIR", dir.path().to_str().unwrap()),
                ],
                &["shanti"],
                Config::default(),
            )
        });

        let args = result.unwrap();
        assert_eq!(args.repos_dirs, vec![canonical(dir.path())]);
        assert_eq!(args.origins.repos_dirs, Origin::Environment);
        // The skip is reported, with the path and where the setting came from.
        assert!(logs.contains("WARN"), "{logs}");
        assert!(logs.contains("gone"), "{logs}");
        assert!(logs.contains("from the environment"), "{logs}");
    }

    #[test]
    fn every_repos_dir_entry_missing_is_rejected_naming_all_of_them() {
        let dir = temp();
        let first = dir.path().join("gone-one");
        let second = dir.path().join("gone-two");
        let list = format!("{}:{}", first.display(), second.display());

        let error = resolve_with_env(
            &[
                ("SHANTI_REPOS_DIR", list.as_str()),
                ("SHANTI_WORKTREES_DIR", dir.path().to_str().unwrap()),
            ],
            &["shanti"],
            Config::default(),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("gone-one"), "{message}");
        assert!(message.contains("gone-two"), "{message}");
        assert!(message.contains("from the environment"), "{message}");
        // Every entry is accounted for with its own reason.
        assert_eq!(message.matches("No such file or directory").count(), 2);
    }

    /// The deliberate asymmetry: a list tolerates a stale member, but a single
    /// directory the user typed does not.
    #[test]
    fn a_single_missing_repos_dir_flag_stays_a_hard_error() {
        let dir = temp();
        let missing = dir.path().join("gone");

        let error = resolve_with(
            &[
                "shanti",
                "--repos-dir",
                missing.to_str().unwrap(),
                "--worktrees-dir",
                dir.path().to_str().unwrap(),
            ],
            Config::default(),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("gone"), "{message}");
        assert!(message.contains("No such file or directory"), "{message}");
    }

    #[test]
    fn a_file_where_a_repos_dir_is_expected_is_rejected() {
        let dir = temp();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"").unwrap();

        let error = resolve_with(&["shanti"], config_with(&[&file], dir.path())).unwrap_err();

        assert!(format!("{error:#}").contains("is not a directory"));
    }

    // --- reporting ---------------------------------------------------------

    #[test]
    fn show_config_lists_every_setting_with_its_origin() {
        let from_config = temp();
        let from_flag = temp();
        let config = Config {
            backend: Backend::Jujutsu,
            editor: Some("nvim".to_string()),
            theme: Some("gruvbox-dark".to_string()),
            ..config_with(&[from_config.path()], from_config.path())
        };

        let args = resolve_with(
            &[
                "shanti",
                "--show-config",
                "--worktrees-dir",
                from_flag.path().to_str().unwrap(),
            ],
            config,
        )
        .unwrap();

        assert!(args.show_config);
        let report = args.report();
        assert!(report.contains("not found"), "{report}");
        assert!(
            report.contains(&format!(
                "worktrees_dir  = {}  (command line)",
                canonical(from_flag.path())
            )),
            "{report}"
        );
        assert!(
            report.contains(&format!(
                "repos_dirs     = {}  (config file)",
                canonical(from_config.path())
            )),
            "{report}"
        );
        assert!(
            report.contains("run_fetch      = false  (built-in default)"),
            "{report}"
        );
        assert!(
            report.contains("backend        = jujutsu  (config file)"),
            "{report}"
        );
        assert!(
            report.contains("editor         = nvim  (config file)"),
            "{report}"
        );
        assert!(
            report.contains("theme          = gruvbox-dark  (config file)"),
            "{report}"
        );
    }

    #[test]
    fn the_report_lists_every_repository_directory() {
        let first = temp();
        let second = temp();

        let args = resolve_with(
            &["shanti"],
            config_with(&[first.path(), second.path()], first.path()),
        )
        .unwrap();

        let report = args.report();
        assert!(report.contains(&canonical(first.path())), "{report}");
        assert!(report.contains(&canonical(second.path())), "{report}");
    }

    /// The precedence order is the contract; spelling it out keeps a future
    /// reordering of the enum from silently changing behaviour.
    #[test]
    fn origins_are_ordered_lowest_to_highest() {
        assert!(Origin::Default < Origin::ConfigFile);
        assert!(Origin::ConfigFile < Origin::Environment);
        assert!(Origin::Environment < Origin::CommandLine);
    }
}
