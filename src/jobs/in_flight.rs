//! The jobs whose answers still matter, and what each one is for.
//!
//! One map, keyed by [`JobId`], with the category carried alongside the id. The
//! alternative — a set per category, plus one holding every id — needs the rule
//! "each category is a subset of the whole" re-applied by hand at every submit,
//! every cancellation and every result. Missing one of those is **silent**: the
//! job finishes, the set it is still in never empties, and the activity
//! indicator spins forever. Here, forgetting a job is one removal that cannot be
//! half-done.
//!
//! # What belongs here, and what does not
//!
//! This answers *what is running, and what for*. It deliberately does **not**
//! know:
//!
//! * how to cancel — that is the [`Worker`](super::Worker)'s. This only says
//!   whether an id was still wanted, which is what makes cancelling safe to
//!   skip;
//! * what a category looks like on screen. Mapping a [`Kind`] to a spinner label
//!   is UI policy and belongs where the spinner is drawn.
//!
//! # The staleness rule
//!
//! A [`JobResult`](super::JobResult) is applied **only** if [`finish`] returns
//! its category. Anything else — a fetch for a repos dir the user has since
//! changed, a lookup for a popup they closed — is dropped without touching
//! state, so a slow answer can never overwrite a newer one. Returning the
//! category rather than a bool is what keeps that rule enforceable: a caller
//! cannot act on the result without first receiving proof it is still wanted.
//!
//! [`finish`]: InFlight::finish

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::JobId;

/// What a job in flight is for.
///
/// Carried alongside the id so that finishing a job needs one removal rather
/// than one per category. Only [`Tracked::Fetch`] carries a payload, because it
/// is the only category that has to answer a question about a *specific*
/// repository — "am I already fetching this one?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tracked {
    /// A repos dir being walked.
    Scan,
    /// One repository re-reading its spaces from disk.
    Refresh,
    /// One repository's remotes being refreshed. The path is the half a plain id
    /// could not carry: it is needed to refuse a second fetch of a repository
    /// already being fetched, and to re-read that one repository when the fetch
    /// lands.
    Fetch(PathBuf),
    /// A created space's post-create hooks.
    Hooks,
    /// Still wanted, but counted by nobody. The PR flow keeps its own step and
    /// routes its own answer, so it needs the staleness rule and none of the
    /// bookkeeping.
    Routed,
}

impl Tracked {
    pub fn kind(&self) -> Kind {
        match self {
            Tracked::Scan => Kind::Scan,
            Tracked::Refresh => Kind::Refresh,
            Tracked::Fetch(_) => Kind::Fetch,
            Tracked::Hooks => Kind::Hooks,
            Tracked::Routed => Kind::Routed,
        }
    }
}

/// A [`Tracked`] without its payload, for counting and for taking a whole round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Scan,
    Refresh,
    Fetch,
    Hooks,
    Routed,
}

/// Every job that has been submitted and whose answer is still wanted.
#[derive(Debug, Default)]
pub struct InFlight {
    jobs: HashMap<JobId, Tracked>,
}

