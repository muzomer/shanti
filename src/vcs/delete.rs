//! What deleting a space would cost.
//!
//! Deletion is the one thing shanti does that the user cannot undo by running
//! shanti again, so it is the one thing that has to ask the status model a
//! question before acting. This module is that question, and nothing else: it
//! turns a [`SpaceStatus`] into *what would be lost* and *whether anything could
//! bring it back*. It decides no keybinding and draws no dialog — the UI reads
//! the answer and picks a proportionate ceremony.
//!
//! Two rules keep it honest:
//!
//! 1. **Safety is decided by the status model, never re-derived here.** The
//!    gate is exactly [`SpaceStatus::has_unsaved_work`] or
//!    [`RemoteState::has_unpushed_work`]. Everything else in this file is
//!    wording. A parallel dirty-check would be a second opinion, and the two
//!    would drift.
//! 2. **The wording is the owning backend's.** "Uncommitted" does not exist in
//!    jj, which auto-commits; "unpushed" there means a bookmark no remote
//!    tracks. A message written in git's vocabulary would be false for half the
//!    list.

use super::{Backend, LocalState, RemoteState, Space, SpaceStatus};

/// One thing that lives only inside a space, and would go with it.
///
/// Modelled as data rather than as a pre-built sentence so the same reading can
/// be phrased in either backend's vocabulary — and counted, tested and reordered
/// without touching prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtRisk {
    /// Git: tracked files differ from the index/HEAD. Has no jj equivalent.
    Uncommitted,
    /// The space was never probed, so nothing can be promised about it.
    Unprobed,
    /// jj: the change carries unresolved conflict markers.
    Conflicted,
    /// jj: several visible commits share this change id.
    Divergent,
    /// Commits (git) or changes (jj) the upstream does not have.
    Unpushed(u32),
    /// No upstream was ever configured: nothing on a remote holds this work.
    NeverPushed,
    /// There was an upstream and it has since disappeared.
    UpstreamGone,
}

impl AtRisk {
    /// One line of plain English, in `backend`'s vocabulary.
    pub fn describe(self, backend: Backend) -> String {
        // The unit of history each backend counts in. jj has no "commit" the
        // user works on; it has changes.
        let (unit, plural) = match backend {
            Backend::Git => ("commit", "commits"),
            Backend::Jj => ("change", "changes"),
        };
        // What a name for a line of work is called on each side.
        let pointer = match backend {
            Backend::Git => "branch",
            Backend::Jj => "bookmark",
        };
        match self {
            AtRisk::Uncommitted => "uncommitted changes in the working tree".to_string(),
            AtRisk::Unprobed => "work shanti could not check for".to_string(),
            AtRisk::Conflicted => "a change with unresolved conflicts".to_string(),
            AtRisk::Divergent => "a divergent change".to_string(),
            AtRisk::Unpushed(1) => format!("1 {} no remote has", unit),
            AtRisk::Unpushed(n) => format!("{} {} no remote has", n, plural),
            AtRisk::NeverPushed => format!("a {} that was never pushed", pointer),
            AtRisk::UpstreamGone => format!("a {} whose remote is gone", pointer),
        }
    }
}

/// What the user is actually agreeing to.
///
/// The middle variant exists because the two backends genuinely differ: the jj
/// adapter snapshots the working copy before forgetting a workspace, so the work
/// survives as a head that `jj undo` restores, while a removed git worktree takes
/// its uncommitted changes with it. Showing one maximally frightening message
/// for both would train the user to ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consequence {
    /// Nothing lives only in this space; deleting it costs a directory.
    Nothing,
    /// Work lives only here, but the backend keeps a way back.
    RecoverableLoss,
    /// Work lives only here and deleting it destroys it.
    PermanentLoss,
}

/// The verdict on one space: how bad deleting it would be, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionRisk {
    backend: Backend,
    consequence: Consequence,
    at_risk: Vec<AtRisk>,
}

impl DeletionRisk {
    /// Assess a space from the snapshot it carries.
    ///
    /// The vocabulary comes from `space.backend` — the backend the deletion will
    /// actually go through — rather than from the status, which may be an
    /// unprobed reading.
    pub fn of(space: &Space) -> Self {
        Self::assess(space.backend, &space.status)
    }

