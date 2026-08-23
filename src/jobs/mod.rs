//! Slow work, moved off the render thread.
//!
//! Everything shanti does that can block — walking a repos dir, talking to a
//! remote, asking GitHub about a pull request, cloning — is described here as a
//! [`Job`], handed to a [`Worker`], and comes back as a [`JobResult`] on the same
//! channel the keyboard and the clock already use ([`crate::events::AppEvent`]).
//! The render thread therefore never waits: it draws, and later it is told.
//!
//! Four properties are load-bearing, and each one is expensive to add later:
//!
//! 1. **Every job has an id.** A result is only data until a caller decides to
//!    apply it, and by the time it lands the user may have moved on — closed the
//!    popup, changed the repos dir, selected something else. The caller keeps the
//!    ids it still cares about and drops results carrying any other id, so a late
//!    answer to an abandoned question can never overwrite the current one.
//! 2. **Jobs are cancellable.** [`Worker::cancel`] drops a job that has not
//!    started yet, and suppresses the result of one that has. A clone takes
//!    minutes; nothing in the UI may be built on the assumption that it will not.
//! 3. **Failure is a result, not a crash.** Job bodies return `Result`, and a body
//!    that panics anyway is caught and turned into an error result. One bad
//!    repository must not take the session down, and no worker thread may die
//!    silently and shrink the pool.
//! 4. **The pool is bounded.** A repos dir with two hundred repositories queues
//!    two hundred jobs and still runs on [`WORKER_THREADS`] threads.
//!
//! # This is not rayon's job
//!
//! rayon stays where it already is: inside [`crate::vcs::open_backends`], fanning
//! out the *CPU-and-disk* half of a scan across the whole machine and finishing.
//! These threads are the opposite kind — few, long-lived, and usually parked in a
//! `read` on a socket. Putting a multi-minute clone on rayon's pool would occupy
//! a core-sized pool with a job that never needs a core.

use std::{
    collections::{HashSet, VecDeque},
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::Sender,
        Arc, Condvar, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use color_eyre::eyre;
use tracing::debug;

use crate::{
    events::AppEvent,
    github::{self, PrFetcher, PrInfo, PrUrl},
    vcs::{self, BoxedVcs, Space},
};

/// How many jobs may be in flight at once.
///
/// Four, and deliberately *not* derived from the core count: these jobs wait on
/// a network or a subprocess, they do not compute, so the right number is set by
/// how many slow things one person can sensibly have going — a clone, a fetch, a
/// PR lookup, and one spare — not by how many cores the machine has. Four also
/// keeps the pool small enough that it can never starve rayon, which *does* want
/// every core for the scan fan-out.
pub const WORKER_THREADS: usize = 4;

/// How long [`Worker`]'s drop waits for its threads before giving up on them.
///
/// An idle thread parked on the condvar leaves in microseconds, so this bound is
/// only ever reached by a thread *inside* a job — a `git clone` half done. That
/// one cannot be interrupted, and waiting for it would freeze the terminal for
/// as long as the clone takes, which is exactly the freeze this module exists to
/// prevent. So shutdown stops caring about it: the queue is cleared, its result
/// is suppressed, and process exit ends it. The pool never *leaks* a thread that
/// could still do something — it only declines to be held hostage by one.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(200);

/// Identifies one submission, so its result can be recognised — or ignored.
///
/// Ids are never reused within a session, which is what makes "is this result
/// still wanted?" answerable by a set-membership test and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(u64);

/// What a job is, without its payload.
///
/// Carried on the result so a caller can route or report it without matching the
/// payload, and so a log names the job even when it failed before producing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    ScanRepositories,
    FetchRemotes,
    FetchPullRequest,
    CloneRepository,
    SpaceStatus,
    #[cfg(test)]
    Custom,
}

impl std::fmt::Display for JobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            JobKind::ScanRepositories => "repository scan",
            JobKind::FetchRemotes => "fetch",
            JobKind::FetchPullRequest => "pull request lookup",
            JobKind::CloneRepository => "clone",
            JobKind::SpaceStatus => "status",
            #[cfg(test)]
            JobKind::Custom => "custom",
        };
        f.write_str(name)
    }
}

