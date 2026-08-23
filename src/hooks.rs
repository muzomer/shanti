//! What happens after a space exists: post-create hooks.
//!
//! A new space is a fresh checkout, so it is missing everything that is not in
//! version control — an ignored `.env`, an `.envrc` direnv has to allow,
//! `node_modules`, a warm `target/`, local editor settings. Doing that by hand
//! in every new space is a meaningful part of the context-switching cost shanti
//! exists to remove, so it is described once in the configuration file and done
//! automatically.
//!
//! # Where hooks are configured
//!
//! In the **user's own configuration file** (`<config dir>/config.toml`), at two
//! levels, because the two real needs differ:
//!
//! * `[hooks]` — global. `direnv allow` is the same in every repository.
//! * `[repos.<name>.hooks]` — per repository. `npm install` is not.
//!
//! Both apply to a matching repository, global first, so the general rule can be
//! stated once and specialised. See [`crate::config::RepoConfig`] for how a
//! repository is keyed (name, or absolute path when two checkouts share a name).
//!
//! Deliberately **not** supported: a hook file inside the repository's working
//! tree. That would make `shanti` clone-and-list a code-execution path — a
//! hostile repository could ship a `.shanti.toml` and run whatever it liked the
//! first time a space was created. Everything shanti runs is written by the user
//! in a file only the user owns.
//!
//! # What a hook receives
//!
//! Commands run with the **new space as the working directory**, so the common
//! hook is just what the user would type there. shanti never interpolates
//! anything into the command string; every value is passed as an environment
//! variable instead:
//!
//! | Variable            | Value                                              |
//! | ------------------- | -------------------------------------------------- |
//! | `SHANTI_SPACE_PATH` | absolute path of the new space (also the cwd)      |
//! | `SHANTI_SPACE_NAME` | the space's name — the branch or bookmark          |
//! | `SHANTI_REPO_PATH`  | absolute path of the source repository             |
//! | `SHANTI_REPO_NAME`  | the repository's display name                      |
//! | `SHANTI_BACKEND`    | `git` or `jj` — how a hook tells the two apart     |
//!
//! `SHANTI_BACKEND` is the discriminator: a hook that must run `git`- or
//! `jj`-specific commands branches on it, and one that does not (the vast
//! majority — copying a file, installing dependencies) can ignore it entirely.
//!
//! There is deliberately **no `SHANTI_BASE`**. The only base shanti has is
//! [`crate::vcs::Vcs::resolve_base`], which returns a sentence for the create
//! prompt ("Will be created from main (default branch)"), not a ref — putting
//! that in an environment variable other people script against would be a
//! promise shanti cannot keep. A hook that needs the base can ask the backend
//! itself; it is already standing in the space. Adding the variable later, once
//! there is a machine-readable base to put in it, is additive.
//!
//! # Failure policy
//!
//! **A hook failing is not space creation failing.** The space is created first
//! and is already usable; a hook only makes it *more* usable. So a failing hook
//! never deletes the space, never rolls anything back, and never turns creation
//! into an error. It is also never swallowed: [`HookPlan::run`] returns a
//! [`HookReport`] listing every outcome, a failure carries the command's
//! combined output, and every failure is logged at `error!` as well.
//!
//! # Blocking
//!
//! [`HookPlan::run`] blocks — `npm install` takes minutes — so it must not be
//! called on the render thread. The plan is therefore split from the run:
//! building a [`HookPlan`] is cheap and pure, and the plan is owned, `Send` and
//! `'static`, as is the [`HookReport`] it produces. Moving it onto
//! [`crate::jobs`] later is one `Job` variant whose body is `plan.run()` and one
//! `JobResult` variant carrying the report; nothing in this module changes.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use color_eyre::eyre;
use tracing::{debug, error, warn};

use crate::{
    config::{Config, Hooks},
    vcs::Backend,
};

