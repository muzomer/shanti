//! Rendering tests (`shanti-b03.2`).
//!
//! Alignment, truncation, status glyphs and popup sizing used to be verified by
//! looking at them, so a regression in any of them shipped silently. These tests
//! render real frames into `ratatui::backend::TestBackend` — an in-memory buffer,
//! no terminal, so they run in CI — and assert on what came out.
//!
//! ## What is asserted, and why it is almost never a whole frame
//!
//! A test that snapshots a frame and compares it byte for byte fails on every
//! legitimate change, and the only cheap way to make it pass again is to
//! regenerate it without reading it — which is not a test, it is a diff someone
//! has to approve twice. So nearly everything below asserts the *property* the
//! design guideline is about:
//!
//! * the separator falls in one column on every row (the spine),
//! * a clipped repository name loses its head and a clipped space name its tail,
//! * the row for a repository's own default space names no space at all,
//! * the gate row of a destructive dialog survives at the size floor,
//! * the base pane below the floor says how big the terminal has to be.
//!
//! The one whole-frame comparison is [`below_the_floor_the_whole_frame_is_the_message`],
//! where the whole frame genuinely *is* the contract: three centred lines and
//! nothing else is the entire specified output at that size. That frame is drawn
//! by `draw_too_small`, which draws no chrome at all, so an always-visible
//! keybinding footer must not appear there either — if one does, that test is
//! the place to decide whether it should.
//!
//! Nothing here asserts that a footer is *absent*, and the one assertion about
//! the bottom of the frame ([`the_bottom_row_always_carries_guidance`]) is
//! deliberately weak enough to survive one arriving.
//!
//! ## Fixtures are real repositories
//!
//! `App` fills its list from a background scan of the disk, and neither the
//! components module nor `SpaceEntry` is public, so a test cannot hand it a
//! synthetic row. Every status below is therefore a repository genuinely put
//! into that state — pushed, reset, diverged, conflicted — which is also why
//! these tests catch a status *reading* regression and not only a drawing one.
//!
//! `App::with_args` takes the resolved configuration directly, so nothing here
//! touches argv, the environment or the config file, and the tests run in
//! parallel.

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
use shanti::jobs::Worker;
use shanti::vcs::jj::JjCli;
use tempfile::{tempdir, TempDir};

/// The size floor the interface declares (`components::MIN_WIDTH`/`MIN_HEIGHT`).
///
/// Not importable — `components` is private — so it is restated here. The message
/// the base pane prints below the floor quotes the same numbers, and
/// [`below_the_floor_the_whole_frame_is_the_message`] is what keeps the copy honest.
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;

/// Comfortably above the floor: nothing is clipped, so a failure at this size is
/// a real one rather than a small terminal.
const ROOMY: (u16, u16) = (140, 50);

/// The narrowest supported frame, where every layout is at its floor.
const FLOOR: (u16, u16) = (MIN_WIDTH, MIN_HEIGHT);

/// Far past any percentage constraint's comfort zone.
const HUGE: (u16, u16) = (400, 120);

/// The separator between the repository column and the space column.
const SEPARATOR: &str = " / ";

/// Everything a row draws left of the repository name: two status cells, a gap,
/// the three-cell backend tag, a gap. Fixed by the layout, so a test can slice a
/// row at it.
const PREFIX_WIDTH: usize = 2 + 1 + 3 + 1;

// ---------------------------------------------------------------------------
// The states a git space can be in
// ---------------------------------------------------------------------------

/// How a fixture worktree relates to its upstream. One variant per `RemoteState`
/// the git backend can report, and each produces a different glyph.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Remote {
    /// No upstream was ever configured: `⬆`.
    NeverPushed,
    /// Pushed and level: `✔`.
    InSync,
    /// Local commits the upstream lacks: `↑`.
    Ahead,
    /// Upstream commits we lack: `↓`.
    Behind,
    /// Both: `↕`.
    Diverged,
    /// The upstream was configured and has since been deleted: `✘`.
    Gone,
}

/// One fixture worktree: which repository, what it is called, and what state to
/// leave it in.
struct Wt {
    repo: &'static str,
    branch: &'static str,
    remote: Remote,
    /// Leave an uncommitted file behind, so the local slot reads `*`.
    dirty: bool,
}

impl Wt {
    fn new(repo: &'static str, branch: &'static str, remote: Remote) -> Self {
        Self {
            repo,
            branch,
            remote,
            dirty: false,
        }
    }

