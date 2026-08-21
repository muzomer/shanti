//! A git worktree, as the UI still sees it today.
//!
//! [`Worktree`] is the pre-[`Space`](crate::vcs::Space) representation: it keeps
//! a live `git2::Worktree` handle, which is exactly what the domain model
//! forbids. It survives until the UI is ported to `Space` (shanti-12z.6); new
//! code should go through [`GitBackend`](super::GitBackend) instead.

use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use git2::Repository;
use std::fs;
use tracing::debug;

#[derive(Clone, Copy)]
pub enum RemoteStatus {
    /// Upstream is configured and the remote tracking ref exists.
    Exists,
    /// Upstream was configured but the remote tracking ref is gone (merged/deleted).
    Gone,
    /// No upstream has ever been configured (never pushed).
    NeverPushed,
}

pub struct Worktree {
    pub git_worktree: git2::Worktree,
    pub remote_status: RemoteStatus,
    pub is_dirty: bool,
}
impl Worktree {
    pub fn path(&self) -> &str {
        self.git_worktree
            .path()
            .to_str()
            .expect("Could not get worktree path")
    }

    pub fn name(&self) -> &str {
        self.git_worktree
            .name()
            .expect("Could not get worktree name")
    }
}

pub fn delete_worktree(worktree: &Worktree) -> eyre::Result<()> {
    remove_worktree(&worktree.git_worktree, worktree.name())
}

/// Remove `git_worktree`: its directory, its registration, and its branch.
///
/// Shared by the legacy [`delete_worktree`] and by
/// [`GitBackend::delete_space`](super::GitBackend), so the two can never drift
/// apart while both exist. `name` is passed in because it is only readable from
/// the handle while the worktree is still registered.
pub(super) fn remove_worktree(git_worktree: &git2::Worktree, name: &str) -> eyre::Result<()> {
    let worktree_path = git_worktree.path();
    if worktree_path.exists() {
        fs::remove_dir_all(worktree_path)
            .wrap_err_with(|| format!("Failed to remove worktree directory for '{}'", name))?;
    }

    git_worktree
        .prune(None)
        .wrap_err_with(|| format!("Failed to prune worktree '{}'", name))?;

    // Branch deletion is best-effort: open_from_worktree can fail if the
    // worktree's gitdir is in an inconsistent state. The directory and git
    // reference are already removed above, so log and move on.
    match Repository::open_from_worktree(git_worktree) {
        Err(e) => debug!(
            "Could not open repo to delete branch for worktree '{}': {}",
            name, e
        ),
        Ok(repo) => match repo.head() {
            Err(e) => debug!("Could not get HEAD for worktree '{}': {}", name, e),
            Ok(mut head) => {
                if head.is_branch() {
                    if let Err(e) = head.delete() {
                        debug!("Could not delete branch for worktree '{}': {}", name, e);
                    }
                } else {
                    debug!(
                        "HEAD of worktree '{}' is not a branch, skipping branch deletion",
                        name
                    );
                }
            }
        },
    }

    Ok(())
}
