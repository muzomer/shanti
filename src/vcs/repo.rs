//! Repository identity: what shanti discovered on disk, and which backend owns it.

use std::path::{Path, PathBuf};

use super::Backend;

/// Stable, opaque identity for a repository.
///
/// Why a newtype instead of passing `PathBuf` around: a [`Space`](super::Space)
/// has to name its owning repo without holding a second copy of the repo's
/// data, and the UI keys caches and selections off that name. Wrapping it makes
/// it impossible to accidentally pass a space name where a repo id is expected,
/// and lets the derivation rule (today: the repo's path) change later without
/// touching every call site.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoId(String);

impl RepoId {
    /// Build an id from an arbitrary string. Callers should prefer
    /// [`RepoId::from_path`] so that ids stay consistent across discovery runs.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Derive the id from the repository's location on disk.
    ///
    /// The path is the only property that is guaranteed unique across the
    /// repositories shanti discovers — two checkouts may share a name, or even
    /// a remote, but not a directory.
    pub fn from_path(path: impl AsRef<Path>) -> Self {
        Self(path.as_ref().to_string_lossy().into_owned())
    }

    /// Borrow the underlying identity, for logging and map lookups.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A repository shanti manages spaces for.
///
/// This is a plain snapshot, deliberately free of any backend handle
/// (`git2::Repository`, a jj process, …): the UI keeps a list of repos alive
/// across frames and across threads, which an open handle would prevent.
/// Anything that needs to *act* on the repository goes through [`Vcs`](super::Vcs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// Stable identity; spaces refer back to their repo by this value.
    pub id: RepoId,
    /// Human-facing name, normally the directory name. Display only — not unique.
    pub name: String,
    /// Root of the repository on disk (the working copy, not the `.git` dir).
    pub path: PathBuf,
    /// Which backend implementation can drive this repository.
    pub backend: Backend,
}

impl Repo {
    /// Convenience constructor that derives [`RepoId`] from `path`, so the two
    /// cannot drift apart.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>, backend: Backend) -> Self {
        let path = path.into();
        Self {
            id: RepoId::from_path(&path),
            name: name.into(),
            path,
            backend,
        }
    }
}
