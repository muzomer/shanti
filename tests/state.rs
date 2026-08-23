//! Integration tests for the `App` state machine (`shanti-b03.3`).
//!
//! These tests are the regression net for the modal-stack rewrite (`shanti-nbt.1`).
//! They drive `App::handle_key` with real key events and assert the resulting
//! state transitions.
//!
//! ## Why the assertions go through the rendered screen
//!
//! `App::focus` and `App::mode` are private and there is no accessor, so from an
//! integration test the only observable surface is (a) the `EventState` returned by
//! `handle_key`, (b) the public `selected_path` field, and (c) what `App::draw`
//! paints. We therefore render into a `TestBackend` and identify the active modal by
//! its *block title* — a stable structural marker, deliberately not the help body
//! text, which is expected to keep changing.
//!
//! ## Why nothing here is serialised
//!
//! `App::with_args` takes the resolved configuration directly, so a `Fixture` points
//! an `App` at its own temp directories without touching argv, the environment, or
//! the configuration file. Nothing is process-global, so the tests run in parallel
//! and a test may hold as many live fixtures as it likes.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use shanti::app::App;
use shanti::cli::Args;
use shanti::events::AppEvent;
use shanti::github::PrInfo;
use shanti::jobs::Worker;
use tempfile::{tempdir, TempDir};

const CONSUMED: &str = "Consumed";
const NOT_CONSUMED: &str = "NotConsumed";
const EXIT: &str = "Exit";

/// Large enough that no popup is clipped, so title markers always render in full.
const SCREEN_W: u16 = 140;
const SCREEN_H: u16 = 50;

// ---------------------------------------------------------------------------
// Observable state
// ---------------------------------------------------------------------------

/// Which modal is on screen. Derived from block titles, never from body text.
#[derive(Debug, PartialEq, Eq)]
enum Modal {
    /// No popup: the worktree list owns the screen.
    None,
    Repositories,
    CreateWorktree,
    Confirm,
    Help,
    PrWorktree,
    SelectReposDir,
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Insert,
    /// The mode indicator is replaced by the error line, so the mode is not visible.
    Hidden,
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    app: App,
    /// Where the worker reports its results.
    ///
    /// Held for the life of the fixture, not just for the boot: the pool sends
    /// on the other end of this channel, and dropping it would make every later
    /// result vanish rather than arrive.
    results: Receiver<AppEvent>,
    repos_dir: TempDir,
    worktrees_dir: TempDir,
    /// Second repos dir, only populated by [`Fixture::with_two_repos_dirs`].
    _extra_repos_dir: Option<TempDir>,
    /// Bare repository standing in for a remote, only populated by
    /// [`Fixture::push`].
    _remote_dir: Option<TempDir>,
    /// Kept so the app can be rebuilt after the fixture changes on disk;
    /// statuses are probed when spaces are listed, not per frame.
    args: Args,
}

impl Fixture {
    /// One repos dir holding two repositories; `alpha` has one linked worktree.
    fn new() -> Self {
        Self::build(false)
    }

    /// Same, but the configuration lists two repos directories — the shape that
    /// makes the clone flow ask which directory to clone into.
    fn with_two_repos_dirs() -> Self {
        Self::build(true)
    }

    /// Two repos dirs of two repositories each, with the startup scan still
    /// running — one result per dir, neither of them applied yet.
    ///
    /// The pool runs on one thread here (see [`booting`]), so the roots land in
    /// the order they were configured: the first `deliver_one` is always the
    /// first repos dir, and the half-filled list always holds its two rows.
    fn streaming() -> Self {
        let repos_dir = tempdir().expect("could not create repos dir");
        let extra = tempdir().expect("could not create second repos dir");
        let worktrees_dir = tempdir().expect("could not create worktrees dir");

        for (dir, repos) in [
            (repos_dir.path(), ["alpha", "beta"]),
            (extra.path(), ["gamma", "delta"]),
        ] {
            for repo in repos {
                init_repo(dir, repo);
                add_worktree(dir, repo, worktrees_dir.path(), &format!("feature-{repo}"));
            }
        }

        let args = Args::for_dirs(
            worktrees_dir.path().display().to_string(),
            vec![
                repos_dir.path().display().to_string(),
                extra.path().display().to_string(),
            ],
        );
        let (app, results) = booting(&args);

        Self {
            app,
            results,
            repos_dir,
            worktrees_dir,
            _extra_repos_dir: Some(extra),
            _remote_dir: None,
            args,
        }
    }

    fn build(two_dirs: bool) -> Self {
        let repos_dir = tempdir().expect("could not create repos dir");
        let worktrees_dir = tempdir().expect("could not create worktrees dir");

        init_repo(repos_dir.path(), "alpha");
        init_repo(repos_dir.path(), "beta");
        add_worktree(
            repos_dir.path(),
            "alpha",
            worktrees_dir.path(),
            "feature-one",
        );

        let extra = if two_dirs {
            Some(tempdir().expect("could not create second repos dir"))
        } else {
            None
        };

        let mut repos_dirs = vec![repos_dir.path().display().to_string()];
        repos_dirs.extend(extra.iter().map(|d| d.path().display().to_string()));

        let args = Args::for_dirs(worktrees_dir.path().display().to_string(), repos_dirs);

        let (app, results) = boot(&args);

        Self {
            app,
            results,
            repos_dir,
            worktrees_dir,
            _extra_repos_dir: extra,
            _remote_dir: None,
            args,
        }
    }

    /// Rebuilds the app from the same configuration, re-probing every status.
    ///
    /// The delete guard reads the snapshot taken when spaces were listed, so a
    /// test that changes a repository has to ask for a fresh reading — exactly
    /// as restarting shanti would.
    fn reload(&mut self) {
        let (app, results) = boot(&self.args);
        self.app = app;
        self.results = results;
    }

    /// Applies the next background result, whatever it is.
    ///
    /// What a single turn of the real loop does: one result in, one redraw. The
    /// streaming tests use it to stop *between* scan results, where the list is
    /// half filled and still has to be usable.
    fn deliver_one(&mut self) {
        match self.results.recv_timeout(Duration::from_secs(10)) {
            Ok(AppEvent::Job(result)) => self.app.handle_job(result),
            Ok(other) => panic!("expected a job result, got {other:?}"),
            Err(error) => panic!("no background result arrived: {error}"),
        }
    }

    /// Applies a background result if one turns up shortly, and shrugs if not.
    ///
    /// For the cancellation tests, where whether a result exists at all depends
    /// on whether the worker had picked the job up before it was cancelled — a
    /// race the app is built to tolerate, so the test tolerates it too.
    fn deliver_any(&mut self) {
        if let Ok(AppEvent::Job(result)) = self.results.recv_timeout(Duration::from_millis(500)) {
            self.app.handle_job(result);
        }
    }