impl InFlight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start tracking a submitted job.
    pub fn track(&mut self, id: JobId, what: Tracked) {
        self.jobs.insert(id, what);
    }

    /// Take a finished job's category, or `None` if nobody is waiting for it.
    ///
    /// `None` is the whole staleness rule: the caller must drop the result
    /// rather than apply it. Returning the category rather than a bool means the
    /// caller does not then have to ask which set it came from.
    pub fn finish(&mut self, id: JobId) -> Option<Tracked> {
        self.jobs.remove(&id)
    }

    /// Stop waiting for a job. `true` if it was still wanted, which is what
    /// tells the caller there is something worth cancelling.
    pub fn forget(&mut self, id: JobId) -> bool {
        self.jobs.remove(&id).is_some()
    }

    /// Forget everything. Used when the pool goes away and nothing can answer.
    pub fn clear(&mut self) {
        self.jobs.clear();
    }

    /// How many jobs of `kind` are running.
    pub fn count(&self, kind: Kind) -> usize {
        self.jobs.values().filter(|t| t.kind() == kind).count()
    }

    /// The ids of every job of `kind`, so a caller can abandon a whole round.
    ///
    /// Returns owned ids rather than an iterator on purpose: every caller goes
    /// on to call something that borrows `self` mutably.
    pub fn ids(&self, kind: Kind) -> Vec<JobId> {
        self.jobs
            .iter()
            .filter(|(_, t)| t.kind() == kind)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Whether `path`'s remotes are already being fetched.
    pub fn is_fetching(&self, path: &Path) -> bool {
        self.jobs
            .values()
            .any(|t| matches!(t, Tracked::Fetch(p) if p == path))
    }

    /// Whether a job of `kind` is running at all.
    pub fn any(&self, kind: Kind) -> bool {
        self.jobs.values().any(|t| t.kind() == kind)
    }

    /// Whether `id` is still wanted. For assertions above all.
    pub fn contains(&self, id: JobId) -> bool {
        self.jobs.contains_key(&id)
    }

    /// How many jobs are running, of every kind.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids come from a `Worker` in production. This module is a child of `jobs`,
    /// so it can mint them directly and exercise the registry without a pool.
    fn ids(n: u64) -> Vec<JobId> {
        (0..n).map(JobId).collect()
    }

    /// Finishing a job forgets it completely, whatever it was for — there is no
    /// second place it could still be counted.
    #[test]
    fn finishing_a_job_forgets_it_once() {
        let id = ids(1)[0];
        let mut flight = InFlight::new();
        flight.track(id, Tracked::Scan);

        assert_eq!(flight.finish(id), Some(Tracked::Scan));
        assert_eq!(flight.count(Kind::Scan), 0);
        assert!(flight.is_empty(), "no set was left holding it");
    }

    /// The staleness rule: an answer nobody is waiting for is recognisable as
    /// such, so the caller can drop it rather than apply it.
    #[test]
    fn an_abandoned_job_reports_no_category() {
        let id = ids(1)[0];
        let mut flight = InFlight::new();
        flight.track(id, Tracked::Refresh);

        assert!(flight.forget(id), "it was still wanted");
        assert!(!flight.forget(id), "and only once");
        assert_eq!(flight.finish(id), None, "its answer is no longer wanted");
    }

    #[test]
    fn categories_are_counted_apart() {
        let id = ids(4);
        let mut flight = InFlight::new();
        flight.track(id[0], Tracked::Scan);
        flight.track(id[1], Tracked::Refresh);
        flight.track(id[2], Tracked::Refresh);
        flight.track(id[3], Tracked::Hooks);

        assert_eq!(flight.count(Kind::Scan), 1);
        assert_eq!(flight.count(Kind::Refresh), 2);
        assert_eq!(flight.count(Kind::Hooks), 1);
        assert_eq!(flight.count(Kind::Fetch), 0);
        assert!(!flight.any(Kind::Fetch));
        assert_eq!(flight.len(), 4);
    }

    /// `ids` is what lets a rescan abandon exactly the previous round without
    /// touching a fetch or a clone that is also in flight.
    #[test]
    fn a_round_can_be_taken_without_disturbing_the_others() {
        let id = ids(3);
        let mut flight = InFlight::new();
        flight.track(id[0], Tracked::Refresh);
        flight.track(id[1], Tracked::Refresh);
        flight.track(id[2], Tracked::Fetch(PathBuf::from("/repos/a")));

        let mut round = flight.ids(Kind::Refresh);
        round.sort();
        assert_eq!(round, vec![id[0], id[1]]);

        for id in round {
            flight.forget(id);
        }
        assert_eq!(flight.count(Kind::Refresh), 0);
        assert_eq!(flight.count(Kind::Fetch), 1, "the fetch was left alone");
    }

    /// A fetch is refused for a repository already being fetched, which needs
    /// the path the id alone could not carry.
    #[test]
    fn a_repository_already_being_fetched_is_recognised() {
        let id = ids(1)[0];
        let mut flight = InFlight::new();
        flight.track(id, Tracked::Fetch(PathBuf::from("/repos/a")));

        assert!(flight.is_fetching(Path::new("/repos/a")));
        assert!(!flight.is_fetching(Path::new("/repos/b")));

        flight.finish(id);
        assert!(!flight.is_fetching(Path::new("/repos/a")), "it finished");
    }
}