/// Environment variable that skips every hook for one invocation of shanti.
///
/// The escape hatch the feature needs: hooks are convenience, and a user who is
/// debugging one — or who just wants a space *now* — must be able to opt out
/// without editing their configuration file.
pub const SKIP_ENV: &str = "SHANTI_NO_HOOKS";

/// How much of a failing command's output is kept in the report.
///
/// A hook is arbitrary code and may print megabytes; the report is held in
/// memory and rendered in a terminal. The **tail** is kept rather than the head
/// because the error is at the end of the output, not at its start.
const MAX_CAPTURED_OUTPUT: usize = 8 * 1024;

/// Everything a hook is told about the space that was just created.
///
/// Owned, so a plan built from it can be moved to a worker thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookTarget {
    /// Absolute path of the new space; also the working directory hooks run in.
    pub space_path: PathBuf,
    /// The space's name — the branch or bookmark it was created for.
    pub space_name: String,
    /// Absolute path of the repository the space belongs to.
    pub repo_path: PathBuf,
    /// The repository's display name; also the key `[repos.<name>]` matches.
    pub repo_name: String,
    /// Which backend created the space.
    pub backend: Backend,
}

impl HookTarget {
    /// The environment every hook command is given, as `(name, value)` pairs.
    ///
    /// One function so the documented interface above and the process actually
    /// spawned cannot drift apart — and so a test can assert the interface
    /// without spawning anything.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("SHANTI_SPACE_PATH", self.space_path.display().to_string()),
            ("SHANTI_SPACE_NAME", self.space_name.clone()),
            ("SHANTI_REPO_PATH", self.repo_path.display().to_string()),
            ("SHANTI_REPO_NAME", self.repo_name.clone()),
            ("SHANTI_BACKEND", self.backend.label().to_string()),
        ]
    }
}

/// The configured hooks, resolved once and asked per space.
///
/// Holds the whole per-repository table rather than one repository's hooks
/// because it outlives any single creation: shanti creates spaces in many
/// repositories in one session.
// Deliberately not `Default`: an all-zero `HookSettings` would read as "no
// hooks" while actually meaning "hooks disabled", and the two answers differ
// once `SKIP_ENV` is in play. Callers say which they mean —
// `from_config(Config::default())` or `disabled()`.
#[derive(Debug, Clone)]
pub struct HookSettings {
    config: Config,
    enabled: bool,
}

impl HookSettings {
    /// Read the hooks from shanti's configuration file, honouring [`SKIP_ENV`].
    ///
    /// For a caller that has no resolved configuration in hand. The TUI is not
    /// one of them: it takes its hooks off [`crate::cli::Args`] with every other
    /// setting, because `--no-hooks` gives them a command-line layer and that is
    /// what the precedence machinery is for.
    pub fn load() -> eyre::Result<Self> {
        Ok(Self::from_config(Config::load()?))
    }

    /// Build settings from a configuration already in hand.
    pub fn from_config(config: Config) -> Self {
        let enabled = !skip_requested();
        if !enabled {
            debug!("{SKIP_ENV} is set, post-create hooks are skipped");
        }
        Self { config, enabled }
    }

    /// Whether hooks will run at all — false under [`SKIP_ENV`] or `--no-hooks`.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// How much is configured, for `--show-config`.
    ///
    /// Counts rather than the lists themselves: the question `--show-config`
    /// answers is "is shanti going to run something I forgot about?", and a
    /// number answers it without printing a shell script into a report.
    pub fn counts(&self) -> HookCounts {
        HookCounts {
            copies: self.config.hooks.copy.len(),
            commands: self.config.hooks.run.len(),
            repos: self
                .config
                .repos
                .values()
                .filter(|repo| !repo.hooks.is_empty())
                .count(),
        }
    }

