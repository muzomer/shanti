//! The single place in shanti that spawns `jj`.
//!
//! Every jj interaction goes through [`JjCli`]. Concentrating it here is what
//! makes the invariants below enforceable at all: they are one code path, not a
//! convention every call site has to remember.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{self, eyre, WrapErr};
use tracing::debug;

use super::template::{Record, Template};
use super::version::{JjVersion, MINIMUM_JJ_VERSION};

/// Escape hatch for a jj that is not on `PATH` (a nix profile, a custom build).
pub const JJ_BINARY_ENV: &str = "SHANTI_JJ_BIN";

/// The binary name looked up on `PATH` when [`JJ_BINARY_ENV`] is unset.
const JJ_BINARY: &str = "jj";

/// Whether jj may snapshot the working copy before doing its work.
///
/// jj normally records any on-disk change as part of running *any* command.
/// That is right for commands the user asked for, and wrong for the background
/// polling a TUI does: it would turn a redraw into a repository mutation and
/// pay for a full working-copy scan on every refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingCopy {
    /// Let jj snapshot first — for commands that change the repository.
    Snapshot,
    /// Pass `--ignore-working-copy` — for reads.
    Ignore,
}

/// A located, version-checked `jj` bound to one repository.
///
/// Construct it with [`JjCli::discover`] once per repository and reuse it: the
/// binary lookup and the version probe each cost a process spawn.
#[derive(Debug, Clone)]
pub struct JjCli {
    program: PathBuf,
    dir: PathBuf,
    version: JjVersion,
}

impl JjCli {
    /// Locate jj, check its version, and bind it to the repository at `dir`.
    ///
    /// Returns an error rather than panicking or exiting when jj is missing or
    /// too old. That is deliberate: jj support is one backend among two, and a
    /// user with no jj installed must keep every git repository fully working.
    /// Callers are expected to degrade — skip jj repos, say why — not to abort.
    pub fn discover(dir: impl Into<PathBuf>) -> eyre::Result<Self> {
        let program = locate_jj()?;
        let version = probe_version(&program)?;

        if !version.is_supported() {
            return Err(eyre!(
                "jj {version} at {} is older than the minimum supported version \
                 {MINIMUM_JJ_VERSION}; shanti reads jj through its template language, \
                 which this version renders differently. Please upgrade jj.",
                program.display()
            ));
        }

        Ok(Self::with_program(program, dir, version))
    }

    /// Build an adapter from an already-known program and version.
    ///
    /// Skips both probes; primarily how tests construct a `JjCli` without a jj
    /// on the machine, and how a caller that discovered jj once can bind it to
    /// further repositories.
    pub fn with_program(
        program: impl Into<PathBuf>,
        dir: impl Into<PathBuf>,
        version: JjVersion,
    ) -> Self {
        Self {
            program: program.into(),
            dir: dir.into(),
            version,
        }
    }

    /// Whether a usable jj exists on this machine at all.
    ///
    /// For the callers that want to decide *whether* to offer jj rather than to
    /// report why it failed.
    pub fn is_available() -> bool {
        locate_jj().is_ok()
    }

    /// The jj binary in use.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The repository this adapter is bound to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The version of jj behind this adapter.
    pub fn version(&self) -> JjVersion {
        self.version
    }

    /// The full argument list for `args`, global flags included.
    ///
    /// Separated from spawning so that command construction — the part that is
    /// easy to get subtly wrong and impossible to notice — is unit-testable
    /// without a jj binary or a repository.
    pub fn argv(&self, working_copy: WorkingCopy, args: &[&str]) -> Vec<String> {
        let mut argv = vec![
            // Never hand output to a pager: shanti owns the terminal, and a
            // pager waiting for input would hang the process with no UI.
            "--no-pager".to_owned(),
            // Never colour output: the escapes would end up inside parsed
            // fields, and shanti styles everything itself.
            "--color=never".to_owned(),
            // Name the repository explicitly. shanti's process cwd is wherever
            // the user launched it from and has nothing to do with the repo
            // being acted on, so relying on it would act on the wrong repo.
            "--repository".to_owned(),
            self.dir.to_string_lossy().into_owned(),
        ];

        if working_copy == WorkingCopy::Ignore {
            argv.push("--ignore-working-copy".to_owned());
        }

        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        argv
    }

    /// Run a jj subcommand that may change the repository, returning stdout.
    pub fn run(&self, args: &[&str]) -> eyre::Result<String> {
        self.spawn(WorkingCopy::Snapshot, args)
    }

    /// Run a read-only jj subcommand, returning stdout.
    pub fn read(&self, args: &[&str]) -> eyre::Result<String> {
        self.spawn(WorkingCopy::Ignore, args)
    }

    /// Run a read-only subcommand and parse its output through `template`.
    ///
    /// `--no-graph` is forced here, not left to the caller: the graph glyphs are
    /// drawn for humans and would prefix the first field of every record.
    pub fn records(&self, args: &[&str], template: &Template) -> eyre::Result<Vec<Record>> {
        let expression = template.expression();
        let stdout = self.spawn(WorkingCopy::Ignore, &record_args(args, &expression))?;
        template.parse(&stdout)
    }

