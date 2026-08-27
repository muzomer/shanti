//! End-to-end tests for the git backend.
//!
//! Worktree creation and deletion are the core of shanti, so these exercise
//! [`GitBackend`] against real repositories on disk rather than mocks. A local
//! *bare* repository plays the part of `origin`, which gives us remote branches,
//! `refs/remotes/origin/HEAD` and upstream tracking with no network access — the
//! suite must stay runnable in CI and offline.
//!
//! These tests also serve as the behavioural baseline for the `src/git/` ->
//! `src/vcs/git/` move: they describe what the backend does, not how.

use std::fs;
use std::path::{Path, PathBuf};

use git2::{BranchType, Repository};
use shanti::config::Config;
use shanti::hooks::HookSettings;
use shanti::vcs::git::GitBackend;
use shanti::vcs::{self, Consequence, DeletionRisk, RemoteState, Space, Vcs};
use tempfile::{tempdir, TempDir};

// --------------------------------------------------------------------------
// Fixtures
// --------------------------------------------------------------------------

/// A repository under test, together with the temporary directory that owns it.
///
/// The `TempDir` is kept alive as a field on purpose: dropping it deletes the
/// repository, and a test that only held the `GitBackend` would be operating on
/// a directory that had already vanished.
struct Fixture {
    _root: TempDir,
    /// Root of the working repository (the "clone").
    repo_path: PathBuf,
    /// Where spaces are created, mirroring `SHANTI_SPACES_DIR`.
    spaces_dir: PathBuf,
    backend: GitBackend,
}

impl Fixture {
    /// A clone of a bare "origin" that carries `main` plus `extra_branches`.
    ///
    /// Cloning (rather than adding a remote by hand) is what gives us a
    /// realistic starting point: real remote-tracking refs and a resolvable
    /// `refs/remotes/origin/HEAD`, which is how the backend finds the default
    /// branch.
    fn with_origin(extra_branches: &[&str]) -> Self {
        let root = tempdir().expect("could not create temporary directory");
        let origin_path = root.path().join("origin.git");
        seed_bare_origin(&origin_path, extra_branches);

        let repo_path = root.path().join("repos").join("project");
        Repository::clone(
            origin_path.to_str().expect("origin path is not UTF-8"),
            &repo_path,
        )
        .expect("could not clone the bare origin");

        Self::open(root, repo_path)
    }

    /// A standalone repository with one commit and no remotes at all.
    fn without_origin() -> Self {
        let root = tempdir().expect("could not create temporary directory");
        let repo_path = root.path().join("repos").join("solo");
        let repo = Repository::init(&repo_path).expect("could not init repository");
        commit_empty(&repo, "initial commit");

        Self::open(root, repo_path)
    }

    fn open(root: TempDir, repo_path: PathBuf) -> Self {
        let spaces_dir = root.path().join("spaces");
        // `run_fetch: false` — the backend's fetch path uses the ssh agent, which
        // is neither available nor needed for a local file-based origin.
        let backend =
            GitBackend::from_path(repo_path.to_str().expect("repo path is not UTF-8"), false)
                .expect("could not open the repository");

        Self {
            _root: root,
            repo_path,
            spaces_dir,
            backend,
        }
    }

    /// Where a space named `name` should be created.
    fn dest(&self, name: &str) -> PathBuf {
        self.spaces_dir.join("project").join(name)
    }

    /// Re-open the repository for assertions.
    ///
    /// The backend keeps its `git2::Repository` private, so verification has to
    /// go through a fresh handle. That is a feature here: it reads the state git
    /// actually persisted rather than whatever the backend cached.
    fn git(&self) -> Repository {
        Repository::open(&self.repo_path).expect("could not re-open the repository")
    }

    fn create(&self, name: &str) -> Space {
        self.backend
            .create_space(name, &self.dest(name))
            .unwrap_or_else(|err| panic!("could not create space '{}': {}", name, err))
    }
}

/// Build a bare repository holding `main` (plus any `extra_branches`), and point
/// its `HEAD` at `main` so clones inherit it as the default branch.
///
/// Commits are written straight into the bare repo through git2's object layer;
/// that avoids needing a second working copy and a push just to seed history.
fn seed_bare_origin(path: &Path, extra_branches: &[&str]) {
    let repo = Repository::init_bare(path).expect("could not init bare origin");
    let main = commit_empty(&repo, "initial commit");
    repo.reference("refs/heads/main", main, true, "seed main")
        .expect("could not create main");
    // Clones read HEAD to decide which branch to check out and where
    // `refs/remotes/origin/HEAD` should point.
    repo.set_head("refs/heads/main")
        .expect("could not point HEAD at main");

    for branch in extra_branches {
        repo.reference(&format!("refs/heads/{}", branch), main, true, "seed branch")
            .unwrap_or_else(|err| panic!("could not create branch '{}': {}", branch, err));
    }
}