    /// The same configuration, with nothing allowed to run.
    ///
    /// What `--no-hooks` produces. The lists are kept rather than thrown away
    /// so `--show-config` can report what *would* have run: "disabled" and
    /// "nothing configured" are different answers, and a user turning hooks off
    /// to debug them needs to see which one they are looking at.
    pub fn switched_off(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Settings that run nothing, for callers that have opted out explicitly.
    pub fn disabled() -> Self {
        Self {
            config: Config::default(),
            enabled: false,
        }
    }

    /// What should run after `target` was created.
    ///
    /// Pure and cheap: it selects and clones lists, spawns nothing and touches
    /// no filesystem, so it is safe to call on the render thread.
    pub fn plan(&self, target: HookTarget) -> HookPlan {
        let mut copies = Vec::new();
        let mut commands = Vec::new();

        if self.enabled {
            // Global first, then the repository's own: the specific rule is
            // appended to the general one rather than replacing it, so a user
            // does not have to repeat `direnv allow` in every repository.
            for hooks in self.hooks_for(&target) {
                copies.extend(hooks.copy.iter().cloned());
                commands.extend(hooks.run.iter().cloned());
            }
        }

        HookPlan {
            target,
            copies,
            commands,
        }
    }

    /// The hook lists that apply to `target`, in the order they run.
    ///
    /// A repository entry matches on its display name or on its absolute path.
    /// Both may be present — the name-keyed entry is the general one, so it goes
    /// first and the path-keyed one specialises it.
    fn hooks_for(&self, target: &HookTarget) -> Vec<&Hooks> {
        let mut applicable = vec![&self.config.hooks];
        let path = target.repo_path.display().to_string();
        for key in [target.repo_name.as_str(), path.as_str()] {
            if let Some(repo) = self.config.repos.get(key) {
                applicable.push(&repo.hooks);
            }
        }
        applicable
    }
}

/// What is configured, without saying what it is. See [`HookSettings::counts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HookCounts {
    /// Files the global `[hooks]` table carries over.
    pub copies: usize,
    /// Commands the global `[hooks]` table runs.
    pub commands: usize,
    /// Repositories with hooks of their own.
    pub repos: usize,
}

impl HookCounts {
    /// Whether nothing at all is configured, so a report can say so in words
    /// rather than as three zeroes.
    pub fn is_empty(&self) -> bool {
        self.copies == 0 && self.commands == 0 && self.repos == 0
    }
}

/// Whether this invocation asked for hooks to be skipped.
///
/// Any non-empty value counts, including `0`: the variable is a switch, and a
/// user who exported it meant to turn hooks off.
fn skip_requested() -> bool {
    std::env::var_os(SKIP_ENV).is_some_and(|value| !value.is_empty())
}

/// Work to do in one new space — the unit that will later be a job.
///
/// Owned and `'static` on purpose; see the module docs on blocking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookPlan {
    /// The space the work is for, and the values its commands are given.
    pub target: HookTarget,
    /// Files to carry over from the repository root, in order.
    pub copies: Vec<PathBuf>,
    /// Shell commands to run in the space, in order, after the copies.
    pub commands: Vec<String>,
}

impl HookPlan {
    /// Whether there is nothing to do, so a caller can avoid the round trip
    /// through a worker entirely.
    pub fn is_empty(&self) -> bool {
        self.copies.is_empty() && self.commands.is_empty()
    }

    /// Do the work. **Blocking** — never call this on the render thread.
    ///
    /// Copies happen before commands, because a command (`direnv allow`, a build)
    /// usually depends on the file a copy brought in. One failing step does not
    /// stop the next: the steps are independent wishes, and a missing `.env`
    /// should not cost the user their dependency install. Everything that
    /// happened, good or bad, comes back in the [`HookReport`].
    pub fn run(&self) -> HookReport {
        let mut outcomes = Vec::new();
        for relative in &self.copies {
            outcomes.push(self.copy(relative));
        }
        for command in &self.commands {
            outcomes.push(self.run_command(command));
        }
        HookReport {
            target: self.target.clone(),
            outcomes,
        }
    }

