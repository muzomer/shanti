//! [`JjBackend`]: the [`Vcs`] implementation backed by the `jj` command line.
//!
//! Two things about jj shape this file, and both are easy to get wrong by
//! analogy with git:
//!
//! 1. **All workspaces of a repository share one repo store.** A single
//!    [`JjCli`] bound to the repository answers for every one of them, so
//!    listing spaces is one process, not one per space. Opening each workspace
//!    directory as if it were a separate repository would work by accident and
//!    cost N spawns per refresh.
//! 2. **A workspace name is not a bookmark name.** In git, shanti's model has
//!    the worktree and its branch share a name; jj has no such rule — a
//!    workspace called `feature` may sit on any bookmark, or none. The
//!    deliberate choice here is that [`Space::name`] is the *workspace* name:
//!    it is the handle `jj workspace forget` and `jj workspace add` take, it is
//!    unique within the repository (which bookmarks-per-workspace is not), and
//!    it is what shanti itself sets when it creates a space.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, eyre, WrapErr};
use tracing::debug;

use crate::vcs::{Backend, Repo, Space, SpaceStatus, Vcs};

use super::base::{self, Base, ANY_REVISION, REAL_TRUNK};
use super::cmd::JjCli;
use super::status::{self, BOOKMARKS};
use super::template::Record;
use super::workspace::{self, Workspace, WORKSPACES};

/// A single jujutsu repository, and everything shanti can do with it.
///
/// The [`JjCli`] never leaves this type: callers get the owned snapshots of
/// [`crate::vcs`] instead, which is what lets the UI hold them across frames.
#[derive(Debug, Clone)]
pub struct JjBackend {
    cli: JjCli,
    /// Backend-neutral snapshot of this repository, derived once at open time:
    /// `repo()` is called per repository per frame and must not re-run any
    /// process or path surgery.
    repo: Repo,
}

impl JjBackend {
    /// Locate jj, check its version, and bind a backend to the repository that
    /// contains `path`.
    ///
    /// Returns an error rather than aborting when jj is missing or too old —
    /// see [`JjCli::discover`]. A user with no jj installed must keep every git
    /// repository fully working.
    pub fn discover(path: impl AsRef<Path>) -> eyre::Result<Self> {
        Self::from_cli(JjCli::discover(path.as_ref())?)
    }

    /// Build a backend from an already-located [`JjCli`].
    ///
    /// Separate from [`JjBackend::discover`] so a caller that found jj once can
    /// bind it to many repositories without re-probing the binary, and so tests
    /// can supply their own.
    pub fn from_cli(cli: JjCli) -> eyre::Result<Self> {
        // Ask jj where the repository actually starts instead of trusting the
        // path we were handed: it may be a subdirectory, and it may differ from
        // jj's own answer by a symlink (/var vs /private/var on macOS). The
        // roots reported by `jj workspace list` are jj's, so the value compared
        // against them has to be jj's too — otherwise `is_default_space` below
        // silently never matches.
        let root = cli
            .read(&["workspace", "root"])
            .wrap_err_with(|| {
                format!("{} is not inside a jujutsu repository", cli.dir().display())
            })?
            .trim()
            .to_owned();
        if root.is_empty() {
            return Err(eyre!(
                "jj reported an empty workspace root for {}",
                cli.dir().display()
            ));
        }

        let root = PathBuf::from(root);
        let repo = Repo::new(directory_name(&root), &root, Backend::Jj);
        Ok(Self { cli, repo })
    }

    /// The adapter this backend drives jj through, for the layers built on top
    /// of it (creating and deleting workspaces).
    pub fn cli(&self) -> &JjCli {
        &self.cli
    }

    /// List the repository's workspaces, in jj's own vocabulary.
    ///
    /// Exposed alongside [`Vcs::spaces`] because the later create/delete work
    /// needs the jj-shaped answer (a workspace name is what `jj workspace
    /// forget` takes), while the UI only ever wants the neutral one.
    pub fn workspaces(&self) -> eyre::Result<Vec<Workspace>> {
        self.cli
            // `jj workspace list` draws no graph and rejects `--no-graph`, hence
            // `plain_records` rather than `records`.
            .plain_records(&["workspace", "list"], &WORKSPACES)
            .wrap_err_with(|| format!("could not list the workspaces of {}", self.repo.name))?
            .iter()
            .map(Workspace::from_record)
            .collect()
    }

