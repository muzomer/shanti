//! [`GitBackend`]: the [`Vcs`] implementation backed by `git2`.

use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use git2::{Cred, RemoteCallbacks};
use std::{fs, path::Path};
use tracing::{debug, error};

use crate::vcs::{Backend, RemoteState, Repo, Space, SpaceStatus, SpaceTip, Vcs};

use super::worktree::remove_worktree;

/// How `branch` relates to its upstream, counts included.
///
/// The counts are what let the renderer distinguish "ahead", "behind" and
/// "diverged" instead of collapsing all three onto one "has an upstream" tick,
/// and they are the git half of the vocabulary jj already speaks.
fn remote_state_of_branch(repo: &git2::Repository, branch: &git2::Branch) -> RemoteState {
    let Some(refname) = branch.get().name() else {
        return RemoteState::Untracked;
    };
    if repo.branch_upstream_name(refname).is_err() {
        return RemoteState::Untracked;
    }
    // Configured, but the tracking ref itself has gone (merged or deleted).
    let Ok(upstream) = branch.upstream() else {
        return RemoteState::Gone;
    };

    let counts = branch
        .get()
        .target()
        .zip(upstream.get().target())
        .and_then(|(local, remote)| repo.graph_ahead_behind(local, remote).ok());
    match counts {
        Some((ahead, behind)) => RemoteState::Tracked {
            ahead: ahead as u32,
            behind: behind as u32,
        },
        // The upstream exists but the walk failed (a corrupt or partial object
        // store). "Unknown" says so; "in sync" would be a guess dressed up as a
        // fact.
        None => RemoteState::Unknown,
    }
}

/// The commit `branch` points at, as the backend-neutral [`SpaceTip`].
///
/// Free of extra I/O: the branch is already open here, and peeling it reads
/// objects the repository has in hand. Every failure — a branch with no target,
/// an unborn branch on a freshly created worktree — is `None` rather than an
/// error, because a space with no readable head is still a space worth listing.
fn tip_of_branch(branch: &git2::Branch) -> Option<SpaceTip> {
    let commit = branch.get().peel_to_commit().ok()?;
    // `summary` is git's own first line, already stripped of the trailing
    // newline; a message that is not valid UTF-8 reads as no subject rather
    // than as lossy mojibake.
    let subject = commit.summary().unwrap_or_default();
    Some(SpaceTip::new(subject, commit.time().seconds()))
}

/// How many files in the worktree at `worktree_path` hold work no commit has,
/// or `None` when the question could not be answered.
///
/// **What is counted, and why:** every file `git status` would list — modified,
/// staged, *and* untracked — with ignored files and submodules left out.
/// Deletion removes the directory, so an untracked file is destroyed exactly as
/// thoroughly as a modified one; a count that quietly left it out would be worse
/// than no count, because the user would trust it. Ignored files are excluded
/// for the same reason inverted: they are build output the user does not think
/// of as work, and counting a `target/` would drown the number that matters.
/// Untracked *directories* count as one entry each (git2's default), which is
/// what `git status` shows and what keeps a fresh `node_modules` from reading as
/// forty thousand losses.
///
/// This answers the git question and only the git question. It used to answer
/// `false` for anything holding a `.jj` directory, because the old status model
/// had no way to say "this space is driven by jj" — so it said "clean", which
/// was a lie rather than an absence. jj spaces now carry their own state (see
/// [`crate::vcs::LocalState`]) and never reach this function: discovery hands
/// every colocated repository to the jj backend.
fn count_uncommitted(worktree_path: &Path) -> Option<u32> {
    let repo = git2::Repository::open(worktree_path).ok()?;
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    // An untracked directory is one entry rather than one per file inside it —
    // the reading a user recognises from `git status`.
    opts.recurse_untracked_dirs(false);
    opts.include_ignored(false);
    opts.exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts)).ok()?;
    Some(statuses.len() as u32)
}

/// Whether the git worktree at `worktree_path` holds work no commit has.
///
/// Deliberately the same walk as [`count_uncommitted`], reduced to a yes/no:
/// two independent dirty-checks would eventually disagree, and the one that
/// guards deletion has to agree with the number the dialog prints. A worktree we
/// cannot open reads as clean here, as it always has — the delete guard treats
/// an unprobed *space* cautiously, which is a different question.
fn is_worktree_dirty(worktree_path: &str) -> bool {
    count_uncommitted(Path::new(worktree_path)).is_some_and(|files| files > 0)
}

