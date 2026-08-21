//! Finding repositories on disk, whatever backend drives them.
//!
//! The walk is deliberately dumb and cheap: it decides what *looks* like a
//! repository from the directory layout alone, without opening a `git2`
//! repository or shelling out to `jj`. Discovery runs over every directory
//! below the repos dirs, so it has to stay a few `stat` calls per candidate.
//!
//! It lives above the backends rather than inside `git/` because the layout is
//! the one thing shanti can read without first knowing which backend is in
//! charge — the walk is what *decides* that.

use std::{
    ffi::OsStr,
    fs::{self, read_dir},
    path::{Path, PathBuf},
};
use tracing::{debug, error};

use super::{Backend, Repo};

/// A repository root the walk recognised, and the backend that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// Root of the repository on disk, canonicalised where possible.
    pub path: PathBuf,
    /// The backend shanti must drive this repository through.
    pub backend: Backend,
}

impl Discovered {
    /// Turn the raw find into the domain model, naming the repo after its
    /// directory (with the conventional `.git` suffix of a bare repo dropped).
    pub fn to_repo(&self) -> Repo {
        let name = self
            .path
            .file_name()
            .unwrap_or(self.path.as_os_str())
            .to_string_lossy();
        let name = name.strip_suffix(".git").unwrap_or(&name);
        Repo::new(name, self.path.clone(), self.backend)
    }
}

/// Finds repository roots under `path`, skipping the `excluded` subtrees — pass
/// the worktrees dir here so managed spaces nested in a repos dir are not
/// rediscovered as repositories of their own.
pub fn discover(path: &Path, excluded: &[PathBuf]) -> Vec<Discovered> {
    // Compare canonical paths so `~/code/../code/wt` and `/Users/x/code/wt`
    // exclude the same subtree; fall back to the path as given if it is missing.
    let excluded: Vec<PathBuf> = excluded
        .iter()
        .map(|ex| fs::canonicalize(ex).unwrap_or_else(|_| ex.clone()))
        .collect();
    let root = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    let mut found: Vec<Discovered> = vec![];
    collect(&root, &excluded, MAX_SCAN_DEPTH, &mut found);
    found
}

/// How deep below a repos dir we look for repositories. Repos are conventionally
/// kept one or two levels down (`<host>/<org>/<repo>`); anything deeper is almost
/// certainly a nested checkout or build output. The cap also makes the walk
/// immune to symlink cycles.
const MAX_SCAN_DEPTH: usize = 4;

/// Directory names that are heavy to walk and never hold a repository we want to
/// manage. Dot-directories are skipped separately, by rule rather than by name.
const SKIPPED_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "build",
    "dist",
    "venv",
    "__pycache__",
];

/// True when the walk should not descend into `name`.
fn is_skipped_dir_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    // Hidden directories (.git, .jj, .venv, .cargo, …) never contain repositories
    // we manage, and `.git` itself is by far the most expensive one to descend
    // into.
    name.starts_with('.') || SKIPPED_DIR_NAMES.iter().any(|skipped| *skipped == name)
}

/// Which backend drives the repository rooted at `dir`, if any.
///
/// jj wins a colocated repository (`.jj` *and* `.git`): jj owns the working copy
/// there, and running raw git commands behind its back leaves its view of that
/// working copy stale — so a colocated repo must be driven through jj even
/// though git would happily open it.
pub fn backend_at(dir: &Path) -> Option<Backend> {
    if !dir.is_dir() {
        return None;
    }
    if is_jj_workspace_root(dir) {
        return Some(Backend::Jj);
    }
    if is_git_workdir(dir) || is_bare_git_dir(dir) {
        return Some(Backend::Git);
    }
    None
}

/// True when `dir` is the root of a *primary* jj workspace.
///
/// Additional workspaces (`jj workspace add`, which is how shanti creates a jj
/// space) keep `.jj/repo` as a *file* pointing back at the repository we already
/// list, so counting them as repositories would show the same repo twice. This
/// mirrors the `.git`-file rule for linked git worktrees.
fn is_jj_workspace_root(dir: &Path) -> bool {
    let jj = dir.join(".jj");
    jj.is_dir() && !jj.join("repo").is_file()
}

/// True when `dir` is a normal git checkout.
///
/// A `.git` *directory* marks one. A `.git` *file* marks a linked worktree or a
/// submodule: those point back at a repository we already list.
fn is_git_workdir(dir: &Path) -> bool {
    dir.join(".git").is_dir()
}

