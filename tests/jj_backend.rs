//! End-to-end tests for the jujutsu backend.
//!
//! The counterpart of `tests/git_backend.rs`, and deliberately shaped like it:
//! real repositories in temporary directories, a local *bare* git repository
//! standing in for `origin` so remote bookmarks exist with no network, and one
//! assertion per question so a blocked expectation cannot park the others.
//!
//! Two things differ from the git suite, both forced by jj itself:
//!
//! 1. **jj is an external binary.** shanti drives jj by spawning it, so these
//!    tests need a `jj` on the machine. A contributor without one must still get
//!    a green `cargo test`, so every test *skips* — `JjFixture::new` returns
//!    `None` and the test returns early, after printing why. This mirrors
//!    `src/vcs/jj/testing.rs`; that fixture is `#[cfg(test)]` and therefore
//!    invisible from an integration test, so the one below is built from the
//!    public API only.
//! 2. **Reads never snapshot.** shanti's jj reads pass `--ignore-working-copy`,
//!    so an edit jj has not recorded yet is invisible to them by design. That is
//!    exactly what makes [`work_left_unsnapshotted_in_a_space_survives_its_deletion`]
//!    the most valuable test here: it is the only one that proves shanti does
//!    not throw such an edit away.

use std::path::{Path, PathBuf};
use std::process::Command;

use shanti::vcs::jj::{JjBackend, JjCli};
use shanti::vcs::{
    backend_at, discover, Backend, JjLocal, LocalState, RemoteState, Space, SpaceStatus, Vcs,
};
use tempfile::{tempdir, TempDir};

// --------------------------------------------------------------------------
// Fixture
// --------------------------------------------------------------------------

/// A throwaway jj repository, the directory that owns it, and an adapter bound
/// to it.
///
/// The `TempDir` is kept as a field on purpose: dropping it deletes the
/// repository, so a test holding only the backend would be driving jj at a
/// directory that had already vanished.
struct JjFixture {
    _dir: TempDir,
    /// The temporary root, canonicalised. jj reports canonical paths (on macOS
    /// `/var` is a symlink to `/private/var`), so a fixture handing tests the
    /// uncanonicalised path would fail every path assertion for a reason that
    /// has nothing to do with the code under test.
    base: PathBuf,
    /// Root of the repository under test.
    root: PathBuf,
    /// Where spaces are created, mirroring `SHANTI_WORKTREES_DIR`.
    spaces_dir: PathBuf,
    /// The located jj, so fixture setup and the code under test agree on which
    /// binary they mean when `SHANTI_JJ_BIN` points somewhere unusual.
    cli: JjCli,
}

impl JjFixture {
    /// A repository named `name` with one described commit and an empty working
    /// copy on top — or `None`, with a printed reason, when jj is unavailable.
    fn new(name: &str) -> Option<Self> {
        Self::init(name, &["git", "init"])
    }

    /// The same repository, colocated with git (`.jj` *and* `.git`), which is
    /// how most people adopt jj on an existing project.
    fn colocated(name: &str) -> Option<Self> {
        Self::init(name, &["git", "init", "--colocate"])
    }

    fn init(name: &str, init_args: &[&str]) -> Option<Self> {
        if !JjCli::is_available() {
            eprintln!("skipping: no jj binary on this machine");
            return None;
        }

        let dir = tempdir().expect("could not create a temporary directory");
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
            base: base.clone(),
            root,
            spaces_dir: base.join("spaces"),
            cli,
        };

