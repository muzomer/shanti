//! What a space looks like right now, in vocabulary both backends can speak.
//!
//! The model is split in two halves because the backends only agree about one
//! of them:
//!
//! * the **remote** half — is there an upstream, does it still exist, how far
//!   apart are we — means the same thing for a git branch and a jj bookmark, so
//!   it is shared;
//! * the **local** half genuinely differs. Git has a dirty working tree; jj
//!   auto-commits, so "dirty" does not exist there and is replaced by states
//!   git has no word for (the change is empty, conflicted, or divergent).
//!
//! Forcing one shape onto both is what produced the workaround this module
//! replaces: `git::repository::is_worktree_dirty` returns `false` whenever a
//! `.jj` directory is present — reporting "clean" because the git-shaped
//! question is unanswerable, which is a lie rather than an absence.
//!
//! Everything here is an owned, `'static` snapshot: the UI keeps statuses
//! across frames and a later background refresh will move them between threads.
//!
//! The glyph mapping lives here too ([`SpaceStatus::glyphs`]). The renderer
//! should never match on [`LocalState`] to decide what to draw — if it did,
//! every new jj state would mean editing the UI. It asks for two glyphs and
//! draws them.

use super::Backend;

/// A snapshot of what a space looks like right now.
///
/// Owned and plain by design: the UI holds these across frames, and a later
/// background refresh will want to send them between threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceStatus {
    /// Relationship to the upstream/remote. Shared vocabulary across backends.
    pub remote: RemoteState,
    /// Backend-specific local state; see [`LocalState`].
    pub local: LocalState,
}

impl SpaceStatus {
    /// Status for a space we have not inspected yet.
    ///
    /// Discovery is cheap but status probing is not, so callers are expected to
    /// list spaces first and fill status in afterwards.
    pub fn unknown(backend: Backend) -> Self {
        Self {
            remote: RemoteState::Unknown,
            local: LocalState::Unknown { backend },
        }
    }

    /// Status of a git space.
    ///
    /// Named constructors rather than struct literals so backends cannot
    /// accidentally pair a git remote reading with a jj local one.
    pub fn git(remote: RemoteState, dirty: bool) -> Self {
        Self {
            remote,
            local: LocalState::Git { dirty },
        }
    }

    /// Status of a jj space. See [`LocalState::Jj`] for what the flags mean.
    pub fn jj(remote: RemoteState, local: JjLocal) -> Self {
        Self {
            remote,
            local: LocalState::Jj(local),
        }
    }

    /// Which backend's vocabulary this status is written in.
    ///
    /// Useful for wording ("worktree" vs "workspace"); *not* for deciding what
    /// to draw — use [`SpaceStatus::glyphs`] for that.
    pub fn backend(&self) -> Backend {
        self.local.backend()
    }

    /// The two-slot indicator: one glyph for the remote half, one for the
    /// local half.
    ///
    /// Two slots rather than one because the current UI overloads a single
    /// slot with four unrelated meanings, so "has an upstream" and "has
    /// uncommitted work" fight over the same character cell.
    pub fn glyphs(&self) -> [StatusGlyph; 2] {
        [self.remote.glyph(), self.local.glyph()]
    }

    /// Whether deleting this space would destroy work that exists nowhere else.
    ///
    /// This is the question the delete guard needs, and it is exactly the
    /// question each backend answers differently: git loses an uncommitted
    /// working tree, while jj has already committed it and instead risks losing
    /// a conflicted or divergent change that no bookmark points at.
    pub fn has_unsaved_work(&self) -> bool {
        self.local.has_unsaved_work()
    }
}

/// How the space relates to its upstream.
///
/// Shared across backends: a git branch's upstream and a jj bookmark's remote
/// tracking state answer the same three questions — is there one, does it still
/// exist, and how far apart are we.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteState {
    /// Not probed yet.
    Unknown,
    /// An upstream is configured and still exists.
    ///
    /// Counts are commits/changes on one side only, so `ahead: 0, behind: 0`
    /// means "in sync" and both non-zero means the histories have diverged.
    Tracked { ahead: u32, behind: u32 },
    /// An upstream was configured but has since disappeared (merged/deleted).
    Gone,
    /// No upstream was ever configured — a local-only branch, or in jj an
    /// untracked bookmark.
    Untracked,
}