    /// Whether `space` is the repository's own working copy.
    ///
    /// This is the guard `delete_space` needs: shanti did not create that
    /// workspace, jj will not let it be forgotten, and removing its directory
    /// would take the repository with it.
    ///
    /// Identified by path rather than by the name `default`, because a
    /// workspace can be renamed (`jj workspace rename`) while the repository
    /// root cannot move.
    pub fn is_default_space(&self, space: &Space) -> bool {
        space.path == self.repo.path
    }

    /// The revision a workspace named `name` would be created on top of.
    ///
    /// See [`Base`] for why the three candidates are these three. Fallible
    /// because both probes talk to jj; [`Vcs::resolve_base`] degrades, while
    /// [`Vcs::create_space`] propagates — creating a workspace at the wrong
    /// revision is worse than refusing to create one.
    pub fn base_for(&self, name: &str) -> eyre::Result<Base> {
        if let Some(remote) = self.remote_carrying(name)? {
            return Ok(Base::RemoteBookmark {
                bookmark: name.to_owned(),
                remote,
            });
        }
        if self.has_trunk()? {
            return Ok(Base::Trunk);
        }
        Ok(Base::WorkingCopy)
    }

    /// The remote that carries a bookmark named `name`, if any.
    ///
    /// Lists every bookmark and matches in Rust rather than passing `name` to
    /// jj: `jj bookmark list <name>` fails outright when the name is unknown,
    /// which is the common case here and not an error at all.
    fn remote_carrying(&self, name: &str) -> eyre::Result<Option<String>> {
        base::remote_carrying(&self.bookmarks()?, name)
    }

    /// Every bookmark of the repository, one row per ref.
    ///
    /// One query for the whole repository, and the reason the remote half of
    /// [`Vcs::spaces`] does not cost a process per space: tracking state
    /// belongs to a bookmark in the shared repo store, not to a workspace.
    fn bookmarks(&self) -> eyre::Result<Vec<Record>> {
        self.cli
            // Like `workspace list`, `bookmark list` draws no graph and rejects
            // `--no-graph`.
            .plain_records(&["bookmark", "list", "--all-remotes"], &BOOKMARKS)
            .wrap_err_with(|| format!("could not list the bookmarks of {}", self.repo.name))
    }

    /// Whether the repository has any git remote at all.
    ///
    /// `jj git remote list` renders one plain `name url` line per remote and
    /// takes no template, so the question is answered by whether it printed
    /// anything. Asked before fetching so that "this repository has no remotes"
    /// stays a fact rather than becoming an error — see [`Vcs::fetch`].
    fn has_remotes(&self) -> eyre::Result<bool> {
        let listed = self
            .cli
            .read(&["git", "remote", "list"])
            .wrap_err_with(|| format!("could not list the remotes of {}", self.repo.name))?;
        Ok(!listed.trim().is_empty())
    }

    /// Make a local bookmark `bookmark` that tracks `bookmark@remote`.
    ///
    /// This is the jj half of what the git backend does when it creates a space
    /// from `origin/<name>`: git makes a local branch with an upstream, jj
    /// tracks the remote bookmark. Both exist for the same reason — the user is
    /// going to push this work back.
    ///
    /// [`Vcs::create_space`] deliberately creates no bookmark of its own (see
    /// in jj a bookmark is only needed at push time, and a
    /// workspace started from `trunk()` has nothing to name yet. A workspace
    /// started from a *remote* bookmark is the exception, because the name it
    /// would push to already exists upstream — that is the whole point of
    /// opening it. Without this, `jj git push` from the new space would have no
    /// bookmark to move, and the space would render as untracked even though it
    /// sits on a pushed change.
    ///
    /// Written as `bookmark@remote` rather than `--remote=`: the flag form is
    /// newer than [`MINIMUM_JJ_VERSION`](super::MINIMUM_JJ_VERSION), and the
    /// `@` form still works (with a deprecation warning on stderr, which shanti
    /// discards on success) across the whole supported range.
    ///
    /// Idempotent: re-tracking an already-tracked bookmark is a warning and a
    /// zero exit, not a failure.
    fn track_bookmark(&self, bookmark: &str, remote: &str) -> eyre::Result<()> {
        debug!(bookmark, remote, "tracking a remote bookmark");
        self.cli
            .run(&["bookmark", "track", &format!("{bookmark}@{remote}")])
            .map(|_| ())
            .wrap_err_with(|| {
                format!(
                    "could not track the bookmark {bookmark}@{remote} of {}",
                    self.repo.name
                )
            })
    }

