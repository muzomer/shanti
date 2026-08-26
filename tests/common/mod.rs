//! Fixtures shared by the integration suites.
//!
//! Each file directly under `tests/` is compiled as its own crate, so nothing is
//! shared between them unless it is put somewhere deliberately. Before this
//! module there was nowhere, and the result was predictable: `git` and
//! `init_repo` existed twice, byte for byte, and `boot` existed twice with two
//! different timeouts. A fixture that drifts is worse than a duplicated one,
//! because it makes two suites disagree about what a repository looks like while
//! both of them pass.
//!
//! This lives in a *subdirectory* rather than as `tests/common.rs` for the usual
//! reason: a file directly under `tests/` becomes a test binary of its own, and
//! one with no `#[test]` in it is reported as an empty suite on every run.
//!
//! # Scope
//!
//! Only fixtures that are genuinely identical across suites belong here. A
//! helper used by one suite stays in that suite, where it can be read next to
//! the tests that need it. The jj fixtures are the deliberate exception and stay
//! in `src/vcs/jj/testing.rs`, which already owns the rule that a missing `jj`
//! makes those tests *skip* rather than fail.

// Each suite uses a different subset of what follows, and an unused import in
// one crate is a warning in a build that denies them. This is the standard cost
// of a shared test module: the alternative is a `cfg` maze that nobody can read.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use shanti::app::App;
use shanti::cli::Args;
use shanti::events::AppEvent;
use shanti::jobs::Worker;

/// How long a suite waits for the startup scan before calling it a failure.
///
/// The two copies this replaces disagreed — thirty seconds in the render suite,
/// ten in the state suite — so the longer one wins. A generous timeout never
/// turns a passing test red; it only delays a failing one, and the render suite
/// pays a real cost on a cold `target/` that the state suite does not.
const SCAN_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs a git command in `cwd`, failing the test with git's own stderr.
///
/// The environment is scrubbed on purpose. A developer's global `user.name`,
/// `init.defaultBranch` or commit template would otherwise leak into the
/// fixture, so the suite would pass on their machine and fail in CI — or worse,
/// the other way round.
pub fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        // Keep the developer's global/system git config out of the fixture.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "shanti test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "shanti test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git is required to run these tests");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Creates a git repository named `name` under `repos_dir`, with one commit on
/// `main`.
///
/// The commit matters: a repository with no commits has no `HEAD` to resolve, so
/// a fixture without one exercises a different path through the backend than any
/// repository a user would point shanti at.
pub fn init_repo(repos_dir: &Path, name: &str) {
    let path = repos_dir.join(name);
    std::fs::create_dir_all(&path).expect("could not create repo dir");
    git(&path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README.md"), "fixture\n").expect("could not write README");
    git(&path, &["add", "README.md"]);
    git(&path, &["commit", "-q", "-m", "init"]);
}

/// An app whose startup scan has finished — the steady state most tests want.
pub fn boot(args: &Args) -> (App, Receiver<AppEvent>) {
    let (mut app, results) = booting(args);
    while app.is_scanning() {
        match results.recv_timeout(SCAN_TIMEOUT) {
            Ok(AppEvent::Job(result)) => app.handle_job(result),
            Ok(other) => panic!("expected a job result, got {other:?}"),
            Err(error) => panic!("the startup scan never finished: {error}"),
        }
    }
    (app, results)
}

/// The same app with its scan still in flight — the state the user sees first.
pub fn booting(args: &Args) -> (App, Receiver<AppEvent>) {
    // The default lookup fails loudly: a PR test that forgot to stub sees an
    // error message rather than silently reaching out to github.com.
    let mut app = App::with_args(
        args.clone(),
        Arc::new(|_| Err(color_eyre::eyre::eyre!("no PR lookup was stubbed"))),
    );
    let (sender, results) = mpsc::channel();
    // One thread, so the scan of the first repos dir finishes before the second
    // one starts: a test that stops half way through a stream then knows which
    // half it has.
    app.attach_worker(Worker::with_threads(sender, 1));
    (app, results)
}

/// A bare character key press.
pub fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

/// A bare non-character key press, such as `Enter` or `Tab`.
pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A control-modified character key press.
pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