impl RemoteState {
    /// Tracked and level with the upstream.
    pub fn in_sync() -> Self {
        RemoteState::Tracked {
            ahead: 0,
            behind: 0,
        }
    }

    /// Whether this space has commits the upstream does not.
    ///
    /// `Untracked` counts as unpushed: nothing on a remote holds this work.
    /// `Gone` does too — the upstream that held it is no longer there.
    pub fn has_unpushed_work(self) -> bool {
        match self {
            RemoteState::Tracked { ahead, .. } => ahead > 0,
            RemoteState::Gone | RemoteState::Untracked => true,
            RemoteState::Unknown => false,
        }
    }

    fn glyph(self) -> StatusGlyph {
        match self {
            RemoteState::Unknown => StatusGlyph::new("·", Tone::Muted, "status not checked yet"),
            RemoteState::Tracked {
                ahead: 0,
                behind: 0,
            } => StatusGlyph::new("✔", Tone::Ok, "in sync with upstream"),
            RemoteState::Tracked { ahead: 0, .. } => {
                StatusGlyph::new("↓", Tone::Info, "behind upstream")
            }
            RemoteState::Tracked { behind: 0, .. } => {
                StatusGlyph::new("↑", Tone::Warn, "ahead of upstream")
            }
            RemoteState::Tracked { .. } => {
                StatusGlyph::new("↕", Tone::Warn, "diverged from upstream")
            }
            RemoteState::Gone => StatusGlyph::new("✘", Tone::Danger, "upstream is gone"),
            RemoteState::Untracked => StatusGlyph::new("⬆", Tone::Warn, "never pushed"),
        }
    }
}

/// Local working state, which the two backends genuinely disagree about:
/// git has a dirty working tree, jj auto-commits and instead has empty,
/// conflicted or divergent changes. Modelling them as separate variants keeps
/// either backend from having to lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalState {
    /// Not probed yet; carries the backend so the renderer still knows which
    /// vocabulary applies.
    Unknown { backend: Backend },
    /// Git-shaped local state.
    Git { dirty: bool },
    /// Jujutsu-shaped local state.
    Jj(JjLocal),
}

impl LocalState {
    /// Which backend's vocabulary this reading is written in.
    pub fn backend(self) -> Backend {
        match self {
            LocalState::Unknown { backend } => backend,
            LocalState::Git { .. } => Backend::Git,
            LocalState::Jj(_) => Backend::Jj,
        }
    }

    /// See [`SpaceStatus::has_unsaved_work`].
    ///
    /// An unprobed space is reported as *having* work: the guard that uses this
    /// protects against data loss, so the safe answer to "I don't know" is the
    /// cautious one.
    pub fn has_unsaved_work(self) -> bool {
        match self {
            LocalState::Unknown { .. } => true,
            LocalState::Git { dirty } => dirty,
            LocalState::Jj(jj) => jj.conflicted || jj.divergent,
        }
    }

    fn glyph(self) -> StatusGlyph {
        match self {
            LocalState::Unknown { .. } => StatusGlyph::new("·", Tone::Muted, "status not checked"),
            LocalState::Git { dirty: true } => {
                StatusGlyph::new("*", Tone::Warn, "uncommitted changes")
            }
            LocalState::Git { dirty: false } => StatusGlyph::clean(),
            LocalState::Jj(jj) => jj.glyph(),
        }
    }
}

/// The jj-native local signals, grouped so [`LocalState::Jj`] stays readable and
/// so new jj states are added in one place.
///
/// All three are independent flags rather than an enum: a change can be empty
/// *and* divergent, and collapsing them would throw information away. Which one
/// wins the single glyph slot is a display decision, made in [`JjLocal::glyph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JjLocal {
    /// The working-copy change has no diff against its parent — jj's "nothing
    /// to see here", and the closest thing jj has to git's "clean".
    pub empty: bool,
    /// The change contains conflict markers left by a rebase or merge.
    pub conflicted: bool,
    /// Several visible commits share this change id, so the change no longer
    /// identifies a single commit.
    pub divergent: bool,
}

