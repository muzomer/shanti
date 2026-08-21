//! Reading `jj workspace list` into something shanti can hold.
//!
//! One template, one process, every workspace: `jj workspace list` renders each
//! workspace through the `WorkspaceRef` type, which exposes the workspace's name,
//! its root directory *and* its working-copy commit. Everything shanti needs to
//! build a [`Space`](crate::vcs::Space) therefore comes back from a single read —
//! no second command per workspace, and no opening each directory as if it were
//! its own repository (it is not: workspaces share one repo store).

use std::path::PathBuf;

use color_eyre::eyre::{self, eyre};

use super::template::{Record, Template};
use crate::vcs::JjLocal;

/// The fields shanti reads for every workspace, as jj template expressions.
///
/// `root` is the reason this can stay one query: jj knows where each workspace
/// lives, so shanti never has to guess a path from a layout convention.
///
/// The three `target.*` flags are the jj-native local signals of
/// [`JjLocal`]; they are read here because they cost nothing extra once this
/// template is already rendering the working-copy commit.
pub const WORKSPACES: Template = Template::new(&[
    ("name", "name"),
    ("root", "root"),
    ("empty", "target.empty()"),
    ("conflicted", "target.conflict()"),
    ("divergent", "target.divergent()"),
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
}

impl Workspace {
    /// Build a workspace from a record rendered by [`WORKSPACES`].
    pub fn from_record(record: &Record) -> eyre::Result<Self> {
        Ok(Self {
            name: record.get("name")?.to_owned(),
            root: PathBuf::from(record.get("root")?),
            local: JjLocal {
                empty: boolean(record, "empty")?,
                conflicted: boolean(record, "conflicted")?,
                divergent: boolean(record, "divergent")?,
            },
        })
    }
}

/// Read a jj `Boolean` field.
///
/// Rejects anything that is not exactly `true`/`false` rather than treating an
/// unexpected value as `false`: a silent `false` here would report a conflicted
/// workspace as clean, which is the class of lie this whole module exists to
/// avoid.
fn boolean(record: &Record, field: &str) -> eyre::Result<bool> {
    match record.get(field)? {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(eyre!(
            "jj rendered {field:?} as {other:?}, which is not a boolean"
        )),
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
        let line =
            ["feature", "/w/feature", "false", "true", "false"].join(&FIELD_SEPARATOR.to_string());
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
    }

    #[test]
    fn a_non_boolean_flag_is_an_error_rather_than_a_false() {
        let line =
            ["feature", "/w/feature", "yes", "false", "false"].join(&FIELD_SEPARATOR.to_string());
        let records = record(&line).unwrap();
        let error = Workspace::from_record(&records[0]).unwrap_err().to_string();

        assert!(error.contains("empty"), "{error}");
        assert!(error.contains("yes"), "{error}");
    }
}
