//! The remote half of a jj space's status: how a workspace relates to a remote.
//!
//! The local half ([`crate::vcs::JjLocal`]) is free — `jj workspace list` renders
//! it while it is already rendering the workspace. The remote half is not, and
//! this module is what keeps it from costing a process per space.
//!
//! # One query for the whole repository
//!
//! Tracking state is a property of a *bookmark*, not of a workspace, and jj
//! keeps bookmarks in the shared repo store. So every workspace's remote half
//! is decided from a single `jj bookmark list --all-remotes` — one process per
//! repository per refresh, no matter how many spaces it has. The join back to a
//! workspace happens in Rust, on the bookmark names `jj workspace list` already
//! reported.
//!
//! # Which bookmark speaks for a workspace
//!
//! jj has no rule that a workspace and a bookmark share a name, so the bookmark
//! has to be found by position instead: it is the one on the workspace's *head*
//! — the working-copy commit, or, when that commit is empty, its parents. The
//! second half matters because jj's working copy is an in-progress child change
//! that is empty until you touch a file; refusing to look through it would hide
//! the bookmark of every workspace nobody has started editing yet.
//!
//! Deliberately *not* the nearest bookmark anywhere up the ancestry. That would
//! find the mainline a space was branched from and report the space as "in sync"
//! while it holds work that was never pushed — the same falsely-clean answer
//! this whole status model was written to stop telling.
//!
//! # jj states the counts from the other end
//!
//! `tracking_ahead_count` on a remote ref counts how far *the remote ref* is
//! ahead of the local bookmark. [`crate::vcs::RemoteState`] counts the other
//! way, as git does — so the two are swapped exactly once, here.

use color_eyre::eyre;

use crate::vcs::RemoteState;

use super::base;
use super::template::Record;
use super::template::Template;

/// The columns shanti reads from `jj bookmark list --all-remotes`.
///
/// One row per ref: a local bookmark renders `remote` as the empty string,
/// which is how the two kinds are told apart without a second query.
///
/// The counts are guarded by `tracked` because jj errors out rather than
/// rendering a number for a ref that has no local counterpart. Their names say
/// whose point of view they are written from — see the module docs.
pub const BOOKMARKS: Template = Template::new(&[
    ("name", "name"),
    ("remote", "remote"),
    ("tracked", "tracked"),
    // False for a tracked remote ref whose bookmark was deleted upstream: the
    // ref is still known, it just points at nothing any more.
    ("present", "present"),
    (
        "remote_ahead",
        "if(tracked, tracking_ahead_count.lower(), \"0\")",
    ),
    (
        "remote_behind",
        "if(tracked, tracking_behind_count.lower(), \"0\")",
    ),
]);

/// The jj template that renders a workspace's own bookmark names.
///
/// Lives here rather than in `workspace.rs` because the rule it encodes — head
/// is the working-copy commit, or its parents when that commit is empty — is
/// this module's, and the field it produces is only ever read by [`remote_of`].
pub const WORKSPACE_BOOKMARKS: &str = concat!(
    "if(target.empty(), ",
    "target.parents().map(|p| p.local_bookmarks().map(|b| b.name()).join(\"\u{1e}\")).join(\"\u{1e}\"), ",
    "target.local_bookmarks().map(|b| b.name()).join(\"\u{1e}\"))",
);

/// How a workspace carrying `bookmarks` relates to its remote, given every
/// bookmark row of the repository.
///
/// `bookmarks` is in jj's own (sorted) order, so a workspace whose head carries
/// several bookmarks gets the same answer on every refresh: the first one that
/// has anything to say about a remote wins.
pub fn remote_of(records: &[Record], bookmarks: &[String]) -> eyre::Result<RemoteState> {
    for bookmark in bookmarks {
        match remote_of_bookmark(records, bookmark)? {
            // No real remote knows this bookmark; another one on the same head
            // still might.
            RemoteState::Untracked => continue,
            state => return Ok(state),
        }
    }
    // Reached with no bookmarks at all, which is the ordinary state of a jj
    // workspace: work exists here and nothing upstream has heard of it.
    Ok(RemoteState::Untracked)
}