/// A unit of slow work, complete with everything it needs to run.
///
/// A job owns its inputs rather than borrowing app state, so a worker can never
/// observe the UI mid-change and the UI never has to be locked. That is why the
/// repository jobs carry a *path* and open the backend themselves: re-opening
/// costs milliseconds next to the network round trip that follows, and it buys a
/// worker that cannot touch anything the render thread holds.
pub enum Job {
    /// Walk the repos dirs and open every repository found.
    ///
    /// The walk is serial and the opening is rayon's; both happen here rather
    /// than on the render thread, which is the whole point.
    ScanRepositories {
        roots: Vec<PathBuf>,
        excluded: Vec<PathBuf>,
    },
    /// Refresh one repository's view of its remotes.
    ///
    /// Goes through [`vcs::refresh`], the single statement of shanti's fetch
    /// policy: a failed fetch costs a stale view of the remotes and nothing
    /// else. The refreshed refs are on disk, so the result carries no data —
    /// the caller re-reads whatever it wants to show.
    FetchRemotes { path: PathBuf },
    /// Look a pull request up, through the injected fetcher so this reaches the
    /// same seam the synchronous flow does.
    FetchPullRequest { fetcher: PrFetcher, url: PrUrl },
    /// Clone a repository into a repos dir. The job most likely to run for
    /// minutes, and the reason [`Worker::cancel`] exists.
    CloneRepository {
        owner: String,
        repo: String,
        repos_dir: String,
    },
    /// Recompute one repository's spaces, including their status.
    SpaceStatus { path: PathBuf },
    /// Test-only body, so the pool can be exercised without a repository, a
    /// network or a clock.
    #[cfg(test)]
    Custom(Box<dyn FnOnce() -> eyre::Result<Completion> + Send>),
}

impl Job {
    pub fn kind(&self) -> JobKind {
        match self {
            Job::ScanRepositories { .. } => JobKind::ScanRepositories,
            Job::FetchRemotes { .. } => JobKind::FetchRemotes,
            Job::FetchPullRequest { .. } => JobKind::FetchPullRequest,
            Job::CloneRepository { .. } => JobKind::CloneRepository,
            Job::SpaceStatus { .. } => JobKind::SpaceStatus,
            #[cfg(test)]
            Job::Custom(_) => JobKind::Custom,
        }
    }

    /// Do the work. Blocking by definition — this only ever runs on a worker.
    fn run(self) -> eyre::Result<Completion> {
        match self {
            Job::ScanRepositories { roots, excluded } => {
                let found: Vec<_> = roots
                    .iter()
                    .flat_map(|root| vcs::discover(root, &excluded))
                    .collect();
                Ok(Completion::Repositories(vcs::open_backends(&found, false)))
            }
            Job::FetchRemotes { path } => {
                for backend in vcs::open_at(&path, false)? {
                    vcs::refresh(backend.as_ref());
                }
                Ok(Completion::Fetched { path })
            }
            Job::FetchPullRequest { fetcher, url } => {
                Ok(Completion::PullRequest(Box::new(fetcher(&url)?)))
            }
            Job::CloneRepository {
                owner,
                repo,
                repos_dir,
            } => {
                github::clone_repository(&owner, &repo, &repos_dir)?;
                Ok(Completion::Cloned {
                    path: PathBuf::from(repos_dir).join(repo),
                })
            }
            Job::SpaceStatus { path } => {
                let mut spaces = Vec::new();
                for backend in vcs::open_at(&path, false)? {
                    spaces.extend(backend.spaces()?);
                }
                Ok(Completion::Spaces(spaces))
            }
            #[cfg(test)]
            Job::Custom(body) => body(),
        }
    }
}

/// Payloads cannot derive `Debug` — a `BoxedVcs` has none, and a `PrInfo` has no
/// business being logged — so a job names itself instead.
impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Job({})", self.kind())
    }
}

/// What a job produced.
pub enum Completion {
    /// Every repository found, already opened through its backends.
    Repositories(Vec<BoxedVcs>),
    /// This repository's remotes were refreshed as far as they could be.
    Fetched { path: PathBuf },
    /// Boxed because a `PrInfo` is far wider than the other arms, and an enum is
    /// as big as its widest arm.
    PullRequest(Box<PrInfo>),
    /// Where the clone landed.
    Cloned { path: PathBuf },
    /// One repository's spaces, freshly read.
    Spaces(Vec<Space>),
}