    /// Copy one path from the repository root into the same place in the space.
    fn copy(&self, relative: &Path) -> HookOutcome {
        // A copy entry names a file *inside* the repository. Rejecting absolute
        // paths and `..` is not a security boundary — the config is the user's
        // own — it is a guard against a typo silently writing outside the space.
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return HookOutcome::CopyFailed {
                path: relative.to_path_buf(),
                error: "must be a relative path inside the repository".to_string(),
            };
        }

        let from = self.target.repo_path.join(relative);
        let to = self.target.space_path.join(relative);

        match std::fs::symlink_metadata(&from) {
            // Nothing to carry over is the normal case, not a failure: a `.env`
            // may simply not exist in this repository.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return HookOutcome::CopySkipped {
                    path: relative.to_path_buf(),
                    reason: "not present in the repository".to_string(),
                }
            }
            Err(error) => {
                return HookOutcome::CopyFailed {
                    path: relative.to_path_buf(),
                    error: error.to_string(),
                }
            }
            Ok(metadata) if metadata.is_dir() => {
                // Copying a directory is a different feature with different
                // hazards (size, symlink loops). Saying so is more useful than
                // half-doing it.
                return HookOutcome::CopyFailed {
                    path: relative.to_path_buf(),
                    error: "is a directory; copy hooks carry files only".to_string(),
                };
            }
            Ok(_) => {}
        }

        if let Some(parent) = to.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                return HookOutcome::CopyFailed {
                    path: relative.to_path_buf(),
                    error: error.to_string(),
                };
            }
        }

        match std::fs::copy(&from, &to) {
            Ok(_) => HookOutcome::Copied {
                path: relative.to_path_buf(),
            },
            Err(error) => HookOutcome::CopyFailed {
                path: relative.to_path_buf(),
                error: error.to_string(),
            },
        }
    }

    /// Run one command in the new space and capture what it said.
    ///
    /// The command goes to a **shell** (`sh -c`), unsplit. That is deliberate:
    /// what the user writes in a config file is a command *line* — `direnv allow
    /// && npm ci`, a `~`, a pipe — and splitting it ourselves would break all of
    /// that while pretending to be safer. It is not less safe either: the string
    /// is the user's own, and shanti never interpolates a space name, branch or
    /// path into it, so no repository or branch name can smuggle a `;` in. The
    /// values reach the command as environment variables, which are opaque to
    /// the shell's parser.
    fn run_command(&self, command: &str) -> HookOutcome {
        debug!(
            command,
            space = %self.target.space_path.display(),
            "running a post-create hook"
        );

        let mut child = shell();
        child
            .arg(command)
            .current_dir(&self.target.space_path)
            .envs(self.target.env());

        match child.output() {
            Ok(output) if output.status.success() => HookOutcome::Ran {
                command: command.to_string(),
            },
            Ok(output) => {
                let status = describe(output.status);
                let combined = tail(&[output.stdout, output.stderr].concat());
                error!(
                    command,
                    %status,
                    space = %self.target.space_path.display(),
                    "a post-create hook failed; the space was created and is intact"
                );
                HookOutcome::Failed {
                    command: command.to_string(),
                    status,
                    output: combined,
                }
            }
            // The shell itself could not be started, or the space vanished.
            Err(error) => {
                error!(command, %error, "could not start a post-create hook");
                HookOutcome::Failed {
                    command: command.to_string(),
                    status: "could not start".to_string(),
                    output: error.to_string(),
                }
            }
        }
    }
}

/// The shell hook commands are handed to.
///
/// Split out so the platform choice is stated once and the tests can rely on it.
fn shell() -> Command {
    if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.arg("/C");
        command
    } else {
        let mut command = Command::new("sh");
        command.arg("-c");
        command
    }
}

/// Human-readable exit status: a code where there is one, a signal otherwise.
fn describe(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => format!("terminated by a signal ({status})"),
    }
}