    fn dirty(mut self) -> Self {
        self.dirty = true;
        self
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// An `App` pointed at temporary directories, with its startup scan drained.
struct Fixture {
    app: App,
    /// Held for the fixture's life: the worker sends on the other end, and
    /// dropping it would make later results vanish rather than arrive.
    _results: Receiver<AppEvent>,
    _repos_dir: TempDir,
    _worktrees_dir: TempDir,
    /// The bare repositories standing in for `origin`, one per worktree that
    /// needed one. Kept alive; dropping them would delete the remotes.
    _remotes: Vec<TempDir>,
}

impl Fixture {
    /// Builds one repository per distinct `repo` name, then each worktree in the
    /// state it asked for.
    fn with_worktrees(worktrees: &[Wt]) -> Self {
        let repos_dir = tempdir().expect("could not create repos dir");
        let worktrees_dir = tempdir().expect("could not create worktrees dir");
        let mut remotes = Vec::new();

        let mut built: Vec<&str> = Vec::new();
        for wt in worktrees {
            if !built.contains(&wt.repo) {
                init_repo(repos_dir.path(), wt.repo);
                built.push(wt.repo);
            }
        }
        for wt in worktrees {
            let repo_path = repos_dir.path().join(wt.repo);
            let target = worktrees_dir.path().join(wt.repo).join(wt.branch);
            git(
                &repo_path,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    wt.branch,
                    target.to_str().expect("utf-8 path"),
                ],
            );
            if let Some(remote) = put_in_state(&repo_path, &target, wt) {
                remotes.push(remote);
            }
        }

        let (app, results) = boot(&Args::for_dirs(
            worktrees_dir.path().display().to_string(),
            vec![repos_dir.path().display().to_string()],
        ));

        Self {
            app,
            _results: results,
            _repos_dir: repos_dir,
            _worktrees_dir: worktrees_dir,
            _remotes: remotes,
        }
    }

    /// A jj repository holding three workspaces, one per local state the jj
    /// backend can report: the default workspace (an empty working copy), a
    /// workspace with work in it, and one holding a genuine merge conflict.
    ///
    /// `None` when there is no jj on this machine, so a contributor without one
    /// still gets a green run. `SHANTI_REQUIRE_JJ` turns the skip into a failure,
    /// which is how CI keeps an all-skipped run from looking like a pass —
    /// the same bargain `tests/jj_backend.rs` strikes.
    fn jj_workspaces() -> Option<Self> {
        if !JjCli::is_available() {
            assert!(
                std::env::var_os("SHANTI_REQUIRE_JJ").is_none(),
                "SHANTI_REQUIRE_JJ is set but no jj binary was found; \
                 install jj or unset SHANTI_REQUIRE_JJ"
            );
            eprintln!("skipping: no jj binary on this machine");
            return None;
        }

        let repos_dir = tempdir().expect("could not create repos dir");
        let worktrees_dir = tempdir().expect("could not create worktrees dir");
        let root = repos_dir.path().join("lotus");
        std::fs::create_dir_all(&root).expect("could not create the repository directory");

        // `JjCli` is used only to find the binary shanti itself would use, so a
        // `SHANTI_JJ_BIN` pointing somewhere unusual moves the fixture with it.
        // The setup commands are spawned directly: they need an isolated
        // identity and subcommands the production adapter has no business
        // offering, and `--repository` cannot name a repository jj has not
        // created yet.
        let cli = JjCli::discover(&root).expect("jj is available but could not be discovered");
        let program = cli.program().to_owned();
        let jj = |dir: &Path, args: &[&str]| jj_in(&program, dir, args);

        jj(&root, &["git", "init"]);
        std::fs::write(root.join("f.txt"), "base\n").expect("could not write a file");
        jj(&root, &["describe", "-m", "first"]);
        jj(&root, &["new"]);

        // A git repository beside it, so the list mixes both backends the way a
        // real repos dir does and every row has to say which one owns it.
        init_repo(repos_dir.path(), "garden");
        let garden = worktrees_dir.path().join("garden").join("bed");
        git(
            &repos_dir.path().join("garden"),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "bed",
                garden.to_str().expect("utf-8 path"),
            ],
        );

        let spaces = worktrees_dir.path().join("lotus");
        std::fs::create_dir_all(&spaces).expect("could not create the spaces dir");
        for name in ["conflicted", "working"] {
            let dest = spaces.join(name);
            jj(
                &root,
                &[
                    "workspace",
                    "add",
                    "--name",
                    name,
                    dest.to_str().expect("utf-8 path"),
                ],
            );
        }

        // `working` gets an ordinary edit, so its working copy is neither empty
        // nor in trouble: the blank local slot.
        std::fs::write(spaces.join("working").join("f.txt"), "in progress\n")
            .expect("could not edit the workspace");
        // shanti's reads pass `--ignore-working-copy`, so an edit jj has not
        // recorded yet is invisible to them *by design*. One jj command in the
        // workspace snapshots it, which is what a user's next jj command would
        // do too.
        jj(&spaces.join("working"), &["status"]);

        // `conflicted` gets a real two-sided conflict, built the way one
        // actually arises: two siblings edit the same line, then both are made
        // parents of the working copy.
        let dest = spaces.join("conflicted");
        let head = || {
            jj(
                &dest,
                &["log", "--no-graph", "-r", "@", "-T", "change_id.short()"],
            )
            .trim()
            .to_owned()
        };
        std::fs::write(dest.join("f.txt"), "theirs\n").expect("could not write a file");
        jj(&dest, &["describe", "-m", "theirs"]);
        let theirs = head();
        jj(&dest, &["new", "@-"]);
        std::fs::write(dest.join("f.txt"), "ours\n").expect("could not write a file");
        jj(&dest, &["describe", "-m", "ours"]);
        let ours = head();
        jj(&dest, &["new", &theirs, &ours]);

        let (app, results) = boot(&Args::for_dirs(
            worktrees_dir.path().display().to_string(),
            vec![repos_dir.path().display().to_string()],
        ));
        Some(Self {
            app,
            _results: results,
            _repos_dir: repos_dir,
            _worktrees_dir: worktrees_dir,
            _remotes: Vec::new(),
        })
    }

