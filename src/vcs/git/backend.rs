//! [`GitBackend`]: the [`Vcs`] implementation backed by `git2`.

use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use git2::{Cred, RemoteCallbacks};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, error};

use crate::vcs::{Backend, LocalState, RemoteState, Repo, Space, SpaceStatus, Vcs};

use super::worktree::remove_worktree;
use super::{RemoteStatus, Worktree};

fn remote_status_of_branch(repo: &git2::Repository, branch: &git2::Branch) -> RemoteStatus {
    let refname = match branch.get().name() {
        Some(n) => n,
        None => return RemoteStatus::NeverPushed,
    };
    match repo.branch_upstream_name(refname) {
        Err(_) => RemoteStatus::NeverPushed,
        Ok(_) => {
            if branch.upstream().is_ok() {
                RemoteStatus::Exists
            } else {
                RemoteStatus::Gone
            }
        }
    }
}

fn is_worktree_dirty(worktree_path: &str) -> bool {
    let path = Path::new(worktree_path);
    if path.join(".jj").exists() {
        return false;
    }
    match git2::Repository::open(path) {
        Ok(repo) => {
            let mut opts = git2::StatusOptions::new();
            opts.include_untracked(false);
            opts.exclude_submodules(true);
            match repo.statuses(Some(&mut opts)) {
                Ok(statuses) => !statuses.is_empty(),
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
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

    /// Borrowed repository name — prefer this over [`GitBackend::name`] in hot paths.
    pub fn name_str(&self) -> &str {
        &self.repo.name
    }

    pub fn name(&self) -> String {
        self.repo.name.clone()
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
    fn add_worktree(&self, worktree_name: &str, dest: &Path) -> eyre::Result<Worktree> {
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

        let remote_status = remote_status_of_branch(&self.inner, &branch);
        Ok(Worktree {
            git_worktree: created_worktree,
            remote_status,
            is_dirty: false,
        })
    }

    /// Where a space named `name` lives under `worktrees_dir`.
    ///
    /// Layout policy that [`Vcs::create_space`] deliberately leaves to the
    /// caller; it stays here only while the UI still calls
    /// [`GitBackend::create_new_worktree`].
    fn worktree_dest(&self, worktrees_dir: &str, name: &str) -> PathBuf {
        PathBuf::from(worktrees_dir)
            .join(self.name_str())
            .join(name)
    }

    /// Legacy entry point kept for the UI, which still speaks in worktrees.
    /// Replaced by [`Vcs::create_space`] in shanti-12z.6.
    pub fn create_new_worktree(
        &self,
        worktree_name: &str,
        worktrees_dir: &str,
    ) -> eyre::Result<Worktree> {
        let dest = self.worktree_dest(worktrees_dir, worktree_name);
        self.add_worktree(worktree_name, &dest)
    }

    /// Legacy listing kept for the UI. Replaced by [`Vcs::spaces`] in shanti-12z.6.
    pub fn git_worktrees(&self) -> Vec<Worktree> {
        let mut git_worktrees: Vec<Worktree> = Vec::new();
        match self.inner.worktrees() {
            Ok(worktrees_arr) => {
                worktrees_arr.iter().for_each(|worktree| {
                    if let Some(worktree_name) = worktree {
                        if let Ok(git_worktree) = self.inner.find_worktree(worktree_name) {
                            let branch = self
                                .inner
                                .find_branch(worktree_name, git2::BranchType::Local);

                            let remote_status = match branch {
                                Ok(ref b) => remote_status_of_branch(&self.inner, b),
                                Err(_) => RemoteStatus::NeverPushed,
                            };

                            let worktree_path =
                                git_worktree.path().to_str().unwrap_or("").to_string();
                            let is_dirty = is_worktree_dirty(&worktree_path);

                            git_worktrees.push(Worktree {
                                git_worktree,
                                remote_status,
                                is_dirty,
                            });
                        }
                    }
                });
            }
            Err(error) => {
                error!("Could not list the worktrees for repository {}", error);
            }
        };
        git_worktrees
    }

    /// Translate a git worktree into the backend-neutral snapshot.
    fn space_of(&self, worktree: &Worktree) -> Space {
        let status = SpaceStatus {
            remote: match worktree.remote_status {
                // The legacy `RemoteStatus` carries no ahead/behind counts, so
                // there is nothing truthful to put here yet. 0/0 renders as the
                // same "in sync" glyph `Exists` already showed, keeping this a
                // pure refactor; computing real counts is shanti-12z.6.
                RemoteStatus::Exists => RemoteState::Tracked {
                    ahead: 0,
                    behind: 0,
                },
                RemoteStatus::Gone => RemoteState::Gone,
                RemoteStatus::NeverPushed => RemoteState::Untracked,
            },
            local: LocalState::Git {
                dirty: worktree.is_dirty,
            },
        };
        Space::new(
            self.repo.id.clone(),
            worktree.name(),
            worktree.path(),
            status,
        )
    }
}

impl Vcs for GitBackend {
    fn repo(&self) -> &Repo {
        &self.repo
    }

    fn spaces(&self) -> eyre::Result<Vec<Space>> {
        // Listing never fails as a whole: a repository whose worktrees cannot be
        // read logs and yields none, which is what the UI has always shown.
        Ok(self
            .git_worktrees()
            .iter()
            .map(|worktree| self.space_of(worktree))
            .collect())
    }

    fn create_space(&self, name: &str, dest: &Path) -> eyre::Result<Space> {
        let worktree = self.add_worktree(name, dest)?;
        Ok(self.space_of(&worktree))
    }

    fn delete_space(&self, space: &Space) -> eyre::Result<()> {
        let worktree = self
            .inner
            .find_worktree(&space.name)
            .wrap_err_with(|| format!("Could not find worktree '{}'", space.name))?;
        remove_worktree(&worktree, &space.name)
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
        assert_eq!(repo.name(), "my-repo");
        assert_eq!(repo.name_str(), "my-repo");
    }

    #[test]
    fn test_name_ignores_trailing_separator() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let path = temp_dir.path().join("my-repo");
        git2::Repository::init(&path).expect("Could not init repository");

        let with_slash = format!("{}/", path.to_str().unwrap());
        let repo = GitBackend::from_path(&with_slash, false).expect("Could not open");
        assert_eq!(repo.name(), "my-repo");
    }

    #[test]
    fn test_name_keeps_dots_in_the_directory_name() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let repo = open_repo(&temp_dir.path().join("my.repo.v2"));
        assert_eq!(repo.name(), "my.repo.v2");
    }

    #[test]
    fn test_name_of_bare_repository() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let path = temp_dir.path().join("bare-repo.git");
        git2::Repository::init_bare(&path).expect("Could not init bare repository");

        let repo = GitBackend::from_path(path.to_str().unwrap(), false).expect("Could not open");
        assert_eq!(repo.name(), "bare-repo");
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
