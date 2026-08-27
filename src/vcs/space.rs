//! The unit of work shanti manages.

use std::path::{Path, PathBuf};

use super::{Backend, RepoId, SpaceStatus, SpaceTip};

/// A checked-out place to work, belonging to exactly one [`Repo`](super::Repo).
///
/// "Space" is deliberately neither "worktree" nor "workspace": it is shanti's
/// own word for the thing the user creates, switches to and deletes. Each
/// backend maps it onto its own concept — a git worktree, a jj workspace — so
/// that no part of the UI has to know which one it is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    /// The repository this space belongs to. An id rather than a `&Repo` so a
    /// list of spaces stays owned, `'static` and cheap to move between threads.
    pub repo: RepoId,
    /// Which backend owns this space — the one that created it and the only one
    /// that can act on it.
    ///
    /// A colocated repository (`.git` *and* `.jj`) is driven by both backends at
    /// once and both are listed, so the repo id alone cannot say where to route
    /// a deletion: a git worktree and a jj workspace of the same repository
    /// share an id, because the id comes from the path. This field is what keeps
    /// a `git worktree remove` from being handed to jj, which knows nothing
    /// about it.
    pub backend: Backend,
    /// User-facing name of the space — in practice the branch or bookmark it
    /// was created for. Unique within a repository.
    pub name: String,
    /// Where the space lives on disk; this is what shanti hands back to the
    /// shell when the user selects it.
    pub path: PathBuf,
    /// Owned snapshot of the space's state, not a live handle, so the UI can
    /// keep spaces across frames and a background refresh can replace them
    /// wholesale later.
    pub status: SpaceStatus,
    /// The last commit made here, when the backend could read one.
    ///
    /// `None` means "not read", never "no commits": a space whose head cannot
    /// be resolved — a brand-new worktree on an unborn branch, a jj listing that
    /// predates the field — still lists, with the detail simply absent.
    pub tip: Option<SpaceTip>,
}

impl Space {
    /// Assemble a space from its parts. Present mostly so backends construct
    /// spaces the same way and future required fields land in one place.
    pub fn new(
        repo: RepoId,
        backend: Backend,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        status: SpaceStatus,
    ) -> Self {
        Self {
            repo,
            backend,
            name: name.into(),
            path: path.into(),
            status,
            tip: None,
        }
    }

    /// The same space, carrying the head commit the backend just read.
    ///
    /// A builder step rather than another argument to [`Space::new`]: the tip is
    /// the one part a backend may legitimately fail to read, and every existing
    /// call site that has no tip to offer should stay unchanged.
    pub fn with_tip(mut self, tip: Option<SpaceTip>) -> Self {
        self.tip = tip;
        self
    }

    /// Whether the space still exists on disk.
    ///
    /// A space can be removed behind shanti's back (`rm -rf`, another tool), and
    /// the snapshot cannot know that on its own.
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// The space's path, for the callers that only need a borrow.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