    /// Repositories with no spaces at all.
    fn with_bare_repos(names: &[&str]) -> Self {
        let repos_dir = tempdir().expect("could not create repos dir");
        let worktrees_dir = tempdir().expect("could not create worktrees dir");
        for name in names {
            init_repo(repos_dir.path(), name);
        }
        let (app, results) = boot(&Args::for_dirs(
            worktrees_dir.path().display().to_string(),
            vec![repos_dir.path().display().to_string()],
        ));
        Self {
            app,
            _results: results,
            _repos_dir: repos_dir,
            _worktrees_dir: worktrees_dir,
            _remotes: Vec::new(),
        }
    }

    // -- driving -----------------------------------------------------------

    fn press(&mut self, key: KeyEvent) {
        self.app.handle_key(key);
    }

    fn press_char(&mut self, c: char) {
        self.press(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    fn type_str(&mut self, text: &str) {
        for c in text.chars() {
            self.press_char(c);
        }
    }

    // -- observing ---------------------------------------------------------

    /// The frame at `size`, one `String` per row, trailing spaces intact so a
    /// column index into the string is a column index on screen.
    fn frame(&mut self, size: (u16, u16)) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).expect("terminal init");
        terminal
            .draw(|frame| self.app.draw(frame))
            .expect("drawing must not fail at any terminal size");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn screen(&mut self, size: (u16, u16)) -> String {
        self.frame(size).join("\n")
    }

    /// The list rows on screen, with the border cells stripped, so column 0 of
    /// the returned string is column 0 of the row.
    fn rows(&mut self, size: (u16, u16)) -> Vec<String> {
        self.frame(size)
            .into_iter()
            .filter_map(|line| {
                let inner = line.strip_prefix('│')?;
                let inner = inner.strip_suffix('│').unwrap_or(inner);
                is_row(inner).then(|| inner.to_owned())
            })
            .collect()
    }

    /// The one row whose text contains `needle`.
    fn row(&mut self, size: (u16, u16), needle: &str) -> String {
        let rows = self.rows(size);
        let mut found = rows.iter().filter(|row| row.contains(needle));
        let row = found
            .next()
            .unwrap_or_else(|| {
                panic!(
                    "no row mentions {needle:?}; rows were:\n{}",
                    rows.join("\n")
                )
            })
            .clone();
        assert!(
            found.next().is_none(),
            "more than one row mentions {needle:?}"
        );
        row
    }

    /// The one row that names no space — a repository's own default space.
    fn default_row(&mut self, size: (u16, u16)) -> String {
        let rows = self.rows(size);
        let mut plain = rows.iter().filter(|row| spine(row).is_none());
        let row = plain
            .next()
            .unwrap_or_else(|| {
                panic!(
                    "no row names a repository alone; rows were:\n{}",
                    rows.join("\n")
                )
            })
            .clone();
        assert!(plain.next().is_none(), "more than one row names no space");
        row
    }
}

/// Whether an inner line is a space row rather than chrome.
///
/// The backend tag column is fixed by the layout — two status cells, a space,
/// then a three-cell tag — so a line carrying a known tag there is a row, and no
/// border, title or empty-state notice ever is.
fn is_row(inner: &str) -> bool {
    let cells = cells(inner);
    if cells.len() < 7 {
        return false;
    }
    let tag: String = cells[3..6].concat();
    tag == "git" || tag == "jj "
}

/// The line split into screen cells, so an index is a column even where a cell
/// holds a multi-byte glyph.
///
/// `TestBackend` stores one symbol per cell and [`Fixture::frame`] concatenated
/// them, so splitting on `char` boundaries is exact here.
fn cells(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in line.char_indices().skip(1) {
        out.push(&line[start..i]);
        start = i;
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}

/// The two status glyphs a row leads with.
fn glyphs(row: &str) -> String {
    cells(row)[..2].concat()
}

/// The local half of the status indicator — the second slot.
fn local_glyph(row: &str) -> String {
    cells(row)[1].to_owned()
}

/// The column the ` / ` separator starts at, or `None` on a row that has none.
fn spine(row: &str) -> Option<usize> {
    let cells = cells(row);
    (0..cells.len().saturating_sub(3)).find(|&i| {
        cells[i..i + 3].concat() == SEPARATOR && cells[i + 3..].iter().any(|cell| *cell != " ")
    })
}

// ---------------------------------------------------------------------------
// Building repositories
// ---------------------------------------------------------------------------

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

/// Runs a jj setup command in `dir` and returns its stdout.
///
/// Deliberately not routed through `JjCli`: fixtures need setup subcommands and
/// an isolated identity that the production adapter has no business offering,
/// and `jj git init` cannot be told `--repository` for a repository that does
/// not exist yet.
fn jj_in(program: &Path, dir: &Path, args: &[&str]) -> String {
    let output = Command::new(program)
        .arg("--no-pager")
        .arg("--color=never")
        .args(args)
        .current_dir(dir)
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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn init_repo(repos_dir: &Path, name: &str) {
    let path = repos_dir.join(name);
    std::fs::create_dir_all(&path).expect("could not create repo dir");
    git(&path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README.md"), "fixture\n").expect("could not write README");
    git(&path, &["add", "README.md"]);
    git(&path, &["commit", "-q", "-m", "init"]);
}

/// Commits a new file in `dir`, so the branch checked out there gains a commit.
fn commit(dir: &Path, marker: &str) {
    std::fs::write(dir.join(format!("{marker}.txt")), format!("{marker}\n"))
        .expect("could not write a file");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", marker]);
}

/// Puts the worktree at `target` into the state `wt` asks for, returning the bare
/// repository standing in for its `origin` when one was needed.
///
/// Every upstream state is produced with real git plumbing rather than by writing
/// config: "ahead" is a commit that was not pushed, "behind" is a push followed by
/// a reset, "gone" is a remote branch that was deleted afterwards. A fabricated
/// config entry would test the fixture, not the backend.
fn put_in_state(repo_path: &Path, target: &Path, wt: &Wt) -> Option<TempDir> {
    // One remote per worktree, named after its branch: several worktrees of one
    // repository each need their own upstream state, and sharing an `origin`
    // would make them push over each other.
    let origin = format!("origin-{}", wt.branch);
    let remote = if wt.remote == Remote::NeverPushed {
        None
    } else {
        let remote = tempdir().expect("could not create remote dir");
        git(remote.path(), &["init", "--bare", "-q"]);
        git(
            repo_path,
            &[
                "remote",
                "add",
                &origin,
                remote.path().to_str().expect("utf-8 path"),
            ],
        );
        git(target, &["push", "-q", "-u", &origin, wt.branch]);
        Some(remote)
    };

    match wt.remote {
        Remote::NeverPushed | Remote::InSync => {}
        Remote::Ahead => commit(target, "local"),
        Remote::Behind => {
            commit(target, "shared");
            git(target, &["push", "-q"]);
            git(target, &["reset", "--hard", "-q", "HEAD~1"]);
        }
        Remote::Diverged => {
            commit(target, "shared");
            git(target, &["push", "-q"]);
            git(target, &["reset", "--hard", "-q", "HEAD~1"]);
            commit(target, "mine");
        }
        // Deleting the remote branch also drops the local tracking ref, which is
        // exactly the state the backend reports as `Gone`.
        Remote::Gone => git(target, &["push", "-q", &origin, &format!(":{}", wt.branch)]),
    }

    if wt.dirty {
        std::fs::write(target.join("scratch.txt"), "unsaved\n").expect("could not dirty the tree");
    }

    remote
}

// ---------------------------------------------------------------------------
// Booting
// ---------------------------------------------------------------------------

/// Builds an app and runs its startup scan to completion, the way the real loop
/// does: one job result in, one redraw.
fn boot(args: &Args) -> (App, Receiver<AppEvent>) {
    let (mut app, results) = booting(args);
    while app.is_scanning() {
        match results.recv_timeout(Duration::from_secs(30)) {
            Ok(AppEvent::Job(result)) => app.handle_job(result),
            Ok(other) => panic!("expected a job result, got {other:?}"),
            Err(error) => panic!("the startup scan never finished: {error}"),
        }
    }
    (app, results)
}

/// The same app with its scan still in flight — the state the user sees first.
fn booting(args: &Args) -> (App, Receiver<AppEvent>) {
    let mut app = App::with_args(
        args.clone(),
        Arc::new(|_| Err(color_eyre::eyre::eyre!("no PR lookup was stubbed"))),
    );
    let (sender, results) = mpsc::channel();
    app.attach_worker(Worker::with_threads(sender, 1));
    (app, results)
}

// ---------------------------------------------------------------------------
// Status glyphs
// ---------------------------------------------------------------------------

/// Every remote state the git backend can report, each paired with a clean and
/// with a dirty working tree, drawn as its own two-glyph pair.
///
/// A property test, not a frame snapshot: what matters is that no two states
/// collapse onto the same pair, and that each pair is the one `SpaceStatus`
/// documents. Where in the frame the row happens to sit is not the contract.
#[test]
fn every_git_status_pair_gets_its_own_two_glyphs() {
    let mut f = Fixture::with_worktrees(&[
        Wt::new("never", "wt-never", Remote::NeverPushed),
        Wt::new("sync", "wt-sync", Remote::InSync),
        Wt::new("ahead", "wt-ahead", Remote::Ahead),
        Wt::new("behind", "wt-behind", Remote::Behind),
        Wt::new("diverged", "wt-diverged", Remote::Diverged),
        Wt::new("gone", "wt-gone", Remote::Gone),
        Wt::new("dirtysync", "wt-dirtysync", Remote::InSync).dirty(),
        Wt::new("dirtynever", "wt-dirtynever", Remote::NeverPushed).dirty(),
    ]);

    // Remote glyph, then local: `·` for unknown never appears here, because a
    // space the scan reached is always probed. See the report.
    let expected = [
        ("wt-never", "⬆ "),
        ("wt-sync", "✔ "),
        ("wt-ahead", "↑ "),
        ("wt-behind", "↓ "),
        ("wt-diverged", "↕ "),
        ("wt-gone", "✘ "),
        ("wt-dirtysync", "✔*"),
        ("wt-dirtynever", "⬆*"),
    ];
    for (name, pair) in expected {
        let row = f.row(ROOMY, name);
        assert_eq!(glyphs(&row), pair, "wrong status glyphs on:\n{row}");
    }

    // The point of two slots: no two states share a pair.
    let mut pairs: Vec<String> = f.rows(ROOMY).iter().map(|row| glyphs(row)).collect();
    pairs.sort();
    let distinct = pairs.len();
    pairs.dedup();
    assert_eq!(pairs.len(), distinct, "two states drew the same glyph pair");
}

/// The jj local states git has no word for, drawn from the same two slots.
///
/// jj auto-commits, so "dirty" does not exist there; what it has instead is an
/// empty change, a conflicted one and a divergent one. The first two are built
/// here for real. Divergence is not: it takes concurrent operations on the same
/// change id, which the app has no way to produce — it is covered by the unit
/// tests in `vcs::status` instead.
#[test]
fn a_jj_workspace_draws_the_local_states_git_has_no_word_for() {
    let Some(mut f) = Fixture::jj_workspaces() else {
        return;
    };

    // The default workspace is the repository's own working copy and names no
    // space, so it is the row with no separator rather than one found by name.
    let default_row = f.default_row(ROOMY);
    for (row, local) in [
        (default_row, "∅"),
        (f.row(ROOMY, "working"), " "),
        (f.row(ROOMY, "conflicted"), "!"),
    ] {
        assert_eq!(local_glyph(&row), local, "wrong local glyph on:\n{row}");
    }
}

/// A colocated-free mixed list still says which backend owns each row: the two
/// behave differently when deleted, and the tag is the only thing that says so.
#[test]
fn both_backends_name_themselves_in_one_list() {
    let Some(mut f) = Fixture::jj_workspaces() else {
        return;
    };
    let tags: Vec<String> = f
        .rows(ROOMY)
        .iter()
        .map(|row| cells(row)[3..6].concat())
        .collect();
    assert!(tags.contains(&"git".to_owned()), "no git row: {tags:?}");
    assert!(tags.contains(&"jj ".to_owned()), "no jj row: {tags:?}");
}

// ---------------------------------------------------------------------------
// The aligned table (shanti-nbt.5, shanti-hq6.7)
// ---------------------------------------------------------------------------

/// The separator forms a vertical spine: the repository column is right-aligned
/// against it, so `repo /` stays contiguous whatever the names are.
#[test]
fn the_separator_falls_in_one_column_on_every_row() {
    let mut f = Fixture::with_worktrees(&[
        Wt::new("a", "one", Remote::NeverPushed),
        Wt::new("mid-length-repo", "two", Remote::NeverPushed),
        Wt::new(
            "considerably-longer-repository",
            "three",
            Remote::NeverPushed,
        ),
    ]);

    for size in [ROOMY, HUGE, FLOOR] {
        let rows = f.rows(size);
        assert_eq!(rows.len(), 3, "expected three rows at {size:?}");
        let columns: Vec<Option<usize>> = rows.iter().map(|row| spine(row)).collect();
        assert!(
            columns
                .iter()
                .all(|column| *column == columns[0] && column.is_some()),
            "the separator wandered between rows at {size:?}:\n{}",
            rows.join("\n")
        );
    }
}

/// The repository name is pushed right up against the separator, so the eye
/// reads `repo / space` as one phrase rather than as two drifting columns.
#[test]
fn a_short_repository_name_still_touches_the_separator() {
    let mut f = Fixture::with_worktrees(&[
        Wt::new("a", "one", Remote::NeverPushed),
        Wt::new("considerably-longer-repository", "two", Remote::NeverPushed),
    ]);
    let row = f.row(ROOMY, "one");
    assert!(
        row.contains("a / one"),
        "the short name floated away from the separator:\n{row}"
    );
}

/// Columns are measured across every filtered row, not the ones scrolled into
/// view — otherwise scrolling shifts the table underneath the reader.
#[test]
fn the_columns_are_measured_across_rows_that_are_off_screen() {
    const BRANCHES: [&str; 20] = [
        "aa", "bb", "cc", "dd", "ee", "ff", "gg", "hh", "ii", "jj", "kk", "ll", "mm", "nn", "oo",
        "pp", "qq", "rr", "ss", "tt",
    ];
    let mut names: Vec<Wt> = BRANCHES
        .into_iter()
        .map(|branch| Wt::new("brief", branch, Remote::NeverPushed))
        .collect();
    // Sorts last, so it is the row *off* the bottom of a short frame — and it is
    // the longest repository name, so the column it needs is the one every
    // visible row must already be drawn in.
    names.push(Wt::new(
        "zz-a-much-longer-repository-name",
        "tail",
        Remote::NeverPushed,
    ));
    let mut f = Fixture::with_worktrees(&names);

    // Short enough that the long-named row is off the bottom.
    let short = (ROOMY.0, MIN_HEIGHT);
    let visible = f.rows(short);
    assert!(
        visible.len() < names.len(),
        "the fixture must not fit on screen, or this proves nothing"
    );
    assert!(
        !visible.iter().any(|row| row.contains("zz-a-much-longer")),
        "the long-named row has to be off-screen for this to prove anything:\n{}",
        visible.join("\n")
    );

    let tall = spine(&f.row(ROOMY, "tail")).expect("the tall frame has a separator");
    for row in &visible {
        assert_eq!(
            spine(row),
            Some(tall),
            "the columns moved when a row went off-screen:\n{row}"
        );
    }
}

/// A repository's own default space is the row's whole subject, so it is named
/// alone: no separator, no space name (`shanti-hq6.7`).
#[test]
fn a_repositorys_default_space_names_no_space_at_all() {
    let Some(mut f) = Fixture::jj_workspaces() else {
        return;
    };
    // The jj repository's default workspace lives at the repository root, and is
    // the only row that is its repository's own working copy.
    let row = f.default_row(ROOMY);
    assert!(
        row.trim_end().ends_with("lotus"),
        "the default row should end at the repository name:\n{row}"
    );
}

/// The two columns are clipped from opposite ends, and each for a reason: the
/// repository is right-aligned so its *tail* is what sits in the column, while a
/// space name reads from the left.
#[test]
fn a_long_repository_is_clipped_from_the_head_and_a_long_space_from_the_tail() {
    let mut f = Fixture::with_worktrees(&[Wt::new(
        "an-extremely-long-repository-name-nobody-would-choose",
        "an-extremely-long-space-name-nobody-would-choose-either",
        Remote::NeverPushed,
    )]);

    let row = f.row((60, 20), "…");
    let column = spine(&row).expect("the row has a separator");
    // Everything left of the repository column is fixed: two status cells, a
    // gap, the three-cell backend tag and a gap.
    let repo: String = cells(&row)[PREFIX_WIDTH..column].concat();
    let space: String = cells(&row)[column + 3..].concat();

    assert!(
        repo.trim_start().starts_with('…') && repo.ends_with("choose"),
        "the repository column must lose its head, not its tail:\n{row}"
    );
    assert!(
        space.starts_with("an-extremely") && space.trim_end().ends_with('…'),
        "the space column must lose its tail, not its head:\n{row}"
    );
}

/// Widths are display cells, not bytes: a row of multi-byte names must line up
/// with a row of ASCII ones.
#[test]
fn column_widths_are_counted_in_cells_not_bytes() {
    let mut f = Fixture::with_worktrees(&[
        Wt::new("naïve-café-ünicode", "one", Remote::NeverPushed),
        Wt::new("plain-ascii-repo-x", "two", Remote::NeverPushed),
    ]);
    let rows = f.rows(ROOMY);
    // The two repository names are the same number of *cells* and a different
    // number of bytes, so a byte-counted layout puts the separators in different
    // columns.
    assert_eq!(
        spine(&rows[0]),
        spine(&rows[1]),
        "multi-byte names moved the spine:\n{}",
        rows.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Empty states (shanti-nbt.6)
// ---------------------------------------------------------------------------

/// An empty pane is what a hung program looks like; during a scan the pane *is*
/// empty for a moment, so one line has to say which of the two this is.
#[test]
fn a_scan_in_flight_says_so() {
    let repos_dir = tempdir().expect("could not create repos dir");
    let worktrees_dir = tempdir().expect("could not create worktrees dir");
    init_repo(repos_dir.path(), "alpha");
    let (app, results) = booting(&Args::for_dirs(
        worktrees_dir.path().display().to_string(),
        vec![repos_dir.path().display().to_string()],
    ));
    let mut f = Fixture {
        app,
        _results: results,
        _repos_dir: repos_dir,
        _worktrees_dir: worktrees_dir,
        _remotes: Vec::new(),
    };
    assert!(
        f.screen(ROOMY).contains("scanning for repositories…"),
        "a list with a scan still running must say so:\n{}",
        f.screen(ROOMY)
    );
}

/// "Nothing was found" is a configuration problem, so the notice names the three
/// settings that would fix it, in the order that wins.
#[test]
fn an_empty_repos_dir_names_the_settings_that_would_fix_it() {
    let mut f = Fixture::with_bare_repos(&[]);
    let screen = f.screen(ROOMY);
    for expected in [
        "no repositories found",
        "nothing was found in the repos dir",
        "--repos-dir",
        "SHANTI_REPOS_DIR or config.toml",
    ] {
        assert!(screen.contains(expected), "missing {expected:?}:\n{screen}");
    }
}

/// Repositories but no spaces is not a problem at all — it is a next step.
///
/// Regression test for a bug these tests found. A user whose repositories simply
/// had no spaces yet was told "no repositories found — set it with --repos-dir,
/// SHANTI_REPOS_DIR or config.toml": a configuration error that did not exist,
/// pointing at settings that were already correct.
///
/// The cause was in `App`, not in the notice. `set_scan` took one `Option` for
/// both the count and whether a scan was running, and `update_scan_indicator`
/// passed `None` as soon as the last job left `scans` — the same `handle_job`
/// call that had just raised `scan_found` to the real number. The final count
/// was erased before it was ever recorded, so `EmptyState::NoSpaces` was
/// unreachable with one repos dir and short by the last batch with several.
/// The two are now separate arguments.
#[test]
fn repositories_without_spaces_say_how_to_make_one() {
    let mut f = Fixture::with_bare_repos(&["alpha", "beta", "gamma"]);
    let screen = f.screen(ROOMY);
    for expected in [
        "no spaces yet",
        "3 repositories, none with a space",
        "press n to create one",
    ] {
        assert!(screen.contains(expected), "missing {expected:?}:\n{screen}");
    }
    assert!(
        !screen.contains("no repositories found"),
        "repositories were found; the notice must not say otherwise:\n{screen}"
    );
}

/// A filter that hides everything is not a missing repository, and must not be
/// reported as one: the way out is the filter the user typed.
#[test]
fn a_filter_that_matches_nothing_offers_the_filter_back() {
    let mut f = Fixture::with_worktrees(&[Wt::new("alpha", "one", Remote::NeverPushed)]);
    f.press_char('i');
    f.type_str("zzzznotathing");

    let screen = f.screen(ROOMY);
    assert!(
        screen.contains("nothing matches the filter") && screen.contains("press / to change it"),
        "the filtered-empty notice is missing:\n{screen}"
    );
    assert!(
        !screen.contains("no repositories found") && !screen.contains("no spaces yet"),
        "a filter miss must not be reported as an empty configuration:\n{screen}"
    );
}

// ---------------------------------------------------------------------------
// The size floor (shanti-hq6.5)
// ---------------------------------------------------------------------------

/// The one whole-frame assertion in this file, and the one place it is right:
/// below the floor the *entire* specified output is three centred lines, so
/// anything else on screen — a border, a row, half a dialog — is the bug.
///
/// It also pins the numbers the message quotes to the floor the code enforces:
/// the frame one cell larger draws the interface instead.
#[test]
fn below_the_floor_the_whole_frame_is_the_message() {
    let mut f = Fixture::with_worktrees(&[Wt::new("alpha", "one", Remote::NeverPushed)]);

    let width = MIN_WIDTH - 1;
    let height = MIN_HEIGHT - 1;
    let blank = " ".repeat(width as usize);
    let centred = |text: &str| {
        let pad = (width as usize - text.chars().count()) / 2;
        let mut line = " ".repeat(pad);
        line.push_str(text);
        line.push_str(&" ".repeat(width as usize - pad - text.chars().count()));
        line
    };

    let expected = vec![
        blank.clone(),
        blank.clone(),
        blank.clone(),
        centred("Terminal too small"),
        centred(&format!("Need {MIN_WIDTH}x{MIN_HEIGHT}")),
        centred(&format!("Have {width}x{height}")),
        blank.clone(),
        blank.clone(),
        blank,
    ];
    assert_eq!(
        f.frame((width, height)),
        expected,
        "the frame below the floor must be the message and nothing else"
    );

    // One cell larger in both directions, and the interface is back.
    assert!(
        f.screen(FLOOR).contains("Worktrees"),
        "the floor itself must draw the interface"
    );
}

/// Below the floor a popup declines to draw rather than overpainting the one
/// message that fits.
#[test]
fn below_the_floor_a_popup_declines_to_draw() {
    let mut f = Fixture::with_worktrees(&[Wt::new("alpha", "one", Remote::NeverPushed)]);
    let small = (MIN_WIDTH - 1, MIN_HEIGHT - 1);

    let before = f.frame(small);
    for key in ['?', 'n', 'p', 'd'] {
        f.press_char(key);
        assert_eq!(
            f.frame(small),
            before,
            "a popup painted over the too-small message after pressing {key:?}"
        );
    }
}

/// The height of a destructive dialog is derived from its body measured at the
/// width it will actually get, and the gate row is reserved before the body — so
/// at the floor the explanation is what gives way, never the row that says how
/// to confirm.
#[test]
fn at_the_floor_a_destructive_dialog_keeps_its_gate_row() {
    // Dirty *and* never pushed: two losses, two removals and an aftermath line —
    // the tallest dialog the app builds, and the one that cannot fit at 40x10.
    let mut f =
        Fixture::with_worktrees(&[Wt::new("alpha", "risky-space", Remote::NeverPushed).dirty()]);
    f.press_char('d');

    let roomy = f.screen(ROOMY);
    for expected in [
        "This would destroy:",
        "1 uncommitted file (modified, staged or untracked)",
        "a branch that was never pushed",
        "Deleting also removes:",
        "This cannot be undone.",
        "Press X to delete it anyway.",
    ] {
        assert!(roomy.contains(expected), "missing {expected:?}:\n{roomy}");
    }

    let floor = f.screen(FLOOR);
    assert!(
        floor.contains("Press X to delete it anyway."),
        "the gate row was clipped at the size floor:\n{floor}"
    );
    assert!(
        floor.contains("⚠"),
        "the dialog lost its title at the size floor:\n{floor}"
    );
}

// ---------------------------------------------------------------------------
// The extremes
// ---------------------------------------------------------------------------

/// Every popup, at the narrowest supported frame and at a very wide one: the
/// layout uses percentage constraints, which misbehave at both ends.
///
/// The assertion is deliberately structural — each popup is on screen, and
/// nothing was painted outside the frame — rather than a picture of any one of
/// them, which would be a snapshot of five dialogs' prose.
#[test]
fn every_popup_survives_the_narrowest_and_the_widest_terminal() {
    for size in [FLOOR, HUGE, (MIN_WIDTH, 60), (240, MIN_HEIGHT)] {
        for (key, marker) in [
            ('?', " Help "),
            ('n', "Repositories"),
            ('p', "Worktree from PR"),
            ('d', "⚠"),
        ] {
            let mut f =
                Fixture::with_worktrees(&[Wt::new("alpha", "one", Remote::NeverPushed).dirty()]);
            f.press_char(key);
            let frame = f.frame(size);

            assert_eq!(frame.len(), size.1 as usize, "wrong number of rows");
            for line in &frame {
                assert_eq!(
                    cells(line).len(),
                    size.0 as usize,
                    "a row is not the width of the frame at {size:?}"
                );
            }
            assert!(
                frame.join("\n").contains(marker),
                "{key:?} drew no popup at {size:?}:\n{}",
                frame.join("\n")
            );
        }
    }
}

/// The list itself at both extremes: still one row per space, still aligned.
#[test]
fn the_list_holds_its_shape_at_both_extremes() {
    let mut f = Fixture::with_worktrees(&[
        Wt::new("alpha", "one", Remote::InSync),
        Wt::new("beta", "two", Remote::Ahead),
    ]);
    for size in [FLOOR, HUGE] {
        let rows = f.rows(size);
        assert_eq!(rows.len(), 2, "wrong number of rows at {size:?}");
        assert_eq!(
            spine(&rows[0]),
            spine(&rows[1]),
            "the spine broke at {size:?}:\n{}",
            rows.join("\n")
        );
    }
}

/// Whatever is on screen, the bottom row carries something to read: the mode,
/// the error, or the keys available. An empty bottom row is a dead end.
///
/// Deliberately weaker than naming what is there — an always-visible keybinding
/// footer is landing in `shanti-hq6.3` and will change the wording. What must
/// not change is that the row says *something*.
#[test]
fn the_bottom_row_always_carries_guidance() {
    for key in [None, Some('?'), Some('n'), Some('p'), Some('d'), Some('i')] {
        let mut f =
            Fixture::with_worktrees(&[Wt::new("alpha", "one", Remote::NeverPushed).dirty()]);
        if let Some(key) = key {
            f.press_char(key);
        }
        let frame = f.frame(ROOMY);
        let bottom = frame.last().expect("the frame has rows").clone();
        assert!(
            bottom.chars().any(|c| c.is_alphanumeric()),
            "nothing to read along the bottom after {key:?}:\n{bottom}"
        );
    }
}
