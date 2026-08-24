//! End-to-end tests for a *colocated* repository — one that has both `.git` and
//! `.jj`, which is how most people adopt jj on an existing project.
//!
//! shanti-12z.5 gave such a repository to jj, because driving it with raw git
//! behind jj's back leaves jj's view of the working copy stale. That is a rule
//! about *writing* to the working copy; applied to *listing*, it made every git
//! worktree of the repository invisible with no message (shanti-nhe.9). These
//! tests describe the fix: both backends are opened, every space says which one
//! owns it, and a deletion goes back through the owner.
//!
//! Like `tests/jj_backend.rs`, every test skips — printing why — on a machine
//! with no `jj`, so `cargo test` stays green for a contributor who has not
//! installed it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use shanti::app::App;
use shanti::cli::Args;
use shanti::events::AppEvent;
use shanti::jobs::Worker;
use shanti::vcs::{self, backends_at, discover, Backend, Vcs};
use tempfile::{tempdir, TempDir};

/// A colocated repository, and the directory that owns it.
///
/// The `TempDir` is a field on purpose: dropping it deletes the repository, so a
/// test holding only a backend would be driving it at a directory that had
/// already vanished.
struct Colocated {
    _dir: TempDir,
    root: PathBuf,
    repos_dir: PathBuf,
    spaces_dir: PathBuf,
}

impl Colocated {
    /// A repository with one commit, colocated with git — or `None`, with a
    /// printed reason, when jj is unavailable.
    fn new() -> Option<Self> {
        if Command::new("jj").arg("--version").output().is_err() {
            eprintln!("skipping: no jj binary on this machine");
            return None;
        }

        let dir = tempdir().expect("could not create a temporary directory");
        let base = dir
            .path()
            .canonicalize()
            .expect("could not canonicalise the temporary directory");
        let repos_dir = base.join("repos");
        let root = repos_dir.join("project");
        std::fs::create_dir_all(&root).expect("could not create the repository directory");

        run("git", &["init", "--quiet"], &root);
        run(
            "git",
            &["config", "user.email", "tests@shanti.invalid"],
            &root,
        );
        run("git", &["config", "user.name", "shanti tests"], &root);
        std::fs::write(root.join("a.txt"), "hello\n").expect("could not write a file");
        run("git", &["add", "."], &root);
        run("git", &["commit", "--quiet", "-m", "first"], &root);
        // Colocating *after* the git history exists is the real-world order, and
        // the one that leaves pre-existing git worktrees behind.
        run("jj", &["git", "init", "--colocate"], &root);
        run(
            "jj",
            &["config", "set", "--repo", "user.name", "shanti tests"],
            &root,
        );
        run(
            "jj",
            &[
                "config",
                "set",
                "--repo",
                "user.email",
                "tests@shanti.invalid",
            ],
            &root,
        );

        Some(Self {
            _dir: dir,
            root,
            repos_dir,
            spaces_dir: base.join("spaces"),
        })
    }

    /// Both backends shanti opens for this repository, the owner first.
    fn backends(&self) -> Vec<Box<dyn Vcs>> {
        let found = discover(&self.repos_dir, std::slice::from_ref(&self.spaces_dir));
        assert_eq!(found.len(), 1, "expected exactly one repository: {found:?}");
        vcs::open_backends(&found, false)
    }

    fn dest(&self, name: &str) -> PathBuf {
        vcs::space_dest(
            &self.spaces_dir.to_string_lossy(),
            &self.root.file_name().expect("named").to_string_lossy(),
            name,
        )
    }

    /// What jj makes of the repository right now: its exit status and stderr.
    /// A corrupted view of the working copy shows up here.
    fn jj_status(&self) -> (bool, String) {
        let out = Command::new("jj")
            .args(["status"])
            .current_dir(&self.root)
            .output()
            .expect("could not run jj");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn run(program: &str, args: &[&str], dir: &Path) {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|err| panic!("could not run {program}: {err}"));
    assert!(status.success(), "{program} {args:?} failed");
}

fn backend_of(backends: &[Box<dyn Vcs>], backend: Backend) -> &dyn Vcs {
    backends
        .iter()
        .find(|open| open.backend() == backend)
        .unwrap_or_else(|| panic!("no {backend} backend was opened"))
        .as_ref()
}

/// Boots an `App` over the fixture and runs its startup scan to completion.
///
/// No frame is drawn, so the layout stays single-pane: `n` opens the picker,
/// whose selection builds the same create prompt the two-pane `n` does — the
/// one place the git-or-jj choice is exercised.
fn boot_app(repos_dir: &Path, spaces_dir: &Path) -> (App, Receiver<AppEvent>) {
    let args = Args::for_dirs(
        spaces_dir.to_string_lossy().into_owned(),
        vec![repos_dir.to_string_lossy().into_owned()],
    );
    let mut app = App::with_args(
        args,
        Arc::new(|_| Err(color_eyre::eyre::eyre!("no PR lookup was stubbed"))),
    );
    let (sender, results) = mpsc::channel();
    app.attach_worker(Worker::with_threads(sender, 1));
    while app.is_scanning() {
        match results.recv_timeout(Duration::from_secs(10)) {
            Ok(AppEvent::Job(result)) => app.handle_job(result),
            other => panic!("expected a job result, got {other:?}"),
        }
    }
    (app, results)
}

fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(ch(c));
    }
}