/// The last [`MAX_CAPTURED_OUTPUT`] bytes of a command's output, as text.
///
/// The tail, because a failure's cause is at the end. Lossy conversion, because
/// a hook may print anything and the report must still be renderable.
fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(MAX_CAPTURED_OUTPUT);
    let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    if start == 0 {
        text
    } else {
        format!("… (truncated)\n{text}")
    }
}

/// What one hook did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// A file was carried over from the repository.
    Copied {
        /// Path, relative to the repository root.
        path: PathBuf,
    },
    /// There was nothing to carry over. Not a failure.
    CopySkipped {
        /// Path, relative to the repository root.
        path: PathBuf,
        /// Why nothing was copied, in words the user can act on.
        reason: String,
    },
    /// A copy was wanted but could not be done.
    CopyFailed {
        /// Path, relative to the repository root.
        path: PathBuf,
        /// What went wrong.
        error: String,
    },
    /// A command ran and succeeded.
    Ran {
        /// The command line, as configured.
        command: String,
    },
    /// A command ran and failed, or could not be started.
    Failed {
        /// The command line, as configured.
        command: String,
        /// Exit status, or why it never started.
        status: String,
        /// Tail of the command's combined stdout and stderr.
        output: String,
    },
}

impl HookOutcome {
    /// Whether this outcome is something the user should be told about loudly.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            HookOutcome::CopyFailed { .. } | HookOutcome::Failed { .. }
        )
    }

    /// One line naming what this outcome was about — the command or the path.
    pub fn subject(&self) -> String {
        match self {
            HookOutcome::Copied { path }
            | HookOutcome::CopySkipped { path, .. }
            | HookOutcome::CopyFailed { path, .. } => path.display().to_string(),
            HookOutcome::Ran { command } | HookOutcome::Failed { command, .. } => command.clone(),
        }
    }
}

/// Everything that happened for one space. Owned, so it can cross a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookReport {
    /// The space the hooks ran for.
    pub target: HookTarget,
    /// Every outcome, in the order it happened.
    pub outcomes: Vec<HookOutcome>,
}

impl HookReport {
    /// The outcomes worth showing the user.
    pub fn failures(&self) -> impl Iterator<Item = &HookOutcome> {
        self.outcomes.iter().filter(|o| o.is_failure())
    }

    /// Whether anything went wrong.
    pub fn failed(&self) -> bool {
        self.failures().next().is_some()
    }

    /// One line for the status bar, or `None` when there is nothing to say.
    ///
    /// Success is silent: the point of a hook is that the user does not have to
    /// think about it. A failure names the first thing that broke and how many
    /// others there were, and states that the space is fine — that is the part a
    /// user needs to hear before they reach for the delete key.
    pub fn summary(&self) -> Option<String> {
        let failures: Vec<_> = self.failures().collect();
        let first = failures.first()?;
        let others = failures.len() - 1;
        let more = match others {
            0 => String::new(),
            1 => " (and 1 more)".to_string(),
            n => format!(" (and {n} more)"),
        };
        Some(format!(
            "Hook failed for {}: {}{more} — the {} was created and is intact",
            self.target.space_name,
            first.subject(),
            self.target.backend.space_noun(),
        ))
    }
}