impl JjLocal {
    /// The ordinary case: work in progress, nothing wrong with it.
    pub fn clean() -> Self {
        Self::default()
    }

    /// One glyph, so severity decides: a conflict blocks work, divergence
    /// silently breaks the change id, and emptiness is merely informational.
    fn glyph(self) -> StatusGlyph {
        if self.conflicted {
            StatusGlyph::new("!", Tone::Danger, "change has conflicts")
        } else if self.divergent {
            StatusGlyph::new("≠", Tone::Danger, "change is divergent")
        } else if self.empty {
            StatusGlyph::new("∅", Tone::Muted, "working copy is empty")
        } else {
            StatusGlyph::clean()
        }
    }
}

/// One slot of the status indicator: what to draw, how loudly, and what it
/// means if the user asks.
///
/// `&'static str` throughout so a glyph is `Copy` and costs nothing to pass
/// around during a render pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusGlyph {
    /// The character to draw. Exactly one cell wide, so slots stay aligned in a
    /// column.
    pub symbol: &'static str,
    /// How much attention it deserves; the UI turns this into a colour.
    pub tone: Tone,
    /// Plain-English meaning, for a legend, a tooltip or a status line.
    pub meaning: &'static str,
}

impl StatusGlyph {
    const fn new(symbol: &'static str, tone: Tone, meaning: &'static str) -> Self {
        Self {
            symbol,
            tone,
            meaning,
        }
    }

    /// Nothing worth saying. Still occupies a cell so columns line up, but it
    /// is deliberately blank rather than a "clean" tick: the eye should only be
    /// caught by states that need action.
    const fn clean() -> Self {
        Self::new(" ", Tone::Muted, "clean")
    }

    /// Whether this slot is saying anything at all.
    pub fn is_blank(self) -> bool {
        self.symbol == " "
    }
}