/// Commit the repository's current (empty) index, returning the new commit id.
///
/// An empty tree is enough: none of these tests care about file contents, only
/// about refs, worktree registration and directories.
fn commit_empty(repo: &Repository, message: &str) -> git2::Oid {
    let tree_id = repo
        .index()
        .expect("could not open the index")
        .write_tree()
        .expect("could not write the tree");
    let tree = repo.find_tree(tree_id).expect("could not find the tree");
    let signature =
        git2::Signature::now("shanti tests", "tests@example.com").expect("could not sign");

    let head = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = head.iter().collect();

    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .expect("could not commit")
}

// --------------------------------------------------------------------------
// Assertions shared between the deletion tests
// --------------------------------------------------------------------------

fn assert_branch_missing(repo: &Repository, name: &str) {
    assert!(
        repo.find_branch(name, BranchType::Local).is_err(),
        "local branch '{}' should have been deleted with its space",
        name
    );
}

fn assert_registration_missing(repo: &Repository, name: &str) {
    assert!(
        repo.find_worktree(name).is_err(),
        "worktree '{}' should no longer be registered",
        name
    );
}

// --------------------------------------------------------------------------
// Creation
// --------------------------------------------------------------------------

/// Case 1: a matching remote branch exists, so the space must track it.
#[test]
fn create_space_tracks_a_matching_remote_branch() {
    let fixture = Fixture::with_origin(&["feature"]);

    let space = fixture.create("feature");

    assert_eq!(space.name, "feature");
    // Compare by suffix, not verbatim: git reports canonicalised paths, and on
    // macOS the temp dir arrives back as /private/var/... instead of /var/...
    assert!(
        space.path.ends_with("spaces/project/feature"),
        "unexpected space path {:?}",
        space.path
    );
    assert!(space.exists(), "the space directory should exist on disk");

    let repo = fixture.git();
    let branch = repo
        .find_branch("feature", BranchType::Local)
        .expect("a local branch should have been created");
    let upstream = branch
        .upstream()
        .expect("the local branch should track origin/feature");
    assert_eq!(
        upstream.name().expect("upstream name"),
        Some("origin/feature")
    );
    // Tracking is what the UI reports; without it the space would render as if it
    // had never been pushed.
    assert!(
        matches!(space.status.remote, RemoteState::Tracked { .. }),
        "expected a tracked remote state, got {:?}",
        space.status.remote
    );
}

/// Case 2: no remote branch of that name, so the space starts from the default
/// branch and has no upstream yet.
#[test]
fn create_space_without_a_remote_branch_starts_from_the_default_branch() {
    let fixture = Fixture::with_origin(&[]);

    let space = fixture.create("brand-new");

    assert!(space.exists(), "the space directory should exist on disk");

    let repo = fixture.git();
    let branch = repo
        .find_branch("brand-new", BranchType::Local)
        .expect("a local branch should have been created");
    assert!(
        branch.upstream().is_err(),
        "a branch with no matching remote must not claim an upstream"
    );
    assert!(
        matches!(space.status.remote, RemoteState::Untracked),
        "expected an untracked remote state, got {:?}",
        space.status.remote
    );

    // It must start from origin/main, not from some unrelated tip.
    let default_tip = repo
        .find_branch("origin/main", BranchType::Remote)
        .expect("origin/main should exist")
        .get()
        .peel_to_commit()
        .expect("origin/main should resolve");
    let branch_tip = branch
        .get()
        .peel_to_commit()
        .expect("branch should resolve");
    assert_eq!(branch_tip.id(), default_tip.id());
}

/// Case 3: a repository with no remote at all still gets a working space, based
/// on HEAD.
#[test]
fn create_space_in_a_repository_without_a_remote() {
    let fixture = Fixture::without_origin();

    let space = fixture.create("offline");

    assert!(space.exists(), "the space directory should exist on disk");
    assert!(
        space.path.join(".git").exists(),
        "the space should be a real worktree checkout"
    );

    let repo = fixture.git();
    repo.find_worktree("offline")
        .expect("the worktree should be registered");
    repo.find_branch("offline", BranchType::Local)
        .expect("a local branch should have been created");
    assert!(
        matches!(space.status.remote, RemoteState::Untracked),
        "expected an untracked remote state, got {:?}",
        space.status.remote
    );
}

