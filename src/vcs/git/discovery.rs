//! Finding git repositories on disk.
//!
//! The walk is deliberately dumb and cheap: it decides what *looks* like a
//! repository from the directory layout alone, without opening anything.
//! Recognising jj-native repositories here is a separate concern (shanti-12z.5).

use rayon::prelude::*;
use std::{
    ffi::OsStr,
    fs::{self, read_dir},
    path::{Path, PathBuf},
};
use tracing::{debug, error};

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
    find_git_dirs(Path::new(path), excluded)
        .par_iter()
        .filter_map(|dir| match GitBackend::from_path(dir, run_fetch) {
            Ok(repo) => Some(repo),
            Err(err) => {
                error!("Could not open repository at {}: {}", dir, err);
                None
            }
        })
        .collect()
}

pub fn worktrees_of_repositories(repositories: &[GitBackend]) -> Vec<Worktree> {
    let mut worktrees: Vec<Worktree> = Vec::new();
    repositories.iter().for_each(|repo| {
        worktrees.append(&mut repo.git_worktrees());
    });
    worktrees
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
    // Hidden directories (.git, .venv, .cargo, …) never contain repositories we
    // manage, and `.git` itself is by far the most expensive one to descend into.
    name.starts_with('.') || SKIPPED_DIR_NAMES.iter().any(|skipped| *skipped == name)
}

/// True when `dir` is the root of a repository we should offer to the user.
///
/// A `.git` *directory* marks a normal checkout. A `.git` *file* marks a linked
/// worktree or a submodule: those point back at a repository we already list, so
/// treating them as repositories would show the same repo twice. A bare repo has
/// no `.git` at all, so it is recognised by its layout instead.
fn is_git_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    if dir.join(".git").is_dir() {
        return true;
    }
    is_bare_git_dir(dir)
}

fn is_bare_git_dir(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

fn is_excluded(dir: &Path, excluded: &[PathBuf]) -> bool {
    excluded.iter().any(|ex| dir.starts_with(ex))
}

/// Finds repository roots under `path`, skipping `excluded` subtrees.
fn find_git_dirs(path: &Path, excluded: &[PathBuf]) -> Vec<String> {
    // Compare canonical paths so `~/code/../code/wt` and `/Users/x/code/wt`
    // exclude the same subtree; fall back to the path as given if it is missing.
    let excluded: Vec<PathBuf> = excluded
        .iter()
        .map(|ex| fs::canonicalize(ex).unwrap_or_else(|_| ex.clone()))
        .collect();
    let root = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    let mut git_dirs: Vec<String> = vec![];
    collect_git_dirs(&root, &excluded, MAX_SCAN_DEPTH, &mut git_dirs);
    git_dirs
}

fn collect_git_dirs(path: &Path, excluded: &[PathBuf], depth: usize, git_dirs: &mut Vec<String>) {
    if !path.is_dir() || is_excluded(path, excluded) {
        return;
    }

    if is_git_dir(path) {
        debug!("Found git repository at: {:?}", path);
        git_dirs.push(path.display().to_string());
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
        collect_git_dirs(&entry_path, excluded, depth - 1, git_dirs);
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

    fn found(root: &Path, excluded: &[PathBuf]) -> Vec<String> {
        find_git_dirs(root, excluded)
    }

    fn contains(dirs: &[String], root: &Path, relative: &str) -> bool {
        let expected = fs::canonicalize(root.join(relative)).expect("path should exist");
        dirs.iter().any(|dir| Path::new(dir) == expected)
    }

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
    fn test_dot_git_file_is_not_a_repository_root() {
        // A linked worktree points at its repository through a `.git` *file*.
        let temp_dir = tempdir().expect("Could not create temporary directory");
        fs::write(
            temp_dir.path().join(".git"),
            "gitdir: /elsewhere/.git/worktrees/x",
        )
        .expect("Could not create .git file");

        assert!(
            !is_git_dir(temp_dir.path()),
            "A worktree pointer file must not be reported as a repository"
        );
    }

    #[test]
    fn test_bare_repository_is_a_repository_root() {
        let temp_dir = tempdir().expect("Could not create temporary directory");
        let bare = temp_dir.path().join("shanti.git");
        make_dirs(&bare, &["objects", "refs"]);
        fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").expect("Could not write HEAD");

        assert!(
            is_git_dir(&bare),
            "Expected the bare repository to be found"
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

        let dirs = found(temp_dir.path(), &[]);
        for path in [
            "first_git_dir",
            "second_git_dir",
            "fourth_git_dir/subdir/subdir",
        ] {
            assert!(
                contains(&dirs, temp_dir.path(), path),
                "Expected {} to be listed in the git subdirectories, but it was not",
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

        let dirs = found(temp_dir.path(), &[]);
        assert!(
            contains(&dirs, temp_dir.path(), "shallow"),
            "Repositories within the depth limit must still be found"
        );
        assert!(
            !contains(&dirs, temp_dir.path(), &deep),
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

        let dirs = found(temp_dir.path(), &[]);
        assert!(contains(&dirs, temp_dir.path(), "repo"));
        assert_eq!(
            dirs.len(),
            1,
            "Only the top-level repo should be found: {:?}",
            dirs
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
        let dirs = found(temp_dir.path(), &excluded);
        assert!(contains(&dirs, temp_dir.path(), "repo"));
        assert_eq!(
            dirs.len(),
            1,
            "Worktrees under the excluded dir must not be listed: {:?}",
            dirs
        );
    }
}