/// Name a repository by its directory, not by string surgery on the git dir.
///
/// A non-bare repository is named after its working directory; a bare one has no
/// working directory, so we fall back to the git dir and drop the conventional
/// `.git` suffix (`/srv/shanti.git` -> `shanti`). Only that trailing suffix is
/// stripped, so a directory that merely contains a dot keeps its full name.
fn repository_name(repo: &git2::Repository) -> String {
    match repo.workdir() {
        Some(workdir) => directory_name(workdir),
        None => {
            let name = directory_name(repo.path());
            name.strip_suffix(".git").unwrap_or(&name).to_string()
        }
    }
}

/// Last real component of `path`, lossily decoded so non-UTF-8 paths cannot panic.
/// `Path::file_name` already ignores trailing separators and `.` components.
fn directory_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// A single git repository, and everything shanti can do with it.
///
/// The open `git2::Repository` never leaves this type: callers get the owned
/// snapshots of [`crate::vcs`] instead, which is what lets the UI hold on to
/// them across frames.
pub struct GitBackend {
    inner: git2::Repository,
    /// Backend-neutral snapshot of this repository. Derived once at open time:
    /// `name()` is called per repository per frame, so it must not redo path
    /// surgery (or allocate) on every call.
    repo: Repo,
}
impl GitBackend {
    pub fn from_path(path: &str, run_fetch: bool) -> eyre::Result<Self> {
        let inner = git2::Repository::open(path)
            .wrap_err_with(|| format!("Could not open repository at {}", path))?;
        // A bare repository has no working directory; its git dir *is* its root.
        let root = inner
            .workdir()
            .unwrap_or_else(|| inner.path())
            .to_path_buf();
        let repo = Repo::new(repository_name(&inner), root, Backend::Git);
        let backend = Self { inner, repo };

        if run_fetch {
            // A repository that cannot reach its remotes is still worth listing,
            // so a failed fetch only costs the user a stale view of the remotes.
            if let Err(err) = backend.fetch() {
                debug!("Could not fetch from remotes. Error: {}", err);
            }
        }
        Ok(backend)
    }

    /// Borrowed repository name. Allocation-free, so it is safe in the render
    /// path; there is deliberately no owned counterpart to reach for by mistake.
    pub fn name_str(&self) -> &str {
        &self.repo.name
    }

    /// Returns a human-readable description of which branch a new space would be
    /// based on.
    ///
    /// Inherent as well as part of [`Vcs`] so the create prompt can call it
    /// without importing the trait; the trait implementation delegates here.
    pub fn resolve_base(&self, worktree_name: &str) -> String {
        let remote_branch_name = format!("origin/{}", worktree_name);
        if self
            .inner
            .find_branch(&remote_branch_name, git2::BranchType::Remote)
            .is_ok()
        {
            return format!("Will track {}", remote_branch_name);
        }
        if let Some(default_name) = self.find_default_branch_name() {
            return format!("Will be created from {} (default branch)", default_name);
        }
        "Will be created from HEAD".to_string()
    }

    /// Returns the short name of the default remote branch (e.g. "main"), by checking
    /// `refs/remotes/origin/HEAD` first, then falling back to common names.
    fn find_default_branch_name(&self) -> Option<String> {
        if let Ok(head_ref) = self.inner.find_reference("refs/remotes/origin/HEAD") {
            if let Ok(resolved) = head_ref.resolve() {
                if let Some(name) = resolved.shorthand() {
                    let short = name.strip_prefix("origin/").unwrap_or(name).to_string();
                    return Some(short);
                }
            }
        }
        for default in &["main", "master"] {
            let remote_name = format!("origin/{}", default);
            if self
                .inner
                .find_branch(&remote_name, git2::BranchType::Remote)
                .is_ok()
            {
                return Some(default.to_string());
            }
        }
        None
    }

