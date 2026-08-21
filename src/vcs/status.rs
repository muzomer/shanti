//! Status of a space, as far as the domain model needs it today.
//!
//! **Boundary note:** the real modelling work — expressing git *and* jj state
//! (tracked/untracked bookmarks, ahead/behind counts, empty, conflicted,
//! divergent changes) — is deliberately not done here. What is present is the
//! minimum that lets [`Space`](super::Space) and the [`Vcs`](super::Vcs) trait
//! typecheck, split so it can grow without changing either: a shared "remote"
//! half and a backend-specific "local" half.

use super::Backend;

/// A snapshot of what a space looks like right now.
///
/// Owned and plain by design: the UI holds these across frames, and a later
/// background refresh (Track D) will want to send them between threads.
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
}

/// How the space relates to its upstream.
///
/// `Tracked` is expected to gain ahead/behind counts; the
/// variant names are chosen so that adding fields is the only change needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteState {
    /// Not probed yet.
    Unknown,
    /// An upstream is configured and still exists.
    Tracked,
    /// An upstream was configured but has since disappeared (merged/deleted).
    Gone,
    /// No upstream was ever configured.
    Untracked,
}

/// Local working state, which the two backends genuinely disagree about:
/// git has a dirty working tree, jj auto-commits and instead has empty,
/// conflicted or divergent changes. Modelling them as separate variants keeps
/// either backend from having to lie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    /// Not probed yet; carries the backend so the renderer still knows which
    /// vocabulary applies.
    Unknown { backend: Backend },
    /// Git-shaped local state.
    Git { dirty: bool },
    /// Jujutsu-shaped local state.
    Jj {
        empty: bool,
        conflicted: bool,
        divergent: bool,
    },
}
