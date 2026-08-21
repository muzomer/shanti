//! A real jj repository in a temporary directory, for the tests that need one.
//!
//! shanti reads jj through its template language, so the only test that proves
//! a template is right is one that runs it against a real `jj`. That binary is
//! not guaranteed to exist on a contributor's machine, so every fixture here
//! *skips* rather than fails when jj is missing: `JjFixture::new` returns
//! `None`, and the test returns early. Same bargain as
//! `cmd::tests::probes_the_version_of_a_real_jj`.

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

use super::backend::JjBackend;
use super::cmd::JjCli;

/// Whether this machine has a jj shanti can drive.
pub fn jj_available() -> bool {
    if JjCli::is_available() {
        return true;
    }
    eprintln!("skipping: no jj binary on this machine");
    false
}

/// A throwaway jj repository with its default workspace and one commit.
///
/// The `TempDir` field is what keeps the directory alive; dropping the fixture
/// removes the repository and every workspace added to it.
pub struct JjFixture {
    /// Kept only to own the directory's lifetime; every path below is derived
    /// from the canonicalised `base` instead.
    _dir: TempDir,
    /// The temporary directory, canonicalised. jj reports canonical paths (on
    /// macOS `/var` is a symlink to `/private/var`), so a fixture that handed
    /// tests the uncanonicalised path would make every path assertion fail for
    /// a reason that has nothing to do with the code under test.
    base: PathBuf,
    root: PathBuf,
    /// The located jj, so fixture setup and the code under test agree on which
    /// binary they mean when `SHANTI_JJ_BIN` points somewhere unusual.
    cli: JjCli,
}

impl JjFixture {
    /// Create a repository named `name`, or `None` if jj is unavailable.
    ///
    /// The name becomes the repository's directory name, which is what
    /// [`crate::vcs::Repo::name`] is derived from, so tests can assert on it.
    pub fn new(name: &str) -> Option<Self> {
        if !jj_available() {
            return None;
        }

        let dir = tempfile::tempdir().expect("could not create a temporary directory");
        let base = dir
            .path()
            .canonicalize()
            .expect("could not canonicalise the temporary directory");
        let root = base.join(name);
        std::fs::create_dir_all(&root).expect("could not create the repository directory");

        // `discover` only locates and version-checks jj; it does not require the
        // repository to exist yet, so it can run before `jj git init`.
        let cli = JjCli::discover(&root).expect("jj is available but could not be discovered");
        let fixture = Self {
            _dir: dir,
            base,
            root,
            cli,
        };
        fixture.jj(&["git", "init"]);
        // A workspace whose working copy has no parent behaves differently
        // enough (the root commit) to be a poor stand-in for a real repository.
        fixture.jj(&["describe", "-m", "first"]);
        fixture.jj(&["new"]);
        Some(fixture)
    }

    /// Write `contents` to `name` in the default workspace and let jj snapshot
    /// it.
    ///
    /// The explicit snapshot matters: shanti's reads pass
    /// `--ignore-working-copy`, so an edit jj has not recorded yet is
    /// deliberately invisible to them.
    pub fn commit_change(&self, name: &str, contents: &str) {
        std::fs::write(self.root.join(name), contents).expect("could not write the test file");
        self.jj(&["status"]);
    }

    /// Add a workspace named `name`, rooted beside the repository.
    pub fn add_workspace(&self, name: &str) {
        let dest = self.workspace_root(name);
        self.jj(&["workspace", "add", "--name", name, dest.to_str().unwrap()]);
    }

    /// Where [`JjFixture::add_workspace`] puts a workspace. Deliberately outside
    /// the repository root, as shanti's own layout is.
    pub fn workspace_root(&self, name: &str) -> PathBuf {
        self.base.join(format!("ws-{name}"))
    }

    /// A backend bound to this repository.
    pub fn backend(&self) -> JjBackend {
        JjBackend::from_cli(self.cli.clone()).expect("could not open the fixture repository")
    }

    /// Run a jj command against the fixture, panicking with jj's own complaint
    /// if it fails — a broken fixture is a test bug, not a condition to handle.
    ///
    /// Deliberately not routed through [`JjCli`]: fixtures need setup commands
    /// and an isolated identity that the production adapter has no business
    /// offering.
    pub fn jj(&self, args: &[&str]) {
        let output = Command::new(self.cli.program())
            .arg("--no-pager")
            .arg("--color=never")
            .args(args)
            .current_dir(&self.root)
            // Isolate from the developer's own jj config, which could otherwise
            // change templates, aliases or the default workspace name.
            .env("JJ_CONFIG", "/dev/null")
            .env("JJ_USER", "shanti tests")
            .env("JJ_EMAIL", "tests@shanti.invalid")
            .output()
            .expect("could not run jj");

        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
