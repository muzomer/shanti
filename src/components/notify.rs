//! What the app has to tell the user, how loudly, and for how long.
//!
//! This replaces `WorktreesComponent::last_error`, one `Option<String>` that
//! was doing three jobs at once: it was the error channel, the warning channel
//! *and* the success channel, so "PR is merged — existing worktree selected"
//! was drawn in bold red as though something had broken. It also never cleared
//! itself — the only thing that removed a message was the next action that
//! happened to set one, so a stale failure could sit on the border for the rest
//! of the session.
//!
//! Three things are fixed here, and each is a field rather than a convention:
//! the [`Severity`] says how loud the message is, the timestamp says when it
//! stops being news, and the owner is [`Notifications`] — no component holds a
//! message of its own any more.
//!
//! # Why not `vcs::Tone`
//!
//! `vcs::status::Tone` already grades a *glyph* from `Muted` to `Danger`, and
//! `theme::tone` is the single place that grading becomes a colour. It is
//! deliberately not reused here, for two reasons:
//!
//! * two of its five levels are unsayable as a notification. `Muted` means
//!   "draw this quietly in a column of other glyphs" — a message nobody is
//!   meant to notice is not a message — and `Ok` has no caller, because in this
//!   app success is silent by policy (a hook that worked says nothing). Reusing
//!   `Tone` would offer callers two levels the status zone cannot honour.
//! * they answer to different owners. `Tone` belongs to the vcs domain
//!   describing a space; a severity belongs to the UI describing an event. If
//!   jj ever needs a sixth tone, notifications should not acquire a sixth
//!   level.
//!
//! What is *not* duplicated is the colour. A severity resolves to a
//! `crate::theme` style and nothing here names a hue, exactly as `theme::tone`
//! does for glyphs.

use std::time::{Duration, Instant};

use ratatui::style::Style;

use crate::theme;

/// How long a notification stays on screen.
///
/// Six seconds. The messages here are unexpected by nature — the user pressed a
/// key and something else happened — so the clock has to cover *noticing* the
/// line as well as reading it, and the longest of them ("Hook failed for X: …
/// the worktree was created and is intact") is around eighty characters. At an
/// unhurried reading pace that is roughly four seconds, plus a beat to look
/// down. Shorter would mean a message the user never got to read, which the
/// issue rightly calls as bad as one that never clears; much longer and the
/// mode indicator — which shares the zone — stays hidden long after the news
/// stopped being news.
pub const VISIBLE_FOR: Duration = Duration::from_secs(6);

/// How loud a notification is. Three levels, because the status zone is one
/// short line and a scale the user has to learn is worse than no scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Neutral news. Nothing failed and nothing is owed: the app did something
    /// the user could not see, or chose a path they should know about.
    Info,
    /// The action worked, but not the way the user probably assumed.
    Warning,
    /// The action did not happen, or something is broken.
    Error,
}

impl Severity {
    /// The one place a severity becomes a style — the palette still owns every
    /// colour, exactly as it does for status glyphs.
    pub fn style(self) -> Style {
        match self {
            Severity::Info => theme::INFO_TEXT,
            Severity::Warning => theme::WARNING_TEXT,
            Severity::Error => theme::DANGER_TEXT,
        }
    }
}

/// One thing to tell the user, and the moment it was said.
#[derive(Debug, Clone)]
pub struct Notification {
    pub text: String,
    pub severity: Severity,
    /// When it went up. Kept rather than a countdown so expiry is a comparison
    /// against the clock instead of state that a dropped frame could corrupt.
    shown_at: Instant,
}

impl Notification {
    fn new(severity: Severity, text: String) -> Self {
        Self {
            text,
            severity,
            shown_at: Instant::now(),
        }
    }

    /// Whether this message has outlived [`VISIBLE_FOR`] as of `now`.
    ///
    /// Takes `now` rather than reading the clock so the rule can be tested
    /// without sleeping.
    fn expired_at(&self, now: Instant) -> bool {
        now.duration_since(self.shown_at) >= VISIBLE_FOR
    }
}