        fixture.jj(init_args);
        // Pin the identity in the *repository's* config rather than in the
        // environment. The backend spawns jj itself and inherits whatever
        // environment the test binary has, so a machine with no `user.name`
        // configured would fail on `jj workspace add` — a fixture problem
        // masquerading as a shanti bug. Repo config reaches every jj run
        // against this repository, shanti's included.
        fixture.jj(&["config", "set", "--repo", "user.name", "shanti tests"]);
        fixture.jj(&[
            "config",
            "set",
            "--repo",
            "user.email",
            "tests@shanti.invalid",
        ]);
        // A working copy whose parent is the root commit behaves differently
        // enough to be a poor stand-in for a real repository.
        fixture.jj(&["describe", "-m", "first"]);
        fixture.jj(&["new"]);
        Some(fixture)
    }

    /// A backend bound to this repository — the object under test.
    fn backend(&self) -> JjBackend {
        JjBackend::from_cli(self.cli.clone()).expect("could not open the fixture repository")
    }

    /// Where a space named `name` should be created, following shanti's own
    /// `<worktrees dir>/<repo>/<name>` layout.
    fn dest(&self, name: &str) -> PathBuf {
        self.spaces_dir
            .join(self.root.file_name().expect("repository has a name"))
            .join(name)
    }

    fn create(&self, name: &str) -> Space {
        self.backend()
            .create_space(name, &self.dest(name))
            .unwrap_or_else(|err| panic!("could not create space '{name}': {err}"))
    }

    /// Give the repository an `origin` carrying a bookmark named `name`,
    /// without touching the network.
    ///
    /// The remote is a bare git repository beside the fixture: jj pushes to it
    /// over the filesystem, which is the only way to get a genuine
    /// `name@origin` — the ref both base resolution and the remote half of
    /// status have to recognise — into a throwaway repository.
    ///
    /// The bookmark is created at `@-`, the described commit: jj refuses to
    /// push a commit with no description, and the working copy has none.
    fn push_bookmark(&self, name: &str) {
        let remote = self.base.join("origin.git");
        let status = Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&remote)
            .status()
            .expect("could not run git");
        assert!(status.success(), "git init --bare failed");

        self.jj(&[
            "git",
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path is not UTF-8"),
        ]);
        self.jj(&["bookmark", "create", "-r", "@-", name]);
        self.jj(&["git", "push", "--allow-new", "--bookmark", name]);
    }

    /// The commit id `revset` resolves to, for the tests that need to say "the
    /// new workspace started *here*".
    fn commit_at(&self, revset: &str) -> String {
        self.render(revset, "commit_id")
    }

    /// The *change* id `revset` resolves to.
    ///
    /// Change ids, not commit ids, are what survives a snapshot: recording an
    /// edit rewrites the working-copy commit but keeps its change id, so this is
    /// the only handle a test can take on a workspace's work *before* deleting
    /// it and still resolve afterwards.
    fn change_at(&self, revset: &str) -> String {
        self.render(revset, "change_id")
    }

    fn render(&self, revset: &str, template: &str) -> String {
        self.jj(&[
            "log",
            "--no-graph",
            "--limit",
            "1",
            "-r",
            revset,
            "-T",
            template,
        ])
        .trim()
        .to_owned()
    }

    /// Run a jj command against the repository root, returning its stdout and
    /// panicking with jj's own complaint if it fails — a broken fixture is a
    /// test bug, not a condition to handle.
    fn jj(&self, args: &[&str]) -> String {
        self.jj_in(&self.root, args)
    }

    /// The same, but in `dir` — jj picks which workspace a command acts on from
    /// the directory it runs in, so a test that wants to touch a *space* has to
    /// say so.
    ///
    /// Deliberately not routed through [`JjCli`]: fixtures need setup
    /// subcommands and an isolated identity that the production adapter has no
    /// business offering.
    fn jj_in(&self, dir: &Path, args: &[&str]) -> String {
        let output = Command::new(self.cli.program())
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
}

/// The one space a test expects, by name.
fn space_named<'a>(spaces: &'a [Space], name: &str) -> &'a Space {
    spaces
        .iter()
        .find(|space| space.name == name)
        .unwrap_or_else(|| {
            let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
            panic!("no space named {name:?}; the repository lists {names:?}")
        })
}