impl std::fmt::Debug for Completion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Completion::Repositories(repos) => write!(f, "Repositories({})", repos.len()),
            Completion::Fetched { path } => write!(f, "Fetched({})", path.display()),
            Completion::PullRequest(_) => f.write_str("PullRequest"),
            Completion::Cloned { path } => write!(f, "Cloned({})", path.display()),
            Completion::Spaces(spaces) => write!(f, "Spaces({})", spaces.len()),
        }
    }
}

/// A finished job, on its way back to the main loop.
///
/// The error lives *inside* the result rather than beside it: a job that failed
/// still happened, still has an id, and still has to reach whoever was waiting —
/// which is how a failure ends up in the notification line instead of in a log
/// nobody reads.
#[derive(Debug)]
pub struct JobResult {
    pub id: JobId,
    pub kind: JobKind,
    pub outcome: eyre::Result<Completion>,
}

/// A bounded pool of threads plus the queue they drain.
///
/// Not `Clone` on purpose: the `Worker` *is* the ownership of those threads, and
/// dropping it is what stops them. Code that needs work done should be handed a
/// job to submit, not a second handle on the pool.
pub struct Worker {
    shared: Arc<Shared>,
    next_id: AtomicU64,
    threads: Vec<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    /// "There is work, or we are stopping." Both wake a parked thread, which is
    /// why no worker can sit through a shutdown.
    work: Condvar,
    /// "A thread has left." Only the drop below waits on this.
    departed: Condvar,
}

struct State {
    queue: VecDeque<(JobId, Job)>,
    /// Submitted and not finished. Cancelling an id that is not here is a no-op,
    /// which is what stops `cancelled` from growing for the life of the session.
    pending: HashSet<JobId>,
    /// Started, then cancelled: the result is thrown away when it arrives.
    cancelled: HashSet<JobId>,
    stopping: bool,
    live: usize,
}

impl Shared {
    /// A poisoned queue is still a perfectly good queue: the only panic that
    /// could poison it happens inside a job body, which is caught, and refusing
    /// to serve the rest of the session over it would be the larger failure.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Worker {
    /// Starts [`WORKER_THREADS`] threads reporting to `results`.
    pub fn new(results: Sender<AppEvent>) -> Self {
        Self::with_threads(results, WORKER_THREADS)
    }

    pub fn with_threads(results: Sender<AppEvent>, threads: usize) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                pending: HashSet::new(),
                cancelled: HashSet::new(),
                stopping: false,
                live: threads,
            }),
            work: Condvar::new(),
            departed: Condvar::new(),
        });

        let threads = (0..threads)
            .map(|_| spawn_worker(Arc::clone(&shared), results.clone()))
            .collect();

        Self {
            shared,
            next_id: AtomicU64::new(0),
            threads,
        }
    }

    /// Queues `job` and returns the id its result will carry.
    ///
    /// Never blocks and never refuses: the queue is unbounded because a queued
    /// job costs a few words, while the thing that must stay bounded — how many
    /// run at once — is bounded by the pool itself.
    pub fn submit(&self, job: Job) -> JobId {
        let id = JobId(self.next_id.fetch_add(1, Ordering::Relaxed));
        {
            let mut state = self.shared.lock();
            if state.stopping {
                return id;
            }
            state.pending.insert(id);
            state.queue.push_back((id, job));
        }
        self.shared.work.notify_one();
        id
    }

    /// Gives up on a job: it is dropped from the queue if it has not started,
    /// and its result is suppressed if it has.
    ///
    /// A running job is *not* interrupted — a `git clone` in progress has no
    /// safe stopping point — so this means "stop caring", not "stop now". That
    /// is enough for correctness: a result nobody applies changes nothing.
    pub fn cancel(&self, id: JobId) {
        let mut state = self.shared.lock();
        if !state.pending.remove(&id) {
            // Never submitted, or already finished: nothing to suppress.
            return;
        }
        if let Some(index) = state.queue.iter().position(|(queued, _)| *queued == id) {
            state.queue.remove(index);
            return;
        }
        state.cancelled.insert(id);
    }

    /// How many jobs are queued or running.
    pub fn pending(&self) -> usize {
        self.shared.lock().pending.len()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        {
            let mut state = self.shared.lock();
            state.stopping = true;
            // Whatever has not started never will; a result from a job already
            // running is dropped by the `stopping` check in the loop below.
            state.queue.clear();
            state.pending.clear();
        }
        self.shared.work.notify_all();

        let all_gone = {
            let state = self.shared.lock();
            let (state, _) = self
                .shared
                .departed
                .wait_timeout_while(state, SHUTDOWN_GRACE, |state| state.live > 0)
                .unwrap_or_else(|e| e.into_inner());
            state.live == 0
        };

        if !all_gone {
            debug!("a worker is still inside a job; leaving it to process exit");
            return;
        }
        for thread in self.threads.drain(..) {
            // Joining can only fail if the thread panicked, and a job panic is
            // caught before it gets that far — so this is a bug report, not a
            // reason to panic in turn while the session is ending.
            if thread.join().is_err() {
                debug!("a worker thread panicked outside a job body");
            }
        }
    }
}