    /// Gives `branch` an upstream that already holds its commits.
    ///
    /// This is the only state the delete guard considers safe — clean and
    /// pushed — and the fixture's worktree is deliberately *not* in it by
    /// default, because a freshly created space never is.
    fn push(&mut self, repo: &str, branch: &str) {
        let remote = tempdir().expect("could not create remote dir");
        git(remote.path(), &["init", "--bare", "-q"]);
        git(
            &self.repo_path(repo),
            &[
                "remote",
                "add",
                "origin",
                remote.path().to_str().expect("utf-8 path"),
            ],
        );
        git(
            &self.worktree_path(repo, branch),
            &["push", "-q", "-u", "origin", branch],
        );
        self._remote_dir = Some(remote);
        self.reload();
    }

    /// Leaves an uncommitted change in a worktree.
    fn dirty(&mut self, repo: &str, branch: &str) {
        std::fs::write(
            self.worktree_path(repo, branch).join("README.md"),
            "edited\n",
        )
        .expect("could not edit the worktree");
        self.reload();
    }

    /// Deletes the selected space through the guarded path: open the dialog and
    /// type the space's name.
    fn delete_deliberately(&mut self, name: &str) {
        self.press_char('d');
        self.type_str(name);
        self.press(key(KeyCode::Enter));
    }

    fn worktree_path(&self, repo: &str, branch: &str) -> std::path::PathBuf {
        self.worktrees_dir.path().join(repo).join(branch)
    }

    fn repo_path(&self, repo: &str) -> std::path::PathBuf {
        self.repos_dir.path().join(repo)
    }

    // -- stubbing the PR lookup ---------------------------------------------

    /// Answers every PR lookup with the same branch.
    ///
    /// Everything past the fetch — the clone prompt, the repos-dir picker, the
    /// branch prompt — is unreachable while the lookup goes to GitHub, so the
    /// tests below hand `App` a canned answer instead.
    fn stub_pr_branch(&mut self, branch: &str, is_merged: bool) {
        let branch = branch.to_string();
        self.app.set_pr_fetcher(Arc::new(move |_| {
            Ok(PrInfo {
                branch_name: branch.clone(),
                is_merged,
            })
        }));
    }

    /// Makes every PR lookup fail, as a missing token or a 404 would.
    fn stub_pr_failure(&mut self, message: &'static str) {
        self.app
            .set_pr_fetcher(Arc::new(move |_| Err(color_eyre::eyre::eyre!(message))));
    }

    // -- driving -----------------------------------------------------------

    /// Returns the `EventState` as a string: the enum is not re-exported from the
    /// crate root, so an integration test cannot name the type. See the report.
    fn press(&mut self, key: KeyEvent) -> String {
        format!("{:?}", self.app.handle_key(key))
    }

    fn press_char(&mut self, c: char) -> String {
        self.press(ch(c))
    }

    fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            self.press_char(c);
        }
    }

    // -- observing ---------------------------------------------------------

    fn screen(&mut self) -> String {
        let mut terminal =
            Terminal::new(TestBackend::new(SCREEN_W, SCREEN_H)).expect("terminal init");
        terminal
            .draw(|frame| self.app.draw(frame))
            .expect("draw failed");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn modal(&mut self) -> Modal {
        let screen = self.screen();
        // Order matters: Help renders on top of the Repositories popup.
        if screen.contains(" Help ") {
            Modal::Help
        // The prompt is titled in the vocabulary of the backend it will create
        // through, so a jj repository says "Workspace" where git says "Worktree".
        } else if screen.contains("New Worktree") || screen.contains("New Workspace") {
            Modal::CreateWorktree
        } else if screen.contains("Worktree from PR") {
            Modal::PrWorktree
        } else if screen.contains("Select Clone Directory") {
            Modal::SelectReposDir
        } else if screen.contains('⚠') {
            Modal::Confirm
        } else if screen.contains("Repositories") {
            Modal::Repositories
        } else {
            Modal::None
        }
    }

    fn mode(&mut self) -> Mode {
        let screen = self.screen();
        match (screen.contains(" NORMAL "), screen.contains(" INSERT ")) {
            (true, false) => Mode::Normal,
            (false, true) => Mode::Insert,
            _ => Mode::Hidden,
        }
    }

    /// Asserts we are back at the bare worktree list in Normal mode.
    fn assert_at_worktree_list(&mut self) {
        assert_eq!(self.modal(), Modal::None, "expected no popup on screen");
        assert_eq!(self.mode(), Mode::Normal, "expected Normal mode");
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Builds an app and runs its startup scan to completion.
///
/// Startup is asynchronous: `App::with_args` reads nothing from disk and the
/// list is filled by scan jobs. A test that wants a populated list therefore has
/// to pump results the way `run_app` does — which is also what keeps every test
/// below on the streaming path rather than on a synchronous one nobody ships.
fn boot(args: &Args) -> (App, Receiver<AppEvent>) {
    let (mut app, results) = booting(args);
    while app.is_scanning() {
        match results.recv_timeout(Duration::from_secs(10)) {
            Ok(AppEvent::Job(result)) => app.handle_job(result),
            Ok(other) => panic!("expected a job result, got {other:?}"),
            Err(error) => panic!("the startup scan never finished: {error}"),
        }
    }
    (app, results)
}

/// The same app with its scan still in flight — the state the user sees first.
fn booting(args: &Args) -> (App, Receiver<AppEvent>) {
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

fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn git(cwd: &Path, args: &[&str]) {
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

fn init_repo(repos_dir: &Path, name: &str) {
    let path = repos_dir.join(name);
    std::fs::create_dir_all(&path).expect("could not create repo dir");
    git(&path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README.md"), "fixture\n").expect("could not write README");
    git(&path, &["add", "README.md"]);
    git(&path, &["commit", "-q", "-m", "init"]);
}

fn add_worktree(repos_dir: &Path, repo: &str, worktrees_dir: &Path, branch: &str) {
    let repo_path = repos_dir.join(repo);
    let target = worktrees_dir.join(repo).join(branch);
    git(
        &repo_path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            target.to_str().expect("utf-8 path"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Fixture sanity
// ---------------------------------------------------------------------------

#[test]
fn starts_on_the_worktree_list_in_normal_mode() {
    let mut f = Fixture::new();
    f.assert_at_worktree_list();
    assert!(
        f.screen().contains("Worktrees (1/1)"),
        "fixture worktree should be listed:\n{}",
        f.screen()
    );
}

/// A colocated repository contributes git worktrees *and* jj workspaces to one
/// list under one name (shanti-nhe.9), and the two behave differently when
/// deleted — so every row says which backend owns it, even in this git-only
/// fixture where the answer is never in doubt.
#[test]
fn worktree_rows_name_the_backend_that_owns_them() {
    let mut f = Fixture::new();
    let screen = f.screen();
    let row = screen
        .lines()
        .find(|line| line.contains("alpha /"))
        .unwrap_or_else(|| panic!("the fixture worktree should be listed:\n{}", screen));

    let (before, _) = row.split_once("alpha /").expect("the row was just matched");
    assert!(
        before.contains("git"),
        "the row must say which backend owns the space:\n{}",
        row
    );
}

#[test]
fn unbound_key_is_not_consumed() {
    let mut f = Fixture::new();
    assert_eq!(f.press(key(KeyCode::F(2))), NOT_CONSUMED);
    f.assert_at_worktree_list();
}

// ---------------------------------------------------------------------------
// Worktrees focus — Normal and Insert mode
// ---------------------------------------------------------------------------

#[test]
fn worktrees_normal_enters_and_leaves_insert_mode() {
    let mut f = Fixture::new();

    assert_eq!(f.press_char('i'), CONSUMED);
    assert_eq!(f.mode(), Mode::Insert);

    assert_eq!(f.press(key(KeyCode::Esc)), CONSUMED);
    assert_eq!(f.mode(), Mode::Normal);

    // `/` is the second binding onto the same action.
    f.press_char('/');
    assert_eq!(f.mode(), Mode::Insert);
}

#[test]
fn worktrees_insert_mode_tab_moves_focus_to_the_list_and_back_to_normal() {
    let mut f = Fixture::new();

    f.press_char('i');
    assert_eq!(f.mode(), Mode::Insert);

    // Tab off the filter → focus is the list → Normal mode.
    assert_eq!(f.press(key(KeyCode::Tab)), CONSUMED);
    assert_eq!(f.mode(), Mode::Normal);

    // Tab back onto the filter → Insert mode again.
    f.press(key(KeyCode::Tab));
    assert_eq!(f.mode(), Mode::Insert);
}

#[test]
fn worktrees_insert_mode_types_into_the_filter() {
    let mut f = Fixture::new();

    f.press_char('i');
    f.type_str("feature");
    assert!(
        f.screen().contains("Worktrees (1/1)"),
        "matching filter should keep the worktree visible"
    );

    f.type_str("-zzz");
    assert!(
        f.screen().contains("Worktrees (0/0)"),
        "non-matching filter should empty the list:\n{}",
        f.screen()
    );

    // Backspace is a distinct action from InsertChar.
    for _ in 0.."-zzz".len() {
        f.press(key(KeyCode::Backspace));
    }
    assert!(f.screen().contains("Worktrees (1/1)"));
}

#[test]
fn worktrees_insert_mode_char_keys_do_not_trigger_normal_mode_actions() {
    let mut f = Fixture::new();
    f.press_char('i');

    // In Insert mode 'q', 'n', 'p' and '?' are literal characters, not actions.
    f.type_str("qnp?");
    assert_eq!(f.modal(), Modal::None, "no popup should have opened");
    assert_eq!(f.mode(), Mode::Insert);
}

#[test]
fn worktrees_enter_selects_a_path_and_exits() {
    let mut f = Fixture::new();
    let expected = std::fs::canonicalize(f.worktree_path("alpha", "feature-one"))
        .expect("worktree should exist");

    assert_eq!(f.press(key(KeyCode::Enter)), EXIT);

    let selected = f.app.selected_path.clone().expect("a path was selected");
    assert_eq!(
        std::fs::canonicalize(selected.trim_end_matches('/')).unwrap(),
        expected
    );
}

#[test]
fn worktrees_q_exits() {
    let mut f = Fixture::new();
    assert_eq!(f.press_char('q'), EXIT);
}

#[test]
fn worktrees_ctrl_c_exits() {
    let mut f = Fixture::new();
    assert_eq!(f.press(ctrl('c')), EXIT);
}

#[test]
fn worktrees_ctrl_c_exits_from_insert_mode() {
    // In Insert mode 'q' is a literal character, so Ctrl+C is the only way out.
    let mut f = Fixture::new();
    f.press_char('i');
    assert_eq!(f.press(ctrl('c')), EXIT);
}

// ---------------------------------------------------------------------------
// Help modal — opening over each parent, and returning to it
// ---------------------------------------------------------------------------

#[test]
fn help_opens_over_the_worktree_list_and_escape_returns() {
    let mut f = Fixture::new();

    assert_eq!(f.press_char('?'), CONSUMED);
    assert_eq!(f.modal(), Modal::Help);

    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
}

#[test]
fn help_toggles_closed_with_the_same_key() {
    let mut f = Fixture::new();

    f.press_char('?');
    assert_eq!(f.modal(), Modal::Help);

    assert_eq!(f.press_char('?'), CONSUMED);
    f.assert_at_worktree_list();
}

#[test]
fn help_over_repositories_returns_to_repositories_not_the_worktree_list() {
    let mut f = Fixture::new();

    f.press_char('n');
    assert_eq!(f.modal(), Modal::Repositories);

    f.press_char('?');
    assert_eq!(f.modal(), Modal::Help);
    assert!(
        f.screen().contains("Repositories"),
        "the parent popup should stay visible behind help:\n{}",
        f.screen()
    );

    f.press(key(KeyCode::Esc));
    assert_eq!(
        f.modal(),
        Modal::Repositories,
        "escape from help must return to the popup it was opened over"
    );

    // And escaping again unwinds the remaining depth.
    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
}

#[test]
fn help_quit_exits() {
    let mut f = Fixture::new();
    f.press_char('?');
    assert_eq!(f.press_char('q'), EXIT);
}

// ---------------------------------------------------------------------------
// Repositories modal
// ---------------------------------------------------------------------------

#[test]
fn repositories_opens_and_escape_returns_to_the_worktree_list() {
    let mut f = Fixture::new();

    assert_eq!(f.press_char('n'), CONSUMED);
    assert_eq!(f.modal(), Modal::Repositories);
    assert_eq!(f.mode(), Mode::Normal);
    assert!(
        f.screen().contains("Repositories (2)"),
        "both fixture repositories should be listed:\n{}",
        f.screen()
    );

    assert_eq!(f.press(key(KeyCode::Esc)), CONSUMED);
    f.assert_at_worktree_list();
}

#[test]
fn repositories_navigation_keys_are_consumed() {
    let mut f = Fixture::new();
    f.press_char('n');

    for k in ['j', 'k', 'g', 'G'] {
        assert_eq!(
            f.press_char(k),
            CONSUMED,
            "'{}' should be handled by the repositories list",
            k
        );
    }
    assert_eq!(f.modal(), Modal::Repositories);
}

#[test]
fn repositories_insert_mode_filters_then_escape_returns_to_normal() {
    let mut f = Fixture::new();
    f.press_char('n');

    assert_eq!(f.press_char('i'), CONSUMED);
    assert_eq!(f.mode(), Mode::Insert);

    f.type_str("alph");
    assert!(
        f.screen().contains("Repositories (1)"),
        "filter should narrow the repository list:\n{}",
        f.screen()
    );

    // Esc in Insert mode leaves the mode, it does not close the popup.
    f.press(key(KeyCode::Esc));
    assert_eq!(f.modal(), Modal::Repositories);
    assert_eq!(f.mode(), Mode::Normal);

    // A second Esc closes the popup.
    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
}

#[test]
fn repositories_tab_toggles_between_filter_and_list() {
    let mut f = Fixture::new();
    f.press_char('n');
    assert_eq!(f.mode(), Mode::Normal);

    f.press_char('i');
    assert_eq!(f.mode(), Mode::Insert);

    f.press(key(KeyCode::Tab));
    assert_eq!(
        f.mode(),
        Mode::Normal,
        "Tab off the filter returns to Normal"
    );

    f.press(key(KeyCode::Tab));
    assert_eq!(
        f.mode(),
        Mode::Insert,
        "Tab back onto the filter re-enters Insert"
    );
}

#[test]
fn repositories_quit_exits() {
    let mut f = Fixture::new();
    f.press_char('n');
    assert_eq!(f.press_char('q'), EXIT);
}

// ---------------------------------------------------------------------------
// Create-worktree modal
// ---------------------------------------------------------------------------

#[test]
fn selecting_a_repository_opens_the_create_worktree_prompt_in_insert_mode() {
    let mut f = Fixture::new();

    f.press_char('n');
    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);
    assert_eq!(f.modal(), Modal::CreateWorktree);
    assert_eq!(f.mode(), Mode::Insert);
}

#[test]
fn cancelling_create_worktree_returns_to_the_worktree_list_with_no_residue() {
    let mut f = Fixture::new();

    f.press_char('n');
    f.press(key(KeyCode::Enter));
    f.type_str("scratch-branch");
    assert!(f.screen().contains("scratch-branch"));

    // Esc from a text prompt closes the whole prompt, skipping the repository list.
    assert_eq!(f.press(key(KeyCode::Esc)), CONSUMED);
    f.assert_at_worktree_list();
    assert!(
        !f.screen().contains("scratch-branch"),
        "cancelled input must not leak onto the worktree list"
    );
    assert!(
        !f.worktree_path("alpha", "scratch-branch").exists(),
        "cancelling must not create anything on disk"
    );

    // Reopening the prompt must not show the abandoned text.
    f.press_char('n');
    f.press(key(KeyCode::Enter));
    assert_eq!(f.modal(), Modal::CreateWorktree);
    assert!(
        !f.screen().contains("scratch-branch"),
        "the prompt must reopen empty:\n{}",
        f.screen()
    );
}

#[test]
fn create_worktree_with_an_empty_name_is_a_no_op_that_closes_the_prompt() {
    let mut f = Fixture::new();

    f.press_char('n');
    f.press(key(KeyCode::Enter));
    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);

    f.assert_at_worktree_list();
    assert!(
        f.screen().contains("Worktrees (1/1)"),
        "no worktree should have been created:\n{}",
        f.screen()
    );
}

#[test]
fn create_worktree_confirms_and_returns_to_the_worktree_list() {
    let mut f = Fixture::new();

    f.press_char('n');
    // The repository list is sorted; select "alpha" explicitly.
    f.press_char('g');
    f.press(key(KeyCode::Enter));
    f.type_str("feature-two");
    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);

    f.assert_at_worktree_list();
    assert!(
        f.screen().contains("feature-two"),
        "the created worktree should appear in the list:\n{}",
        f.screen()
    );
    assert!(
        f.repo_path("alpha").exists(),
        "the source repository should be untouched"
    );
}

#[test]
fn create_worktree_ctrl_c_exits() {
    let mut f = Fixture::new();
    f.press_char('n');
    f.press(key(KeyCode::Enter));
    assert_eq!(f.press(ctrl('c')), EXIT);
}

// ---------------------------------------------------------------------------
// Confirm modal (delete flow)
// ---------------------------------------------------------------------------

#[test]
fn delete_with_confirmation_opens_the_confirm_modal() {
    let mut f = Fixture::new();

    assert_eq!(f.press_char('d'), CONSUMED);
    assert_eq!(f.modal(), Modal::Confirm);
}

#[test]
fn cancelling_the_confirm_modal_leaves_the_worktree_untouched() {
    let mut f = Fixture::new();
    let path = f.worktree_path("alpha", "feature-one");

    f.press_char('d');
    assert_eq!(f.modal(), Modal::Confirm);

    assert_eq!(f.press(key(KeyCode::Esc)), CONSUMED);
    f.assert_at_worktree_list();
    assert!(path.exists(), "cancelling must not delete the worktree");
    assert!(f.screen().contains("Worktrees (1/1)"));
}

/// A space that is clean and pushed still deletes in one confirmation: the
/// guard must not tax the ordinary case.
#[test]
fn confirming_the_delete_removes_the_worktree_and_returns_to_the_list() {
    let mut f = Fixture::new();
    f.push("alpha", "feature-one");
    let path = f.worktree_path("alpha", "feature-one");
    assert!(path.exists());

    f.press_char('d');
    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);

    assert_eq!(f.modal(), Modal::None);
    assert!(!path.exists(), "the worktree directory should be gone");
    assert!(
        f.screen().contains("Worktrees (0/0)"),
        "the deleted worktree should be gone from the list:\n{}",
        f.screen()
    );
}

/// The bug this guard exists for: the fixture worktree was never pushed, so its
/// branch lives nowhere else. Enter — the key every other dialog is dismissed
/// with — must not be able to destroy it.
#[test]
fn enter_cannot_delete_a_worktree_that_was_never_pushed() {
    let mut f = Fixture::new();
    let path = f.worktree_path("alpha", "feature-one");

    f.press_char('d');
    assert_eq!(f.modal(), Modal::Confirm);
    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);

    assert_eq!(
        f.modal(),
        Modal::Confirm,
        "the dialog must stay up until the override is given"
    );
    assert!(path.exists(), "Enter alone must not delete unpushed work");
    assert!(f.screen().contains("Worktrees (1/1)"));
}

/// The override for permanent loss: type the space's own name, which also makes
/// the user look at which row they are standing on.
#[test]
fn typing_the_name_deletes_a_worktree_that_was_never_pushed() {
    let mut f = Fixture::new();
    let path = f.worktree_path("alpha", "feature-one");

    f.delete_deliberately("feature-one");

    assert_eq!(f.modal(), Modal::None);
    assert!(!path.exists(), "the worktree directory should be gone");
    assert!(f.screen().contains("Worktrees (0/0)"));
}

/// A near-miss is not a confirmation.
#[test]
fn a_wrong_name_does_not_delete_the_worktree() {
    let mut f = Fixture::new();
    let path = f.worktree_path("alpha", "feature-one");

    f.press_char('d');
    f.type_str("feature-on");
    f.press(key(KeyCode::Enter));

    assert_eq!(f.modal(), Modal::Confirm);
    assert!(path.exists(), "a mistyped name must not delete anything");
}

/// The dialog has to say what is about to be lost, in git's vocabulary, and that
/// there is no way back.
#[test]
fn the_dialog_names_what_would_be_lost() {
    let mut f = Fixture::new();
    f.press_char('d');
    let screen = f.screen();

    assert!(
        screen.contains("a branch that was never pushed"),
        "the dialog must say what would be destroyed:\n{}",
        screen
    );
    assert!(
        screen.contains("cannot be undone"),
        "a git worktree's loss is permanent and must say so:\n{}",
        screen
    );
}

/// The count rises with the number of files, and untracked files are in it:
/// deletion removes the directory, so a file that was never `git add`ed is lost
/// exactly as thoroughly as an edited one.
#[test]
fn the_dialog_counts_every_file_it_says_it_counts() {
    let mut f = Fixture::new();
    f.push("alpha", "feature-one");
    f.dirty("alpha", "feature-one");
    std::fs::write(
        f.worktree_path("alpha", "feature-one").join("scratch.txt"),
        "never added\n",
    )
    .expect("could not write an untracked file");
    f.reload();

    f.press_char('d');
    let screen = f.screen();
    assert!(
        screen.contains("2 uncommitted files (modified, staged or untracked)"),
        "an untracked file must be counted alongside the edited one:\n{}",
        screen
    );
}

/// The branch is deleted with the worktree, and that is true even when nothing
/// is at risk — the case whose dialog lists no losses at all. A user must not
/// have to infer it from "the directory goes".
#[test]
fn the_dialog_says_the_branch_goes_with_the_worktree() {
    let mut f = Fixture::new();
    f.push("alpha", "feature-one");

    f.press_char('d');
    let screen = f.screen();
    assert!(
        screen.contains("Deleting also removes:"),
        "the dialog must say what deletion removes:\n{}",
        screen
    );
    assert!(
        screen.contains("the branch it has checked out"),
        "the branch goes too and the dialog must say so:\n{}",
        screen
    );
    assert!(
        screen.contains("the worktree directory and its registration"),
        "the directory and the registration both go:\n{}",
        screen
    );
    // git has no bookmark to spare; that sentence belongs to jj alone.
    assert!(
        !screen.contains("bookmark"),
        "git's dialog must not speak jj's vocabulary:\n{}",
        screen
    );
}

/// Pushing the branch is not enough while the working tree still differs from
/// it: the file the user edited exists in no object store anywhere.
#[test]
fn uncommitted_changes_are_guarded_even_when_the_branch_is_pushed() {
    let mut f = Fixture::new();
    f.push("alpha", "feature-one");
    f.dirty("alpha", "feature-one");
    let path = f.worktree_path("alpha", "feature-one");

    f.press_char('d');
    let screen = f.screen();
    // A number, not a phrase: "1 uncommitted file" stops a user where
    // "uncommitted changes" does not. The parenthetical says what was counted,
    // so the figure cannot be read as narrower than it is.
    assert!(
        screen.contains("1 uncommitted file (modified, staged or untracked)"),
        "the dialog must count the uncommitted work:\n{}",
        screen
    );

    f.press(key(KeyCode::Enter));
    assert!(
        path.exists(),
        "Enter alone must not delete uncommitted work"
    );
}

/// 'D' skips the question, not the guard.
#[test]
fn force_delete_skips_the_dialog_only_when_nothing_would_be_lost() {
    let mut f = Fixture::new();
    f.push("alpha", "feature-one");
    let path = f.worktree_path("alpha", "feature-one");

    assert_eq!(f.press_char('D'), CONSUMED);
    assert_eq!(
        f.modal(),
        Modal::None,
        "a clean, pushed worktree needs no dialog"
    );
    assert!(!path.exists(), "the worktree directory should be gone");
}

#[test]
fn force_delete_still_raises_the_dialog_when_work_would_be_lost() {
    let mut f = Fixture::new();
    let path = f.worktree_path("alpha", "feature-one");

    assert_eq!(f.press_char('D'), CONSUMED);
    assert_eq!(
        f.modal(),
        Modal::Confirm,
        "force delete must not skip the guard"
    );
    assert!(
        path.exists(),
        "nothing may be destroyed before the override"
    );
}

#[test]
fn delete_with_confirmation_on_an_empty_list_does_not_open_the_modal() {
    let mut f = Fixture::new();

    // Remove the only worktree first, deliberately.
    f.delete_deliberately("feature-one");
    assert!(f.screen().contains("Worktrees (0/0)"));

    assert_eq!(f.press_char('d'), CONSUMED);
    assert_eq!(
        f.modal(),
        Modal::None,
        "with nothing selected there is nothing to confirm"
    );
}

#[test]
fn confirm_modal_quit_exits() {
    let mut f = Fixture::new();
    f.push("alpha", "feature-one");
    f.press_char('d');
    assert_eq!(f.press_char('q'), EXIT);
}

/// A guarded dialog is a typing surface, so 'q' types rather than quits — the
/// program must still be leavable, and nothing may be deleted on the way out.
#[test]
fn the_guarded_dialog_still_quits_on_ctrl_c() {
    let mut f = Fixture::new();
    f.press_char('d');
    assert_eq!(f.press_char('q'), CONSUMED);
    assert_eq!(f.press(ctrl('c')), EXIT);
    assert!(f.worktree_path("alpha", "feature-one").exists());
}

// ---------------------------------------------------------------------------
// PR-worktree modal
// ---------------------------------------------------------------------------

#[test]
fn pr_prompt_opens_in_insert_mode_and_escape_returns() {
    let mut f = Fixture::new();

    assert_eq!(f.press_char('p'), CONSUMED);
    assert_eq!(f.modal(), Modal::PrWorktree);
    assert_eq!(f.mode(), Mode::Insert);

    assert_eq!(f.press(key(KeyCode::Esc)), CONSUMED);
    f.assert_at_worktree_list();
}

#[test]
fn pr_auto_clone_prompt_opens_in_insert_mode_and_escape_returns() {
    let mut f = Fixture::new();

    assert_eq!(f.press_char('P'), CONSUMED);
    assert_eq!(f.modal(), Modal::PrWorktree);
    assert_eq!(f.mode(), Mode::Insert);

    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
}

#[test]
fn cancelling_the_pr_prompt_clears_the_typed_url() {
    let mut f = Fixture::new();

    f.press_char('p');
    f.type_str("https://github.com/acme/widget/pull/7");
    assert!(f.screen().contains("acme/widget"));

    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();

    f.press_char('p');
    assert_eq!(f.modal(), Modal::PrWorktree);
    assert!(
        !f.screen().contains("acme/widget"),
        "the PR prompt must reopen empty:\n{}",
        f.screen()
    );
}

#[test]
fn submitting_an_unparseable_pr_url_keeps_the_prompt_open() {
    let mut f = Fixture::new();

    f.press_char('p');
    // Not a GitHub PR URL: rejected by parsing, before any network call.
    f.type_str("not-a-pr-url");
    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);

    assert_eq!(
        f.modal(),
        Modal::PrWorktree,
        "a rejected URL must keep the prompt open so it can be corrected"
    );

    // The prompt is still usable: backspacing and escaping still work.
    f.press(key(KeyCode::Backspace));
    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
}

#[test]
fn pr_prompt_ctrl_c_exits() {
    let mut f = Fixture::new();
    f.press_char('p');
    assert_eq!(f.press(ctrl('c')), EXIT);
}

// ---------------------------------------------------------------------------
// PR flow behind a stubbed lookup
//
// These drive the steps that only exist once a PR has been fetched. None of them
// reaches `git clone`: cloning is the step *after* a destination is chosen, and
// every test stops at or before that point, so nothing here touches the network.
// ---------------------------------------------------------------------------

/// Types a PR URL into the prompt, submits it, and waits for the lookup.
///
/// The lookup is a job now: Enter only *starts* it, so every test that used to
/// see the next step appear on the same keystroke has to pump the result the way
/// the real loop does. That is the point of the issue — the keystroke returns
/// immediately, whatever GitHub is doing.
fn submit_pr(f: &mut Fixture, url: &str) -> String {
    f.type_str(url);
    let state = f.press(key(KeyCode::Enter));
    f.deliver_one();
    state
}

#[test]
fn a_submitted_pr_url_leaves_the_prompt_up_and_spinning() {
    let mut f = Fixture::new();
    f.stub_pr_branch("feature-from-pr", false);

    f.press_char('p');
    f.type_str("https://github.com/acme/alpha/pull/7");
    // Enter returns without the answer: nothing has been pumped yet.
    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);

    assert_eq!(f.modal(), Modal::PrWorktree, "the prompt must stay up");
    let screen = f.screen();
    assert!(
        screen.contains("Looking up acme/alpha #7"),
        "the wait must name what it is waiting for:\n{screen}"
    );
    // A frozen frame is exactly what this issue is about, so the indicator has
    // to move: two consecutive frames must not be identical.
    assert_ne!(f.screen(), screen, "the spinner should be turning");

    // And the answer still lands when it arrives.
    f.deliver_one();
    assert_eq!(f.modal(), Modal::CreateWorktree);
}

#[test]
fn escaping_a_pending_lookup_abandons_it() {
    let mut f = Fixture::new();
    f.stub_pr_branch("feature-from-pr", false);

    f.press_char('p');
    f.type_str("https://github.com/acme/alpha/pull/7");
    f.press(key(KeyCode::Enter));

    assert_eq!(f.press(key(KeyCode::Esc)), CONSUMED);
    f.assert_at_worktree_list();

    // Two things can have happened to the job, and the flow must survive both:
    // it never started, so no result is ever sent; or it was already running,
    // and its answer arrives to be dropped on the floor rather than reopening a
    // flow the user walked away from.
    f.deliver_any();
    f.assert_at_worktree_list();
}

#[test]
fn a_second_enter_during_a_lookup_does_not_start_a_second_one() {
    let mut f = Fixture::new();
    f.stub_pr_branch("feature-from-pr", false);

    f.press_char('p');
    f.type_str("https://github.com/acme/alpha/pull/7");
    f.press(key(KeyCode::Enter));
    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);
    // Typing is refused too: the URL on screen has to keep describing the lookup
    // that is actually out.
    f.type_str("xyz");
    assert!(!f.screen().contains("pull/7xyz"));

    f.deliver_one();
    assert_eq!(f.modal(), Modal::CreateWorktree);
    assert!(
        f.results.try_recv().is_err(),
        "one submission must produce exactly one job"
    );
}

