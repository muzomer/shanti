//! Opening the git repositories that the backend-neutral walk found.
//!
//! The walk itself now lives in [`crate::vcs::discover`], because deciding which
//! backend owns a directory cannot be a git-only concern. What is left here is
//! the git half: turning the roots git can drive into open [`GitBackend`]s.

use rayon::prelude::*;
use std::path::{Path, PathBuf};
use tracing::{debug, error};

use crate::vcs::discover::{self, Discovered};

use super::{GitBackend, Worktree};

pub fn list_repositories(path: &str, run_fetch: bool) -> Vec<GitBackend> {
    list_repositories_excluding(path, run_fetch, &[])
}

/// Same as [`list_repositories`], but skips anything under `excluded` — pass the
/// worktrees dir here so managed worktrees nested in a repos dir are not
/// rediscovered as repositories of their own.
pub fn list_repositories_excluding(
    path: &str,
    run_fetch: bool,
    excluded: &[PathBuf],
) -> Vec<GitBackend> {
    debug!("Listing repositories in: {}", path);
    discover::discover(Path::new(path), excluded)
        .par_iter()
        .filter(|found| git_can_open(found))
        .filter_map(|found| {
            let dir = found.path.display().to_string();
            match GitBackend::from_path(&dir, run_fetch) {
                Ok(repo) => Some(repo),
                Err(err) => {
                    error!("Could not open repository at {}: {}", dir, err);
                    None
                }
            }
        })
        .collect()
}

/// True when git can drive this find, whichever backend *owns* it.
///
/// A colocated repo is owned by jj but still has a real `.git` dir, and until the
/// UI picks a backend per repository (see the note in `git/mod.rs`) dropping
/// those here would make them vanish from the repositories list — including
/// shanti's own colocated checkout. So this is deliberately wider than
/// `backend == Git`: it asks what git *can* open, not what should drive it.
fn git_can_open(found: &Discovered) -> bool {
    found.backend == crate::vcs::Backend::Git || found.path.join(".git").is_dir()
}

pub fn worktrees_of_repositories(repositories: &[GitBackend]) -> Vec<Worktree> {
    let mut worktrees: Vec<Worktree> = Vec::new();
    repositories.iter().for_each(|repo| {
        worktrees.append(&mut repo.git_worktrees());
    });
    worktrees
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::Backend;
    use std::path::PathBuf;

    fn find(path: &str, backend: Backend) -> Discovered {
        Discovered {
            path: PathBuf::from(path),
            backend,
        }
    }

    #[test]
    fn test_jj_native_repository_is_not_handed_to_git() {
        // No `.git` on disk, so git2 could only fail to open it.
        assert!(!git_can_open(&find("/nowhere/jj-only", Backend::Jj)));
    }
}