/// Creating a space also makes it visible through the listing the UI reads.
#[test]
fn a_created_space_is_listed() {
    let fixture = Fixture::with_origin(&["feature"]);
    fixture.create("feature");

    let spaces = fixture
        .backend
        .spaces()
        .expect("listing spaces should work");
    let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["feature"]);
}

// --------------------------------------------------------------------------
// Deletion
// --------------------------------------------------------------------------

/// Create a space, optionally delete its directory behind shanti's back, then
/// ask the backend to delete it.
///
/// The two deletion scenarios differ only in that flag, and each is asserted
/// from two angles — directory plus registration, and the branch — so the
/// setup is shared rather than copied four times.
fn delete_created_space(remove_directory_first: bool) -> (Fixture, Space) {
    let fixture = Fixture::with_origin(&["feature"]);
    let space = fixture.create("feature");

    if remove_directory_first {
        fs::remove_dir_all(&space.path).expect("could not remove the space directory by hand");
    }

    fixture
        .backend
        .delete_space(&space)
        .expect("deleting the space should succeed");

    (fixture, space)
}

/// Case 4: deleting a space removes its directory and its registration under
/// `.git/worktrees`, and it stops being listed.
#[test]
fn delete_space_removes_directory_and_registration() {
    let (fixture, space) = delete_created_space(false);

    assert!(
        !space.path.exists(),
        "the space directory should have been removed"
    );
    assert_registration_missing(&fixture.git(), "feature");
    assert!(
        fixture
            .backend
            .spaces()
            .expect("listing spaces should work")
            .is_empty(),
        "the deleted space should no longer be listed"
    );
}

/// Case 4, continued: the local branch must go with the space, or the user is
/// left with an orphaned branch they never asked to keep.
///
/// Ignored pending shanti-12z.4. The current delete path removes the directory
/// *before* it tries to delete the branch, and it reaches the branch through
/// `Repository::open_from_worktree`, which then fails — so branch deletion is
/// best-effort and never actually happens. Once that ordering is fixed (prune,
/// delete the branch, then remove the directory) this should pass unchanged;
/// the assertion is deliberately not weakened to match today's behaviour.
#[test]
fn delete_space_removes_the_branch() {
    let (fixture, _space) = delete_created_space(false);

    assert_branch_missing(&fixture.git(), "feature");
}

/// Case 5: the directory was removed behind shanti's back (`rm -rf`, another
/// tool). Deletion must still succeed and still drop the registration.
#[test]
fn delete_space_whose_directory_was_already_removed() {
    let (fixture, space) = delete_created_space(true);

    assert!(!space.path.exists());
    assert_registration_missing(&fixture.git(), "feature");
}

/// Case 5, continued: a space whose directory is already gone must still take
/// its branch with it.
///
/// Ignored pending shanti-12z.4 — same cause as
/// [`delete_space_removes_the_branch`]. This is the case that produced the
/// apologetic "best-effort" comment in the delete path.
#[test]
fn delete_space_whose_directory_was_already_removed_removes_the_branch() {
    let (fixture, _space) = delete_created_space(true);

    assert_branch_missing(&fixture.git(), "feature");
}

// --------------------------------------------------------------------------
// The delete guard
// --------------------------------------------------------------------------

/// A space tracking an up-to-date remote branch, with nothing edited, is the one
/// case shanti deletes on a single confirmation.
#[test]
fn a_clean_tracking_space_is_free_to_delete() {
    let fixture = Fixture::with_origin(&["feature"]);
    let space = fixture.create("feature");

    assert_eq!(space.status.remote, RemoteState::in_sync());
    assert!(DeletionRisk::of(&space).is_safe());
}

