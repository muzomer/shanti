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
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use shanti::app::App;
use shanti::cli::Args;
use shanti::github::PrInfo;
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
    repos_dir: TempDir,
    worktrees_dir: TempDir,
    /// Second repos dir, only populated by [`Fixture::with_two_repos_dirs`].
    _extra_repos_dir: Option<TempDir>,
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

        // The default lookup fails loudly: a PR test that forgot to stub sees an
        // error message rather than silently reaching out to github.com.
        let app = App::with_args(
            args,
            Arc::new(|_| Err(color_eyre::eyre::eyre!("no PR lookup was stubbed"))),
        );

        Self {
            app,
            repos_dir,
            worktrees_dir,
            _extra_repos_dir: extra,
        }
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
    assert_eq!(f.press(key(KeyCode::F(1))), NOT_CONSUMED);
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

#[test]
fn confirming_the_delete_removes_the_worktree_and_returns_to_the_list() {
    let mut f = Fixture::new();
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

#[test]
fn force_delete_skips_the_confirm_modal() {
    let mut f = Fixture::new();
    let path = f.worktree_path("alpha", "feature-one");

    assert_eq!(f.press_char('D'), CONSUMED);
    assert_eq!(f.modal(), Modal::None, "force delete must not open a modal");
    assert!(!path.exists(), "the worktree directory should be gone");
}

#[test]
fn delete_with_confirmation_on_an_empty_list_does_not_open_the_modal() {
    let mut f = Fixture::new();

    // Remove the only worktree first.
    f.press_char('D');
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
    f.press_char('d');
    assert_eq!(f.press_char('q'), EXIT);
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

/// Types a PR URL into the prompt and submits it.
fn submit_pr(f: &mut Fixture, url: &str) -> String {
    f.type_str(url);
    f.press(key(KeyCode::Enter))
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
