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
pub mod jj;
mod repo;
mod space;
pub mod status;

use std::path::Path;

use color_eyre::eyre;

pub use backend::Backend;
pub use repo::{Repo, RepoId};
pub use space::Space;
pub use status::{LocalState, RemoteState, SpaceStatus};

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
/// run them off the render thread, which the owned return types allow.
pub trait Vcs {
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
}