    /// The verdict for a status that is not (yet) attached to a space.
    pub fn assess(backend: Backend, status: &SpaceStatus) -> Self {
        // The gate, and the only place it is decided. `has_unsaved_work` already
        // answers cautiously for an unprobed space, which is why nothing here
        // has to special-case "I don't know".
        let loses_work = status.has_unsaved_work() || status.remote.has_unpushed_work();

        let consequence = match (loses_work, backend) {
            (false, _) => Consequence::Nothing,
            // jj snapshots the working copy before forgetting the workspace, so
            // the work is still in the operation log.
            (true, Backend::Jj) => Consequence::RecoverableLoss,
            // git prunes the worktree and deletes the branch; an uncommitted
            // file is then in no object store anywhere.
            (true, Backend::Git) => Consequence::PermanentLoss,
        };

        Self {
            backend,
            consequence,
            at_risk: items_at_risk(status),
        }
    }

    /// Whether the ordinary one-keypress confirmation is enough.
    pub fn is_safe(&self) -> bool {
        self.consequence == Consequence::Nothing
    }

    pub fn consequence(&self) -> Consequence {
        self.consequence
    }

    /// The individual readings behind the verdict, for callers that want them
    /// unphrased.
    pub fn items(&self) -> &[AtRisk] {
        &self.at_risk
    }

    /// One line per thing that would be lost, in the owning backend's words.
    pub fn losses(&self) -> Vec<String> {
        self.at_risk
            .iter()
            .map(|item| item.describe(self.backend))
            .collect()
    }

    /// The sentence that says whether there is a way back. `None` when there is
    /// nothing to lose in the first place.
    ///
    /// Deliberately short: it is drawn inside a dialog whose width is a fraction
    /// of the terminal's, and a truncated warning is worse than a terse one.
    pub fn aftermath(&self) -> Option<&'static str> {
        match self.consequence {
            Consequence::Nothing => None,
            Consequence::RecoverableLoss => Some("jj can bring it back: jj undo"),
            Consequence::PermanentLoss => Some("This cannot be undone."),
        }
    }
}