/// The app's single notification slot.
///
/// One slot, newest wins — not a queue. Only one line is ever on screen, so a
/// queue would mean the *oldest* news is what the user sees, and a failure that
/// just happened would wait behind an informational message from six seconds
/// ago. Everything is logged anyway, so nothing is lost by dropping the older
/// line.
#[derive(Debug, Default)]
pub struct Notifications {
    current: Option<Notification>,
}

impl Notifications {
    /// Neutral news; see [`Severity::Info`].
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Severity::Info, text.into());
    }

    /// It worked, but not as the user likely expected; see [`Severity::Warning`].
    pub fn warn(&mut self, text: impl Into<String>) {
        self.push(Severity::Warning, text.into());
    }

    /// It did not happen; see [`Severity::Error`].
    pub fn error(&mut self, text: impl Into<String>) {
        self.push(Severity::Error, text.into());
    }

    /// Takes the line down early.
    ///
    /// For the case expiry cannot cover: an action that *succeeded* where the
    /// last one failed. The old failure is not merely stale, it is now wrong,
    /// and waiting out its clock would say the opposite of what just happened.
    pub fn clear(&mut self) {
        self.current = None;
    }

    /// The message to draw, or `None` when there is nothing to say.
    pub fn current(&self) -> Option<&Notification> {
        self.current.as_ref()
    }

    /// Drops the message once it has had its time.
    ///
    /// Driven by the app's existing 100 ms tick, like the list spinner: a timer
    /// of its own would be a second thing to start, stop and get wrong, and
    /// expiring only when the next frame happens to be drawn would leave a
    /// message up indefinitely on an idle screen.
    pub fn expire(&mut self) {
        self.expire_at(Instant::now());
    }

    fn expire_at(&mut self, now: Instant) {
        if self.current.as_ref().is_some_and(|n| n.expired_at(now)) {
            self.current = None;
        }
    }

    fn push(&mut self, severity: Severity, text: String) {
        self.current = Some(Notification::new(severity, text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_stays_up_until_its_time_is_out() {
        let mut notifications = Notifications::default();
        notifications.error("no such remote");
        let shown_at = notifications.current().expect("shown").shown_at;

        notifications.expire_at(shown_at + VISIBLE_FOR - Duration::from_millis(1));
        assert!(
            notifications.current().is_some(),
            "a message the user may still be reading was dropped"
        );

        notifications.expire_at(shown_at + VISIBLE_FOR);
        assert!(
            notifications.current().is_none(),
            "a message outlived its clock"
        );
    }

    /// The bug the timestamp exists for: a second message must get its own full
    /// reading time, not the remainder of the one it replaced.
    #[test]
    fn a_newer_message_restarts_the_clock() {
        let mut notifications = Notifications::default();
        notifications.info("PR is merged");
        let first = notifications.current().expect("shown").shown_at;

        notifications.error("delete failed");
        let second = notifications.current().expect("shown").shown_at;

        assert!(second >= first);
        notifications.expire_at(first + VISIBLE_FOR);
        assert_eq!(
            notifications.current().map(|n| n.severity),
            Some(Severity::Error),
            "the newer message expired on the older one's clock"
        );
    }

    /// Newest wins: one line, and the news the user needs is the last thing
    /// that happened.
    #[test]
    fn the_newest_message_is_the_one_on_screen() {
        let mut notifications = Notifications::default();
        notifications.info("PR is merged — existing worktree selected");
        notifications.error("Hook failed");

        let current = notifications.current().expect("shown");
        assert_eq!(current.severity, Severity::Error);
        assert_eq!(current.text, "Hook failed");
    }

    #[test]
    fn success_takes_a_stale_failure_down_at_once() {
        let mut notifications = Notifications::default();
        notifications.error("could not create the space");
        notifications.clear();
        assert!(notifications.current().is_none());
    }
}
