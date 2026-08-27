//! Reading `jj workspace list` into something shanti can hold.
//!
//! One template, one process, every workspace: `jj workspace list` renders each
//! workspace through the `WorkspaceRef` type, which exposes the workspace's name,
//! its root directory *and* its working-copy commit. Everything shanti needs to
//! build a [`Space`](crate::vcs::Space) therefore comes back from a single read —
//! no second command per workspace, and no opening each directory as if it were
//! its own repository (it is not: workspaces share one repo store).

use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, WrapErr};
use tracing::debug;

use super::cmd::JjCli;
use super::status;
use super::template::{Record, Template};
use crate::vcs::{JjLocal, SpaceTip};

/// The fields shanti reads for every workspace, as jj template expressions.
///
/// `root` is the reason this can stay one query: jj knows where each workspace
/// lives, so shanti never has to guess a path from a layout convention.
///
/// The three `target.*` flags are the jj-native local signals of
/// [`JjLocal`]; they are read here because they cost nothing extra once this
/// template is already rendering the working-copy commit.
///
/// `bookmarks` is the same bargain applied to the remote half: the names it
/// yields are joined against one repository-wide `jj bookmark list` instead of
/// costing a query per workspace. Which commit's bookmarks those are is
/// [`super::status`]'s rule, so the expression lives there.
pub const WORKSPACES: Template = Template::new(&[
    ("name", "name"),
    ("root", "root"),
    ("empty", "target.empty()"),
    ("conflicted", "target.conflict()"),
    ("divergent", "target.divergent()"),
    ("bookmarks", status::WORKSPACE_BOOKMARKS),
    ("subject", "target.description().first_line()"),
    // Unix seconds, not jj's `.ago()`: the listing outlives many frames, so the
    // age has to be derived when it is drawn rather than frozen when it is read.
    (
        "committed_at",
        "target.committer().timestamp().format(\"%s\")",
    ),
]);

/// One row of `jj workspace list`.
///
/// A plain owned snapshot, like everything else that crosses out of a backend:
/// it holds no handle on the repository it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// jj's name for the workspace. Independent of any bookmark name — unlike
    /// git, where shanti's model has the worktree and the branch share a name.
    pub name: String,
    /// Absolute path to the workspace root, as reported by jj itself.
    pub root: PathBuf,
    /// State of the workspace's working-copy commit.
    pub local: JjLocal,
    /// Local bookmarks sitting on the workspace's head, in jj's order. Empty
    /// for the ordinary jj workspace, which carries no bookmark at all.
    pub bookmarks: Vec<String>,
    /// The working-copy commit's description and date.
    ///
    /// Optional because only the timestamp can fail to parse, and a workspace
    /// whose date jj rendered unexpectedly is still a workspace: the detail goes
    /// missing, the space does not.
    pub tip: Option<SpaceTip>,
}

impl Workspace {
    /// Build a workspace from a record rendered by [`WORKSPACES`].
    pub fn from_record(record: &Record) -> eyre::Result<Self> {
        // The subject is a template field like any other and is read strictly.
        // Only the timestamp is allowed to be unreadable, and an unreadable one
        // costs the tip rather than the whole row — see [`Workspace::tip`].
        let subject = record.get("subject")?;
        Ok(Self {
            name: record.get("name")?.to_owned(),
            root: PathBuf::from(record.get("root")?),
            local: JjLocal {
                empty: record.boolean("empty")?,
                conflicted: record.boolean("conflicted")?,
                divergent: record.boolean("divergent")?,
            },
            bookmarks: record.list("bookmarks")?,
            tip: record
                .get("committed_at")?
                .parse::<i64>()
                .ok()
                .map(|committed_at| SpaceTip::new(subject, committed_at)),
        })
    }
}

/// Remove the workspace `name`, rooted at `root`: its work, then its
/// registration, then its directory.
///
/// The order is load-bearing, and it is the same constraint the git side obeys
/// in `git::worktree::remove_worktree` rather than a jj quirk: the directory
/// goes last, because everything that reads a space's state needs the working
/// copy to still be there. Removing it first leaves the workspace registered in
/// the shared repo store, and jj then complains about a working copy that has
/// vanished on every later operation.
///
/// The steps line up with git's as follows:
///
/// | step | git | jj |
/// | --- | --- | --- |
/// | keep the work | (nothing: git never had it) | snapshot the working copy |
/// | deregister | prune the worktree | `jj workspace forget` |
/// | branch | delete the checked-out branch | (nothing: see below) |
/// | directory | `remove_dir_all` | `remove_dir_all` |
///
/// There is no jj counterpart to git's branch deletion. Bookmarks are
/// repo-level and outlive the workspace that happened to sit on one, and a
/// workspace's name is not a bookmark's name anyway; deleting a bookmark is a
/// separate user intent and not this function's business.
pub(super) fn remove_workspace(cli: &JjCli, name: &str, root: &Path) -> eyre::Result<()> {
    snapshot(cli, name, root);

    debug!(workspace = name, "forgetting a jj workspace");
    // `run`, not `read`: forgetting changes the repository.
    //
    // Idempotent by jj's own design — forgetting a workspace it does not know
    // is a warning and a zero exit, not a failure — so deleting a space twice
    // converges on "removed", exactly as it does on the git side.
    cli.run(&["workspace", "forget", name])
        .wrap_err_with(|| format!("could not forget the jj workspace {name:?}"))?;

    if root.exists() {
        std::fs::remove_dir_all(root).wrap_err_with(|| {
            format!(
                "could not remove the directory {} of the jj workspace {name:?}",
                root.display()
            )
        })?;
    }

    Ok(())
}