    fn spawn(&self, working_copy: WorkingCopy, args: &[&str]) -> eyre::Result<String> {
        let argv = self.argv(working_copy, args);
        let described = describe(&self.program, &argv);
        debug!(command = %described, "running jj");

        let output = Command::new(&self.program)
            .args(&argv)
            // Belt and braces with `--repository`: some jj subcommands resolve
            // paths relative to the cwd, which must never be shanti's own.
            .current_dir(&self.dir)
            .output()
            .wrap_err_with(|| format!("failed to run {described}"))?;

        interpret(
            &described,
            output.status.success(),
            output.status.code(),
            &output.stdout,
            &output.stderr,
        )
    }
}

/// The subcommand arguments for a templated read: the caller's own plus the
/// flags that make jj's output machine-readable. A free function so the
/// composition can be asserted without spawning jj.
fn record_args<'a>(args: &[&'a str], expression: &'a str) -> Vec<&'a str> {
    let mut full = args.to_vec();
    full.extend(["--no-graph", "--template", expression]);
    full
}

/// Find the jj binary, preferring [`JJ_BINARY_ENV`] over a `PATH` search.
///
/// The search is done here rather than left to `Command` so that "jj is not
/// installed" is reported once, up front, with the searched locations — instead
/// of surfacing as an opaque `NotFound` from whichever command ran first.
fn locate_jj() -> eyre::Result<PathBuf> {
    if let Some(configured) = std::env::var_os(JJ_BINARY_ENV) {
        let path = PathBuf::from(configured);
        if is_executable_file(&path) {
            return Ok(path);
        }
        return Err(eyre!(
            "{JJ_BINARY_ENV} points at {}, which is not an executable file",
            path.display()
        ));
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(JJ_BINARY);
        if is_executable_file(&candidate) {
            return Ok(candidate);
        }
    }

    Err(eyre!(
        "could not find the `{JJ_BINARY}` executable on PATH. Install jujutsu, or set \
         {JJ_BINARY_ENV} to its location. Git repositories are unaffected."
    ))
}