fn spawn_worker(shared: Arc<Shared>, results: Sender<AppEvent>) -> JoinHandle<()> {
    thread::spawn(move || {
        // Announces the thread's departure however the loop below ends, so a
        // shutdown can never wait on a thread that is already gone.
        let _departure = Departure(&shared);

        while let Some((id, job)) = next_job(&shared) {
            let kind = job.kind();

            // A job body is arbitrary code — git2, a subprocess, someone else's
            // parser. If it panics the pool must lose the job, not the thread:
            // an uncaught panic here would silently shrink the pool, and four
            // panics later every submission would wait forever.
            let outcome = match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                Ok(outcome) => outcome,
                Err(_) => Err(eyre::eyre!("the {kind} job panicked")),
            };

            let wanted = {
                let mut state = shared.lock();
                state.pending.remove(&id);
                !state.cancelled.remove(&id) && !state.stopping
            };
            if !wanted {
                debug!(?id, %kind, "dropping the result of a cancelled job");
                continue;
            }

            let result = JobResult { id, kind, outcome };
            // A closed channel means the main loop is gone: stop, do not spin.
            if results.send(AppEvent::Job(result)).is_err() {
                return;
            }
        }
    })
}

/// Blocks until there is a job, or until the pool is stopping (`None`).
fn next_job(shared: &Arc<Shared>) -> Option<(JobId, Job)> {
    let mut state = shared.lock();
    loop {
        if state.stopping {
            return None;
        }
        if let Some(task) = state.queue.pop_front() {
            return Some(task);
        }
        state = shared.work.wait(state).unwrap_or_else(|e| e.into_inner());
    }
}

struct Departure<'a>(&'a Arc<Shared>);