#[test]
fn keys_pressed_during_a_lookup_do_not_disturb_it() {
    let mut f = Fixture::new();
    f.stub_pr_branch("feature-from-pr", false);

    f.press_char('p');
    f.type_str("https://github.com/acme/alpha/pull/7");
    f.press(key(KeyCode::Enter));

    // Everything but Escape and Ctrl+C is swallowed by the waiting prompt: the
    // field is a text field, so these are characters rather than commands, and
    // none of them may reach the list underneath.
    for k in ['?', 'q', 'd', 'j'] {
        assert_eq!(f.press_char(k), CONSUMED);
    }
    assert_eq!(f.modal(), Modal::PrWorktree);

    f.deliver_one();
    assert_eq!(f.modal(), Modal::CreateWorktree);
}

#[test]
fn a_failing_pr_lookup_keeps_the_prompt_open_with_the_error() {
    let mut f = Fixture::new();
    f.stub_pr_failure("GitHub auth failed");

    f.press_char('p');
    assert_eq!(
        submit_pr(&mut f, "https://github.com/acme/alpha/pull/7"),
        CONSUMED
    );

    assert_eq!(f.modal(), Modal::PrWorktree, "the prompt must stay open");
    assert!(
        f.screen().contains("GitHub auth failed"),
        "the failure should be shown in the prompt:\n{}",
        f.screen()
    );
}