/// The create prompt on a colocated repository lets the user pick the backend:
/// Tab switches it, and the space that results is that backend's own kind.
#[test]
fn the_create_prompt_chooses_git_or_jj_on_a_colocated_repository() {
    let Some(fixture) = Colocated::new() else {
        return;
    };

    // Default: the owner (jj) makes a jj workspace.
    {
        let (mut app, _results) = boot_app(&fixture.repos_dir, &fixture.spaces_dir);
        app.handle_key(ch('n')); // the picker
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // pick the repo
        typed(&mut app, "via-jj");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // create

        let dest = fixture.dest("via-jj");
        assert!(
            dest.join(".jj").exists(),
            "the default backend must make a jj workspace at {dest:?}"
        );
    }

    // Tab once: git makes a git worktree instead.
    {
        let (mut app, _results) = boot_app(&fixture.repos_dir, &fixture.spaces_dir);
        app.handle_key(ch('n'));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)); // switch to git
        typed(&mut app, "via-git");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let dest = fixture.dest("via-git");
        assert!(
            dest.join(".git").exists(),
            "Tab must switch to git and make a git worktree at {dest:?}"
        );
        assert!(
            !dest.join(".jj").exists(),
            "a git worktree is not a jj workspace"
        );
    }
}

/// jj owns a colocated repository, but git can still drive it — and must, for
/// the worktrees it made.
#[test]
fn discovery_reports_both_backends_of_a_colocated_repository() {
    let Some(fixture) = Colocated::new() else {
        return;
    };

    assert_eq!(
        backends_at(&fixture.root),
        Some((Backend::Jj, vec![Backend::Git])),
    );
}

/// Both backends of one colocated repository must share a single `RepoId`, so
/// the repositories list shows it once, not once per backend.
///
/// The regression: `RepoId` was the raw path string, and libgit2's `workdir()`
/// ends in a separator where jj uses the bare path — two ids for one directory,
/// so the list showed a "jj" row and a "git" row for the same repository.
#[test]
fn both_backends_of_a_colocated_repository_share_one_id() {
    let Some(fixture) = Colocated::new() else {
        return;
    };
    let backends = fixture.backends();
    assert_eq!(backends.len(), 2, "a colocated repo opens as two backends");

    let ids: Vec<_> = backends.iter().map(|b| b.repo().id.clone()).collect();
    assert_eq!(
        ids[0], ids[1],
        "the git and jj backends of one directory must have the same id"
    );
}

/// The bug: 23 git worktrees, one row on screen. Both backends are opened now,
/// so both kinds of space are listed.
#[test]
fn a_colocated_repository_lists_git_worktrees_alongside_jj_workspaces() {
    let Some(fixture) = Colocated::new() else {
        return;
    };
    let backends = fixture.backends();
    assert_eq!(backends.len(), 2, "a colocated repo opens as two backends");

    backend_of(&backends, Backend::Git)
        .create_space("from-git", &fixture.dest("from-git"))
        .expect("could not create the git worktree");
    backend_of(&backends, Backend::Jj)
        .create_space("from-jj", &fixture.dest("from-jj"))
        .expect("could not create the jj workspace");

    let mut listed: Vec<(String, Backend)> = backends
        .iter()
        .flat_map(|backend| backend.spaces().expect("could not list the spaces"))
        .map(|space| (space.name, space.backend))
        .collect();
    listed.sort();

    assert_eq!(
        listed,
        vec![
            ("default".to_string(), Backend::Jj),
            ("from-git".to_string(), Backend::Git),
            ("from-jj".to_string(), Backend::Jj),
        ],
        "both backends' spaces must be listed, each tagged with its owner"
    );
}

/// The routing question, end to end: a git worktree is removed through git —
/// which is how it was created — and jj is left in a state it can still read.
///
/// The stale-working-copy risk shanti-12z.5 named is about jj's *own* working
/// copy, which a linked git worktree is not. jj imports the branch deletion as
/// an ordinary bookmark deletion; that is what the last assertion pins down.
#[test]
fn deleting_a_git_worktree_of_a_colocated_repository_leaves_jj_healthy() {
    let Some(fixture) = Colocated::new() else {
        return;
    };
    let backends = fixture.backends();
    let git = backend_of(&backends, Backend::Git);
    let jj = backend_of(&backends, Backend::Jj);

    let space = git
        .create_space("from-git", &fixture.dest("from-git"))
        .expect("could not create the git worktree");
    assert_eq!(space.backend, Backend::Git, "the owner is recorded");

    git.delete_space(&space)
        .expect("could not delete the space");

    assert!(!space.path.exists(), "the worktree directory must be gone");
    assert!(
        !git.spaces()
            .expect("could not list the git spaces")
            .iter()
            .any(|listed| listed.name == "from-git"),
        "the worktree registration must be gone too"
    );

    let (ok, stderr) = fixture.jj_status();
    assert!(ok, "jj could not read the repository afterwards: {stderr}");
    assert_eq!(
        jj.spaces()
            .expect("could not list the jj spaces")
            .iter()
            .map(|space| space.name.clone())
            .collect::<Vec<_>>(),
        vec!["default".to_string()],
        "deleting a git worktree must not disturb the jj workspaces"
    );
}