/// Run `plan` and log what happened, for callers that have nowhere to show it.
///
/// The floor of the failure policy: even with no UI in the picture, a failing
/// hook is reported rather than lost.
pub fn run_and_log(plan: &HookPlan) -> HookReport {
    let report = plan.run();
    if let Some(summary) = report.summary() {
        warn!("{summary}");
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::config::RepoConfig;

    /// A repository and a space on disk, so hooks have something real to act on.
    struct Fixture {
        _dir: tempfile::TempDir,
        repo: PathBuf,
        space: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("could not create a temporary directory");
            let repo = dir.path().join("repos").join("shanti");
            let space = dir.path().join("spaces").join("shanti").join("feature");
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::create_dir_all(&space).unwrap();
            Self {
                _dir: dir,
                repo,
                space,
            }
        }

        fn target(&self) -> HookTarget {
            HookTarget {
                space_path: self.space.clone(),
                space_name: "feature".to_string(),
                repo_path: self.repo.clone(),
                repo_name: "shanti".to_string(),
                backend: Backend::Git,
            }
        }

        fn plan(&self, copies: &[&str], commands: &[&str]) -> HookPlan {
            HookPlan {
                target: self.target(),
                copies: copies.iter().map(PathBuf::from).collect(),
                commands: commands.iter().map(|c| c.to_string()).collect(),
            }
        }
    }

    fn config(toml: &str) -> Config {
        toml::from_str(toml).expect("the test configuration should parse")
    }

    /// Hooks must be off unless the user turned them on: an empty config may
    /// never run anything.
    #[test]
    fn hooks_do_nothing_without_configuration() {
        let fixture = Fixture::new();
        let settings = HookSettings::from_config(Config::default());
        let plan = settings.plan(fixture.target());
        assert!(plan.is_empty());
        assert!(plan.run().outcomes.is_empty());
    }

    #[test]
    fn a_command_that_succeeds_is_reported_as_ran() {
        let fixture = Fixture::new();
        let report = fixture.plan(&[], &["exit 0"]).run();
        assert_eq!(
            report.outcomes,
            vec![HookOutcome::Ran {
                command: "exit 0".to_string()
            }]
        );
        assert!(!report.failed());
        assert_eq!(report.summary(), None);
    }

    /// The failure policy, stated as a test: the space survives, the output is
    /// kept, and the user is told.
    #[test]
    fn a_command_that_fails_keeps_its_output_and_leaves_the_space_alone() {
        let fixture = Fixture::new();
        let report = fixture
            .plan(&[], &["echo out; echo boom >&2; exit 3"])
            .run();

        let HookOutcome::Failed { status, output, .. } = &report.outcomes[0] else {
            panic!("expected a failure, got {:?}", report.outcomes);
        };
        assert!(status.contains('3'), "{status}");
        assert!(output.contains("out"), "{output}");
        assert!(output.contains("boom"), "{output}");

        assert!(report.failed());
        let summary = report.summary().expect("a failure has a summary");
        assert!(summary.contains("feature"), "{summary}");
        assert!(summary.contains("intact"), "{summary}");
        // The space is untouched — a failing hook never removes it.
        assert!(fixture.space.is_dir());
    }

    /// A command naming a program that is not installed must be a reported
    /// failure, never a panic and never a silent success.
    #[test]
    fn a_command_that_does_not_exist_is_a_reported_failure() {
        let fixture = Fixture::new();
        let report = fixture
            .plan(&[], &["shanti-definitely-not-a-real-program"])
            .run();
        assert!(report.failed(), "{:?}", report.outcomes);
        assert!(fixture.space.is_dir());
    }

    /// Later hooks still run after an earlier one fails: they are independent
    /// wishes, not steps of one transaction.
    #[test]
    fn a_failing_hook_does_not_stop_the_ones_after_it() {
        let fixture = Fixture::new();
        let report = fixture.plan(&[], &["exit 1", "exit 0"]).run();
        assert!(matches!(report.outcomes[0], HookOutcome::Failed { .. }));
        assert!(matches!(report.outcomes[1], HookOutcome::Ran { .. }));
    }

    #[test]
    fn commands_run_in_the_new_space() {
        let fixture = Fixture::new();
        let report = fixture.plan(&[], &["pwd > where"]).run();
        assert!(!report.failed());
        let written = std::fs::read_to_string(fixture.space.join("where")).unwrap();
        // Compared canonically: macOS hands out `/var` paths that resolve to
        // `/private/var`, and the shell reports the resolved one.
        assert_eq!(
            std::fs::canonicalize(written.trim()).unwrap(),
            std::fs::canonicalize(&fixture.space).unwrap()
        );
    }

    /// The documented interface, pinned. Other people's hooks depend on these
    /// exact names, so a rename has to break this test.
    #[test]
    fn a_command_is_given_every_documented_value() {
        let fixture = Fixture::new();
        let report = fixture
            .plan(
                &[],
                &[
                    "printf '%s\\n' \"$SHANTI_SPACE_PATH\" \"$SHANTI_SPACE_NAME\" \
                   \"$SHANTI_REPO_PATH\" \"$SHANTI_REPO_NAME\" \
                   \"$SHANTI_BACKEND\" > env",
                ],
            )
            .run();
        assert!(!report.failed(), "{:?}", report.outcomes);

        let lines: Vec<String> = std::fs::read_to_string(fixture.space.join("env"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(
            lines,
            vec![
                fixture.space.display().to_string(),
                "feature".to_string(),
                fixture.repo.display().to_string(),
                "shanti".to_string(),
                "git".to_string(),
            ]
        );
    }

    /// The backend reaches the hook as its own label, which is how a hook that
    /// needs to branch tells git from jj.
    #[test]
    fn the_backend_reaches_the_hook_as_its_label() {
        let fixture = Fixture::new();
        let mut target = fixture.target();
        target.backend = Backend::Jj;
        let env = target.env();
        assert!(env.contains(&("SHANTI_BACKEND", "jj".to_string())));
    }

    /// A name with a shell metacharacter must stay data. It travels in the
    /// environment, so the shell never parses it.
    #[test]
    fn values_cannot_be_smuggled_into_the_command() {
        let fixture = Fixture::new();
        let mut target = fixture.target();
        target.space_name = "feature; touch pwned".to_string();
        let plan = HookPlan {
            target,
            copies: vec![],
            commands: vec!["echo \"$SHANTI_SPACE_NAME\" > name".to_string()],
        };
        assert!(!plan.run().failed());
        assert!(!fixture.space.join("pwned").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.space.join("name")).unwrap(),
            "feature; touch pwned\n"
        );
    }

    #[test]
    fn a_copy_carries_an_ignored_file_into_the_space() {
        let fixture = Fixture::new();
        std::fs::write(fixture.repo.join(".env"), "TOKEN=1").unwrap();
        let report = fixture.plan(&[".env"], &[]).run();
        assert_eq!(
            report.outcomes,
            vec![HookOutcome::Copied {
                path: PathBuf::from(".env")
            }]
        );
        assert_eq!(
            std::fs::read_to_string(fixture.space.join(".env")).unwrap(),
            "TOKEN=1"
        );
    }

    /// A nested path is created on the way, so `.vscode/settings.json` works
    /// without the user pre-making the directory.
    #[test]
    fn a_copy_creates_the_directories_it_needs() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.repo.join(".vscode")).unwrap();
        std::fs::write(fixture.repo.join(".vscode/settings.json"), "{}").unwrap();
        let report = fixture.plan(&[".vscode/settings.json"], &[]).run();
        assert!(!report.failed(), "{:?}", report.outcomes);
        assert!(fixture.space.join(".vscode/settings.json").is_file());
    }

    #[test]
    fn a_missing_source_is_skipped_not_failed() {
        let fixture = Fixture::new();
        let report = fixture.plan(&[".env"], &[]).run();
        assert!(!report.failed());
        assert!(matches!(
            report.outcomes[0],
            HookOutcome::CopySkipped { .. }
        ));
    }

    #[test]
    fn a_copy_that_escapes_the_repository_is_refused() {
        let fixture = Fixture::new();
        let report = fixture.plan(&["../secrets", "/etc/hosts"], &[]).run();
        assert_eq!(report.failures().count(), 2);
    }

    #[test]
    fn a_directory_is_refused_with_a_reason() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.repo.join("node_modules")).unwrap();
        let report = fixture.plan(&["node_modules"], &[]).run();
        let HookOutcome::CopyFailed { error, .. } = &report.outcomes[0] else {
            panic!("expected a refusal, got {:?}", report.outcomes);
        };
        assert!(error.contains("directory"), "{error}");
    }

    /// Copies come first because a command usually depends on what they brought.
    #[test]
    fn copies_run_before_commands() {
        let fixture = Fixture::new();
        std::fs::write(fixture.repo.join(".env"), "TOKEN=1").unwrap();
        let report = fixture.plan(&[".env"], &["test -f .env"]).run();
        assert!(!report.failed(), "{:?}", report.outcomes);
    }

    #[test]
    fn global_hooks_apply_to_every_repository() {
        let fixture = Fixture::new();
        let settings = HookSettings::from_config(config(
            r#"
            [hooks]
            copy = [".envrc"]
            run = ["direnv allow"]
            "#,
        ));
        let plan = settings.plan(fixture.target());
        assert_eq!(plan.copies, vec![PathBuf::from(".envrc")]);
        assert_eq!(plan.commands, vec!["direnv allow".to_string()]);
    }

    /// The precedence rule: the general list first, the repository's own after.
    #[test]
    fn repository_hooks_are_appended_to_the_global_ones() {
        let fixture = Fixture::new();
        let settings = HookSettings::from_config(config(
            r#"
            [hooks]
            run = ["direnv allow"]

            [repos.shanti.hooks]
            run = ["cargo fetch"]
            "#,
        ));
        assert_eq!(
            settings.plan(fixture.target()).commands,
            vec!["direnv allow".to_string(), "cargo fetch".to_string()]
        );
    }

    #[test]
    fn another_repositorys_hooks_are_not_run() {
        let fixture = Fixture::new();
        let settings = HookSettings::from_config(config(
            r#"
            [repos.other.hooks]
            run = ["should not run"]
            "#,
        ));
        assert!(settings.plan(fixture.target()).is_empty());
    }

    /// Two checkouts can share a directory name, so a path key has to work — and
    /// to be additive with the name key rather than shadowed by it.
    #[test]
    fn a_repository_can_be_keyed_by_its_path() {
        let fixture = Fixture::new();
        let mut repos = BTreeMap::new();
        repos.insert(
            fixture.repo.display().to_string(),
            RepoConfig {
                hooks: Hooks {
                    copy: vec![],
                    run: vec!["by path".to_string()],
                },
            },
        );
        repos.insert(
            "shanti".to_string(),
            RepoConfig {
                hooks: Hooks {
                    copy: vec![],
                    run: vec!["by name".to_string()],
                },
            },
        );
        let settings = HookSettings::from_config(Config {
            repos,
            ..Config::default()
        });
        assert_eq!(
            settings.plan(fixture.target()).commands,
            vec!["by name".to_string(), "by path".to_string()]
        );
    }

    /// The per-invocation escape hatch. Disabling has to happen at plan time so
    /// that a caller can see there is nothing to submit.
    #[test]
    fn disabled_settings_plan_nothing() {
        let fixture = Fixture::new();
        let plan = HookSettings::disabled().plan(fixture.target());
        assert!(plan.is_empty());
    }

    #[test]
    fn output_is_truncated_to_its_tail() {
        let long = vec![b'x'; MAX_CAPTURED_OUTPUT + 100];
        let text = tail(&long);
        assert!(text.starts_with("… (truncated)"));
        assert!(text.len() < MAX_CAPTURED_OUTPUT + 100);
    }

    #[test]
    fn a_plan_can_move_to_another_thread() {
        // Guards the property the background worker will need: nothing in a plan
        // or a report borrows, so both can cross a channel unchanged.
        fn assert_sendable<T: Send + 'static>() {}
        assert_sendable::<HookPlan>();
        assert_sendable::<HookReport>();
    }
}