/// A bare repository has no working directory, so it is recognised by layout.
fn is_bare_git_dir(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

fn is_excluded(dir: &Path, excluded: &[PathBuf]) -> bool {
    excluded.iter().any(|ex| dir.starts_with(ex))
}

fn collect(path: &Path, excluded: &[PathBuf], depth: usize, found: &mut Vec<Discovered>) {
    if !path.is_dir() || is_excluded(path, excluded) {
        return;
    }

    if let Some(backend) = backend_at(path) {
        debug!("Found a {} repository at: {:?}", backend, path);
        found.push(Discovered {
            path: path.to_path_buf(),
            backend,
        });
        // A repository is a leaf: nested checkouts are not ours to manage.
        return;
    }

    if depth == 0 {
        debug!("Reached the maximum scan depth at: {:?}", path);
        return;
    }

    let entries = match read_dir(path) {
        Ok(entries) => entries,
        Err(err) => {
            error!("Could not read the directory {}: {}", path.display(), err);
            return;
        }
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }
        if entry_path.file_name().is_some_and(is_skipped_dir_name) {
            continue;
        }
        collect(&entry_path, excluded, depth - 1, found);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_dirs(root: &Path, paths: &[&str]) {
        for path in paths {
            fs::DirBuilder::new()
                .recursive(true)
                .create(root.join(path))
                .unwrap_or_else(|err| panic!("Could not create {}: {}", path, err));
        }
    }

    fn backend_of(found: &[Discovered], root: &Path, relative: &str) -> Option<Backend> {
        let expected = fs::canonicalize(root.join(relative)).expect("path should exist");
        found.iter().find(|d| d.path == expected).map(|d| d.backend)
    }

    #[test]
    fn test_plain_directory_is_not_a_repository() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        assert_eq!(backend_at(temp_dir.path()), None);
    }

    #[test]
    fn test_git_checkout_is_a_git_repository() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        make_dirs(temp_dir.path(), &[".git"]);

        assert_eq!(backend_at(temp_dir.path()), Some(Backend::Git));
    }

    #[test]
    fn test_jj_native_repository_is_discovered() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        make_dirs(temp_dir.path(), &["jj_only/.jj/repo"]);

        let found = discover(temp_dir.path(), &[]);
        assert_eq!(
            backend_of(&found, temp_dir.path(), "jj_only"),
            Some(Backend::Jj),
            "A jj repository without a .git dir must still be discovered: {:?}",
            found
        );
    }

    #[test]
    fn test_colocated_repository_is_driven_through_jj() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        make_dirs(temp_dir.path(), &["colocated/.jj/repo", "colocated/.git"]);

        let found = discover(temp_dir.path(), &[]);
        assert_eq!(
            backend_of(&found, temp_dir.path(), "colocated"),
            Some(Backend::Jj),
            "jj owns a colocated repository; driving it with raw git corrupts its working copy view"
        );
    }

    #[test]
    fn test_plain_git_repository_is_unaffected_by_jj_detection() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        make_dirs(temp_dir.path(), &["plain/.git"]);

        let found = discover(temp_dir.path(), &[]);
        assert_eq!(
            backend_of(&found, temp_dir.path(), "plain"),
            Some(Backend::Git)
        );
    }

    #[test]
    fn test_dot_git_file_is_not_a_repository_root() {
        // A linked worktree points at its repository through a `.git` *file*.
        let temp_dir = tempdir().expect("Could not create temporary directory");
        fs::write(
            temp_dir.path().join(".git"),
            "gitdir: /elsewhere/.git/worktrees/x",
        )
        .expect("Could not create .git file");

        assert_eq!(backend_at(temp_dir.path()), None);
    }

    #[test]
    fn test_additional_jj_workspace_is_not_a_repository_root() {
        // `jj workspace add` leaves `.jj/repo` as a file pointing at the repo.
        let temp_dir = tempdir().expect("Could not create temporary directory");
        make_dirs(temp_dir.path(), &[".jj"]);
        fs::write(temp_dir.path().join(".jj/repo"), "/elsewhere/.jj/repo")
            .expect("Could not create the .jj/repo pointer file");

        assert_eq!(backend_at(temp_dir.path()), None);
    }

    #[test]
    fn test_bare_repository_is_a_repository_root() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let bare = temp_dir.path().join("shanti.git");
        make_dirs(&bare, &["objects", "refs"]);
        fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").expect("Could not write HEAD");

        assert_eq!(backend_at(&bare), Some(Backend::Git));
        assert_eq!(
            Discovered {
                path: bare,
                backend: Backend::Git,
            }
            .to_repo()
            .name,
            "shanti",
            "A bare repo is named without its conventional .git suffix"
        );
    }

    #[test]
    fn test_list() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        make_dirs(
            temp_dir.path(),
            &[
                "first_git_dir/.git",
                "second_git_dir/.git",
                "third_git_dir/subdir/subdir/",
                "fourth_git_dir/subdir/subdir/.git",
            ],
        );

        let found = discover(temp_dir.path(), &[]);
        for path in [
            "first_git_dir",
            "second_git_dir",
            "fourth_git_dir/subdir/subdir",
        ] {
            assert!(
                backend_of(&found, temp_dir.path(), path).is_some(),
                "Expected {} to be listed in the discovered repositories, but it was not",
                path
            );
        }
    }

    #[test]
    fn test_scan_stops_at_max_depth() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let mut deep = String::from("a");
        for _ in 0..MAX_SCAN_DEPTH {
            deep.push_str("/a");
        }
        make_dirs(
            temp_dir.path(),
            &[&format!("{}/.git", deep), "shallow/.git"],
        );

        let found = discover(temp_dir.path(), &[]);
        assert!(
            backend_of(&found, temp_dir.path(), "shallow").is_some(),
            "Repositories within the depth limit must still be found"
        );
        assert!(
            backend_of(&found, temp_dir.path(), &deep).is_none(),
            "A repository deeper than MAX_SCAN_DEPTH must not be found"
        );
    }

    #[test]
    fn test_scan_skips_heavy_and_hidden_directories() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        make_dirs(
            temp_dir.path(),
            &[
                "repo/.git",
                "repo/node_modules/dep/.git",
                "target/leftover/.git",
                ".cache/clone/.git",
            ],
        );

        let found = discover(temp_dir.path(), &[]);
        assert!(backend_of(&found, temp_dir.path(), "repo").is_some());
        assert_eq!(
            found.len(),
            1,
            "Only the top-level repo should be found: {:?}",
            found
        );
    }

    #[test]
    fn test_scan_excludes_the_worktrees_dir() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        make_dirs(
            temp_dir.path(),
            &["repo/.git", "worktrees/repo/feature-branch/.git"],
        );

        let excluded = vec![temp_dir.path().join("worktrees")];
        let found = discover(temp_dir.path(), &excluded);
        assert!(backend_of(&found, temp_dir.path(), "repo").is_some());
        assert_eq!(
            found.len(),
            1,
            "Worktrees under the excluded dir must not be listed: {:?}",
            found
        );
    }
}