/// How loudly a glyph should be drawn.
///
/// Semantic rather than a `ratatui::Color` on purpose: the domain model must not
/// depend on the UI toolkit, and the theme gets to decide what "danger" looks
/// like. Mapping five tones to five colours is the one match the renderer keeps
/// — and it never has to know which backend it is drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Secondary or unknown; draw dimmed.
    Muted,
    /// Everything is as it should be.
    Ok,
    /// Worth knowing, not a problem.
    Info,
    /// Needs the user's attention eventually.
    Warn,
    /// Needs it now, or work could be lost.
    Danger,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole module exists so statuses can be computed off the render
    /// thread and sent back; a non-`Send` field would break that silently.
    #[test]
    fn status_is_an_owned_sendable_snapshot() {
        fn assert_send_static<T: Send + 'static>(_: &T) {}
        assert_send_static(&SpaceStatus::unknown(Backend::Jj));
    }

    #[test]
    fn unknown_keeps_the_backend_it_was_built_for() {
        let status = SpaceStatus::unknown(Backend::Jj);
        assert_eq!(status.backend(), Backend::Jj);
        assert_eq!(status.remote, RemoteState::Unknown);
    }

    /// Every distinct git situation stays distinct once expressed as a
    /// `RemoteState` and a dirty flag. Two situations that collapsed onto one
    /// value would make the two spaces indistinguishable in the list.
    #[test]
    fn distinct_git_situations_stay_distinct() {
        let remotes = [
            RemoteState::in_sync(),
            RemoteState::Gone,
            RemoteState::Untracked,
        ];

        let converted: Vec<SpaceStatus> = remotes
            .into_iter()
            .flat_map(|remote| [false, true].map(|dirty| SpaceStatus::git(remote, dirty)))
            .collect();

        assert_eq!(converted.len(), 6);
        for (i, a) in converted.iter().enumerate() {
            for b in &converted[i + 1..] {
                assert_ne!(a, b, "two old states collapsed onto one new one");
            }
        }
    }

    #[test]
    fn ahead_and_behind_get_their_own_glyphs() {
        let cases = [
            (RemoteState::in_sync(), "✔"),
            (
                RemoteState::Tracked {
                    ahead: 2,
                    behind: 0,
                },
                "↑",
            ),
            (
                RemoteState::Tracked {
                    ahead: 0,
                    behind: 3,
                },
                "↓",
            ),
            (
                RemoteState::Tracked {
                    ahead: 2,
                    behind: 3,
                },
                "↕",
            ),
            (RemoteState::Gone, "✘"),
            (RemoteState::Untracked, "⬆"),
            (RemoteState::Unknown, "·"),
        ];

        for (remote, expected) in cases {
            assert_eq!(remote.glyph().symbol, expected, "for {remote:?}");
        }
    }

    #[test]
    fn a_clean_space_leaves_the_local_slot_blank() {
        assert!(SpaceStatus::git(RemoteState::in_sync(), false).glyphs()[1].is_blank());
        assert!(SpaceStatus::jj(RemoteState::in_sync(), JjLocal::clean()).glyphs()[1].is_blank());
    }

    #[test]
    fn a_dirty_git_space_marks_the_local_slot() {
        let glyph = SpaceStatus::git(RemoteState::in_sync(), true).glyphs()[1];
        assert_eq!(glyph.symbol, "*");
        assert_eq!(glyph.tone, Tone::Warn);
    }

    /// Conflicts outrank divergence, which outranks emptiness — one slot, so
    /// the most actionable state has to win.
    #[test]
    fn jj_states_are_shown_in_severity_order() {
        let all = JjLocal {
            empty: true,
            conflicted: true,
            divergent: true,
        };
        assert_eq!(LocalState::Jj(all).glyph().symbol, "!");

        let divergent = JjLocal {
            conflicted: false,
            ..all
        };
        assert_eq!(LocalState::Jj(divergent).glyph().symbol, "≠");

        let empty = JjLocal {
            divergent: false,
            ..divergent
        };
        assert_eq!(LocalState::Jj(empty).glyph().symbol, "∅");
    }

    /// jj auto-commits, so an ordinary jj space never has "unsaved" work the
    /// way a dirty git worktree does — the git-shaped question would have had
    /// to answer `false` here anyway, but now for a stated reason.
    #[test]
    fn unsaved_work_is_backend_specific() {
        assert!(SpaceStatus::git(RemoteState::in_sync(), true).has_unsaved_work());
        assert!(!SpaceStatus::git(RemoteState::in_sync(), false).has_unsaved_work());

        assert!(!SpaceStatus::jj(RemoteState::in_sync(), JjLocal::clean()).has_unsaved_work());
        let conflicted = JjLocal {
            conflicted: true,
            ..JjLocal::clean()
        };
        assert!(SpaceStatus::jj(RemoteState::in_sync(), conflicted).has_unsaved_work());
    }

    /// A space we never managed to probe must not be reported as safe to lose.
    #[test]
    fn an_unprobed_space_is_assumed_to_hold_work() {
        assert!(SpaceStatus::unknown(Backend::Git).has_unsaved_work());
    }

    #[test]
    fn unpushed_work_covers_gone_and_untracked_upstreams() {
        assert!(RemoteState::Tracked {
            ahead: 1,
            behind: 0
        }
        .has_unpushed_work());
        assert!(!RemoteState::in_sync().has_unpushed_work());
        assert!(RemoteState::Gone.has_unpushed_work());
        assert!(RemoteState::Untracked.has_unpushed_work());
    }

    /// The point of having two slots: a space can be behind its upstream *and*
    /// dirty, and one cell could only report whichever the code checked first.
    #[test]
    fn both_slots_can_speak_at_once() {
        let status = SpaceStatus::git(RemoteState::Gone, true);
        let [remote, local] = status.glyphs();
        assert_eq!(remote.symbol, "✘");
        assert_eq!(local.symbol, "*");
    }
}