    /// Whether the repository has a mainline `trunk()` can point at.
    fn has_trunk(&self) -> eyre::Result<bool> {
        let records = self
            .cli
            .records(&["log", "--limit", "1", "-r", REAL_TRUNK], &ANY_REVISION)
            .wrap_err_with(|| format!("could not resolve trunk() in {}", self.repo.name))?;
        Ok(!records.is_empty())
    }

    /// Create the workspace `name` at `dest`, on top of `base`.
    ///
    /// `dest` is the caller's choice: the layout policy lives with the UI, not
    /// with a backend (see [`Vcs::create_space`]).
    fn add_workspace(&self, name: &str, dest: &Path, base: &Base) -> eyre::Result<()> {
        let destination = dest.to_str().ok_or_else(|| {
            eyre!(
                "cannot create a jj workspace at {}: the path is not valid UTF-8",
                dest.display()
            )
        })?;
        // jj creates the workspace directory itself but not the parent shanti's
        // layout puts it under.
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("could not create the directory {}", parent.display()))?;
        }

        let revset = base.revset();
        let mut args = vec!["workspace", "add", "--name", name];
        if let Some(revset) = revset.as_deref() {
            args.extend(["--revision", revset]);
        }
        args.push(destination);

        debug!(workspace = name, ?base, "adding a jj workspace");
        // `run`, not `read`: adding a workspace changes the repository, so jj
        // must be allowed to snapshot the working copy first.
        self.cli.run(&args).wrap_err_with(|| {
            format!(
                "could not create the jj workspace {name:?} in {}",
                self.repo.name
            )
        })?;
        Ok(())
    }

    /// Translate a jj workspace into the backend-neutral snapshot.
    ///
    /// `bookmarks` is the repository's whole bookmark listing, passed in rather
    /// than fetched here so that listing every space costs one bookmark query,
    /// not one per space.
    ///
    /// A remote half that cannot be decided degrades to
    /// [`RemoteState::Unknown`] — "not checked" — instead of failing the whole
    /// listing: a bookmark shanti cannot read is no reason to hide the space it
    /// belongs to.
    fn space_of(&self, workspace: &Workspace, bookmarks: &[Record]) -> Space {
        let remote = status::remote_of(bookmarks, &workspace.bookmarks).unwrap_or_else(|error| {
            debug!(
                workspace = %workspace.name,
                %error,
                "could not read the bookmark state of a jj workspace"
            );
            crate::vcs::RemoteState::Unknown
        });
        Space::new(
            self.repo.id.clone(),
            Backend::Jj,
            workspace.name.clone(),
            workspace.root.clone(),
            SpaceStatus::jj(remote, workspace.local),
        )
        .with_tip(workspace.tip.clone())
    }
}

impl Vcs for JjBackend {
    fn repo(&self) -> &Repo {
        &self.repo
    }

    /// Two jj invocations for the whole repository, however many spaces it has:
    /// one `workspace list` for the local half, one `bookmark list` for the
    /// remote half. This runs on every refresh, so the cost has to be per
    /// repository rather than per space.
    fn spaces(&self) -> eyre::Result<Vec<Space>> {
        let workspaces = self.workspaces()?;
        let bookmarks = self.bookmarks()?;
        Ok(workspaces
            .iter()
            .map(|workspace| self.space_of(workspace, &bookmarks))
            .collect())
    }

    /// Add a jj workspace named `name` at `dest`, on top of [`JjBackend::base_for`].
    ///
    /// The returned [`Space`] is read back from `jj workspace list` rather than
    /// assembled here: jj owns the workspace's root path and the state of its
    /// new working-copy commit, and reading them back is also what proves the
    /// workspace really is registered with the repository.
    ///
    /// A space started from a remote bookmark gets that bookmark tracked
    /// locally first — see [`JjBackend::track_bookmark`] for why, and why the
    /// other two bases get no bookmark. Tracking is done *before* the workspace
    /// exists so that a failure leaves nothing half-created: a space the user
    /// cannot push from is a worse outcome than a space that was never made.
    fn create_space(&self, name: &str, dest: &Path) -> eyre::Result<Space> {
        let base = self.base_for(name)?;
        if let Base::RemoteBookmark { bookmark, remote } = &base {
            self.track_bookmark(bookmark, remote)?;
        }
        self.add_workspace(name, dest, &base)?;

        let bookmarks = self.bookmarks()?;
        self.workspaces()?
            .iter()
            .find(|workspace| workspace.name == name)
            .map(|workspace| self.space_of(workspace, &bookmarks))
            .ok_or_else(|| eyre!("jj created the workspace {name:?} but does not list it as one"))
    }