impl Drop for Departure<'_> {
    fn drop(&mut self) {
        self.0.lock().live -= 1;
        self.0.departed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    /// A gate a test can hold a job on, and open when it likes.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        changed: Condvar,
    }

    impl Gate {
        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.changed.wait(open).unwrap();
            }
        }

        fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }

    /// A job that reports when it starts, then blocks until `gate` opens.
    fn blocking(gate: Arc<Gate>, started: mpsc::Sender<()>) -> Job {
        Job::Custom(Box::new(move || {
            let _ = started.send(());
            gate.wait();
            Ok(Completion::Spaces(Vec::new()))
        }))
    }

    fn ok_job() -> Job {
        Job::Custom(Box::new(|| Ok(Completion::Spaces(Vec::new()))))
    }

    fn recv(rx: &mpsc::Receiver<AppEvent>) -> JobResult {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(AppEvent::Job(result)) => result,
            other => panic!("expected a job result, got {other:?}"),
        }
    }

    #[test]
    fn a_result_carries_the_id_of_its_submission() {
        let (tx, rx) = mpsc::channel();
        let worker = Worker::with_threads(tx, 1);
        let first = worker.submit(ok_job());
        let second = worker.submit(ok_job());

        assert_ne!(first, second, "ids are never reused");
        let ids = [recv(&rx).id, recv(&rx).id];
        assert!(ids.contains(&first) && ids.contains(&second));
    }

    /// The bound that matters: far more jobs than threads, and never more than
    /// `threads` of them running at once.
    #[test]
    fn no_more_jobs_run_at_once_than_the_pool_has_threads() {
        let (tx, rx) = mpsc::channel();
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let worker = Worker::with_threads(tx, 2);

        for _ in 0..50 {
            let running = Arc::clone(&running);
            let peak = Arc::clone(&peak);
            worker.submit(Job::Custom(Box::new(move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(1));
                running.fetch_sub(1, Ordering::SeqCst);
                Ok(Completion::Spaces(Vec::new()))
            })));
        }

        for _ in 0..50 {
            recv(&rx);
        }
        assert!(peak.load(Ordering::SeqCst) <= 2, "the pool grew past 2");
    }

    /// A queued job that is cancelled must never run at all.
    #[test]
    fn cancelling_a_queued_job_stops_it_from_running() {
        let (tx, rx) = mpsc::channel();
        let (started, has_started) = mpsc::channel();
        let gate = Arc::new(Gate::default());
        let worker = Worker::with_threads(tx, 1);

        // Occupies the only thread, so the next submission is certainly queued.
        worker.submit(blocking(Arc::clone(&gate), started));
        has_started.recv().expect("the first job started");

        let ran = Arc::new(AtomicUsize::new(0));
        let queued = {
            let ran = Arc::clone(&ran);
            worker.submit(Job::Custom(Box::new(move || {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok(Completion::Spaces(Vec::new()))
            })))
        };

        worker.cancel(queued);
        gate.open();

        recv(&rx); // the blocking job's own result
        assert_eq!(ran.load(Ordering::SeqCst), 0, "a cancelled job ran");
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a cancelled job reported a result"
        );
    }

    /// A job already running cannot be interrupted, so the guarantee is the
    /// weaker — and sufficient — one: its result never arrives.
    #[test]
    fn cancelling_a_running_job_suppresses_its_result() {
        let (tx, rx) = mpsc::channel();
        let (started, has_started) = mpsc::channel();
        let gate = Arc::new(Gate::default());
        let worker = Worker::with_threads(tx, 1);

        let id = worker.submit(blocking(Arc::clone(&gate), started));
        has_started.recv().expect("the job started");
        worker.cancel(id);
        gate.open();

        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "the result of a cancelled job was delivered"
        );
    }

    /// The requirement in one test: a failing job is a message, not a crash.
    #[test]
    fn a_failing_job_comes_back_as_an_error_result() {
        let (tx, rx) = mpsc::channel();
        let worker = Worker::with_threads(tx, 1);
        worker.submit(Job::Custom(Box::new(|| Err(eyre::eyre!("no such remote")))));

        let error = recv(&rx).outcome.expect_err("the job failed");
        assert!(format!("{error:#}").contains("no such remote"));
    }

    /// A panic in a job body is the same thing as a failure, and costs neither
    /// the thread nor the jobs queued behind it.
    #[test]
    fn a_panicking_job_becomes_an_error_and_leaves_the_pool_usable() {
        let (tx, rx) = mpsc::channel();
        let worker = Worker::with_threads(tx, 1);
        worker.submit(Job::Custom(Box::new(|| panic!("boom"))));
        worker.submit(ok_job());

        let panicked = recv(&rx);
        assert!(panicked.outcome.is_err(), "a panic must arrive as an error");
        assert!(
            recv(&rx).outcome.is_ok(),
            "the pool died with the panicking job"
        );
    }

    /// Dropping the pool must not hang: idle threads have to notice the stop
    /// request instead of staying parked on the condvar.
    #[test]
    fn dropping_the_worker_stops_its_threads() {
        let (tx, _rx) = mpsc::channel();
        drop(Worker::with_threads(tx, 4));
    }

    /// And dropping it while a job is stuck must not hang either — the freeze
    /// this module exists to prevent must not reappear at quit.
    #[test]
    fn dropping_the_worker_does_not_wait_for_a_stuck_job() {
        let (tx, _rx) = mpsc::channel();
        let (started, has_started) = mpsc::channel();
        let gate = Arc::new(Gate::default());
        let worker = Worker::with_threads(tx, 1);
        worker.submit(blocking(Arc::clone(&gate), started));
        has_started.recv().expect("the job started");

        let began = std::time::Instant::now();
        drop(worker);
        assert!(began.elapsed() < SHUTDOWN_GRACE * 4, "shutdown waited");
        gate.open();
    }
}
