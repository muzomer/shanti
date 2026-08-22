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
use shanti::vcs::git::GitBackend;
use shanti::vcs::{Consequence, DeletionRisk, RemoteState, Space, Vcs};
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
    /// Where spaces are created, mirroring `SHANTI_WORKTREES_DIR`.
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

    // Staged rather than merely written: the fixture's history is an empty
    // tree, so an untracked file alone is not what "dirty" means here.
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
}