#[test]
fn a_pr_on_a_known_repo_opens_the_branch_prompt_prefilled() {
    let mut f = Fixture::new();
    f.stub_pr_branch("feature-from-pr", false);

    f.press_char('p');
    assert_eq!(
        submit_pr(&mut f, "https://github.com/acme/alpha/pull/7"),
        CONSUMED
    );

    assert_eq!(f.modal(), Modal::CreateWorktree);
    assert!(
        f.screen().contains("feature-from-pr"),
        "the PR branch should be prefilled:\n{}",
        f.screen()
    );

    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
}

#[test]
fn a_pr_whose_branch_is_already_checked_out_just_selects_that_worktree() {
    let mut f = Fixture::new();
    // `feature-one` is the worktree the fixture creates.
    f.stub_pr_branch("feature-one", false);
    let expected = std::fs::canonicalize(f.worktree_path("alpha", "feature-one"))
        .expect("worktree should exist");

    f.press_char('p');
    submit_pr(&mut f, "https://github.com/acme/alpha/pull/7");

    f.assert_at_worktree_list();
    assert_eq!(f.press(key(KeyCode::Enter)), EXIT);
    let selected = f.app.selected_path.clone().expect("a path was selected");
    assert_eq!(
        std::fs::canonicalize(selected.trim_end_matches('/')).unwrap(),
        expected
    );
}