/// The tracking state of one bookmark, across every remote that carries it.
///
/// `origin` wins when several do, matching [`base::remote_carrying`]; otherwise
/// the first row jj listed does, so the answer stays stable between runs.
fn remote_of_bookmark(records: &[Record], bookmark: &str) -> eyre::Result<RemoteState> {
    let mut found: Option<RemoteState> = None;
    for record in records {
        if record.get("name")? != bookmark {
            continue;
        }
        let remote = record.get("remote")?;
        // Skips the local row and the `git` pseudo-remote of a colocated repo,
        // which only mirrors local refs and would make a never-pushed bookmark
        // look pushed.
        if !base::is_real_remote(remote) {
            continue;
        }
        // A remote ref nothing local tracks is not this workspace's upstream —
        // somebody else's bookmark that happens to share the name.
        if !record.boolean("tracked")? {
            continue;
        }

        let state = if record.boolean("present")? {
            RemoteState::Tracked {
                ahead: record.count("remote_behind")?,
                behind: record.count("remote_ahead")?,
            }
        } else {
            RemoteState::Gone
        };

        if remote == base::PREFERRED_REMOTE {
            return Ok(state);
        }
        found.get_or_insert(state);
    }
    Ok(found.unwrap_or(RemoteState::Untracked))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::jj::template::FIELD_SEPARATOR;
    use pretty_assertions::assert_eq;

    /// Build rows as jj would render them for [`BOOKMARKS`].
    fn rows(rows: &[(&str, &str, bool, bool, u32, u32)]) -> Vec<Record> {
        let output: String = rows
            .iter()
            .map(|(name, remote, tracked, present, ahead, behind)| {
                [
                    (*name).to_owned(),
                    (*remote).to_owned(),
                    tracked.to_string(),
                    present.to_string(),
                    ahead.to_string(),
                    behind.to_string(),
                ]
                .join(&FIELD_SEPARATOR.to_string())
                    + "\n"
            })
            .collect();
        BOOKMARKS.parse(&output).unwrap()
    }

    fn remote(records: &[Record], bookmark: &str) -> RemoteState {
        remote_of(records, &[bookmark.to_owned()]).unwrap()
    }

    /// The ordinary jj workspace: a change with no bookmark on it at all.
    #[test]
    fn a_workspace_with_no_bookmark_has_nothing_upstream() {
        let records = rows(&[("main", "origin", true, true, 0, 0)]);
        assert_eq!(remote_of(&records, &[]).unwrap(), RemoteState::Untracked);
    }

    #[test]
    fn a_bookmark_level_with_its_remote_is_in_sync() {
        let records = rows(&[("feature", "", false, true, 0, 0)])
            .into_iter()
            .chain(rows(&[("feature", "origin", true, true, 0, 0)]))
            .collect::<Vec<_>>();
        assert_eq!(remote(&records, "feature"), RemoteState::in_sync());
    }

    /// The counts jj reports are the remote's, so they have to arrive swapped.
    #[test]
    fn jjs_counts_are_read_from_the_local_bookmarks_point_of_view() {
        // The remote ref is behind by two: locally we are two commits *ahead*.
        let records = rows(&[("feature", "origin", true, true, 0, 2)]);
        assert_eq!(
            remote(&records, "feature"),
            RemoteState::Tracked {
                ahead: 2,
                behind: 0
            }
        );

        let records = rows(&[("feature", "origin", true, true, 3, 0)]);
        assert_eq!(
            remote(&records, "feature"),
            RemoteState::Tracked {
                ahead: 0,
                behind: 3
            }
        );
    }

    #[test]
    fn a_local_only_bookmark_is_untracked_rather_than_in_sync() {
        let records = rows(&[("feature", "", false, true, 0, 0)]);
        assert_eq!(remote(&records, "feature"), RemoteState::Untracked);
    }

    /// The colocated trap: `feature@git` mirrors the local ref and says nothing
    /// about whether the bookmark was ever pushed anywhere.
    #[test]
    fn the_git_pseudo_remote_never_makes_a_bookmark_look_pushed() {
        let records = rows(&[
            ("feature", "", false, true, 0, 0),
            ("feature", "git", true, true, 0, 0),
        ]);
        assert_eq!(remote(&records, "feature"), RemoteState::Untracked);
    }

    #[test]
    fn a_remote_ref_nothing_tracks_is_not_this_workspaces_upstream() {
        let records = rows(&[("feature", "origin", false, true, 0, 0)]);
        assert_eq!(remote(&records, "feature"), RemoteState::Untracked);
    }

    #[test]
    fn a_bookmark_deleted_upstream_is_gone() {
        let records = rows(&[("feature", "origin", true, false, 0, 3)]);
        assert_eq!(remote(&records, "feature"), RemoteState::Gone);
    }

    #[test]
    fn origin_wins_over_the_other_remotes_that_carry_the_same_name() {
        let records = rows(&[
            ("feature", "fork", true, true, 0, 5),
            ("feature", "origin", true, true, 0, 0),
        ]);
        assert_eq!(remote(&records, "feature"), RemoteState::in_sync());
    }

    /// A head can carry several bookmarks; the first with an upstream answers,
    /// and a purely local one beside it must not silence it.
    #[test]
    fn a_local_only_bookmark_does_not_shadow_a_tracked_one_on_the_same_head() {
        let records = rows(&[
            ("scratch", "", false, true, 0, 0),
            ("feature", "origin", true, true, 0, 0),
        ]);
        let bookmarks = ["scratch".to_owned(), "feature".to_owned()];
        assert_eq!(
            remote_of(&records, &bookmarks).unwrap(),
            RemoteState::in_sync()
        );
    }

    /// The rule the module docs state, asserted on the template itself: an
    /// empty working copy looks through to its parents, a non-empty one does
    /// not.
    #[test]
    fn the_template_only_looks_past_an_empty_working_copy() {
        assert!(WORKSPACE_BOOKMARKS.contains("target.empty()"));
        assert!(WORKSPACE_BOOKMARKS.contains("target.parents()"));
        // Local bookmarks only: a remote ref on the head says where the remote
        // is, not which bookmark this workspace is working on.
        assert!(!WORKSPACE_BOOKMARKS.contains("remote_bookmarks"));
    }
}