    /// Create a worktree for `worktree_name` at `dest`, creating the parent
    /// directory if it does not exist yet.
    ///
    /// The base is resolved in three steps: a remote branch of the same name, the
    /// repository's default branch, and finally HEAD (by letting git pick).
    fn add_worktree(&self, worktree_name: &str, dest: &Path) -> eyre::Result<Space> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .wrap_err_with(|| format!("Could not create worktrees directory {:?}", parent))?;
        }

        // If a remote branch with the same name exists, base the new worktree on it.
        // Otherwise fall back to the repository's default branch, then HEAD.
        let remote_branch_name = format!("origin/{}", worktree_name);
        let local_branch = if let Ok(remote_branch) = self
            .inner
            .find_branch(&remote_branch_name, git2::BranchType::Remote)
        {
            let commit = remote_branch.get().peel_to_commit().wrap_err_with(|| {
                format!("Could not resolve remote branch '{}'", remote_branch_name)
            })?;
            let branch = match self
                .inner
                .find_branch(worktree_name, git2::BranchType::Local)
            {
                Ok(existing) => existing,
                Err(_) => {
                    let mut new_branch = self
                        .inner
                        .branch(worktree_name, &commit, false)
                        .wrap_err_with(|| {
                            format!(
                                "Could not create local branch '{}' from remote",
                                worktree_name
                            )
                        })?;
                    new_branch
                        .set_upstream(Some(&remote_branch_name))
                        .wrap_err_with(|| {
                            format!("Could not set upstream for branch '{}'", worktree_name)
                        })?;
                    new_branch
                }
            };
            Some(branch)
        } else {
            // No matching remote branch — base on the default branch if available
            self.find_default_branch_name().and_then(|default_name| {
                let remote_name = format!("origin/{}", default_name);
                let default_branch = self
                    .inner
                    .find_branch(&remote_name, git2::BranchType::Remote)
                    .ok()?;
                let commit = default_branch.get().peel_to_commit().ok()?;
                self.inner.branch(worktree_name, &commit, false).ok()
            })
        };

        let mut create_worktree_options = git2::WorktreeAddOptions::new();
        create_worktree_options.checkout_existing(true);
        if let Some(ref branch) = local_branch {
            create_worktree_options.reference(Some(branch.get()));
        }

        let created_worktree = self
            .inner
            .worktree(worktree_name, dest, Some(&create_worktree_options))
            .wrap_err_with(|| format!("Could not create worktree '{}'", worktree_name))?;

        let branch = self
            .inner
            .find_branch(worktree_name, git2::BranchType::Local)
            .wrap_err_with(|| {
                format!(
                    "Could not find branch '{}' after creating worktree",
                    worktree_name
                )
            })?;

        Ok(self.space_of(worktree_name, created_worktree.path(), Some(&branch)))
    }

    /// Translate a git worktree into the backend-neutral snapshot.
    ///
    /// `branch` is the local branch the worktree has checked out, when there is
    /// one; a detached worktree has no upstream to describe, so it reads as
    /// untracked rather than as an error.
    fn space_of(&self, name: &str, path: &Path, branch: Option<&git2::Branch>) -> Space {
        let remote = branch.map_or(RemoteState::Untracked, |branch| {
            remote_state_of_branch(&self.inner, branch)
        });
        let dirty = is_worktree_dirty(&path.to_string_lossy());
        Space::new(
            self.repo.id.clone(),
            Backend::Git,
            name,
            path,
            SpaceStatus::git(remote, dirty),
        )
        .with_tip(branch.and_then(tip_of_branch))
    }
}

impl Vcs for GitBackend {
    fn repo(&self) -> &Repo {
        &self.repo
    }

    /// Only *linked* worktrees are spaces. The repository's own working copy is
    /// not one — shanti did not create it, and `git worktree list`'s inclusion
    /// of it is a listing convenience, not a statement that it is disposable.
    fn spaces(&self) -> eyre::Result<Vec<Space>> {
        let names = self
            .inner
            .worktrees()
            .wrap_err_with(|| format!("Could not list the worktrees of {}", self.repo.name))?;

        Ok(names
            .iter()
            .flatten()
            .filter_map(|name| {
                let worktree = match self.inner.find_worktree(name) {
                    Ok(worktree) => worktree,
                    Err(error) => {
                        // A registration we cannot open is one broken space, not
                        // a broken repository; the rest still list.
                        error!("Could not open the worktree {}: {}", name, error);
                        return None;
                    }
                };
                let branch = self.inner.find_branch(name, git2::BranchType::Local).ok();
                Some(self.space_of(name, worktree.path(), branch.as_ref()))
            })
            .collect())
    }

    fn create_space(&self, name: &str, dest: &Path) -> eyre::Result<Space> {
        self.add_worktree(name, dest)
    }

    fn delete_space(&self, space: &Space) -> eyre::Result<()> {
        let worktree = self
            .inner
            .find_worktree(&space.name)
            .wrap_err_with(|| format!("Could not find worktree '{}'", space.name))?;
        remove_worktree(&self.inner, &worktree, &space.name)
    }