/// The bug this guard exists for: an edit that lives only in the worktree is
/// destroyed by deletion, and no object store anywhere has a copy.
#[test]
fn a_dirty_space_is_a_permanent_loss() {
    let fixture = Fixture::with_origin(&["feature"]);
    let space = fixture.create("feature");

    // Staged rather than merely written, so this test keeps describing the
    // tracked-file case; the untracked one has a test of its own below.
    let worktree = Repository::open(&space.path).expect("could not open the space");
    fs::write(space.path.join("a.txt"), "work in progress\n").expect("could not write");
    let mut index = worktree.index().expect("could not open the index");
    index
        .add_path(Path::new("a.txt"))
        .expect("could not stage the file");
    index.write().expect("could not write the index");

    let spaces = fixture.backend.spaces().expect("could not list spaces");
    let listed = spaces
        .iter()
        .find(|space| space.name == "feature")
        .expect("the space should be listed");

    let risk = DeletionRisk::of(listed);
    assert!(!risk.is_safe(), "a dirty worktree must not delete freely");
    assert_eq!(risk.consequence(), Consequence::PermanentLoss);
    assert_eq!(risk.losses(), ["uncommitted changes in the working tree"]);

    // The snapshot carries no number; the backend is what supplies one.
    let counted = risk.counting_files(fixture.backend.uncommitted_files(listed));
    assert_eq!(
        counted.losses(),
        ["1 uncommitted file (modified, staged or untracked)"]
    );
}

/// A file the user wrote but never ran `git add` on is destroyed by deletion
/// exactly as thoroughly as a staged one, so it has to be both counted and
/// guarded. Excluding it would produce the worst possible outcome: a confident
/// number that omits the work most likely to be lost.
#[test]
fn an_untracked_file_is_counted_and_guarded() {
    let fixture = Fixture::with_origin(&["feature"]);
    let space = fixture.create("feature");

    fs::write(space.path.join("notes.md"), "never added\n").expect("could not write");

    let spaces = fixture.backend.spaces().expect("could not list spaces");
    let listed = spaces
        .iter()
        .find(|space| space.name == "feature")
        .expect("the space should be listed");

    let risk = DeletionRisk::of(listed).counting_files(fixture.backend.uncommitted_files(listed));
    assert_eq!(risk.consequence(), Consequence::PermanentLoss);
    assert_eq!(
        risk.losses(),
        ["1 uncommitted file (modified, staged or untracked)"]
    );
}

/// The count is the number of *files*, and it agrees with what the dialog says
/// it counted: one edited, one staged, one never added.
#[test]
fn the_count_covers_modified_staged_and_untracked_files() {
    let fixture = Fixture::with_origin(&["feature"]);
    let space = fixture.create("feature");
    let worktree = Repository::open(&space.path).expect("could not open the space");

    // Committed first, so that editing it afterwards is a *modification* rather
    // than a third untracked file.
    fs::write(space.path.join("tracked.txt"), "original\n").expect("could not write");
    let mut index = worktree.index().expect("could not open the index");
    index
        .add_path(Path::new("tracked.txt"))
        .expect("could not stage");
    index.write().expect("could not write the index");
    let tree_id = index.write_tree().expect("could not write the tree");
    let tree = worktree
        .find_tree(tree_id)
        .expect("could not find the tree");
    let signature =
        git2::Signature::now("shanti", "shanti@example.com").expect("could not build a signature");
    let parent = worktree
        .head()
        .and_then(|head| head.peel_to_commit())
        .expect("could not read HEAD");
    worktree
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "add tracked.txt",
            &tree,
            &[&parent],
        )
        .expect("could not commit");
    drop(tree);

    fs::write(space.path.join("tracked.txt"), "edited\n").expect("could not write");
    fs::write(space.path.join("staged.txt"), "staged\n").expect("could not write");
    let mut index = worktree.index().expect("could not open the index");
    index
        .add_path(Path::new("staged.txt"))
        .expect("could not stage");
    index.write().expect("could not write the index");
    fs::write(space.path.join("new.txt"), "never added\n").expect("could not write");

    assert_eq!(fixture.backend.uncommitted_files(&space), Some(3));
}

/// Ignored files are build output, not work. Counting a `target/` would drown
/// the number that matters, which is the number the user is about to lose.
#[test]
fn ignored_files_are_not_counted() {
    let fixture = Fixture::with_origin(&["feature"]);
    let space = fixture.create("feature");

    fs::write(space.path.join(".gitignore"), "build/\n").expect("could not write");
    fs::create_dir(space.path.join("build")).expect("could not create dir");
    fs::write(space.path.join("build/out.o"), "binary\n").expect("could not write");

    // The .gitignore itself is untracked, and is the only thing counted.
    assert_eq!(fixture.backend.uncommitted_files(&space), Some(1));
}