/// The jj half of a status, or a panic naming what arrived instead.
fn jj_local(status: &SpaceStatus) -> JjLocal {
    match status.local {
        LocalState::Jj(local) => local,
        other => panic!("expected a jj local state, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// Discovery
// --------------------------------------------------------------------------

/// Both shapes of jj repository must be driven through jj. The colocated case
/// is the one that could plausibly go wrong: git would happily open it, and
/// driving it through git behind jj's back leaves jj's view of the working copy
/// stale.
#[test]
fn jj_native_and_colocated_repositories_are_both_driven_through_jj() {
    let Some(native) = JjFixture::new("native") else {
        return;
    };
    let Some(colocated) = JjFixture::colocated("colocated") else {
        return;
    };

    assert_eq!(backend_at(&native.root), Some(Backend::Jj));
    assert!(
        colocated.root.join(".git").exists(),
        "the colocated fixture should have a .git as well as a .jj"
    );
    assert_eq!(backend_at(&colocated.root), Some(Backend::Jj));
}

/// A space is a workspace of a repository, not a repository of its own: the
/// walk must not offer it as a second copy of the same project.
#[test]
fn a_space_is_not_rediscovered_as_a_repository() {
    let Some(fixture) = JjFixture::new("one-repo") else {
        return;
    };
    let space = fixture.create("feature");

    let found = discover(&fixture.base, &[]);

    assert_eq!(
        found.len(),
        1,
        "expected only the repository itself, found {found:?}"
    );
    assert_eq!(found[0].path, fixture.root);
    assert_eq!(found[0].backend, Backend::Jj);
    assert_eq!(backend_at(&space.path), None);
}

// --------------------------------------------------------------------------
// Creation
// --------------------------------------------------------------------------

/// Case 1: what the UI asks for after a create — the space is on disk, at the
/// path shanti chose, and the listing knows about it.
#[test]
fn a_created_space_is_listed_at_the_chosen_path() {
    let Some(fixture) = JjFixture::new("listed") else {
        return;
    };

    let space = fixture.create("feature");
    assert_eq!(space.path, fixture.dest("feature"));
    assert!(space.exists(), "the space directory should exist on disk");

    let backend = fixture.backend();
    let spaces = backend.spaces().expect("listing spaces should work");
    let mut names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    // The repository's own working copy is a space too, and always listed.
    assert_eq!(names, ["default", "feature"]);
    assert_eq!(
        space_named(&spaces, "feature").path,
        fixture.dest("feature")
    );
    assert_eq!(space_named(&spaces, "feature").repo, backend.repo().id);
}

/// Case 2a: a bookmark of that name exists on a remote, so the space starts
/// from it — the case that matters when someone recreates a space for work that
/// already exists upstream.
#[test]
fn create_space_starts_from_a_matching_remote_bookmark() {
    let Some(fixture) = JjFixture::new("from-remote") else {
        return;
    };
    fixture.push_bookmark("feature");

    let backend = fixture.backend();
    assert_eq!(
        backend.resolve_base("feature"),
        "Will start from feature@origin"
    );

    let space = fixture.create("feature");
    assert!(space.exists());
    // `feature@` is the new workspace's working copy; its parent is the base.
    assert_eq!(
        fixture.commit_at("feature@-"),
        fixture.commit_at("feature@origin"),
        "the new workspace should sit on top of the remote bookmark"
    );
}

/// Case 2b: no bookmark of that name anywhere, so the space starts from the
/// repository's mainline instead. `trunk()` is jj's own answer to "what is
/// mainline here", which is why shanti does not resolve it to a branch name.
#[test]
fn create_space_without_a_matching_remote_bookmark_starts_from_trunk() {
    let Some(fixture) = JjFixture::new("from-trunk") else {
        return;
    };
    // `main` on a remote is what makes `trunk()` resolve to something real.
    fixture.push_bookmark("main");

    let backend = fixture.backend();
    assert_eq!(backend.resolve_base("brand-new"), "Will start from trunk()");

    let space = fixture.create("brand-new");
    assert!(space.exists());
    assert_eq!(
        fixture.commit_at("brand-new@-"),
        fixture.commit_at("main@origin"),
        "the new workspace should sit on top of trunk()"
    );
    // Its working copy is empty and sits directly on `main`, so the remote half
    // is read through the parent and reports "in sync" — correctly: no work
    // exists here yet that the remote has not got. The moment the user records
    // anything, the head stops carrying a bookmark and this becomes
    // `Untracked`, which is the falsely-clean answer the status model exists to
    // avoid.
    assert_eq!(space.status.remote, RemoteState::in_sync());
}

/// Case 2c: a repository with no remote and no mainline yet still gets a
/// working space — jj starts it beside the current working copy.
#[test]
fn create_space_in_a_repository_without_a_remote() {
    let Some(fixture) = JjFixture::new("offline") else {
        return;
    };

    let backend = fixture.backend();
    assert_eq!(
        backend.resolve_base("offline"),
        "Will start beside the current working copy"
    );

    let space = fixture.create("offline");
    assert!(space.exists(), "the space directory should exist on disk");
    assert!(
        space.path.join(".jj").exists(),
        "the space should be a real jj workspace"
    );
    assert_eq!(space.status.remote, RemoteState::Untracked);
}

// --------------------------------------------------------------------------
// Deletion
// --------------------------------------------------------------------------

/// Create a space and delete it, optionally removing its directory behind
/// shanti's back first.
///
/// The two deletion scenarios differ only in that flag, and each is asserted
/// from more than one angle, so the setup is shared rather than copied.
fn delete_created_space(fixture: &JjFixture, remove_directory_first: bool) -> Space {
    let space = fixture.create("feature");

    if remove_directory_first {
        std::fs::remove_dir_all(&space.path).expect("could not remove the space directory by hand");
    }

    fixture
        .backend()
        .delete_space(&space)
        .expect("deleting the space should succeed");

    space
}

/// Case 3: deleting a space forgets the workspace and removes its directory,
/// and it stops being listed.
#[test]
fn delete_space_forgets_the_workspace_and_removes_the_directory() {
    let Some(fixture) = JjFixture::new("deleted") else {
        return;
    };
    let space = delete_created_space(&fixture, false);

    assert!(
        !space.path.exists(),
        "the space directory should have been removed"
    );

    let spaces = fixture
        .backend()
        .spaces()
        .expect("listing spaces should work");
    let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["default"],
        "the deleted workspace should no longer be registered"
    );
}

/// Case 4: the directory was removed behind shanti's back (`rm -rf`, another
/// tool). Deletion must still succeed and still drop the registration —
/// otherwise the space is stranded in the list forever.
#[test]
fn delete_space_whose_directory_was_already_removed() {
    let Some(fixture) = JjFixture::new("already-gone") else {
        return;
    };
    let space = delete_created_space(&fixture, true);

    assert!(!space.path.exists());
    let spaces = fixture
        .backend()
        .spaces()
        .expect("listing spaces should work");
    let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["default"]);
}

/// Case 5: deleting twice converges on "removed" rather than failing. The UI
/// can hold a stale space (a refresh has not landed yet, two keystrokes race),
/// and the second delete must be a no-op, not an error dialog.
#[test]
fn deleting_a_space_twice_converges_on_removed() {
    let Some(fixture) = JjFixture::new("twice") else {
        return;
    };
    let space = delete_created_space(&fixture, false);

    fixture
        .backend()
        .delete_space(&space)
        .expect("deleting an already-deleted space should be a no-op");

    assert!(!space.path.exists());
}

/// Case 6, and the most valuable test in this file: **deleting a space must not
/// destroy work jj has not snapshotted yet**.
///
/// jj auto-commits, but only when a jj command actually runs in that workspace.
/// An edit made since the last one exists on disk and nowhere else — and
/// shanti's own reads pass `--ignore-working-copy`, so they cannot see it. If
/// delete simply removed the directory, that edit would be gone with no trace
/// and no undo.
///
/// shanti-nhe.4 makes delete snapshot first, so the edit becomes the
/// workspace's working-copy commit and survives the forget as an ordinary head:
/// still in `jj log`, still recoverable long after the directory is gone. The
/// change *id* is the handle that survives, because snapshotting rewrites the
/// commit but keeps its change id.
#[test]
fn work_left_unsnapshotted_in_a_space_survives_its_deletion() {
    let Some(fixture) = JjFixture::new("preserved") else {
        return;
    };
    let space = fixture.create("feature");

    // Written straight to disk, with no jj command run in the space afterwards:
    // this is precisely the state jj has not recorded.
    const WORK: &str = "work jj has not seen yet\n";
    std::fs::write(space.path.join("preserved.txt"), WORK).expect("could not write the test file");
    let change = fixture.change_at("feature@");

    fixture
        .backend()
        .delete_space(&space)
        .expect("deleting the space should succeed");
    assert!(!space.path.exists(), "the directory should still be gone");

    // The work is findable in the repository, by content.
    assert_eq!(
        fixture.jj(&["file", "show", "-r", &change, "preserved.txt"]),
        WORK,
        "the unsnapshotted edit was lost when the space was deleted"
    );
    // And reachable from `jj log`, not merely resurrectable from the oplog.
    let heads = fixture.jj(&["log", "--no-graph", "-r", "heads(all())", "-T", "change_id"]);
    assert!(
        heads.contains(&change),
        "the preserved change {change} is not a head; jj log shows {heads}"
    );
}

/// Case 8: the repository's own working copy is not a space shanti created. jj
/// will not let it be forgotten, and removing its directory would take the
/// source repository with it.
#[test]
fn deleting_the_repositorys_own_workspace_is_refused() {
    let Some(fixture) = JjFixture::new("guarded") else {
        return;
    };

    let backend = fixture.backend();
    let spaces = backend.spaces().expect("listing spaces should work");
    let default = space_named(&spaces, "default");

    let error = backend
        .delete_space(default)
        .expect_err("deleting the repository's own workspace must be refused")
        .to_string();
    assert!(
        error.contains("refusing to delete it"),
        "unhelpful refusal: {error}"
    );
    assert!(
        fixture.root.exists() && fixture.root.join(".jj").exists(),
        "the repository itself must survive a refused delete"
    );
}

// --------------------------------------------------------------------------
// Status
// --------------------------------------------------------------------------

/// A space shanti has just created holds nothing and is on no bookmark: empty
/// locally, unheard-of upstream.
#[test]
fn a_fresh_space_is_empty_and_never_pushed() {
    let Some(fixture) = JjFixture::new("fresh") else {
        return;
    };
    let space = fixture.create("feature");

    assert_eq!(space.status.remote, RemoteState::Untracked);
    assert_eq!(
        jj_local(&space.status),
        JjLocal {
            empty: true,
            conflicted: false,
            divergent: false,
        }
    );
    // jj auto-commits, so an ordinary jj space never has "unsaved" work the way
    // a dirty git worktree does.
    assert!(!space.status.has_unsaved_work());
    assert!(space.status.remote.has_unpushed_work());
}

/// The counterpart: the local flags must actually move, or a hard-coded
/// `empty: true` would pass the test above just as well.
#[test]
fn a_space_with_recorded_edits_is_not_empty() {
    let Some(fixture) = JjFixture::new("edited") else {
        return;
    };
    let space = fixture.create("feature");
    std::fs::write(space.path.join("a.txt"), "hello\n").expect("could not write the test file");
    // Snapshot it, as any jj command run in the space would.
    fixture.jj_in(&space.path, &["status"]);

    let spaces = fixture.backend().spaces().expect("listing spaces works");
    assert_eq!(
        jj_local(&space_named(&spaces, "feature").status),
        JjLocal {
            empty: false,
            conflicted: false,
            divergent: false,
        }
    );
}

/// A conflicted space must say so: this is the state that blocks work, and the
/// one git has no vocabulary for at all.
#[test]
fn a_conflicted_space_is_reported_as_conflicted() {
    let Some(fixture) = JjFixture::new("conflicted") else {
        return;
    };
    let space = fixture.create("feature");
    conflict_in(&fixture, &space);

    let spaces = fixture.backend().spaces().expect("listing spaces works");
    let status = &space_named(&spaces, "feature").status;

    assert!(
        jj_local(status).conflicted,
        "expected a conflict: {status:?}"
    );
    // A conflict is work at risk, and the glyph has to be the loud one.
    assert!(status.has_unsaved_work());
    assert_eq!(status.glyphs()[1].symbol, "!");
}

/// Put `space`'s working copy on a merge of two siblings that each rewrite the
/// same file — the smallest genuine conflict jj can be given.
///
/// The siblings are built in the repository's own workspace and the merge is
/// made *in the space*, because jj decides which workspace a command acts on
/// from the directory it runs in.
fn conflict_in(fixture: &JjFixture, space: &Space) {
    std::fs::write(fixture.root.join("a.txt"), "one\n").expect("could not write the test file");
    fixture.jj(&["describe", "-m", "one"]);
    fixture.jj(&["bookmark", "create", "side-a", "-r", "@"]);

    fixture.jj(&["new", "@-"]);
    std::fs::write(fixture.root.join("a.txt"), "two\n").expect("could not write the test file");
    fixture.jj(&["describe", "-m", "two"]);
    fixture.jj(&["bookmark", "create", "side-b", "-r", "@"]);

    fixture.jj_in(&space.path, &["new", "side-a", "side-b"]);
}

/// The three signals the UI has to keep apart must not collapse onto one
/// rendering.
#[test]
fn conflicted_empty_and_ahead_spaces_are_distinguishable() {
    let Some(fixture) = JjFixture::new("distinct") else {
        return;
    };
    fixture.push_bookmark("main");

    let empty = fixture.create("empty").status;
    let ahead = ahead_space(&fixture).status;

    let conflicted_space = fixture.create("conflicted");
    conflict_in(&fixture, &conflicted_space);
    let spaces = fixture.backend().spaces().expect("listing spaces works");
    let conflicted = space_named(&spaces, "conflicted").status.clone();

    assert_ne!(empty, ahead);
    assert_ne!(empty, conflicted);
    assert_ne!(ahead, conflicted);

    let rendered: Vec<[&str; 2]> = [&empty, &ahead, &conflicted]
        .iter()
        .map(|status| {
            let [remote, local] = status.glyphs();
            [remote.symbol, local.symbol]
        })
        .collect();
    // The fresh space is empty and parked on `main`, hence "in sync" + "empty".
    assert_eq!(rendered[0], ["✔", "∅"], "empty");
    // Ahead of the remote, and parked on a fresh empty change — the two slots
    // speak independently, which is the whole point of having two.
    assert_eq!(rendered[1], ["↑", "∅"], "ahead");
    assert_eq!(rendered[2], ["⬆", "!"], "conflicted");
}

/// jj states tracking counts from the *remote ref's* point of view, and the
/// backend swaps them exactly once (shanti-nhe.5). A swap that went missing
/// would report this space as one commit *behind*, which is the opposite advice
/// to give the user — so pin the direction here as well as in the unit tests.
#[test]
fn a_space_ahead_of_its_remote_is_ahead_and_not_behind() {
    let Some(fixture) = JjFixture::new("ahead") else {
        return;
    };
    fixture.push_bookmark("main");

    let space = ahead_space(&fixture);

    assert_eq!(
        space.status.remote,
        RemoteState::Tracked {
            ahead: 1,
            behind: 0
        }
    );
    assert!(space.status.remote.has_unpushed_work());
}

/// And the mirror image, which is what makes the test above more than a
/// coincidence: a bookmark the remote has moved past reads as *behind*.
#[test]
fn a_space_whose_remote_moved_on_is_behind_and_not_ahead() {
    let Some(fixture) = JjFixture::new("behind") else {
        return;
    };
    fixture.push_bookmark("main");
    let pushed = fixture.commit_at("main@origin");

    // Push a second commit, then rewind the *local* bookmark to the first: the
    // remote now holds one change this bookmark does not.
    std::fs::write(fixture.root.join("a.txt"), "upstream work\n").expect("could not write");
    fixture.jj(&["describe", "-m", "second"]);
    fixture.jj(&["bookmark", "set", "main", "-r", "@"]);
    fixture.jj(&["git", "push", "--bookmark", "main"]);
    fixture.jj(&[
        "bookmark",
        "set",
        "main",
        "-r",
        &pushed,
        "--allow-backwards",
    ]);

    // Sit the workspace on the rewound bookmark. An empty child is what any
    // workspace parked on a bookmark looks like.
    fixture.jj(&["new", "main"]);

    let spaces = fixture.backend().spaces().expect("listing spaces works");
    let status = &space_named(&spaces, "default").status;

    assert_eq!(
        status.remote,
        RemoteState::Tracked {
            ahead: 0,
            behind: 1
        },
        "jj's counts are the remote's; they must arrive swapped"
    );
    assert!(!status.remote.has_unpushed_work());
}

/// A space one unpushed commit past its remote bookmark.
///
/// Shared by the two tests above because building it is four jj commands of
/// setup that say nothing about what is being asserted.
fn ahead_space(fixture: &JjFixture) -> Space {
    std::fs::write(fixture.root.join("ahead.txt"), "new work\n").expect("could not write");
    fixture.jj(&["describe", "-m", "ahead"]);
    fixture.jj(&["bookmark", "set", "main", "-r", "@"]);
    // Start a fresh empty change, as any further work in the space would.
    fixture.jj(&["new"]);

    let spaces = fixture.backend().spaces().expect("listing spaces works");
    space_named(&spaces, "default").clone()
}

// --------------------------------------------------------------------------
// Not implemented yet
// --------------------------------------------------------------------------

/// `fetch` is shanti-nhe.6 and deliberately unimplemented. It must fail loudly
/// and name the issue rather than quietly reporting success, which would make
/// the UI claim the status it shows is fresh.
#[test]
fn fetching_is_not_implemented_and_says_so() {
    let Some(fixture) = JjFixture::new("no-fetch") else {
        return;
    };

    let error = fixture
        .backend()
        .fetch()
        .expect_err("fetch must not silently succeed")
        .to_string();
    assert!(error.contains("shanti-nhe.6"), "unhelpful error: {error}");
}
