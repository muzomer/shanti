//! Which version-control system drives a repository.

/// The version-control system backing a [`Repo`](super::Repo).
///
/// Kept as a small closed enum rather than a trait-level type parameter: the
/// repository list is heterogeneous at runtime, and the renderer needs a cheap,
/// `Copy` tag it can match on for badges and help text without reaching for a
/// backend handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Backend {
    /// Plain git; a space is a `git worktree`.
    Git,
    /// Jujutsu; a space is a `jj workspace`.
    Jj,
}

impl Backend {
    /// Short label for the UI. Lives here so every view spells it the same way.
    pub fn label(self) -> &'static str {
        match self {
            Backend::Git => "git",
            Backend::Jj => "jj",
        }
    }

    /// The backend's own word for a space, for messages addressed to users who
    /// think in git or jj terms ("delete this worktree" / "…this workspace").
    pub fn space_noun(self) -> &'static str {
        match self {
            Backend::Git => "worktree",
            Backend::Jj => "workspace",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}
