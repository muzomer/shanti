//! Where a new jj workspace starts from.
//!
//! The git backend answers this question with branches: a remote branch of the
//! same name, else the default branch, else HEAD. jj has no branches, so the
//! answer is spelled in jj's own vocabulary instead of mapping git's onto it:
//!
//! * **a bookmark of that name on a remote** — the direct equivalent of
//!   `origin/<name>`, and the case that matters when someone recreates a space
//!   for work that already exists upstream;
//! * **[`trunk()`]** — jj's own revset function for "the mainline of this
//!   repository". It is deliberately *not* resolved to a branch name here: jj
//!   already knows how to answer that, and asking it keeps shanti out of the
//!   business of guessing between `main`, `master` and a configured alias;
//! * **jj's own default** — for a repository with no mainline yet (a fresh
//!   `jj git init`, where `trunk()` degrades to the root commit). Passing no
//!   revision at all makes `jj workspace add` start the workspace beside the
//!   current working copy, which is both useful and jj's documented behaviour.
//!
//! In every case the new working-copy commit is created *on top of* the base
//! rather than editing it, because `jj workspace add -r` takes the revisions to
//! use as the new change's parents.
//!
//! [`trunk()`]: https://jj-vcs.github.io/jj/latest/revsets/#trunk

use super::template::{Record, Template};
use color_eyre::eyre;

/// The columns shanti reads from `jj bookmark list --all-remotes`.
///
/// A local bookmark renders `remote` as the empty string, which is how the two
/// kinds are told apart without a second query.
pub const REMOTE_BOOKMARKS: Template = Template::new(&[("name", "name"), ("remote", "remote")]);

/// A revset that is empty exactly when the repository has no mainline of its
/// own: jj's `trunk()` degrades to the root commit rather than to nothing, and
/// the root commit is not somewhere anyone wants to start working.
pub const REAL_TRUNK: &str = "trunk() ~ root()";

/// Anything at all about the revisions `REAL_TRUNK` matched — the query only
/// ever asks whether the answer is empty.
pub const ANY_REVISION: Template = Template::new(&[("commit", "commit_id.short()")]);

/// jj's pseudo-remote for the git refs of a colocated repository. It mirrors
/// local state, so treating it as a remote would report a bookmark that was
/// never pushed anywhere as existing upstream.
const GIT_PSEUDO_REMOTE: &str = "git";

/// The remote shanti prefers when several carry the same bookmark, matching the
/// git backend's hard-coded `origin`.
const PREFERRED_REMOTE: &str = "origin";

/// The revision a new workspace will be created on top of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// A bookmark of the requested name exists on this remote.
    RemoteBookmark { bookmark: String, remote: String },
    /// The repository's mainline, as jj itself defines it.
    Trunk,
    /// No mainline yet; let jj start the workspace beside the current working
    /// copy.
    WorkingCopy,
}

impl Base {
    /// The revset for `jj workspace add --revision`, or `None` to pass no
    /// `--revision` at all and take jj's default.
    pub fn revset(&self) -> Option<String> {
        match self {
            Self::RemoteBookmark { bookmark, remote } => Some(format!(
                "remote_bookmarks(exact:{}, remote=exact:{})",
                quote(bookmark),
                quote(remote)
            )),
            Self::Trunk => Some("trunk()".to_owned()),
            Self::WorkingCopy => None,
        }
    }

    /// One line for the create prompt, in jj's vocabulary rather than git's:
    /// nothing here is "tracked", because `jj workspace add` starts a change on
    /// top of the base and sets up no tracking of its own.
    pub fn hint(&self) -> String {
        match self {
            Self::RemoteBookmark { bookmark, remote } => {
                format!("Will start from {bookmark}@{remote}")
            }
            Self::Trunk => "Will start from trunk()".to_owned(),
            Self::WorkingCopy => "Will start beside the current working copy".to_owned(),
        }
    }
}