/// Everything the status is saying, itemised.
///
/// Kept separate from the safety decision above so that wording can grow richer
/// without any chance of it changing what is allowed.
fn items_at_risk(status: &SpaceStatus) -> Vec<AtRisk> {
    let mut items = Vec::new();
    match status.local {
        LocalState::Unknown { .. } => items.push(AtRisk::Unprobed),
        LocalState::Git { dirty: true } => items.push(AtRisk::Uncommitted),
        LocalState::Git { dirty: false } => {}
        LocalState::Jj(jj) => {
            // Both, when both are true: they are independent failures and the
            // user should hear about each.
            if jj.conflicted {
                items.push(AtRisk::Conflicted);
            }
            if jj.divergent {
                items.push(AtRisk::Divergent);
            }
            // `empty` is not a risk — it is the opposite of one.
        }
    }
    match status.remote {
        RemoteState::Tracked { ahead, .. } if ahead > 0 => items.push(AtRisk::Unpushed(ahead)),
        RemoteState::Gone => items.push(AtRisk::UpstreamGone),
        RemoteState::Untracked => items.push(AtRisk::NeverPushed),
        // Behind-only and in-sync lose nothing; an unprobed remote is already
        // covered by the local half, which fails safe.
        _ => {}
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::JjLocal;

    fn git(remote: RemoteState, dirty: bool) -> DeletionRisk {
        DeletionRisk::assess(Backend::Git, &SpaceStatus::git(remote, dirty))
    }

    fn jj(remote: RemoteState, local: JjLocal) -> DeletionRisk {
        DeletionRisk::assess(Backend::Jj, &SpaceStatus::jj(remote, local))
    }

    /// The only case that deletes on one keypress: nothing lives only here.
    #[test]
    fn a_clean_pushed_space_is_safe_in_both_backends() {
        assert!(git(RemoteState::in_sync(), false).is_safe());
        assert!(jj(RemoteState::in_sync(), JjLocal::clean()).is_safe());
    }

    /// Being behind the upstream costs nothing on deletion — the work is on the
    /// remote, which is where "behind" means it came from.
    #[test]
    fn being_behind_the_upstream_is_not_a_loss() {
        let behind = RemoteState::Tracked {
            ahead: 0,
            behind: 7,
        };
        assert!(git(behind, false).is_safe());
    }

    #[test]
    fn a_dirty_git_worktree_is_permanently_lost() {
        let risk = git(RemoteState::in_sync(), true);
        assert_eq!(risk.consequence(), Consequence::PermanentLoss);
        assert_eq!(risk.losses(), ["uncommitted changes in the working tree"]);
    }

    /// The asymmetry that the dialog exists to tell the truth about: the same
    /// "work only lives here" reading is recoverable under jj.
    #[test]
    fn the_same_reading_is_recoverable_under_jj() {
        let conflicted = JjLocal {
            conflicted: true,
            ..JjLocal::clean()
        };
        let risk = jj(RemoteState::in_sync(), conflicted);
        assert_eq!(risk.consequence(), Consequence::RecoverableLoss);
        assert_eq!(risk.aftermath(), Some("jj can bring it back: jj undo"));
    }

    /// "Uncommitted" is meaningless in jj and "unpushed" means an untracked
    /// bookmark, so neither message may be written in git's words.
    #[test]
    fn each_backend_is_described_in_its_own_vocabulary() {
        assert_eq!(
            git(RemoteState::Untracked, false).losses(),
            ["a branch that was never pushed"]
        );
        assert_eq!(
            jj(RemoteState::Untracked, JjLocal::clean()).losses(),
            ["a bookmark that was never pushed"]
        );
        assert_eq!(
            git(
                RemoteState::Tracked {
                    ahead: 3,
                    behind: 0
                },
                false
            )
            .losses(),
            ["3 commits no remote has"]
        );
        assert_eq!(
            jj(
                RemoteState::Tracked {
                    ahead: 3,
                    behind: 0
                },
                JjLocal::clean()
            )
            .losses(),
            ["3 changes no remote has"]
        );
    }

    #[test]
    fn one_unpushed_commit_is_not_pluralised() {
        assert_eq!(
            git(
                RemoteState::Tracked {
                    ahead: 1,
                    behind: 0
                },
                false
            )
            .losses(),
            ["1 commit no remote has"]
        );
    }

    /// Two independent readings are two lines; neither hides the other.
    #[test]
    fn every_reading_gets_its_own_line() {
        let risk = git(RemoteState::Gone, true);
        assert_eq!(
            risk.losses(),
            [
                "uncommitted changes in the working tree",
                "a branch whose remote is gone"
            ]
        );

        let both = JjLocal {
            conflicted: true,
            divergent: true,
            empty: false,
        };
        assert_eq!(jj(RemoteState::in_sync(), both).items().len(), 2);
    }

    /// An empty jj change is jj's version of "clean", not a warning.
    #[test]
    fn an_empty_jj_change_is_not_a_loss() {
        let empty = JjLocal {
            empty: true,
            ..JjLocal::clean()
        };
        assert!(jj(RemoteState::in_sync(), empty).is_safe());
    }

    /// `LocalState::Unknown` reports unsaved work on purpose; the guard must
    /// carry that through instead of quietly treating "not probed" as "clean".
    #[test]
    fn an_unprobed_space_is_never_safe() {
        for backend in [Backend::Git, Backend::Jj] {
            let risk = DeletionRisk::assess(backend, &SpaceStatus::unknown(backend));
            assert!(!risk.is_safe(), "{backend} unprobed");
            assert_eq!(risk.losses(), ["work shanti could not check for"]);
        }
    }

    /// The itemisation is only ever wording: it must agree with the status
    /// model's own verdict for every state either backend can report.
    #[test]
    fn the_itemisation_never_disagrees_with_the_gate() {
        let remotes = [
            RemoteState::Unknown,
            RemoteState::in_sync(),
            RemoteState::Tracked {
                ahead: 2,
                behind: 0,
            },
            RemoteState::Tracked {
                ahead: 0,
                behind: 2,
            },
            RemoteState::Tracked {
                ahead: 2,
                behind: 2,
            },
            RemoteState::Gone,
            RemoteState::Untracked,
        ];
        let jj_locals = [true, false]
            .into_iter()
            .flat_map(|empty| {
                [true, false].into_iter().flat_map(move |conflicted| {
                    [true, false].into_iter().map(move |divergent| JjLocal {
                        empty,
                        conflicted,
                        divergent,
                    })
                })
            })
            .collect::<Vec<_>>();

        let mut statuses: Vec<(Backend, SpaceStatus)> = Vec::new();
        for remote in remotes {
            for dirty in [true, false] {
                statuses.push((Backend::Git, SpaceStatus::git(remote, dirty)));
            }
            for local in &jj_locals {
                statuses.push((Backend::Jj, SpaceStatus::jj(remote, *local)));
            }
        }
        statuses.push((Backend::Git, SpaceStatus::unknown(Backend::Git)));
        statuses.push((Backend::Jj, SpaceStatus::unknown(Backend::Jj)));

        for (backend, status) in statuses {
            let risk = DeletionRisk::assess(backend, &status);
            assert_eq!(
                risk.is_safe(),
                risk.items().is_empty(),
                "wording and gate disagree for {status:?}"
            );
            assert_eq!(risk.aftermath().is_none(), risk.is_safe(), "for {status:?}");
        }
    }
}