    /// Forget the jj workspace behind `space`, then remove its directory.
    ///
    /// The ordering, and what happens to the change the workspace held, are
    /// [`workspace::remove_workspace`]'s to explain; it is the mirror of the git
    /// backend's `remove_worktree`, deliberately down to the shape.
    ///
    /// The refusal here is jj-specific: the repository's own working copy is not
    /// a space shanti created. jj will not let it be forgotten, and removing its
    /// directory would take the source repository with it.
    fn delete_space(&self, space: &Space) -> eyre::Result<()> {
        if self.is_default_space(space) {
            return Err(eyre!(
                "{:?} is the working copy of the repository {} itself, not a space shanti created; \
                 refusing to delete it",
                space.name,
                self.repo.name
            ));
        }

        workspace::remove_workspace(&self.cli, &space.name, &space.path).wrap_err_with(|| {
            format!(
                "could not delete the jj workspace {:?} of {}",
                space.name, self.repo.name
            )
        })
    }

    /// `jj git fetch` against every remote.
    ///
    /// `--all-remotes`, not jj's default of the single remote configured under
    /// `git.fetch`: shanti reports a bookmark's state across every remote that
    /// carries it (see [`base::remote_carrying`] and [`super::status`]), so
    /// refreshing one would leave the rest of that view quietly stale. The `git`
    /// pseudo-remote of a colocated repository is not a fetchable remote, so it
    /// cannot be pulled from by accident.
    ///
    /// A repository with no remotes has nothing to fetch and is *not* an error —
    /// jj says "No git remotes to fetch from" and exits non-zero, but nothing
    /// about that repository's view of the world is stale. The git backend
    /// answers the same way (its loop over remotes simply does not run), and the
    /// contract this method is judged by is "is what the UI shows out of date",
    /// not "did a process succeed".
    fn fetch(&self) -> eyre::Result<()> {
        if !self.has_remotes()? {
            debug!(repo = %self.repo.name, "no jj remotes to fetch from");
            return Ok(());
        }

        // `run`, not `read`: a fetch writes new refs into the shared repo store,
        // so jj is allowed to snapshot the working copy first — exactly as it
        // would for the same command typed by hand.
        self.cli
            .run(&["git", "fetch", "--all-remotes"])
            .map(|_| ())
            .wrap_err_with(|| format!("could not fetch the remotes of {}", self.repo.name))
    }

    /// The hint the create prompt shows while the user types a name.
    ///
    /// Runs on every keystroke and cannot report failure, so a jj that will not
    /// answer degrades to trunk — the answer for the overwhelming majority of
    /// repositories — instead of blocking the prompt or showing an error there.
    /// [`Vcs::create_space`] resolves the base again for real, and *does* fail
    /// loudly, so a wrong hint can never become a wrongly-based workspace.
    fn resolve_base(&self, name: &str) -> String {
        self.base_for(name)
            .unwrap_or_else(|error| {
                debug!(%error, "could not resolve the jj base; assuming trunk()");
                Base::Trunk
            })
            .hint()
    }
}

