//! The newest commit — or jj change — a space is sitting on.
//!
//! The list can say whether a space is dirty or unpushed, but not whether it is
//! *finished with*: that judgement needs the last thing done in it, and its age.
//! Both backends can answer while they are already listing spaces, so the
//! reading is part of the same snapshot rather than a second, slower pass.
//!
//! The timestamp is kept as plain Unix seconds and the age is derived at render
//! time. A snapshot that stored "3 hours ago" would be wrong the moment it was
//! taken, and the list outlives many frames.

/// The head commit of a space, as far as the UI is concerned.
///
/// Owned and backend-neutral, like everything else that crosses out of `vcs`: a
/// git commit summary and a jj change description are the same field here, and
/// the pane that draws them never learns which one it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceTip {
    /// First line of the commit message, empty when there is none. jj's
    /// working-copy commit is routinely undescribed, so "empty" is an ordinary
    /// reading rather than a fault.
    pub subject: String,
    /// When it was committed, in Unix seconds. Signed because that is what both
    /// git2 and jj hand back, and because a repository can carry a pre-1970
    /// date.
    pub committed_at: i64,
}

impl SpaceTip {
    pub fn new(subject: impl Into<String>, committed_at: i64) -> Self {
        Self {
            subject: subject.into(),
            committed_at,
        }
    }

    /// The subject, or a stand-in when the commit carries no message.
    ///
    /// The stand-in is chosen here rather than in the pane so that both
    /// backends' idea of "undescribed" reads identically.
    pub fn subject_or_placeholder(&self) -> &str {
        if self.subject.trim().is_empty() {
            "(no description)"
        } else {
            self.subject.trim()
        }
    }

    /// How old the commit is, relative to `now` (Unix seconds), in the coarse
    /// shorthand a list scan wants: `2m`, `5h`, `3d`, `7w`.
    ///
    /// Coarse on purpose — the question the pane answers is "did I touch this
    /// this week?", not "when exactly?" — and one unit only, so the value column
    /// stays narrow enough to sit beside a subject.
    ///
    /// A commit dated in the future (a skewed clock, a rewritten history) reads
    /// as `now` rather than as a negative age.
    pub fn age(&self, now: i64) -> String {
        let seconds = (now - self.committed_at).max(0);
        const MINUTE: i64 = 60;
        const HOUR: i64 = 60 * MINUTE;
        const DAY: i64 = 24 * HOUR;
        const WEEK: i64 = 7 * DAY;
        // A chain rather than a match on ranges: the arms are open-ended and
        // deliberately overlap, which is exactly what a range match forbids.
        if seconds < MINUTE {
            "now".to_string()
        } else if seconds < HOUR {
            format!("{}m", seconds / MINUTE)
        } else if seconds < DAY {
            format!("{}h", seconds / HOUR)
        } else if seconds < WEEK {
            format!("{}d", seconds / DAY)
        } else {
            format!("{}w", seconds / WEEK)
        }
    }
}

/// The wall clock, in the same unit [`SpaceTip::age`] expects.
///
/// A clock before the epoch is impossible in practice but not in the type, and
/// a panic in the render thread would be a poor trade for it: it reads as 0,
/// which makes every commit look old rather than crashing the UI.
pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn age_of(seconds_ago: i64) -> String {
        SpaceTip::new("x", 1_000_000).age(1_000_000 + seconds_ago)
    }

    #[test]
    fn the_age_is_one_coarse_unit() {
        assert_eq!(age_of(0), "now");
        assert_eq!(age_of(59), "now");
        assert_eq!(age_of(60), "1m");
        assert_eq!(age_of(60 * 90), "1h");
        assert_eq!(age_of(60 * 60 * 30), "1d");
        assert_eq!(age_of(60 * 60 * 24 * 20), "2w");
    }

    /// A clock that disagrees with the repository must not render a negative
    /// age; the pane would show "-3d ago", which reads as a bug in shanti.
    #[test]
    fn a_commit_from_the_future_is_not_negative() {
        assert_eq!(age_of(-5000), "now");
    }

    #[test]
    fn an_undescribed_commit_says_so_rather_than_showing_a_blank() {
        assert_eq!(
            SpaceTip::new("   ", 0).subject_or_placeholder(),
            "(no description)"
        );
        assert_eq!(SpaceTip::new(" hi ", 0).subject_or_placeholder(), "hi");
    }
}
