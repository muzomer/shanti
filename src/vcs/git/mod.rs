//! The git backend.
//!
//! This module is the only place in shanti that is allowed to name a `git2`
//! type: everything it exposes is either shanti's own domain model
//! ([`Space`](crate::vcs::Space), [`Repo`](crate::vcs::Repo)) or a wrapper
//! defined here. Keeping the dependency boxed in is what makes a second backend
//! (jujutsu) possible without touching the UI.

mod backend;
mod discovery;
mod worktree;

pub use backend::GitBackend;
// `list_repositories_excluding` has no caller yet: wiring it into `app.rs` (so the
// worktrees dir is excluded and nested worktrees stop being rediscovered as
// repositories) is tracked by shanti-gmf.9. Drop this attribute with that change.
#[allow(unused_imports)]
pub use discovery::{list_repositories, list_repositories_excluding, worktrees_of_repositories};
pub use worktree::{delete_worktree, RemoteStatus, Worktree};