/// Last real component of `path`, lossily decoded so non-UTF-8 paths cannot
/// panic. `Path::file_name` already ignores trailing separators.
fn directory_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::{JjLocal, LocalState, RemoteState};
    use pretty_assertions::assert_eq;

    use crate::vcs::jj::testing::{self, JjFixture};

    #[test]
    fn a_repository_with_two_extra_workspaces_lists_three_spaces() {
        let Some(fixture) = JjFixture::new("three-spaces") else {
            return;
        };
        fixture.add_workspace("feature");
        fixture.add_workspace("bugfix");

        let backend = fixture.backend();
        let mut spaces = backend.spaces().unwrap();
        spaces.sort_by(|a, b| a.name.cmp(&b.name));

        let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["bugfix", "default", "feature"]);

        for space in &spaces {
            assert!(space.exists(), "{:?} does not exist on disk", space.path);
            assert_eq!(space.repo, backend.repo().id);
        }
        assert_eq!(
            spaces[2].path,
            fixture.workspace_root("feature"),
            "the path must come from jj, not from a layout guess"
        );
    }

    /// The delete guard's whole question: shanti did not create the repository's
    /// own working copy and must never offer to remove it.
    #[test]
    fn the_default_workspace_is_distinguishable_from_the_ones_shanti_added() {
        let Some(fixture) = JjFixture::new("default-guard") else {
            return;
        };
        fixture.add_workspace("feature");

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();

        for space in &spaces {
            let expected = space.name == "default";
            assert_eq!(backend.is_default_space(space), expected, "{space:?}");
        }
    }

    /// Renaming is exactly why the guard keys on the path: a workspace called
    /// `default` may not be the repository's own, and vice versa.
    #[test]
    fn the_guard_survives_a_renamed_default_workspace() {
        let Some(fixture) = JjFixture::new("renamed-default") else {
            return;
        };
        fixture.jj(&["workspace", "rename", "mainline"]);

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();

        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].name, "mainline");
        assert!(backend.is_default_space(&spaces[0]));
    }

    /// Workspaces share one repo store, so one adapter answers for all of them;
    /// the repo snapshot every space points back to must be that one repo.
    #[test]
    fn every_space_belongs_to_the_one_shared_repository() {
        let Some(fixture) = JjFixture::new("shared-store") else {
            return;
        };
        fixture.add_workspace("feature");

        let backend = fixture.backend();
        let repo = backend.repo();
        assert_eq!(repo.backend, Backend::Jj);
        assert_eq!(repo.name, "shared-store");

        let ids: Vec<_> = backend
            .spaces()
            .unwrap()
            .into_iter()
            .map(|space| space.repo)
            .collect();
        assert!(ids.iter().all(|id| *id == repo.id), "{ids:?}");
    }

    /// A workspace shanti has just created carries no bookmark, so nothing
    /// upstream has heard of it — "not pushed", never "in sync".
    #[test]
    fn a_fresh_workspace_reports_an_empty_change_and_nothing_upstream() {
        let Some(fixture) = JjFixture::new("fresh-status") else {
            return;
        };
        fixture.add_workspace("feature");

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();
        let feature = spaces.iter().find(|s| s.name == "feature").unwrap();

        assert_eq!(feature.status.remote, RemoteState::Untracked);
        assert_eq!(
            feature.status.local,
            LocalState::Jj(JjLocal {
                empty: true,
                conflicted: false,
                divergent: false,
            })
        );
        assert_eq!(feature.status.backend(), Backend::Jj);
    }

    /// The counterpart to the test above: the flags must actually move, or a
    /// hard-coded `empty: true` would pass just as well.
    #[test]
    fn a_workspace_with_recorded_edits_is_not_empty() {
        let Some(fixture) = JjFixture::new("edited-status") else {
            return;
        };
        fixture.commit_change("a.txt", "hello\n");

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();

        assert_eq!(
            spaces[0].status.local,
            LocalState::Jj(JjLocal {
                empty: false,
                conflicted: false,
                divergent: false,
            })
        );
        // jj auto-commits, so an ordinary edit is *not* work at risk — unlike a
        // dirty git worktree.
        assert!(!spaces[0].status.has_unsaved_work());
    }

    /// The remote half, end to end: an empty working copy on top of a pushed
    /// bookmark is level with its remote.
    #[test]
    fn a_space_sitting_on_a_pushed_bookmark_is_in_sync() {
        let Some(fixture) = JjFixture::new("in-sync") else {
            return;
        };
        fixture.push_bookmark("main");

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();

        assert_eq!(spaces[0].status.remote, RemoteState::in_sync());
        assert!(!spaces[0].status.remote.has_unpushed_work());
    }

    /// The acceptance criterion's "ahead of its remote": the bookmark moved on
    /// and nobody pushed it. jj states that count from the remote's end, so a
    /// swapped reading would show this as *behind*.
    #[test]
    fn a_space_whose_bookmark_moved_on_without_a_push_is_ahead() {
        let Some(fixture) = JjFixture::new("ahead") else {
            return;
        };
        fixture.push_bookmark("main");
        fixture.commit_change("a.txt", "new work\n");
        fixture.jj(&["bookmark", "set", "main", "-r", "@"]);
        // Start a fresh empty change, as any further work in the space would.
        fixture.jj(&["new"]);

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();

        assert_eq!(
            spaces[0].status.remote,
            RemoteState::Tracked {
                ahead: 1,
                behind: 0
            }
        );
        assert!(spaces[0].status.remote.has_unpushed_work());
    }

    /// A bookmark deleted upstream — a merged pull request, typically — must
    /// read as gone rather than as an ordinary tracked bookmark.
    #[test]
    fn a_space_whose_bookmark_was_deleted_upstream_is_gone() {
        let Some(fixture) = JjFixture::new("gone") else {
            return;
        };
        fixture.push_bookmark("main");
        // Move the local bookmark on, so jj keeps it when the remote one
        // disappears instead of propagating the deletion.
        fixture.commit_change("a.txt", "new work\n");
        fixture.jj(&["bookmark", "set", "main", "-r", "@"]);
        fixture.jj(&["new"]);

        fixture.delete_on_remote("main");
        fixture.jj(&["git", "fetch"]);

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();
        assert_eq!(spaces[0].status.remote, RemoteState::Gone);
    }

    /// Work committed on top of a pushed bookmark is unpushed work, even though
    /// the bookmark itself has not moved: the space's head no longer carries
    /// one, and shanti must not answer with the bookmark further back.
    #[test]
    fn work_beyond_the_bookmark_is_not_reported_as_in_sync() {
        let Some(fixture) = JjFixture::new("beyond-bookmark") else {
            return;
        };
        fixture.push_bookmark("main");
        fixture.commit_change("a.txt", "new work\n");

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();

        assert_eq!(spaces[0].status.remote, RemoteState::Untracked);
        assert!(spaces[0].status.remote.has_unpushed_work());
    }

    /// The acceptance criterion: conflicted, empty and ahead must be three
    /// different things on screen, not three blanks.
    #[test]
    fn conflicted_empty_and_ahead_spaces_render_distinctly() {
        let Some(fixture) = JjFixture::new("distinct") else {
            return;
        };
        fixture.push_bookmark("main");

        // An empty space, level with the remote.
        let empty = fixture.backend().spaces().unwrap().remove(0);

        // The same space, one unpushed commit beyond the bookmark.
        fixture.commit_change("a.txt", "one\n");
        fixture.jj(&["describe", "-m", "one"]);
        fixture.jj(&["bookmark", "set", "main", "-r", "@"]);
        fixture.jj(&["new"]);
        let ahead = fixture.backend().spaces().unwrap().remove(0);

        // And again with a merge of two siblings that both add `a.txt`.
        fixture.jj(&["new", "main-"]);
        fixture.commit_change("a.txt", "two\n");
        fixture.jj(&["describe", "-m", "two"]);
        fixture.jj(&["new", "main", "@"]);
        let conflicted = fixture.backend().spaces().unwrap().remove(0);

        assert_eq!(empty.status.glyphs()[1].symbol, "∅");
        assert_eq!(
            ahead.status.remote,
            RemoteState::Tracked {
                ahead: 1,
                behind: 0
            }
        );
        assert_eq!(conflicted.status.glyphs()[1].symbol, "!");

        let rendered: Vec<[&str; 2]> = [&empty, &ahead, &conflicted]
            .iter()
            .map(|space| {
                let [remote, local] = space.status.glyphs();
                [remote.symbol, local.symbol]
            })
            .collect();
        for (i, a) in rendered.iter().enumerate() {
            for b in &rendered[i + 1..] {
                assert_ne!(a, b, "two states render the same: {rendered:?}");
            }
        }
    }

    /// The remote half must survive several spaces at once: they share one
    /// bookmark listing, so a mistake in the join would show up as one space
    /// wearing another's tracking state.
    #[test]
    fn spaces_sharing_one_bookmark_listing_keep_their_own_remote_state() {
        let Some(fixture) = JjFixture::new("shared-listing") else {
            return;
        };
        fixture.push_bookmark("main");
        // Started beside the working copy, so its head is the bookmarked commit.
        fixture.add_workspace("on-main");
        // The default workspace moves past the bookmark instead.
        fixture.commit_change("a.txt", "new work\n");

        let backend = fixture.backend();
        let mut spaces = backend.spaces().unwrap();
        spaces.sort_by(|a, b| a.name.cmp(&b.name));
        let state = |name: &str| {
            spaces
                .iter()
                .find(|space| space.name == name)
                .unwrap()
                .status
                .remote
        };

        assert_eq!(state("on-main"), RemoteState::in_sync());
        assert_eq!(state("default"), RemoteState::Untracked);
    }

    /// Exercises the trait through a trait object: that is how the repository
    /// list holds backends.
    #[test]
    fn the_backend_is_usable_through_the_trait_object() {
        let Some(fixture) = JjFixture::new("trait-object") else {
            return;
        };

        let backend = fixture.backend();
        let vcs: &dyn Vcs = &backend;
        assert_eq!(vcs.backend(), Backend::Jj);
        assert_eq!(vcs.spaces().unwrap().len(), 1);
    }

    /// A repository with no remotes has nothing to fetch, and that is a fact
    /// rather than a failure: jj exits non-zero saying so, and `fetch` must not
    /// pass that on as "your view of the remotes is stale".
    #[test]
    fn fetching_a_repository_with_no_remotes_is_not_an_error() {
        let Some(fixture) = JjFixture::new("no-remotes") else {
            return;
        };
        let backend = fixture.backend();

        assert!(backend.fetch().is_ok());
    }

    /// The end-to-end check the issue asks for: what shanti creates must come
    /// back out of `jj workspace list`, at the path shanti chose.
    #[test]
    fn a_created_space_is_listed_afterwards() {
        let Some(fixture) = JjFixture::new("create") else {
            return;
        };
        let backend = fixture.backend();
        let dest = fixture.workspace_root("feature");

        let created = backend.create_space("feature", &dest).unwrap();
        assert_eq!(created.name, "feature");
        assert_eq!(created.path, dest);
        assert!(
            created.exists(),
            "{:?} does not exist on disk",
            created.path
        );
        assert_eq!(created.repo, backend.repo().id);
        assert_eq!(created.status.backend(), Backend::Jj);

        let names: Vec<String> = backend
            .spaces()
            .unwrap()
            .into_iter()
            .map(|space| space.name)
            .collect();
        assert!(names.contains(&"feature".to_owned()), "{names:?}");
        // Created, not adopted: the repository's own working copy is untouched.
        assert!(!backend.is_default_space(&created));
    }

    /// The layout is the caller's business, so a destination several levels
    /// deep must be created rather than rejected.
    #[test]
    fn the_destination_directorys_parents_are_created() {
        let Some(fixture) = JjFixture::new("nested-dest") else {
            return;
        };
        let backend = fixture.backend();
        let dest = fixture
            .workspace_root("nested")
            .join("repo")
            .join("feature");

        let created = backend.create_space("feature", &dest).unwrap();
        assert_eq!(created.path, dest);
    }

    /// With no remote bookmark and no mainline, a fresh `jj git init` has only
    /// its working copy to start beside — `trunk()` there is the root commit,
    /// which is not a useful place to begin.
    #[test]
    fn a_repository_with_no_mainline_starts_beside_the_working_copy() {
        let Some(fixture) = JjFixture::new("no-mainline") else {
            return;
        };
        let backend = fixture.backend();

        assert_eq!(backend.base_for("feature").unwrap(), Base::WorkingCopy);
        assert_eq!(
            backend.resolve_base("feature"),
            "Will start beside the current working copy"
        );

        // The new change must still descend from the fixture's real commit, not
        // from the root commit.
        let dest = fixture.workspace_root("feature");
        backend.create_space("feature", &dest).unwrap();
        assert_eq!(
            fixture.commit_at("feature@-"),
            fixture.commit_at("default@-"),
        );
    }

    /// The acceptance criterion: a matching bookmark on the remote wins, and the
    /// new workspace really is a child of it.
    #[test]
    fn a_matching_remote_bookmark_is_the_base() {
        let Some(fixture) = JjFixture::new("remote-base") else {
            return;
        };
        fixture.push_bookmark("feature");
        let backend = fixture.backend();

        assert_eq!(
            backend.base_for("feature").unwrap(),
            Base::RemoteBookmark {
                bookmark: "feature".to_owned(),
                remote: "origin".to_owned(),
            }
        );
        assert_eq!(
            backend.resolve_base("feature"),
            "Will start from feature@origin"
        );

        let dest = fixture.workspace_root("feature");
        backend.create_space("feature", &dest).unwrap();
        assert_eq!(
            fixture.commit_at("feature@-"),
            fixture.commit_at("feature@origin"),
            "the workspace's change must sit on top of the remote bookmark"
        );
    }

    /// A name that has no bookmark anywhere falls through to trunk, which now
    /// exists because the push above gave the repository a mainline.
    #[test]
    fn an_unknown_name_falls_through_to_trunk() {
        let Some(fixture) = JjFixture::new("trunk-base") else {
            return;
        };
        // The default trunk() alias looks for main/master/trunk on a remote.
        fixture.push_bookmark("main");
        let backend = fixture.backend();

        assert_eq!(backend.base_for("something-new").unwrap(), Base::Trunk);
        assert_eq!(
            backend.resolve_base("something-new"),
            "Will start from trunk()"
        );

        let dest = fixture.workspace_root("something-new");
        backend.create_space("something-new", &dest).unwrap();
        assert_eq!(
            fixture.commit_at("something-new@-"),
            fixture.commit_at("trunk()"),
        );
    }

    /// jj refuses a duplicate workspace name, and that refusal must reach the
    /// user with jj's own words rather than as a silent success.
    #[test]
    fn creating_a_workspace_whose_name_is_taken_fails_with_jjs_complaint() {
        let Some(fixture) = JjFixture::new("duplicate") else {
            return;
        };
        let backend = fixture.backend();
        let dest = fixture.workspace_root("feature");
        backend.create_space("feature", &dest).unwrap();

        let report = backend
            .create_space("feature", &fixture.workspace_root("feature-again"))
            .unwrap_err();
        // `{:#}` renders the whole chain: shanti's context *and* the stderr jj
        // failed with, which is the half that explains what to do about it.
        let error = format!("{report:#}");
        assert!(error.contains("feature"), "{error}");
        assert!(error.contains("already exists"), "{error}");
    }

    /// The acceptance criterion: no stale entry in `jj workspace list`, and no
    /// directory left behind.
    #[test]
    fn a_deleted_space_leaves_no_registration_and_no_directory() {
        let Some(fixture) = JjFixture::new("delete") else {
            return;
        };
        let backend = fixture.backend();
        let dest = fixture.workspace_root("feature");
        let space = backend.create_space("feature", &dest).unwrap();

        backend.delete_space(&space).unwrap();

        let names: Vec<String> = backend
            .spaces()
            .unwrap()
            .into_iter()
            .map(|space| space.name)
            .collect();
        assert_eq!(names, ["default"], "the workspace is still registered");
        assert!(!dest.exists(), "{} is still on disk", dest.display());
    }

    /// The safety question jj raises and git does not: the workspace may hold
    /// the only copy of some work. Deleting the space must not destroy it — the
    /// change is snapshotted first and stays in the repo afterwards.
    #[test]
    fn deleting_a_space_keeps_the_work_it_held() {
        let Some(fixture) = JjFixture::new("delete-keeps-work") else {
            return;
        };
        let backend = fixture.backend();
        let dest = fixture.workspace_root("feature");
        let space = backend.create_space("feature", &dest).unwrap();
        // Written but never snapshotted: exactly the state a user leaves a
        // space in after editing files without running jj there.
        std::fs::write(dest.join("a.txt"), "work worth keeping\n").unwrap();

        backend.delete_space(&space).unwrap();

        // Every commit the fixture makes on its own is empty, so a non-empty
        // one in the repo can only be the change the deleted space held.
        let log = backend
            .cli()
            .read(&["log", "--no-graph", "-r", "all()", "-T", "empty ++ \"\n\""])
            .unwrap();
        assert!(
            log.lines().any(|line| line == "false"),
            "the work the space held is gone: {log}"
        );
    }

    /// The mirror image of the git side's already-removed directory: the
    /// registration must still be cleaned up, or jj complains about a vanished
    /// working copy on every later operation.
    #[test]
    fn a_space_whose_directory_was_removed_by_hand_is_still_forgotten() {
        let Some(fixture) = JjFixture::new("delete-vanished") else {
            return;
        };
        let backend = fixture.backend();
        let dest = fixture.workspace_root("feature");
        let space = backend.create_space("feature", &dest).unwrap();
        std::fs::remove_dir_all(&dest).unwrap();

        backend.delete_space(&space).unwrap();

        let names: Vec<String> = backend
            .spaces()
            .unwrap()
            .into_iter()
            .map(|space| space.name)
            .collect();
        assert_eq!(names, ["default"], "the workspace is still registered");
    }

    /// Deleting twice converges on "removed" rather than failing, matching the
    /// git backend.
    #[test]
    fn deleting_the_same_space_twice_converges_on_removed() {
        let Some(fixture) = JjFixture::new("delete-twice") else {
            return;
        };
        let backend = fixture.backend();
        let dest = fixture.workspace_root("feature");
        let space = backend.create_space("feature", &dest).unwrap();

        backend.delete_space(&space).unwrap();
        backend
            .delete_space(&space)
            .expect("deleting an already-deleted space should succeed");

        assert!(!dest.exists());
    }

    /// shanti did not create the repository's own working copy, and removing it
    /// would take the source repository with it.
    #[test]
    fn deleting_the_repositorys_own_workspace_is_refused() {
        let Some(fixture) = JjFixture::new("delete-default") else {
            return;
        };
        let backend = fixture.backend();
        let default = backend.spaces().unwrap().remove(0);

        let error = format!("{:#}", backend.delete_space(&default).unwrap_err());
        assert!(error.contains("refusing to delete"), "{error}");
        assert!(
            backend.repo().path.exists(),
            "the repository itself was removed"
        );
    }

    #[test]
    fn a_directory_outside_any_jj_repository_is_an_error() {
        if !testing::jj_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(JjBackend::discover(dir.path()).is_err());
    }
}