/// A file that exists and carries an execute bit (any execute bit is enough —
/// shanti is not the right place to reimplement the kernel's permission check).
fn is_executable_file(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Ask jj its version. Deliberately does not go through [`JjCli`]: it must work
/// before an adapter exists, and `--version` takes no repository.
fn probe_version(program: &Path) -> eyre::Result<JjVersion> {
    let argv = ["--no-pager", "--color=never", "--version"];
    let described = describe(program, &argv);

    let output = Command::new(program)
        .args(argv)
        .output()
        .wrap_err_with(|| format!("failed to run {described}"))?;

    let stdout = interpret(
        &described,
        output.status.success(),
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )?;

    JjVersion::parse_version_output(&stdout).wrap_err_with(|| {
        format!(
            "could not determine the version of jj at {}",
            program.display()
        )
    })
}

/// Turn a finished process into either its stdout or an error carrying jj's own
/// complaint. jj explains failures well (conflicting bookmark, no such
/// workspace); repeating that verbatim beats inventing a vaguer message.
fn interpret(
    described: &str,
    success: bool,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> eyre::Result<String> {
    if !success {
        let stderr = String::from_utf8_lossy(stderr);
        let stderr = stderr.trim();
        let status = match code {
            Some(code) => format!("exit code {code}"),
            // No code means a signal killed it, which jj cannot explain itself.
            None => "terminated by signal".to_owned(),
        };
        let detail = if stderr.is_empty() {
            "no stderr output".to_owned()
        } else {
            stderr.to_owned()
        };
        return Err(eyre!("{described} failed ({status}): {detail}"));
    }

    // Lossless: a field mangled by a lossy conversion would parse cleanly and be
    // wrong, which is worse than failing here.
    String::from_utf8(stdout.to_vec())
        .wrap_err_with(|| format!("{described} produced output that is not valid UTF-8"))
}

/// A copy-pasteable rendering of a command, for logs and error messages.
fn describe(program: &Path, args: &[impl AsRef<OsStr>]) -> String {
    let mut described = program.display().to_string();
    for arg in args {
        described.push(' ');
        described.push_str(&arg.as_ref().to_string_lossy());
    }
    described
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::jj::template::FIELD_SEPARATOR;
    use pretty_assertions::assert_eq;

    fn cli() -> JjCli {
        JjCli::with_program("/usr/bin/jj", "/repos/shanti", JjVersion::new(0, 40, 0))
    }

    #[test]
    fn every_invocation_disables_the_pager_and_colour() {
        let argv = cli().argv(WorkingCopy::Snapshot, &["workspace", "list"]);

        assert!(argv.contains(&"--no-pager".to_owned()), "{argv:?}");
        assert!(argv.contains(&"--color=never".to_owned()), "{argv:?}");
    }

    #[test]
    fn every_invocation_names_the_repository_explicitly() {
        let argv = cli().argv(WorkingCopy::Snapshot, &["workspace", "list"]);

        assert_eq!(
            argv,
            vec![
                "--no-pager",
                "--color=never",
                "--repository",
                "/repos/shanti",
                "workspace",
                "list",
            ]
        );
    }

    #[test]
    fn reads_do_not_snapshot_the_working_copy() {
        let argv = cli().argv(WorkingCopy::Ignore, &["log"]);
        assert!(
            argv.contains(&"--ignore-working-copy".to_owned()),
            "{argv:?}"
        );
    }

    #[test]
    fn writes_do_snapshot_the_working_copy() {
        let argv = cli().argv(WorkingCopy::Snapshot, &["workspace", "add", "x"]);
        assert!(
            !argv.contains(&"--ignore-working-copy".to_owned()),
            "{argv:?}"
        );
    }

    #[test]
    fn global_flags_come_before_the_subcommand() {
        // jj rejects global flags placed after the subcommand's own arguments.
        let argv = cli().argv(WorkingCopy::Ignore, &["workspace", "list"]);
        let subcommand = argv.iter().position(|arg| arg == "workspace").unwrap();

        assert!(argv[..subcommand]
            .iter()
            .all(|arg| arg.starts_with("--") || arg == "/repos/shanti"));
    }

    #[test]
    fn a_non_zero_exit_becomes_an_error_carrying_trimmed_stderr() {
        let error = interpret(
            "jj workspace list",
            false,
            Some(1),
            b"",
            b"  Error: No such workspace: nope\n\n",
        )
        .unwrap_err()
        .to_string();

        assert_eq!(
            error,
            "jj workspace list failed (exit code 1): Error: No such workspace: nope"
        );
    }

    #[test]
    fn a_failure_without_stderr_still_says_what_happened() {
        let error = interpret("jj log", false, None, b"", b"   \n")
            .unwrap_err()
            .to_string();

        assert_eq!(
            error,
            "jj log failed (terminated by signal): no stderr output"
        );
    }

    #[test]
    fn success_returns_stdout_untouched() {
        // Trailing newlines are structure, not noise: they terminate records.
        let stdout = interpret("jj log", true, Some(0), b"default\nfeature\n", b"").unwrap();
        assert_eq!(stdout, "default\nfeature\n");
    }

    #[test]
    fn non_utf8_stdout_is_an_error_rather_than_silently_mangled() {
        assert!(interpret("jj log", true, Some(0), &[0xff, 0xfe], b"").is_err());
    }

    #[test]
    fn a_missing_jj_binary_is_reported_without_mentioning_git_repos_breaking() {
        let error = locate_jj_in(&[]).unwrap_err().to_string();

        assert!(error.contains(JJ_BINARY_ENV), "{error}");
        assert!(error.contains("Git repositories are unaffected"), "{error}");
    }

    /// Test-only mirror of the PATH branch of [`locate_jj`], so the message can
    /// be asserted without mutating the process environment (which would race
    /// against the other tests in this binary).
    fn locate_jj_in(dirs: &[PathBuf]) -> eyre::Result<PathBuf> {
        for dir in dirs {
            let candidate = dir.join(JJ_BINARY);
            if is_executable_file(&candidate) {
                return Ok(candidate);
            }
        }
        Err(eyre!(
            "could not find the `{JJ_BINARY}` executable on PATH. Install jujutsu, or set \
             {JJ_BINARY_ENV} to its location. Git repositories are unaffected."
        ))
    }

    #[test]
    fn a_plain_file_without_an_execute_bit_is_not_the_jj_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(JJ_BINARY);
        std::fs::write(&path, b"not executable").unwrap();

        assert!(!is_executable_file(&path));
        assert!(locate_jj_in(&[dir.path().to_owned()]).is_err());
    }

    #[test]
    fn an_executable_on_the_search_path_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(JJ_BINARY);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        assert_eq!(locate_jj_in(&[dir.path().to_owned()]).unwrap(), path);
    }

    #[test]
    fn record_reads_ask_for_no_graph_and_an_explicit_template() {
        // `records` composes the caller's args with the flags that make output
        // machine-readable; assert the composition without spawning anything.
        const TEMPLATE: Template =
            Template::new(&[("name", "name"), ("target", "target.change_id()")]);
        let expression = TEMPLATE.expression();

        let argv = cli().argv(
            WorkingCopy::Ignore,
            &record_args(&["workspace", "list"], &expression),
        );

        assert!(argv.contains(&"--no-graph".to_owned()), "{argv:?}");
        assert!(argv.contains(&expression), "{argv:?}");
        assert!(expression.contains(FIELD_SEPARATOR), "{expression:?}");
    }

    /// The one test that needs a real jj. It skips rather than fails when jj is
    /// absent, so a contributor without jj still gets a green `cargo test`.
    #[test]
    fn probes_the_version_of_a_real_jj() {
        let Ok(program) = locate_jj() else {
            eprintln!("skipping: no jj binary on this machine");
            return;
        };

        let version = probe_version(&program).unwrap();
        assert!(
            version.is_supported(),
            "installed jj {version} is below the supported minimum {MINIMUM_JJ_VERSION}"
        );
    }
}
