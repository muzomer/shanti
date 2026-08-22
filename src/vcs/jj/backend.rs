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

use crate::vcs::{Backend, RemoteState, Repo, Space, SpaceStatus, Vcs};

use super::base::{self, Base, ANY_REVISION, REAL_TRUNK, REMOTE_BOOKMARKS};
use super::cmd::JjCli;
use super::workspace::{Workspace, WORKSPACES};

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
        let records = self
            .cli
            // Like `workspace list`, `bookmark list` draws no graph and rejects
            // `--no-graph`.
            .plain_records(&["bookmark", "list", "--all-remotes"], &REMOTE_BOOKMARKS)
            .wrap_err_with(|| format!("could not list the bookmarks of {}", self.repo.name))?;
        base::remote_carrying(&records, name)
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
    fn space_of(&self, workspace: &Workspace) -> Space {
        // The local half is real: it came back with the listing. The remote
        // half needs the workspace's nearest bookmark and its tracking counts,
        // which is shanti-nhe.5; `Unknown` renders as "not checked" rather than
        // claiming a state this backend has not looked at.
        let status = SpaceStatus::jj(RemoteState::Unknown, workspace.local);
        Space::new(
            self.repo.id.clone(),
            workspace.name.clone(),
            workspace.root.clone(),
            status,
        )
    }
}

impl Vcs for JjBackend {
    fn repo(&self) -> &Repo {
        &self.repo
    }

    fn spaces(&self) -> eyre::Result<Vec<Space>> {
        Ok(self
            .workspaces()?
            .iter()
            .map(|workspace| self.space_of(workspace))
            .collect())
    }

    /// Add a jj workspace named `name` at `dest`, on top of [`JjBackend::base_for`].
    ///
    /// The returned [`Space`] is read back from `jj workspace list` rather than
    /// assembled here: jj owns the workspace's root path and the state of its
    /// new working-copy commit, and reading them back is also what proves the
    /// workspace really is registered with the repository.
    fn create_space(&self, name: &str, dest: &Path) -> eyre::Result<Space> {
        let base = self.base_for(name)?;
        self.add_workspace(name, dest, &base)?;

        self.workspaces()?
            .iter()
            .find(|workspace| workspace.name == name)
            .map(|workspace| self.space_of(workspace))
            .ok_or_else(|| eyre!("jj created the workspace {name:?} but does not list it as one"))
    }

    /// Not implemented yet: deleting jj workspaces — which must `jj workspace
    /// forget` before removing the directory — is shanti-nhe.4.
    fn delete_space(&self, space: &Space) -> eyre::Result<()> {
        Err(eyre!(
            "deleting the jj workspace {:?} is not implemented yet (shanti-nhe.4)",
            space.name
        ))
    }

    /// Not implemented yet: mapping fetch onto `jj git fetch` is shanti-nhe.6.
    fn fetch(&self) -> eyre::Result<()> {
        Err(eyre!(
            "fetching a jujutsu repository is not implemented yet (shanti-nhe.6)"
        ))
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
    use crate::vcs::{JjLocal, LocalState};
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

    /// The local half is free with the listing, so it must be truthful; the
    /// remote half is deliberately left unprobed for shanti-nhe.5.
    #[test]
    fn a_fresh_workspace_reports_an_empty_change_and_an_unprobed_remote() {
        let Some(fixture) = JjFixture::new("fresh-status") else {
            return;
        };
        fixture.add_workspace("feature");

        let backend = fixture.backend();
        let spaces = backend.spaces().unwrap();
        let feature = spaces.iter().find(|s| s.name == "feature").unwrap();

        assert_eq!(feature.status.remote, RemoteState::Unknown);
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

    /// The deferred halves must fail loudly and name their issue, so a caller
    /// wired up early gets an explanation instead of silent success.
    #[test]
    fn the_deferred_operations_say_which_issue_owns_them() {
        let Some(fixture) = JjFixture::new("deferred") else {
            return;
        };
        let backend = fixture.backend();
        let space = backend.spaces().unwrap().remove(0);

        let delete = backend.delete_space(&space).unwrap_err().to_string();
        assert!(delete.contains("shanti-nhe.4"), "{delete}");

        let fetch = backend.fetch().unwrap_err().to_string();
        assert!(fetch.contains("shanti-nhe.6"), "{fetch}");
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

    #[test]
    fn a_directory_outside_any_jj_repository_is_an_error() {
        if !testing::jj_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(JjBackend::discover(dir.path()).is_err());
    }
}
