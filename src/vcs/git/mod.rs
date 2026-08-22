//! The git backend.
//!
//! This module is the only place in shanti that is allowed to name a `git2`
//! type: everything it exposes is shanti's own domain model
//! ([`Space`](crate::vcs::Space), [`Repo`](crate::vcs::Repo)). Keeping the
//! dependency boxed in is what makes a second backend (jujutsu) possible
//! without touching the UI.

mod backend;
mod worktree;

pub use backend::GitBackend;