#[test]
fn a_merged_pr_on_an_existing_worktree_reports_why_nothing_was_created() {
    let mut f = Fixture::new();
    f.stub_pr_branch("feature-one", true);

    f.press_char('p');
    submit_pr(&mut f, "https://github.com/acme/alpha/pull/7");

    assert_eq!(f.modal(), Modal::None);
    assert!(
        f.screen().contains("merged"),
        "a merged PR should say so rather than fail silently:\n{}",
        f.screen()
    );
}

#[test]
fn a_pr_on_an_unknown_repo_offers_to_clone_it() {
    let mut f = Fixture::new();
    f.stub_pr_branch("feature-from-pr", false);

    f.press_char('p');
    assert_eq!(
        submit_pr(&mut f, "https://github.com/acme/widget/pull/7"),
        CONSUMED
    );

    assert_eq!(f.modal(), Modal::Confirm);
    let screen = f.screen();
    assert!(
        screen.contains("Clone Repository") && screen.contains("git@github.com:acme/widget.git"),
        "the clone prompt should name the remote it would clone:\n{}",
        screen
    );

    // Declining must leave the filesystem alone.
    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
    assert!(
        !f.repo_path("widget").exists(),
        "cancelling the clone prompt must not clone anything"
    );
}

#[test]
fn confirming_a_clone_with_several_repos_dirs_asks_which_one() {
    let mut f = Fixture::with_two_repos_dirs();
    f.stub_pr_branch("feature-from-pr", false);

    f.press_char('p');
    submit_pr(&mut f, "https://github.com/acme/widget/pull/7");
    assert_eq!(f.modal(), Modal::Confirm);

    assert_eq!(f.press(key(KeyCode::Enter)), CONSUMED);
    assert_eq!(
        f.modal(),
        Modal::SelectReposDir,
        "with more than one repos dir the flow must ask where to clone"
    );
    assert!(
        f.screen()
            .contains(&f.repos_dir.path().display().to_string()),
        "the configured repos dirs should be listed:\n{}",
        f.screen()
    );

    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
    assert!(
        !f.repo_path("widget").exists(),
        "cancelling the picker must not clone anything"
    );
}