/// Let jj record whatever the workspace holds, before the directory holding it
/// is removed.
///
/// jj auto-commits, but only when a jj command actually runs in that workspace:
/// edits made since the last one exist on disk and nowhere else. Snapshotting
/// first turns them into the workspace's working-copy commit, which survives
/// the forget below as an ordinary change in the repo — still reachable from
/// `jj log`, and recoverable long after the directory is gone. Skip this step
/// and deleting a space could silently destroy work no other copy of exists.
///
/// A working-copy commit that is genuinely empty and undescribed is abandoned
/// by the forget instead, which is the outcome we want: nothing was lost.
///
/// Best-effort on purpose. This is a safety net, not a precondition: a
/// workspace whose directory was already removed by hand has nothing left to
/// snapshot, and failing here would strand its registration forever. Whether to
/// *refuse* deleting a space that still holds work is a separate question, and
/// is asked before we ever get here.
fn snapshot(cli: &JjCli, name: &str, root: &Path) {
    // A second adapter, bound to the workspace rather than to the repository:
    // jj picks which workspace to snapshot from the path it is pointed at, so
    // the repository-wide `cli` would snapshot the default workspace instead of
    // this one. Built from the already-located binary, so it costs no re-probe.
    let workspace_cli = JjCli::with_program(cli.program(), root, cli.version());
    // Any command that snapshots would do; `status` is the cheapest that does
    // nothing else.
    if let Err(error) = workspace_cli.run(&["status"]) {
        debug!(
            workspace = name,
            %error,
            "could not snapshot the workspace before forgetting it; continuing"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::jj::template::FIELD_SEPARATOR;
    use pretty_assertions::assert_eq;

    fn record(line: &str) -> eyre::Result<Vec<Record>> {
        WORKSPACES.parse(&format!("{line}\n"))
    }

    #[test]
    fn the_template_asks_jj_for_the_root_path_itself() {
        // The path must come from jj, not from shanti reconstructing a layout.
        let expression = WORKSPACES.expression();
        assert!(expression.contains("root"), "{expression}");
    }

    #[test]
    fn a_row_becomes_a_workspace() {
        let line = [
            "feature",
            "/w/feature",
            "false",
            "true",
            "false",
            "feature",
            "wire the pane up",
            "1000000",
        ]
        .join(&FIELD_SEPARATOR.to_string());
        let records = record(&line).unwrap();
        let workspace = Workspace::from_record(&records[0]).unwrap();

        assert_eq!(workspace.name, "feature");
        assert_eq!(workspace.root, PathBuf::from("/w/feature"));
        assert_eq!(
            workspace.local,
            JjLocal {
                empty: false,
                conflicted: true,
                divergent: false,
            }
        );
        assert_eq!(workspace.bookmarks, ["feature"]);
        assert_eq!(
            workspace.tip,
            Some(SpaceTip::new("wire the pane up", 1_000_000))
        );
    }

    /// jj's working-copy commit is usually undescribed, and a date shanti cannot
    /// read is a jj it does not recognise. Neither may cost the row: the detail
    /// goes missing, the workspace still lists.
    #[test]
    fn an_unreadable_date_costs_the_tip_and_nothing_else() {
        let undescribed = [
            "feature",
            "/w/feature",
            "true",
            "false",
            "false",
            "",
            "",
            "1000000",
        ]
        .join(&FIELD_SEPARATOR.to_string());
        let workspace = Workspace::from_record(&record(&undescribed).unwrap()[0]).unwrap();
        assert_eq!(workspace.tip, Some(SpaceTip::new("", 1_000_000)));

        let undated = [
            "feature",
            "/w/feature",
            "true",
            "false",
            "false",
            "",
            "hi",
            "yesterday",
        ]
        .join(&FIELD_SEPARATOR.to_string());
        let workspace = Workspace::from_record(&record(&undated).unwrap()[0]).unwrap();
        assert_eq!(workspace.name, "feature");
        assert_eq!(workspace.tip, None);
    }

    /// The common case: a workspace nobody has bookmarked. It must read as no
    /// bookmarks, not as one bookmark with an empty name.
    #[test]
    fn a_workspace_on_no_bookmark_lists_none() {
        let line = [
            "feature",
            "/w/feature",
            "true",
            "false",
            "false",
            "",
            "",
            "1000000",
        ]
        .join(&FIELD_SEPARATOR.to_string());
        let records = record(&line).unwrap();
        assert!(Workspace::from_record(&records[0])
            .unwrap()
            .bookmarks
            .is_empty());
    }

    #[test]
    fn a_non_boolean_flag_is_an_error_rather_than_a_false() {
        let line = [
            "feature",
            "/w/feature",
            "yes",
            "false",
            "false",
            "",
            "",
            "1000000",
        ]
        .join(&FIELD_SEPARATOR.to_string());
        let records = record(&line).unwrap();
        let error = Workspace::from_record(&records[0]).unwrap_err().to_string();

        assert!(error.contains("empty"), "{error}");
        assert!(error.contains("yes"), "{error}");
    }
}