/// A space whose directory is gone cannot be walked. That is "no answer", not
/// "nothing to lose" — the guard's own verdict is what decides safety.
#[test]
fn a_missing_directory_yields_no_count() {
    let fixture = Fixture::with_origin(&["feature"]);
    let space = fixture.create("feature");
    fs::remove_dir_all(&space.path).expect("could not remove the space");

    assert_eq!(fixture.backend.uncommitted_files(&space), None);
}

/// A branch with no upstream is unpushed work even with a spotless working
/// tree: deleting the space deletes the branch with it.
#[test]
fn a_never_pushed_space_is_not_free_to_delete() {
    let fixture = Fixture::without_origin();
    let space = fixture.create("feature");

    let risk = DeletionRisk::of(&space);
    assert!(!risk.is_safe());
    assert_eq!(risk.losses(), ["a branch that was never pushed"]);
    // `delete_space` really does delete refs/heads/<name>, so the dialog is
    // entitled to promise it — see `delete_space_removes_the_branch`.
    assert_eq!(
        risk.removals(),
        [
            "the worktree directory and its registration",
            "the branch it has checked out"
        ]
    );
}

// --------------------------------------------------------------------------
// Post-create hooks
// --------------------------------------------------------------------------

/// The whole point of the feature, end to end: a space that is *usable* the
/// moment it appears. An ignored `.env` is carried over from the repository and
/// a command runs in the new directory, against a real git worktree.
#[test]
fn creation_runs_the_configured_hooks_in_the_new_space() {
    let fixture = Fixture::with_origin(&[]);
    fs::write(fixture.repo_path.join(".env"), "TOKEN=1").expect("could not write .env");

    let settings = HookSettings::from_config(
        toml::from_str(
            r#"
            [hooks]
            copy = [".env"]

            [repos.project.hooks]
            run = ["cp .env .env.local"]
            "#,
        )
        .expect("the test configuration should parse"),
    );

    let (space, plan) = vcs::create_space_with_hooks(
        &fixture.backend,
        "feature",
        &fixture.dest("feature"),
        &settings,
    )
    .expect("creation should succeed");

    // The values the interface promises, taken from the space that was created.
    assert_eq!(plan.target.space_path, space.path);
    assert_eq!(plan.target.space_name, "feature");
    // By suffix, for the same reason as elsewhere here: git hands back the
    // canonicalised path, which on macOS is /private/var/… not /var/….
    assert!(
        plan.target.repo_path.ends_with("repos/project"),
        "unexpected repo path {:?}",
        plan.target.repo_path
    );
    assert_eq!(plan.target.repo_name, "project");
    assert_eq!(plan.target.backend, shanti::vcs::Backend::Git);

    let report = plan.run();
    assert!(!report.failed(), "{:?}", report.outcomes);
    assert_eq!(
        fs::read_to_string(space.path.join(".env")).expect("the copy should have landed"),
        "TOKEN=1"
    );
    assert!(space.path.join(".env.local").is_file());
}

/// The failure policy, end to end: the worktree git created survives a hook
/// that fails, stays registered, and the failure is reported rather than lost.
#[test]
fn a_failing_hook_leaves_a_real_space_intact() {
    let fixture = Fixture::with_origin(&[]);
    let settings = HookSettings::from_config(
        toml::from_str("[hooks]\nrun = [\"echo nope >&2; exit 2\"]\n")
            .expect("the test configuration should parse"),
    );

    let (space, plan) = vcs::create_space_with_hooks(
        &fixture.backend,
        "feature",
        &fixture.dest("feature"),
        &settings,
    )
    .expect("creation should succeed even though the hook will fail");

    let report = plan.run();
    assert!(report.failed());
    assert!(report
        .summary()
        .expect("a failure has a summary")
        .contains("intact"));

    // The space is exactly as `create_space` left it: on disk and listed.
    assert!(space.path.is_dir());
    assert!(fixture
        .backend
        .spaces()
        .expect("could not list spaces")
        .iter()
        .any(|s| s.name == "feature"));
}

/// Nothing configured must mean nothing spawned — the plan is empty, so a caller
/// can skip the worker round trip entirely.
#[test]
fn creation_without_configured_hooks_plans_nothing() {
    let fixture = Fixture::with_origin(&[]);
    let (_space, plan) = vcs::create_space_with_hooks(
        &fixture.backend,
        "feature",
        &fixture.dest("feature"),
        &HookSettings::from_config(Config::default()),
    )
    .expect("creation should succeed");
    assert!(plan.is_empty());
}