#[test]
fn auto_clone_skips_the_confirmation_and_goes_straight_to_the_picker() {
    let mut f = Fixture::with_two_repos_dirs();
    f.stub_pr_branch("feature-from-pr", false);

    // 'P' is the auto-clone variant: it must not ask for confirmation first.
    f.press_char('P');
    assert_eq!(
        submit_pr(&mut f, "https://github.com/acme/widget/pull/7"),
        CONSUMED
    );

    assert_eq!(f.modal(), Modal::SelectReposDir);

    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
    assert!(!f.repo_path("widget").exists());
}

#[test]
fn help_over_the_clone_directory_picker_returns_to_the_picker() {
    let mut f = Fixture::with_two_repos_dirs();
    f.stub_pr_branch("feature-from-pr", false);

    f.press_char('P');
    submit_pr(&mut f, "https://github.com/acme/widget/pull/7");
    assert_eq!(f.modal(), Modal::SelectReposDir);

    assert_eq!(f.press_char('?'), CONSUMED);
    assert_eq!(f.modal(), Modal::Help);
    f.press(key(KeyCode::Esc));
    assert_eq!(f.modal(), Modal::SelectReposDir);

    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
}

// ---------------------------------------------------------------------------
// Multiple repos dirs
// ---------------------------------------------------------------------------

#[test]
fn multiple_repos_dirs_still_start_on_the_worktree_list() {
    // The configuration that enables the repos-dir picker; the picker itself is
    // driven in the PR-flow tests above, through the stubbed lookup.
    let mut f = Fixture::with_two_repos_dirs();
    f.assert_at_worktree_list();
    assert!(f.screen().contains("Worktrees (1/1)"));

    f.press_char('n');
    assert_eq!(f.modal(), Modal::Repositories);
    f.press(key(KeyCode::Esc));
    f.assert_at_worktree_list();
}

// ---------------------------------------------------------------------------
// Escape at every reachable depth
// ---------------------------------------------------------------------------

#[test]
fn escape_unwinds_every_reachable_depth_back_to_the_worktree_list() {
    // Each entry is the key sequence that opens a modal; Escape must always land
    // back on the worktree list in Normal mode with nothing left behind.
    let openers: Vec<(&str, Vec<KeyEvent>)> = vec![
        ("help", vec![ch('?')]),
        ("repositories", vec![ch('n')]),
        ("help over repositories", vec![ch('n'), ch('?')]),
        ("create worktree", vec![ch('n'), key(KeyCode::Enter)]),
        ("pr prompt", vec![ch('p')]),
        ("pr prompt (auto clone)", vec![ch('P')]),
        ("confirm delete", vec![ch('d')]),
    ];

    for (name, keys) in openers {
        let mut f = Fixture::new();
        for k in keys {
            f.press(k);
        }
        assert_ne!(f.modal(), Modal::None, "{name}: expected a modal to open");

        // At most two Escapes: help over a popup is the deepest reachable stack.
        f.press(key(KeyCode::Esc));
        if f.modal() != Modal::None {
            f.press(key(KeyCode::Esc));
        }

        assert_eq!(
            f.modal(),
            Modal::None,
            "{name}: escape should unwind to the worktree list"
        );
        assert_eq!(
            f.mode(),
            Mode::Normal,
            "{name}: mode should be Normal again"
        );
        assert!(
            f.worktree_path("alpha", "feature-one").exists(),
            "{name}: cancelling must not touch the filesystem"
        );
    }
}