/// The remote carrying `bookmark`, from rows rendered by [`REMOTE_BOOKMARKS`].
///
/// `origin` wins when several remotes carry the same name; otherwise the first
/// row jj listed does, so the answer stays stable between two runs (jj sorts
/// its listing) instead of depending on iteration order.
pub fn remote_carrying(records: &[Record], bookmark: &str) -> eyre::Result<Option<String>> {
    let mut found: Option<String> = None;
    for record in records {
        if record.get("name")? != bookmark {
            continue;
        }
        let remote = record.get("remote")?;
        if remote.is_empty() || remote == GIT_PSEUDO_REMOTE {
            continue;
        }
        if remote == PREFERRED_REMOTE {
            return Ok(Some(remote.to_owned()));
        }
        found.get_or_insert_with(|| remote.to_owned());
    }
    Ok(found)
}

/// A jj string literal holding `value`.
///
/// Bookmark names come from the user, so they are quoted rather than pasted
/// into a revset: an unescaped quote would otherwise turn a name into revset
/// syntax.
fn quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::jj::template::FIELD_SEPARATOR;
    use pretty_assertions::assert_eq;

    /// Build rows as jj would render them for [`REMOTE_BOOKMARKS`].
    fn rows(pairs: &[(&str, &str)]) -> Vec<Record> {
        let output: String = pairs
            .iter()
            .map(|(name, remote)| format!("{name}{FIELD_SEPARATOR}{remote}\n"))
            .collect();
        REMOTE_BOOKMARKS.parse(&output).unwrap()
    }

    #[test]
    fn a_local_only_bookmark_is_not_a_remote_one() {
        let records = rows(&[("feature", "")]);
        assert_eq!(remote_carrying(&records, "feature").unwrap(), None);
    }

    #[test]
    fn the_git_pseudo_remote_of_a_colocated_repository_is_ignored() {
        // `feature@git` only mirrors the local ref; it says nothing about
        // whether the bookmark was ever pushed.
        let records = rows(&[("feature", ""), ("feature", "git")]);
        assert_eq!(remote_carrying(&records, "feature").unwrap(), None);
    }

    #[test]
    fn origin_wins_over_the_other_remotes_that_carry_the_same_name() {
        let records = rows(&[("feature", "fork"), ("feature", "origin")]);
        assert_eq!(
            remote_carrying(&records, "feature").unwrap().as_deref(),
            Some("origin")
        );
    }

    #[test]
    fn without_origin_the_first_remote_listed_is_used() {
        let records = rows(&[("feature", "fork"), ("feature", "upstream")]);
        assert_eq!(
            remote_carrying(&records, "feature").unwrap().as_deref(),
            Some("fork")
        );
    }

    #[test]
    fn another_bookmarks_remote_is_not_mistaken_for_this_ones() {
        let records = rows(&[("other", "origin")]);
        assert_eq!(remote_carrying(&records, "feature").unwrap(), None);
    }

    #[test]
    fn a_remote_bookmark_becomes_an_exact_revset() {
        let base = Base::RemoteBookmark {
            bookmark: "feature".to_owned(),
            remote: "origin".to_owned(),
        };
        assert_eq!(
            base.revset().as_deref(),
            Some(r#"remote_bookmarks(exact:"feature", remote=exact:"origin")"#)
        );
    }

    #[test]
    fn a_quote_in_a_bookmark_name_cannot_escape_the_revset() {
        let base = Base::RemoteBookmark {
            bookmark: r#"we"ird"#.to_owned(),
            remote: "origin".to_owned(),
        };
        let revset = base.revset().unwrap();
        assert!(revset.contains(r#"exact:"we\"ird""#), "{revset}");
    }

    #[test]
    fn no_mainline_means_no_revision_flag_at_all() {
        assert_eq!(Base::WorkingCopy.revset(), None);
        assert_eq!(Base::Trunk.revset().as_deref(), Some("trunk()"));
    }

    /// The hint is the only jj text the create prompt shows, so it must not
    /// borrow git's words for concepts jj does not have.
    #[test]
    fn the_hints_speak_jj_not_git() {
        let hints = [
            Base::RemoteBookmark {
                bookmark: "feature".to_owned(),
                remote: "origin".to_owned(),
            }
            .hint(),
            Base::Trunk.hint(),
            Base::WorkingCopy.hint(),
        ];
        for hint in &hints {
            assert!(!hint.contains("branch"), "{hint}");
            assert!(!hint.contains("origin/"), "{hint}");
        }
        assert_eq!(hints[0], "Will start from feature@origin");
    }
}
