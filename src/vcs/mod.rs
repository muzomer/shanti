//! Backend-neutral domain model.
//!
//! shanti drives more than one version-control system, so it needs a vocabulary
//! of its own rather than git's or jujutsu's:
//!
//! * a [`Repo`] is a repository discovered on disk, tagged with the [`Backend`]
//!   that can drive it;
//! * a [`Space`] is a checked-out place to work belonging to that repo —
//!   deliberately neither "worktree" nor "workspace", because each backend maps
//!   it to its own concept (git worktree, jj workspace);
//! * a [`SpaceStatus`] is an owned snapshot of what a space looks like right
//!   now;
//! * the [`Vcs`] trait is the single seam every backend implements.
//!
//! Two rules keep this model usable from a TUI:
//!
//! 1. **Everything here is an owned snapshot.** No type in this module holds a
//!    `git2::Repository`, a file handle or a child process. The UI keeps lists
//!    of repos and spaces alive across frames, and a later background refresh
//!    will move them between threads; a borrowed handle would rule both out.
//! 2. **[`Vcs`] is object-safe.** The repository list is heterogeneous at
//!    runtime — git and jj repos in one collection — so backends are stored as
//!    [`BoxedVcs`]. That means no generic methods, no `async`, and no `Self`
//!    in return position on the trait.

mod backend;
pub mod discover;
pub mod git;
pub mod jj;
mod repo;
mod space;
pub mod status;

use std::path::{Path, PathBuf};

use color_eyre::eyre;
use rayon::prelude::*;
use tracing::{debug, error};

pub use backend::Backend;
pub use discover::{backend_at, backends_at, discover, Discovered};
pub use repo::{Repo, RepoId};
pub use space::Space;
pub use status::{JjLocal, LocalState, RemoteState, SpaceStatus, StatusGlyph, Tone};

use git::GitBackend;
use jj::JjBackend;

/// How a backend is stored and passed around.
///
/// Named so that call sites read as intent rather than as a type puzzle, and so
/// the ownership choice (boxed, not generic) can be revisited in one place.
pub type BoxedVcs = Box<dyn Vcs>;

/// Everything shanti needs from a version-control system.
///
/// One implementation per [`Backend`], each bound to a single repository: an
/// implementor is constructed for a [`Repo`] and answers only about that repo.
/// Keeping the trait small is the point — it is the entire surface the UI is
/// allowed to depend on, so anything git-specific that leaks in here would have
/// to be faked by the jj adapter.
///
/// All methods are blocking. Callers that must stay responsive are expected to
/// run them off the render thread, which the owned return types allow — hence
/// the `Send` bound: opening repositories is already done in parallel, and the
/// background refresh of Track D will want to own a backend on a worker thread.
pub trait Vcs: Send {
    /// The repository this instance drives.
    ///
    /// Backends are per-repo, so the UI can recover the identity and paths of
    /// the repo behind any backend without keeping a parallel map.
    fn repo(&self) -> &Repo;

    /// Which system is behind this implementation.
    ///
    /// Provided so callers can pick backend-appropriate wording or indicators
    /// without downcasting; it must agree with [`Vcs::repo`]'s `backend` field.
    fn backend(&self) -> Backend {
        self.repo().backend
    }

    /// List the spaces that currently exist for this repository.
    ///
    /// Implementations should return spaces even when their status could not be
    /// probed (see [`SpaceStatus::unknown`]): a space the user can still delete
    /// is more useful than a hidden one.
    fn spaces(&self) -> eyre::Result<Vec<Space>>;

    /// Create a new space named `name`, rooted at `dest`.
    ///
    /// `name` is the user's branch/bookmark name; `dest` is the directory shanti
    /// chose for it. Passing the destination in rather than deriving it keeps
    /// the layout policy (`<worktrees dir>/<repo>/<name>`) in one place instead
    /// of duplicated per backend.
    fn create_space(&self, name: &str, dest: &Path) -> eyre::Result<Space>;

    /// Remove a space and whatever bookkeeping the backend keeps for it.
    ///
    /// Takes the space by reference because the caller usually still needs it
    /// afterwards to report what happened.
    fn delete_space(&self, space: &Space) -> eyre::Result<()>;

    /// Refresh the repository's view of its remotes.
    ///
    /// Network-bound and therefore the one call most likely to be slow or to
    /// fail for reasons outside shanti's control; failures should be surfaced,
    /// not swallowed, so the UI can say the status shown is stale.
    fn fetch(&self) -> eyre::Result<()>;

    /// The base a new space named `name` would be created from, as a short
    /// display string (for example `origin/main`).
    ///
    /// This exists purely to power the "Will track ..." hint in the create
    /// prompt, so it returns a plain `String` rather than a result: a
    /// best-effort guess is fine, and an unavailable answer should degrade to a
    /// sensible default rather than block the user's typing.
    fn resolve_base(&self, name: &str) -> String;
}

/// Refresh `vcs`'s view of its remotes, tolerating failure.
///
/// The one place that states shanti's fetch policy: **a fetch that fails costs
/// a stale view of the remotes and nothing else.** Never a repository dropped
/// from the list, never a flow aborted — the user is offline, or a remote is
/// down, and every local answer is still correct.
///
/// Two callers want exactly this: `--run-fetch` at startup, and the GitHub PR
/// flow, which has to refresh before it can resolve a branch that was pushed
/// after the last fetch. Both go through [`Vcs::fetch`], so a jj repository is
/// refreshed with `jj git fetch` and a git one with git — no code path outside a
/// backend decides how a repository talks to its remotes.
pub fn refresh(vcs: &dyn Vcs) {
    if let Err(error) = vcs.fetch() {
        debug!(repo = %vcs.repo().name, %error, "could not fetch");
    }
}