    fn fetch(&self) -> eyre::Result<()> {
        let remotes = self
            .inner
            .remotes()
            .wrap_err("Could not list the repository's remotes")?;
        for remote in remotes.iter().flatten() {
            // One unreachable remote must not hide the others, so per-remote
            // failures are logged rather than propagated.
            if let Err(err) = fetch_with_prune(&self.inner, remote) {
                debug!("Could not fetch from remote. Error: {}", err);
            }
        }
        Ok(())
    }

    fn resolve_base(&self, name: &str) -> String {
        // The inherent method is the implementation; see its doc comment.
        GitBackend::resolve_base(self, name)
    }

    /// See [`count_uncommitted`] for what the number counts.
    ///
    /// Taken from the space's directory rather than from its status snapshot,
    /// because the snapshot only carries a yes/no. That costs one extra status
    /// walk, which is affordable precisely because this is asked once, when a
    /// delete dialog opens — never per frame and never per row.
    fn uncommitted_files(&self, space: &Space) -> Option<u32> {
        count_uncommitted(space.path())
    }
}

fn fetch_with_prune(git_repo: &git2::Repository, remote_name: &str) -> Result<(), git2::Error> {
    let refspecs: Vec<String> = vec![];
    let mut fetch_opts = git2::FetchOptions::new();

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, _allowed_types| {
        Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
    });
    fetch_opts.prune(git2::FetchPrune::On);
    fetch_opts.remote_callbacks(callbacks);
    git_repo
        .find_remote(remote_name)?
        .fetch(&refspecs, Some(&mut fetch_opts), None)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::RepoId;
    use tempfile::tempdir;

    fn open_repo(path: &Path) -> GitBackend {
        git2::Repository::init(path).expect("Could not init repository");
        GitBackend::from_path(path.to_str().unwrap(), false).expect("Could not open")
    }

    #[test]
    fn test_name_of_normal_repository() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let repo = open_repo(&temp_dir.path().join("my-repo"));
        assert_eq!(repo.name_str(), "my-repo");
        assert_eq!(repo.name_str(), "my-repo");
    }

    #[test]
    fn test_name_ignores_trailing_separator() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let path = temp_dir.path().join("my-repo");
        git2::Repository::init(&path).expect("Could not init repository");

        let with_slash = format!("{}/", path.to_str().unwrap());
        let repo = GitBackend::from_path(&with_slash, false).expect("Could not open");
        assert_eq!(repo.name_str(), "my-repo");
    }

    #[test]
    fn test_name_keeps_dots_in_the_directory_name() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let repo = open_repo(&temp_dir.path().join("my.repo.v2"));
        assert_eq!(repo.name_str(), "my.repo.v2");
    }

    #[test]
    fn test_name_of_bare_repository() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let path = temp_dir.path().join("bare-repo.git");
        git2::Repository::init_bare(&path).expect("Could not init bare repository");

        let repo = GitBackend::from_path(path.to_str().unwrap(), false).expect("Could not open");
        assert_eq!(repo.name_str(), "bare-repo");
    }

    /// The repo snapshot is what the UI will key on after shanti-12z.6, so its
    /// identity must match the path the repository was discovered at.
    #[test]
    fn test_repo_snapshot_describes_the_repository() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let backend = open_repo(&temp_dir.path().join("my-repo"));

        let repo = Vcs::repo(&backend);
        assert_eq!(repo.name, "my-repo");
        assert_eq!(repo.backend, Backend::Git);
        // The path git reports may be canonicalised (on macOS /var -> /private/var),
        // so compare by component rather than to `path` verbatim.
        assert!(repo.path.ends_with("my-repo"), "{:?}", repo.path);
        assert_eq!(repo.id, RepoId::from_path(&repo.path));
    }

    /// Exercises the trait through a trait object: that is how the repository
    /// list will hold backends, and it also proves the delegating trait methods
    /// do not call themselves.
    #[test]
    fn test_backend_is_usable_through_the_trait_object() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let backend = open_repo(&temp_dir.path().join("my-repo"));

        let vcs: &dyn Vcs = &backend;
        assert_eq!(vcs.backend(), Backend::Git);
        assert!(vcs.spaces().expect("Listing spaces should work").is_empty());
        assert_eq!(vcs.resolve_base("feature"), "Will be created from HEAD");
    }
}
