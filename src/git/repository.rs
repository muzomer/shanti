use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use git2::{Cred, RemoteCallbacks};
use rayon::prelude::*;
use std::{
    ffi::OsStr,
    fs::{self, read_dir},
    path::{Path, PathBuf},
};
use tracing::{debug, error};

use super::RemoteStatus;

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

pub struct Repository {
    inner: git2::Repository,
    /// Derived once at open time: `name()` is called per repository per frame,
    /// so it must not redo path surgery (or allocate) on every call.
    name: String,
}
impl Repository {
    pub fn from_path(path: &str, run_fetch: bool) -> eyre::Result<Self> {
        let repo = git2::Repository::open(path)
            .wrap_err_with(|| format!("Could not open repository at {}", path))?;
        if run_fetch {
            // git fetch --prune
            if let Err(err) = repo.remotes().map(|remotes| {
                remotes.iter().for_each(|remote| {
                    if let Some(name) = remote {
                        if let Err(e) = fetch_with_prune(&repo, name) {
                            debug!("Could not fetch from remote. Error: {}", e);
                        }
                    }
                });
            }) {
                debug!("Could not fetch from remotes. Error: {}", err);
            }
        }
        let name = repository_name(&repo);
        Ok(Self { inner: repo, name })
    }
    pub fn create_new_worktree(
        &self,
        worktree_name: &str,
        worktrees_dir: &str,
    ) -> eyre::Result<super::Worktree> {
        let repo_worktrees_dir = PathBuf::from(worktrees_dir).join(self.name_str());
        let new_worktree_dir = PathBuf::from(&repo_worktrees_dir).join(worktree_name);

        fs::create_dir_all(&repo_worktrees_dir).wrap_err_with(|| {
            format!(
                "Could not create worktrees directory {:?}",
                repo_worktrees_dir
            )
        })?;

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
            .worktree(
                worktree_name,
                new_worktree_dir.as_path(),
                Some(&create_worktree_options),
            )
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
        Ok(super::Worktree {
            git_worktree: created_worktree,
            remote_status,
            is_dirty: false,
        })
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

    /// Returns a human-readable description of which branch a new worktree would be based on.
    pub fn resolve_base_branch(&self, worktree_name: &str) -> String {
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

    /// Borrowed repository name — prefer this over [`Repository::name`] in hot paths.
    pub fn name_str(&self) -> &str {
        &self.name
    }

    pub fn name(&self) -> String {
        self.name.clone()
    }

    pub fn worktrees(&self) -> Vec<super::Worktree> {
        let mut git_worktrees: Vec<super::Worktree> = Vec::new();
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

                            git_worktrees.push(super::Worktree {
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
pub fn list_repositories(path: &str, run_fetch: bool) -> Vec<Repository> {
    debug!("Listing repositories in: {}", path);
    find_git_dirs(Path::new(path))
        .par_iter()
        .filter_map(|dir| match Repository::from_path(dir, run_fetch) {
            Ok(repo) => Some(repo),
            Err(err) => {
                error!("Could not open repository at {}: {}", dir, err);
                None
            }
        })
        .collect()
}

pub fn worktrees_of_repositories(repositories: &[Repository]) -> Vec<super::Worktree> {
    let mut worktrees: Vec<super::Worktree> = Vec::new();
    repositories.iter().for_each(|repo| {
        worktrees.append(&mut repo.worktrees());
    });
    worktrees
}

fn is_git_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    match read_dir(dir) {
        Ok(entries) => {
            let mut result = false;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.file_name() == Some(OsStr::new(".git")) {
                    result = true;
                    break;
                }
            }
            result
        }
        Err(err) => {
            error!("Could not read the directory {}: {}", dir.display(), err);
            false
        }
    }
}

fn find_git_dirs(path: &Path) -> Vec<String> {
    let mut git_dirs: Vec<String> = vec![];

    if !path.is_dir() {
        return git_dirs;
    }

    if is_git_dir(path) {
        debug!("Found git repository at: {:?}", path);
        git_dirs.push(path.to_path_buf().display().to_string());
        return git_dirs;
    }

    match read_dir(path) {
        Err(err) => {
            error!("Could not read the directory {}: {}", path.display(), err);
            git_dirs
        }
        Ok(entries) => {
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }

                if is_git_dir(&entry.path()) {
                    debug!("Found git repository at: {:?}", entry.path());
                    git_dirs.push(entry.path().to_path_buf().display().to_string());
                } else {
                    debug!(
                        "No git repository found at: {:?}, continuing search",
                        entry.path()
                    );
                    let sub_entries = find_git_dirs(&entry.path());
                    git_dirs.extend(sub_entries);
                }
            }
            git_dirs
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_not_git_dir() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        assert!(
            !is_git_dir(temp_dir.path()),
            "Expected is_git_dir to be false, but it was true"
        );
    }

    #[test]
    fn test_git_dir() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        fs::DirBuilder::new()
            .create(temp_dir.path().join(".git"))
            .expect("Could not create .git directory inside the temporary dir");

        assert!(
            is_git_dir(temp_dir.path()),
            "Expected is_git_dir to be true, but it was false"
        );
    }

    #[test]
    fn test_list() {
        let temp_dir = tempdir().expect("Could not create temporary directory");

        for path in [
            "first_git_dir/.git",
            "second_git_dir/.git",
            "third_git_dir/subdir/subdir/",
            "fourth_git_dir/subdir/subdir/.git",
        ] {
            fs::DirBuilder::new()
                .recursive(true)
                .create(temp_dir.path().join(path))
                .unwrap_or_else(|_| {
                    panic!(
                        "Could not create {} directory inside the temporary dir",
                        path
                    )
                });
        }

        for path in [
            "first_git_dir",
            "second_git_dir",
            "fourth_git_dir/subdir/subdir",
        ] {
            let expected_dir = temp_dir.path().join(path);
            assert!(
                find_git_dirs(temp_dir.path())
                    .iter()
                    .any(|dir| dir == expected_dir.to_str().unwrap()),
                "Expected {} to be listed in the git subdirectories, but it was not included",
                expected_dir.to_str().unwrap()
            )
        }
    }

    #[test]
    fn test_name_of_normal_repository() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let path = temp_dir.path().join("my-repo");
        git2::Repository::init(&path).expect("Could not init repository");

        let repo = Repository::from_path(path.to_str().unwrap(), false).expect("Could not open");
        assert_eq!(repo.name(), "my-repo");
        assert_eq!(repo.name_str(), "my-repo");
    }

    #[test]
    fn test_name_ignores_trailing_separator() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let path = temp_dir.path().join("my-repo");
        git2::Repository::init(&path).expect("Could not init repository");

        let with_slash = format!("{}/", path.to_str().unwrap());
        let repo = Repository::from_path(&with_slash, false).expect("Could not open");
        assert_eq!(repo.name(), "my-repo");
    }

    #[test]
    fn test_name_keeps_dots_in_the_directory_name() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let path = temp_dir.path().join("my.repo.v2");
        git2::Repository::init(&path).expect("Could not init repository");

        let repo = Repository::from_path(path.to_str().unwrap(), false).expect("Could not open");
        assert_eq!(repo.name(), "my.repo.v2");
    }

    #[test]
    fn test_name_of_bare_repository() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let path = temp_dir.path().join("bare-repo.git");
        git2::Repository::init_bare(&path).expect("Could not init bare repository");

        let repo = Repository::from_path(path.to_str().unwrap(), false).expect("Could not open");
        assert_eq!(repo.name(), "bare-repo");
    }
}