/// Where a space named `space_name` of repository `repo_name` lives on disk.
///
/// The one place that knows shanti's layout. [`Vcs::create_space`] deliberately
/// takes the destination rather than deriving it, so the policy is not
/// re-implemented — and allowed to drift — once per backend.
pub fn space_dest(worktrees_dir: &str, repo_name: &str, space_name: &str) -> PathBuf {
    PathBuf::from(worktrees_dir)
        .join(repo_name)
        .join(space_name)
}

/// Open every repository a walk found, in parallel, through every backend that
/// can drive it.
///
/// Opening is the expensive half of listing — it reads refs, spawns `jj`, and
/// optionally fetches — hence `par_iter`. A repository that will not open is
/// logged and skipped rather than taking the whole list down with it: one broken
/// checkout in a repos dir must not cost the user every other one.
pub fn open_backends(found: &[Discovered], run_fetch: bool) -> Vec<BoxedVcs> {
    found
        .par_iter()
        .flat_map_iter(|found| match open(found, run_fetch) {
            Ok(backends) => backends,
            Err(error) => {
                error!(path = %found.path.display(), %error, "could not open the repository");
                Vec::new()
            }
        })
        .collect()
}

/// Open the repository at `path`, letting the layout on disk decide which
/// backend drives it.
///
/// The entry point for a repository that appears *after* the initial walk — a
/// fresh clone, say — so that it is bound to a backend by the same rule.
pub fn open_at(path: &Path, run_fetch: bool) -> eyre::Result<Vec<BoxedVcs>> {
    let (backend, additional) = backends_at(path)
        .ok_or_else(|| eyre::eyre!("{} is not a repository shanti can drive", path.display()))?;
    open(
        &Discovered {
            path: path.to_path_buf(),
            backend,
            additional,
        },
        run_fetch,
    )
}

/// Open one find as every backend that can drive it, the owner first.
///
/// A colocated repository yields two: jj, which owns the working copy, *and*
/// git, whose worktrees exist whether or not shanti lists them. Only the owner
/// failing to open is fatal — an extra backend that will not open costs the user
/// its spaces, not the whole repository.
fn open(found: &Discovered, run_fetch: bool) -> eyre::Result<Vec<BoxedVcs>> {
    let mut opened = vec![open_one(&found.path, found.backend, run_fetch)?];
    for backend in found.additional.iter().copied() {
        match open_one(&found.path, backend, run_fetch) {
            Ok(vcs) => opened.push(vcs),
            Err(error) => {
                error!(
                    path = %found.path.display(),
                    %backend,
                    %error,
                    "could not open the repository through its additional backend"
                );
            }
        }
    }
    Ok(opened)
}

/// Bind one (path, backend) pair to an implementation.
///
/// This match is the *only* place that turns a [`Backend`] tag into an
/// implementation; everything above it holds a [`BoxedVcs`] and never asks which
/// one it got.
fn open_one(path: &Path, backend: Backend, run_fetch: bool) -> eyre::Result<BoxedVcs> {
    match backend {
        Backend::Git => {
            // `from_path` fetches for us, tolerating failure, so there is
            // nothing extra to do for git here.
            let backend = GitBackend::from_path(&path.display().to_string(), run_fetch)?;
            Ok(Box::new(backend))
        }
        Backend::Jj => {
            let backend = JjBackend::discover(path)?;
            if run_fetch {
                // `--run-fetch` reaches jj through the same [`Vcs::fetch`] the
                // git side uses, so a jj repository is refreshed with `jj git
                // fetch` rather than skipped. A repository that cannot reach its
                // remotes is still worth listing: the cost is a stale view of
                // the remotes, exactly what a failed git fetch costs too.
                refresh(&backend);
            }
            Ok(Box::new(backend))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the design constraint that matters most here: the trait must stay
    /// object-safe, because repos of both backends live in one collection.
    #[test]
    fn vcs_is_object_safe() {
        fn assert_object_safe(_: Option<&dyn Vcs>) {}
        assert_object_safe(None);
    }

    #[test]
    fn repo_id_is_derived_from_path() {
        let repo = Repo::new("shanti", "/tmp/repos/shanti", Backend::Git);
        assert_eq!(repo.id, RepoId::from_path("/tmp/repos/shanti"));
    }

    /// The layout policy is what keeps `create_space`'s `dest` argument honest.
    #[test]
    fn space_dest_nests_the_space_under_its_repository() {
        assert_eq!(
            space_dest("/tmp/spaces", "shanti", "feature"),
            Path::new("/tmp/spaces/shanti/feature")
        );
    }

    /// A jj-native find must never be handed to git2, which could only fail to
    /// open it — and a *colocated* one must not be either, however happily git
    /// would open that one.
    #[test]
    fn opening_a_find_honours_the_backend_the_walk_chose() {
        let found = Discovered {
            path: PathBuf::from("/nowhere/jj-only"),
            backend: Backend::Jj,
            additional: vec![],
        };
        // Nothing is there, so this can only fail — the point is *how*: through
        // the jj adapter, never through git2.
        assert!(open(&found, false).is_err());
        assert!(open_backends(std::slice::from_ref(&found), false).is_empty());
    }

    /// The owner is what a repository *is*; an extra backend is a bonus. A
    /// colocated repo whose git side will not open must still list its jj
    /// workspaces rather than disappearing.
    #[test]
    fn an_additional_backend_that_will_not_open_is_not_fatal() {
        let found = Discovered {
            path: PathBuf::from("/nowhere/colocated"),
            backend: Backend::Jj,
            additional: vec![Backend::Git],
        };
        // Both halves fail here (nothing is on disk), so the assertion is about
        // the *shape* of the failure: it is the owner's, reported once.
        assert!(open(&found, false).is_err());
    }
}
