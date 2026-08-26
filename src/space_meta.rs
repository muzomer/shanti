//! What shanti remembers about a space that no backend can tell it.
//!
//! A space created from a pull request came from a URL, and nothing on disk
//! records that: git knows the branch, jj knows the bookmark, and neither has a
//! word for "this is PR 412". Losing it means the one piece of context that
//! explains why the space exists is the one piece the tool cannot show.
//!
//! So it is kept here, in a small file shanti owns outright — unlike the
//! configuration file, which belongs to the user and is edited in place. The
//! rules that follow from owning it:
//!
//! * **it is a cache, not a source of truth.** A missing, unreadable or
//!   half-written file costs the PR line in the detail pane and nothing else,
//!   so every read degrades to "remember nothing" rather than failing a start;
//! * **it is keyed by the space's path**, which is what the UI has in hand and
//!   what stays stable while a branch is renamed underneath it;
//! * **it forgets.** An entry whose directory is gone is dropped on the next
//!   write, so deleting spaces cannot grow the file forever.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, Context};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// File name used inside the data directory.
pub const FILE_NAME: &str = "spaces.toml";

/// What is remembered about one space.
///
/// A struct rather than a bare string so that the next thing worth remembering —
/// the base a space was created from, when it was created — is a new key in an
/// existing table rather than a new file format.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SpaceRecord {
    /// URL of the pull request the space was created for.
    pub pr: Option<String>,
}

/// Everything shanti remembers, and the file it came from.
///
/// `BTreeMap` for the ordering alone: the file is rewritten whole, and a stable
/// order keeps a diff of it readable by whoever opens it out of curiosity.
#[derive(Debug, Default)]
pub struct SpaceMeta {
    path: PathBuf,
    entries: BTreeMap<String, SpaceRecord>,
}

impl SpaceMeta {
    /// Read what is remembered at `path`.
    ///
    /// Infallible on purpose — see the module note. A file that cannot be read
    /// or parsed is logged and treated as empty, because the alternative is
    /// refusing to start over a cache.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = match std::fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_else(|error| {
                debug!(file = %path.display(), %error, "could not parse the space metadata; starting empty");
                BTreeMap::new()
            }),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    debug!(file = %path.display(), %error, "could not read the space metadata");
                }
                BTreeMap::new()
            }
        };
        Self { path, entries }
    }

    /// A store that remembers nothing and writes nowhere.
    ///
    /// The seam tests construct an `App` through: no file, no data directory,
    /// and [`SpaceMeta::remember_pr`] silently does nothing.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The pull request `path` was created for, if shanti made it and remembers.
    pub fn pr_of(&self, path: &Path) -> Option<&str> {
        self.entries.get(&key_of(path))?.pr.as_deref()
    }

    /// Remember that the space at `path` came from `url`, and write the file.
    ///
    /// The in-memory half is updated first and unconditionally: a write that
    /// fails costs the memory at the next start, not this session's pane.
    pub fn remember_pr(&mut self, path: &Path, url: impl Into<String>) -> eyre::Result<()> {
        self.entries.entry(key_of(path)).or_default().pr = Some(url.into());
        self.save()
    }

    /// Rewrite the file, dropping entries whose space is gone.
    ///
    /// Nothing is written when the store has no path — the test seam — so an
    /// `App` built without a data directory touches no disk at all.
    fn save(&mut self) -> eyre::Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        self.entries.retain(|key, _| Path::new(key).exists());
        let contents = toml::to_string_pretty(&self.entries)
            .wrap_err("Could not render what shanti remembers about its spaces")?;
        crate::config::write_atomically(&self.path, &contents).wrap_err_with(|| {
            format!(
                "Could not write the space metadata file {}",
                self.path.display()
            )
        })
    }
}

/// The key one space is filed under.
///
/// The path as written, not canonicalised: canonicalising touches the disk and
/// this runs while the UI is drawing. Both the writer and the reader take the
/// path from the same [`Space`](crate::vcs::Space), so they agree by
/// construction.
fn key_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn what_is_remembered_survives_a_reload() {
        let dir = tempdir().unwrap();
        let space = dir.path().join("feature");
        std::fs::create_dir(&space).unwrap();
        let file = dir.path().join(FILE_NAME);

        let mut meta = SpaceMeta::load(&file);
        meta.remember_pr(&space, "https://github.com/o/r/pull/7")
            .unwrap();

        let reloaded = SpaceMeta::load(&file);
        assert_eq!(
            reloaded.pr_of(&space),
            Some("https://github.com/o/r/pull/7")
        );
        assert_eq!(reloaded.pr_of(Path::new("/elsewhere")), None);
    }

    /// The file is a cache. Anything wrong with it costs one line of a pane, so
    /// it must read as "nothing remembered" rather than as an error.
    #[test]
    fn an_unreadable_file_is_an_empty_memory() {
        let dir = tempdir().unwrap();
        let file = dir.path().join(FILE_NAME);
        std::fs::write(&file, "this is not toml {{{").unwrap();

        assert_eq!(SpaceMeta::load(&file).pr_of(Path::new("/x")), None);
        assert_eq!(
            SpaceMeta::load(dir.path().join("absent.toml")).pr_of(Path::new("/x")),
            None
        );
    }

    /// Deleting spaces must not grow the file forever.
    #[test]
    fn an_entry_whose_space_is_gone_is_dropped_on_the_next_write() {
        let dir = tempdir().unwrap();
        let file = dir.path().join(FILE_NAME);
        let alive = dir.path().join("alive");
        let dead = dir.path().join("dead");
        std::fs::create_dir(&alive).unwrap();
        std::fs::create_dir(&dead).unwrap();

        let mut meta = SpaceMeta::load(&file);
        meta.remember_pr(&dead, "https://github.com/o/r/pull/1")
            .unwrap();
        std::fs::remove_dir(&dead).unwrap();
        meta.remember_pr(&alive, "https://github.com/o/r/pull/2")
            .unwrap();

        let reloaded = SpaceMeta::load(&file);
        assert_eq!(reloaded.pr_of(&dead), None);
        assert!(reloaded.pr_of(&alive).is_some());
    }

    /// The seam an `App` in a test is built through: no path, no writes.
    #[test]
    fn an_in_memory_store_writes_nothing() {
        let mut meta = SpaceMeta::in_memory();
        meta.remember_pr(Path::new("/w/feature"), "https://github.com/o/r/pull/3")
            .unwrap();
        assert_eq!(
            meta.pr_of(Path::new("/w/feature")),
            Some("https://github.com/o/r/pull/3")
        );
    }
}
