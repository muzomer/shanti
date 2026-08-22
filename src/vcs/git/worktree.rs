//! Removing a git worktree, in the order that keeps its branch reachable.
//!
//! Nothing here is part of shanti's domain model: the only caller is
//! [`GitBackend::delete_space`](super::GitBackend), which speaks
//! [`Space`](crate::vcs::Space) on both sides of it.

use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use git2::Repository;
use std::fs;
use tracing::debug;

/// Remove `git_worktree`: its registration, then its branch, then its directory.
///
/// That order is load-bearing, and it is a constraint the two backends share
/// rather than a git quirk: which branch a space sits on can only be read while
/// the space is still registered *and* its working directory still exists, so
/// the directory has to go last. (jj needs `jj workspace forget` before the same
/// removal, for the same reason.) Removing the directory first — as this used to
/// — destroyed the very state the branch lookup needs, which is what made branch
/// deletion look unreliable enough to be downgraded to a debug log.
///
/// `parent` is the repository the worktree belongs to. It is what makes the
/// branch reachable even when the working directory was already removed by hand;
/// the caller is the backend, which always has it open.
///
/// `name` is passed in because it is only readable from the handle while the
/// worktree is still registered.
pub(super) fn remove_worktree(
    parent: &Repository,
    git_worktree: &git2::Worktree,
    name: &str,
) -> eyre::Result<()> {
    // Decide what to delete first: `open_from_worktree` needs both the
    // registration and the working directory, and the next step removes one of
    // them. The handle itself outlives the prune — branches live in the shared
    // ref store, not in the worktree's own gitdir.
    let from_worktree = Repository::open_from_worktree(git_worktree).ok();
    let branch_ref = match &from_worktree {
        Some(repo) => checked_out_branch(repo, name),
        None => {
            // Someone removed the directory by hand. Cleaning up the rest is
            // still the right outcome, so fall back to shanti's own convention:
            // it creates the branch and the worktree under one name.
            debug!(
                "Worktree '{}' has no readable working directory; assuming its branch is '{}'",
                name, name
            );
            Some(format!("refs/heads/{}", name))
        }
    };

    // `valid` is required because the working directory is deliberately still
    // there. A *locked* worktree still refuses to be pruned, which now fails
    // before anything has been destroyed instead of after.
    let mut prune_options = git2::WorktreePruneOptions::new();
    prune_options.valid(true);
    git_worktree
        .prune(Some(&mut prune_options))
        .wrap_err_with(|| format!("Failed to prune worktree '{}'", name))?;

    // With the registration gone, nothing has the branch checked out any more,
    // so this is an ordinary reference delete rather than a best-effort attempt.
    // It goes through the caller's handle: the worktree's own is unusable once
    // the working directory is missing, which is exactly the case that used to
    // leave orphaned branches behind. A detached worktree has no branch of ours
    // to delete, which is the `None` this skips.
    if let Some(refname) = branch_ref {
        delete_branch(parent, &refname, name)?;
    }

    let worktree_path = git_worktree.path();
    if worktree_path.exists() {
        fs::remove_dir_all(worktree_path)
            .wrap_err_with(|| format!("Failed to remove worktree directory for '{}'", name))?;
    }

    Ok(())
}

/// Full ref name of the branch `repo` has checked out, or `None` when its HEAD
/// is detached or unreadable.
fn checked_out_branch(repo: &Repository, name: &str) -> Option<String> {
    match repo.head() {
        Err(e) => {
            debug!("Could not get HEAD for worktree '{}': {}", name, e);
            None
        }
        Ok(head) => {
            if !head.is_branch() {
                debug!(
                    "HEAD of worktree '{}' is not a branch, skipping branch deletion",
                    name
                );
                return None;
            }
            head.name().map(str::to_string)
        }
    }
}

/// Delete `refname` from `repo`.
///
/// A branch that is already gone is not a failure: deleting a space twice, or
/// after deleting its branch by hand, should still converge on "removed". A
/// delete that is attempted and *fails* is now reported instead of logged away.
fn delete_branch(repo: &Repository, refname: &str, name: &str) -> eyre::Result<()> {
    match repo.find_reference(refname) {
        Err(e) => {
            debug!(
                "Branch '{}' of worktree '{}' is already gone: {}",
                refname, name, e
            );
            Ok(())
        }
        Ok(mut reference) => reference.delete().wrap_err_with(|| {
            format!(
                "Failed to delete branch '{}' of worktree '{}'",
                refname, name
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A repository with one commit and a worktree named `feature`.
    fn repo_with_worktree(base: &Path) -> (Repository, git2::Worktree) {
        let repo = Repository::init(base.join("repo")).expect("Could not init repository");
        let signature =
            git2::Signature::now("shanti", "shanti@example.com").expect("Could not sign");
        let tree_id = {
            let mut index = repo.index().expect("Could not open index");
            index.write_tree().expect("Could not write tree")
        };
        let tree = repo.find_tree(tree_id).expect("Could not find tree");
        repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
            .expect("Could not commit");
        drop(tree);

        let spaces = base.join("spaces");
        fs::create_dir_all(&spaces).expect("Could not create spaces directory");
        let git_worktree = repo
            .worktree("feature", &spaces.join("feature"), None)
            .expect("Could not create worktree");
        (repo, git_worktree)
    }

    fn has_branch(repo: &Repository, name: &str) -> bool {
        repo.find_branch(name, git2::BranchType::Local).is_ok()
    }

    fn worktree_names(repo: &Repository) -> Vec<String> {
        repo.worktrees()
            .expect("Could not list worktrees")
            .iter()
            .flatten()
            .map(str::to_string)
            .collect()
    }

    /// The whole point of the ordering: with the directory removed last, the
    /// branch lookup still works, so all three artefacts are gone afterwards.
    #[test]
    fn test_delete_removes_registration_branch_and_directory() {
        let temp_dir = tempfile::tempdir().expect("Could not create temporary directory");
        let (repo, worktree) = repo_with_worktree(temp_dir.path());
        let path = worktree.path().to_path_buf();

        remove_worktree(&repo, &worktree, "feature").expect("Deleting the worktree should succeed");

        assert!(worktree_names(&repo).is_empty(), "registration still there");
        assert!(!has_branch(&repo, "feature"), "branch still there");
        assert!(!path.exists(), "directory still there");
    }

    /// A directory removed by hand must not block the rest of the cleanup.
    #[test]
    fn test_delete_cleans_up_when_the_directory_is_already_gone() {
        let temp_dir = tempfile::tempdir().expect("Could not create temporary directory");
        let (repo, worktree) = repo_with_worktree(temp_dir.path());
        fs::remove_dir_all(worktree.path()).expect("Could not remove directory");

        // The parent handle is what lets the branch be found without the
        // worktree.
        remove_worktree(&repo, &worktree, "feature").expect("Deleting the worktree should succeed");

        assert!(worktree_names(&repo).is_empty(), "registration still there");
        assert!(!has_branch(&repo, "feature"), "branch still there");
    }

    /// Deleting the same space twice converges instead of failing: the branch is
    /// already gone the second time round.
    #[test]
    fn test_delete_is_idempotent_when_the_branch_is_already_gone() {
        let temp_dir = tempfile::tempdir().expect("Could not create temporary directory");
        let (repo, worktree) = repo_with_worktree(temp_dir.path());
        repo.find_reference("refs/heads/feature")
            .expect("Could not find branch")
            .delete()
            .expect("Could not delete branch");

        remove_worktree(&repo, &worktree, "feature").expect("Deleting the worktree should succeed");

        assert!(worktree_names(&repo).is_empty(), "registration still there");
    }
}