// ---------------------------------------------------------------------------
// Streaming discovery (shanti-hml.3)
// ---------------------------------------------------------------------------

/// The whole point of the issue: there is a UI before there is a repository.
#[test]
fn the_first_frame_is_drawn_before_any_repository_is_found() {
    let mut f = Fixture::streaming();

    assert!(f.app.is_scanning(), "the scan should still be running");
    let screen = f.screen();
    assert!(screen.contains("Worktrees (0/0)"), "{screen}");
    assert!(
        screen.contains("scanning"),
        "an empty list must say why it is empty: {screen}"
    );
    f.assert_at_worktree_list();
}

/// The spinner counts what has arrived, and stops when the last root lands.
#[test]
fn the_spinner_reports_progress_and_then_goes_away() {
    let mut f = Fixture::streaming();

    f.deliver_one();
    assert!(f.app.is_scanning(), "one root of two is not the whole scan");
    let screen = f.screen();
    assert!(
        screen.contains("scanning\u{2026} 2 repos"),
        "the spinner should count the repositories found so far: {screen}"
    );

    f.deliver_one();
    assert!(!f.app.is_scanning());
    assert!(
        !f.screen().contains("scanning"),
        "the spinner outlived the scan"
    );
}

/// The requirement most easily dropped: the list is navigable while the scan is
/// still running, over whatever has arrived so far.
#[test]
fn the_list_can_be_navigated_during_a_scan() {
    let mut f = Fixture::streaming();
    f.deliver_one();
    assert!(f.app.is_scanning(), "this must be tested mid-scan");

    assert!(
        f.screen().contains("Worktrees (1/2)"),
        "the first root's rows should already be selectable"
    );
    assert_eq!(f.press_char('j'), CONSUMED);
    assert!(f.screen().contains("Worktrees (2/2)"), "j did not move");
    assert_eq!(f.press_char('k'), CONSUMED);
    assert!(f.screen().contains("Worktrees (1/2)"), "k did not move");

    // And selecting one mid-scan hands back its path, as it would at rest.
    assert_eq!(f.press(key(KeyCode::Enter)), EXIT);
    let selected = f.app.selected_path.clone().expect("a path was selected");
    assert!(
        selected.ends_with("feature-alpha"),
        "the row under the cursor was not the one handed back: {selected}"
    );
}

/// Filtering works mid-scan, and a batch landing underneath must not throw the
/// filter away — the regression `set_spaces` exists to prevent.
#[test]
fn a_filter_typed_during_a_scan_survives_the_next_batch() {
    let mut f = Fixture::streaming();
    f.deliver_one();

    f.press_char('i');
    f.type_str("feature-alpha");
    let filtered = f.screen();
    assert!(
        filtered.contains("Worktrees (1/1)"),
        "the filter should narrow the arrived rows: {filtered}"
    );

    f.deliver_one();
    assert!(!f.app.is_scanning(), "both roots should have landed");

    let after = f.screen();
    assert!(
        after.contains("Worktrees (1/1)"),
        "the filter was lost when the second batch arrived: {after}"
    );
    assert!(
        after.contains("feature-alpha"),
        "the typed text is gone from the filter line: {after}"
    );
    assert_eq!(f.mode(), Mode::Insert, "focus left the filter");

    // Clearing it brings every row of both roots back.
    for _ in 0.."feature-alpha".len() {
        f.press(key(KeyCode::Backspace));
    }
    assert!(
        f.screen().contains("Worktrees (1/4)"),
        "every row of both roots should be back"
    );
}

// ---------------------------------------------------------------------------
// The keybinding footer (shanti-hq6.3)
// ---------------------------------------------------------------------------

/// Renders into a terminal of a chosen size, which the footer tests need: the
/// point of a footer is what it gives up when the frame is narrow, and
/// [`Fixture::screen`] is deliberately wide enough to hide that.
fn screen_at(f: &mut Fixture, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal init");
    terminal
        .draw(|frame| f.app.draw(frame))
        .expect("draw failed");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The bottom row, where the status zone and the footer share one border.
fn bottom_row(screen: &str) -> String {
    screen.lines().last().unwrap_or_default().to_owned()
}

#[test]
fn the_space_list_names_its_keys_without_opening_help() {
    let mut f = Fixture::new();
    let bottom = bottom_row(&f.screen());

    for hint in [
        "[j/k] move",
        "[n] new",
        "[d] delete",
        "[Enter] path",
        "[?] help",
    ] {
        assert!(
            bottom.contains(hint),
            "the footer should carry {hint}:\n{bottom}"
        );
    }
    // The status zone keeps its own end of the same border.
    assert!(
        bottom.contains(" NORMAL "),
        "the mode indicator and the footer must coexist:\n{bottom}"
    );
}

#[test]
fn the_footer_follows_the_input_mode() {
    let mut f = Fixture::new();
    f.press_char('i');

    let bottom = bottom_row(&f.screen());
    assert!(
        bottom.contains("[Esc] normal"),
        "filter mode must advertise the way back out:\n{bottom}"
    );
    assert!(
        !bottom.contains("[n] new"),
        "a key that types a character while filtering must not be offered as a command:\n{bottom}"
    );
}

/// What a 40-column terminal gets: whole entries dropped from the least
/// important end, never a hint cut in half or wrapped onto another row.
#[test]
fn the_footer_sheds_entries_on_a_narrow_terminal() {
    let mut f = Fixture::new();
    let bottom = bottom_row(&screen_at(&mut f, 40, 10));

    assert!(bottom.contains("[?] help"), "{bottom}");
    assert!(bottom.contains("[q] quit"), "{bottom}");
    assert!(
        !bottom.contains("path") && !bottom.contains("delete"),
        "the heavier entries should have been dropped whole:\n{bottom}"
    );
    assert_eq!(
        bottom.chars().count(),
        40,
        "the footer must stay on its own row:\n{bottom}"
    );
}

/// The picker used to hide its footer while a filter was being typed — the one
/// moment its user is most likely to have forgotten the way out.
#[test]
fn the_repository_picker_keeps_its_footer_in_filter_mode() {
    let mut f = Fixture::new();
    f.press_char('n');
    f.press_char('i');

    let screen = f.screen();
    assert!(
        screen.contains("[Esc] list"),
        "the picker's footer should follow it into filter mode:\n{screen}"
    );
}
