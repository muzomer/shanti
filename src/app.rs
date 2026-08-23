use std::{collections::HashSet, path::PathBuf};

use color_eyre::eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Constraint, Layout},
    Frame,
};
use tracing::debug;

use crate::{
    cli,
    components::{
        spaces_of, worktrees_bindings, Action, AppContext, ConfirmComponent, EventState,
        HelpComponent, Modal, ModalFlow, PrWorktreeComponent, RepositoriesComponent,
        RepositoriesModal, SpaceEntry, WorktreesComponent,
    },
    github,
    jobs::{Completion, Job, JobId, JobResult, Worker},
    keymap::{self, InputMode},
    vcs::{BoxedVcs, Consequence, DeletionRisk, RepoId, Space},
};

/// The worktree list plus a stack of modals on top of it.
///
/// `App` owns no per-modal state: a modal carries everything it needs and returns
/// a [`ModalFlow`] saying what should happen to the stack. Drawing walks the stack
/// bottom-to-top, so help layers over whatever it was opened above with no special
/// case, and the effective input mode is simply the top modal's — popping restores
/// the layer below's mode by construction.
pub struct App {
    worktrees_component: WorktreesComponent,
    repositories_component: RepositoriesComponent,
    modals: Vec<Box<dyn Modal>>,
    args: cli::Args,
    /// How the PR flow looks a pull request up. Held here, not reached for
    /// inside the modal, so the whole flow can be pointed at another source.
    pr_fetcher: github::PrFetcher,
    /// Input mode of the worktree list, the one layer that is not a modal.
    mode: InputMode,
    pub selected_path: Option<String>,
    /// The pool slow work is handed to, once a main loop exists to receive its
    /// results. `None` in a test, where every job is simply never submitted.
    jobs: Option<Worker>,
    /// The jobs whose answers still matter.
    ///
    /// This is the whole staleness rule: a [`JobResult`] is applied **only** if
    /// its id is in here. Anything else — a fetch for a repos dir the user has
    /// since changed, a PR lookup for a popup they closed — is dropped without
    /// touching state, so a slow answer can never overwrite a newer one.
    outstanding: HashSet<JobId>,
    /// The repos dirs still to be walked.
    ///
    /// Held rather than walked in the constructor: discovery is a job now, so
    /// these are the *inputs* to that job and nothing reads the disk until there
    /// is a worker to read it on.
    scan_roots: Vec<PathBuf>,
    /// Kept out of the walk — the worktrees dir, so the spaces living inside it
    /// are not rediscovered as repositories in their own right.
    excluded: Vec<PathBuf>,
    /// The scan jobs still running; a subset of `outstanding`.
    ///
    /// Tracked apart from `outstanding` because the spinner is about the *scan*
    /// specifically: a fetch still running behind a finished scan must not keep the
    /// list saying "scanning".
    scans: HashSet<JobId>,
    /// How many repositories the current scan has reported — the spinner's count.
    scan_found: usize,
    /// A job changed what is on disk, so the list is due a re-read.
    spaces_stale: bool,
}

impl App {
    /// Resolves the configuration from the command line and builds an `App`.
    ///
    /// Fallible because the configuration comes from outside: a directory the
    /// user named may not exist. The error is returned so `main` can report it
    /// on stderr, which is the only place in the program allowed to end it.
    pub fn new() -> Result<App> {
        let args = cli::Args::try_new()?;
        Ok(Self::with_args(args, github::live_fetcher()))
    }

    /// Builds an `App` from configuration the caller already resolved.
    ///
    /// This is the seam that keeps construction free of process-global state:
    /// nothing here reads argv, the environment, or the configuration file, so a
    /// test can point an `App` at its own temp directories — and at its own PR
    /// lookup — without disturbing any other test running beside it.
    pub fn with_args(args: cli::Args, pr_fetcher: github::PrFetcher) -> App {
        // Nothing here touches the disk. Walking the repos dirs, opening every
        // repository and reading every space all used to happen right here,
        // before the first frame — which is why a large repos dir opened onto a
        // blank terminal. All of it is a job now: this builds an empty list, and
        // the loop draws it immediately.
        let excluded = vec![PathBuf::from(&args.worktrees_dir)];
        let scan_roots = args.repos_dirs.iter().map(PathBuf::from).collect();

        Self {
            worktrees_component: WorktreesComponent::new(Vec::new()),
            repositories_component: RepositoriesComponent::new(Vec::new()),
            modals: Vec::new(),
            args,
            pr_fetcher,
            mode: InputMode::Normal,
            selected_path: None,
            jobs: None,
            outstanding: HashSet::new(),
            scan_roots,
            excluded,
            scans: HashSet::new(),
            scan_found: 0,
            spaces_stale: false,
        }
    }

    /// Gives the app somewhere to put slow work, and starts whatever was waiting
    /// for one.
    ///
    /// Called by [`crate::run_app`] once the event channel exists. An `App`
    /// without a worker is not broken — it is one nobody is drawing, and it
    /// simply never submits anything.
    pub fn attach_worker(&mut self, worker: Worker) {
        self.jobs = Some(worker);
        self.start_scan();
    }

    /// Drops the pool, cancelling everything queued.
    pub fn detach_worker(&mut self) {
        self.outstanding.clear();
        self.scans.clear();
        self.update_scan_indicator();
        self.jobs = None;
    }

    /// Whether repositories are still being discovered.
    ///
    /// Public because it is the one thing a caller outside the loop — a test
    /// above all — cannot otherwise tell: while this is true an empty list means
    /// "not found yet", and once it is false it means "there are none".
    pub fn is_scanning(&self) -> bool {
        !self.scans.is_empty()
    }

    /// Starts discovery: one [`Job::ScanRepositories`] per configured repos dir.
    ///
    /// **One job per root, not one job for the whole list.** A job produces
    /// exactly one result, so the roots are what decide how finely the list can
    /// stream: with two repos dirs the faster one fills the list while the
    /// slower is still being walked, instead of both landing together at the
    /// end. Going finer — a job per repository — would be worse on both counts:
    /// the walk would have to happen on the render thread to know what to
    /// submit, and `open_backends`' rayon fan-out (which wants the whole
    /// machine) would be chopped up into a four-thread pool sized for jobs that
    /// wait on a network.
    ///
    /// Idempotent: any scan already running is abandoned and the list emptied
    /// first, so a second call can neither leave behind repositories that are no
    /// longer there nor show anything twice.
    fn start_scan(&mut self) {
        for id in std::mem::take(&mut self.scans) {
            self.abandon(id);
        }
        self.repositories_component.replace_backends(Vec::new());
        self.worktrees_component.set_spaces(Vec::new());
        self.scan_found = 0;

        for root in std::mem::take(&mut self.scan_roots) {
            debug!("listing repositories in: {}", root.display());
            let job = Job::ScanRepositories {
                roots: vec![root.clone()],
                excluded: self.excluded.clone(),
            };
            match self.submit(job) {
                Some(id) => {
                    self.scans.insert(id);
                }
                // No worker, so the root was not walked: keep it, and attaching
                // one later still scans it.
                None => self.scan_roots.push(root),
            }
        }
        self.update_scan_indicator();
    }

    /// Queues `job` and remembers that its answer is wanted.
    ///
    /// `None` means there is no worker, and therefore that the job did not run —
    /// callers must treat that as "not now", never as "already done".
    fn submit(&mut self, job: Job) -> Option<JobId> {
        let id = self.jobs.as_ref()?.submit(job);
        self.outstanding.insert(id);
        Some(id)
    }

    /// Stops caring about a job: its result, if it ever arrives, is dropped.
    ///
    /// Cancelling in the pool as well as forgetting the id is not redundant —
    /// forgetting protects the state, cancelling saves the work.
    fn abandon(&mut self, id: JobId) {
        // A scan nobody is waiting for is also one the spinner must stop
        // counting, or an abandoned root would leave it turning forever.
        self.scans.remove(&id);
        if self.outstanding.remove(&id) {
            if let Some(worker) = &self.jobs {
                worker.cancel(id);
            }
        }
    }

    /// Takes in a finished job, or ignores it if the app has moved on.
    pub fn handle_job(&mut self, result: JobResult) {
        let kind = result.kind;
        if !self.outstanding.remove(&result.id) {
            debug!(id = ?result.id, %kind, "ignoring the result of an abandoned job");
            return;
        }
        // A scan is over whether it found repositories or failed trying, so this
        // is taken here rather than in the arms below: a spinner left running by
        // an error would be a spinner that never stops.
        self.scans.remove(&result.id);

        match result.outcome {
            // Where a background failure becomes visible. It is deliberately the
            // same line a failed delete uses: a job that could not run is news
            // for the user, not for the log file.
            Err(error) => {
                self.worktrees_component.last_error = Some(format!("{kind} failed: {error:#}"));
            }
            // The refreshed refs are on disk; the list is what has to catch up.
            Ok(Completion::Fetched { path }) => {
                debug!(repo = %path.display(), "fetched");
                self.spaces_stale = true;
            }
            // A root finished walking. Its repositories go straight in, so the
            // list grows under the user while the other roots are still out.
            Ok(Completion::Repositories(found)) => self.repositories_found(found),
            Ok(completion) => {
                debug!(?completion, %kind, "no consumer for this result yet");
            }
        }
        self.update_scan_indicator();
    }

    /// Takes in one scan result: the repositories, their spaces, and — under
    /// `--run-fetch` — a fetch queued behind each of them.
    fn repositories_found(&mut self, found: Vec<BoxedVcs>) {
        // The rows are built from the batch rather than by re-listing everything
        // already on screen: a hundred repositories arriving in ten batches
        // would otherwise cost ten full re-reads of every space's status.
        let (entries, failed): (Vec<SpaceEntry>, Vec<String>) = spaces_of(&found);

        // One fetch per *repository*, not per backend: a colocated repository is
        // open twice and has one set of remotes.
        let mut seen: HashSet<RepoId> = HashSet::new();
        let fetch: Vec<PathBuf> = found
            .iter()
            .filter(|backend| seen.insert(backend.repo().id.clone()))
            .map(|backend| backend.repo().path.clone())
            .collect();
        self.scan_found += seen.len();

        self.repositories_component.add_repositories(found);
        self.worktrees_component.extend(entries);
        let previous_error = self.worktrees_component.last_error.take();
        self.worktrees_component.last_error = listing_failure_notice(&failed).or(previous_error);

        if self.args.run_fetch {
            for path in fetch {
                self.submit(Job::FetchRemotes { path });
            }
        }
    }

    /// Keeps the spinner in step with the scan: shown with its count while any
    /// root is still being walked, gone the moment the last one lands.
    fn update_scan_indicator(&mut self) {
        let found = self.is_scanning().then_some(self.scan_found);
        self.worktrees_component.set_scan(found);
    }

    /// Applies whatever background work has landed since the last tick.
    ///
    /// Re-reading the spaces is done here, once, rather than per result: two
    /// hundred repositories finishing their fetches would otherwise rebuild the
    /// same list two hundred times, each rebuild costing every repository a
    /// status read.
    pub fn on_tick(&mut self) {
        // The spinner's only clock; see `WorktreesComponent::tick`.
        self.worktrees_component.tick();
        if self.spaces_stale && self.outstanding.is_empty() {
            self.spaces_stale = false;
            self.reload_spaces();
        }
    }

    /// Re-reads every repository's spaces and hands the new rows to the list.
    ///
    /// The rebuild is something the *app* decided to do, in the middle of a
    /// session the user is driving, so it costs them nothing: the filter they
    /// typed, the pane they had focused and the row they were on all survive it
    /// — see [`WorktreesComponent::set_spaces`].
    fn reload_spaces(&mut self) {
        let previous_error = self.worktrees_component.last_error.take();
        let (spaces, failed) = self.repositories_component.collect_spaces();
        self.worktrees_component.set_spaces(spaces);
        // A listing failure is the newer news; anything else already on screen
        // (a fetch that failed, say) is still true and stays.
        self.worktrees_component.last_error = listing_failure_notice(&failed).or(previous_error);
    }

    /// Points the PR flow at a different lookup than the live GitHub one.
    pub fn set_pr_fetcher(&mut self, fetch: github::PrFetcher) {
        self.pr_fetcher = fetch;
    }

    /// The mode the next key is resolved in: the top modal's, or the list's.
    fn effective_mode(&self) -> InputMode {
        self.modals.last().map_or(self.mode, |m| m.mode())
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let [full_area] = Layout::default()
            .constraints([Constraint::Percentage(100)])
            .areas(frame.area());

        let mode = self.effective_mode();
        let Self {
            worktrees_component,
            repositories_component,
            modals,
            args,
            ..
        } = self;

        worktrees_component.draw(frame, full_area, mode, modals.is_empty());

        let mut ctx = AppContext {
            worktrees: worktrees_component,
            repositories: repositories_component,
            args,
        };
        // Bottom-to-top: each modal clears its own area, so the one on top wins.
        for modal in modals.iter_mut() {
            let area = modal.area(full_area);
            modal.draw(frame, area, &mut ctx);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EventState {
        let action = match keymap::resolve(self.effective_mode(), key) {
            Some(action) => action,
            None => return EventState::NotConsumed,
        };

        // Quit and help are stack-wide, so no layer has to remember to handle
        // them — the gap that used to make '?' dead inside some popups.
        match action {
            Action::Quit => return EventState::Exit,
            Action::ShowHelp => {
                self.show_help();
                return EventState::Consumed;
            }
            _ => {}
        }

        if self.modals.is_empty() {
            return self.handle_worktrees_action(action);
        }
        self.dispatch_to_top_modal(action)
    }

    /// Applies a bracketed paste to whichever text field currently has focus.
    ///
    /// The terminal hands the whole paste over as one event, but every component
    /// below speaks in single-character actions, so it is fanned out here instead
    /// of teaching each of them a second insertion path. Control characters are
    /// dropped: a URL copied with its trailing newline must not also press Enter.
    pub fn handle_paste(&mut self, text: &str) {
        if self.effective_mode() != InputMode::Insert {
            return;
        }
        for c in text.chars().filter(|c| !c.is_control()) {
            let action = Action::InsertChar(c);
            if self.modals.is_empty() {
                self.handle_worktrees_action(action);
            } else {
                self.dispatch_to_top_modal(action);
            }
        }
    }

    /// Help is a plain modal pushed over whatever is on top. The help popup
    /// itself consumes `ShowHelp` by closing, which is what makes '?' a toggle;
    /// every other modal ignores it and gets its own bindings shown instead.
    fn show_help(&mut self) {
        if !self.modals.is_empty()
            && self.dispatch_to_top_modal(Action::ShowHelp) != EventState::NotConsumed
        {
            return;
        }
        let entries = self
            .modals
            .last()
            .map_or_else(worktrees_bindings, |m| m.help());
        self.modals.push(Box::new(HelpComponent::new(entries)));
    }

    fn dispatch_to_top_modal(&mut self, action: Action) -> EventState {
        let Self {
            worktrees_component,
            repositories_component,
            modals,
            args,
            ..
        } = self;

        let flow = {
            let mut ctx = AppContext {
                worktrees: worktrees_component,
                repositories: repositories_component,
                args,
            };
            match modals.last_mut() {
                Some(modal) => modal.handle(action, &mut ctx),
                None => return EventState::NotConsumed,
            }
        };

        match flow {
            ModalFlow::Consumed => EventState::Consumed,
            ModalFlow::Ignored => EventState::NotConsumed,
            ModalFlow::Close => {
                modals.pop();
                EventState::Consumed
            }
            ModalFlow::Replace(next) => {
                modals.pop();
                modals.push(next);
                EventState::Consumed
            }
        }
    }

    fn handle_worktrees_action(&mut self, action: Action) -> EventState {
        match action {
            Action::OpenRepositories => {
                self.modals.push(Box::new(RepositoriesModal::new()));
                EventState::Consumed
            }
            Action::OpenPrWorktree => {
                self.modals.push(Box::new(PrWorktreeComponent::new(
                    false,
                    self.pr_fetcher.clone(),
                )));
                EventState::Consumed
            }
            Action::OpenPrWorktreeAutoClone => {
                self.modals.push(Box::new(PrWorktreeComponent::new(
                    true,
                    self.pr_fetcher.clone(),
                )));
                EventState::Consumed
            }
            // 'D' skips the *question*, never the guard: a space holding work
            // that exists nowhere else still raises the dialog that says so.
            Action::ForceDelete => {
                match self.selected_space_risk() {
                    Some((_, risk)) if risk.is_safe() => self.delete_selected_worktree(),
                    Some((space, risk)) => self.modals.push(Box::new(confirm_delete(&space, risk))),
                    None => {}
                }
                EventState::Consumed
            }
            Action::DeleteWithConfirmation => {
                if let Some((space, risk)) = self.selected_space_risk() {
                    self.modals.push(Box::new(confirm_delete(&space, risk)));
                }
                EventState::Consumed
            }
            Action::EnterInsertMode => {
                self.mode = InputMode::Insert;
                self.worktrees_component.focus_filter();
                EventState::Consumed
            }
            Action::ExitInsertMode => {
                self.mode = InputMode::Normal;
                self.worktrees_component.focus_list();
                EventState::Consumed
            }
            Action::FocusNext => {
                self.worktrees_component.toggle_focus();
                self.mode = if self.worktrees_component.is_filter_focused() {
                    InputMode::Insert
                } else {
                    InputMode::Normal
                };
                EventState::Consumed
            }
            _ => {
                let result = self.worktrees_component.handle_action(action);
                if result == EventState::Exit {
                    self.selected_path = self.worktrees_component.selected_worktree_path();
                }
                result
            }
        }
    }

    /// The selected space and what deleting it would cost, or `None` when the
    /// list is empty.
    fn selected_space_risk(&mut self) -> Option<(Space, DeletionRisk)> {
        let space = self.worktrees_component.selected_space()?;
        // The status snapshot says whether the space is dirty, never by how
        // much, so the number is asked of the owning backend here — once, as the
        // dialog opens. A repository we cannot find a backend for still gets a
        // verdict; it just gets it without a count.
        let files = self
            .repositories_component
            .backend_for(&space)
            .and_then(|vcs| vcs.uncommitted_files(&space));
        let risk = DeletionRisk::of(&space).counting_files(files);
        Some((space, risk))
    }

    fn delete_selected_worktree(&mut self) {
        let Self {
            worktrees_component,
            repositories_component,
            ..
        } = self;
        match worktrees_component.delete_selected_space(repositories_component) {
            Ok(()) => worktrees_component.last_error = None,
            Err(e) => worktrees_component.last_error = Some(format!("{:#}", e)),
        }
    }
}

/// Tell the user which repositories could not be asked for their spaces.
///
/// A backend method that fails must never be silent: the list would simply be
/// short, and a missing space reads as shanti having lost it.
fn listing_failure_notice(failed: &[String]) -> Option<String> {
    if failed.is_empty() {
        return None;
    }
    Some(format!(
        "Could not list the spaces of {}",
        failed.join(", ")
    ))
}

/// The one confirmation the worktree list itself raises, sized to what is about
/// to be lost.
///
/// Three shapes, and the space's own status picks between them:
///
/// * **nothing at risk** — one question, Enter confirms, as before;
/// * **recoverable loss** — the losses are listed and a distinct key is
///   required, because jj snapshots the working copy before forgetting a
///   workspace and `jj undo` brings it back;
/// * **permanent loss** — the losses are listed and the space's *name* has to be
///   typed, because a removed git worktree takes its uncommitted changes with
///   it and nothing anywhere else has a copy.
///
/// Everything is worded in the vocabulary of whichever backend owns the space,
/// so a jj user is not warned about "uncommitted changes" they cannot have.
fn confirm_delete(space: &Space, risk: DeletionRisk) -> ConfirmComponent {
    let noun = space.backend.space_noun();
    let label = if risk.is_safe() {
        format!("Delete this {}?", noun)
    } else {
        format!("This {} holds work that exists nowhere else.", noun)
    };

    let confirm = ConfirmComponent::new(
        format!("Delete {}", noun),
        label,
        space.path.display().to_string(),
        Box::new(|ctx| {
            match ctx.worktrees.delete_selected_space(ctx.repositories) {
                Ok(()) => ctx.worktrees.last_error = None,
                Err(e) => ctx.worktrees.last_error = Some(format!("{:#}", e)),
            }
            ModalFlow::Close
        }),
    )
    // Said on every shape of the dialog, including the safe one: a clean,
    // pushed git space loses no work and still has its branch deleted, and
    // "the directory goes" is not something the user should have to extend
    // to the branch on their own.
    .removing(
        risk.removals().into_iter().map(str::to_string).collect(),
        risk.retained().map(str::to_string),
    );

    match risk.consequence() {
        Consequence::Nothing => confirm,
        Consequence::RecoverableLoss => confirm
            .at_risk(risk.losses(), risk.aftermath().map(str::to_string))
            .require_override(),
        Consequence::PermanentLoss => confirm
            .at_risk(risk.losses(), risk.aftermath().map(str::to_string))
            .require_phrase(space.name.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{events::AppEvent, github::PrInfo, jobs::Completion};
    use color_eyre::eyre;
    use std::sync::mpsc::{self, Receiver};

    fn app_with_worker() -> (App, Receiver<AppEvent>) {
        let dir = std::env::temp_dir().display().to_string();
        let args = cli::Args::for_dirs(dir.clone(), vec![]);
        let fetcher: github::PrFetcher = std::sync::Arc::new(|_| {
            Ok(PrInfo {
                branch_name: "unused".into(),
                is_merged: false,
            })
        });

        let (sender, receiver) = mpsc::channel();
        let mut app = App::with_args(args, fetcher);
        app.attach_worker(Worker::with_threads(sender, 1));
        (app, receiver)
    }

    fn next_result(receiver: &Receiver<AppEvent>) -> JobResult {
        match receiver.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(AppEvent::Job(result)) => result,
            other => panic!("expected a job result, got {other:?}"),
        }
    }

    /// Two git repositories in a fresh repos dir, plus the args pointing at it.
    fn two_repositories() -> (tempfile::TempDir, cli::Args) {
        let repos = tempfile::tempdir().expect("a temp dir");
        for name in ["one", "two"] {
            let path = repos.path().join(name);
            std::fs::create_dir_all(&path).expect("a repo dir");
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&path)
                .status()
                .expect("git init");
        }
        let args = cli::Args::for_dirs(
            repos.path().join("spaces").display().to_string(),
            vec![repos.path().display().to_string()],
        );
        (repos, args)
    }

    /// The freeze this issue is about: construction reads nothing, so the first
    /// frame does not wait on a repos dir of any size.
    #[test]
    fn construction_walks_no_repos_dir() {
        let (_repos, args) = two_repositories();
        let fetcher: github::PrFetcher = std::sync::Arc::new(|_| eyre::bail!("unused"));
        let app = App::with_args(args, fetcher);

        assert!(
            !app.is_scanning(),
            "nothing can be scanning without a worker"
        );
        assert_eq!(app.scan_roots.len(), 1, "the root was not kept for later");
        assert_eq!(app.scan_found, 0, "a repository was opened before the loop");
    }

    /// Discovery is one job per repos dir, and it only starts once there is
    /// somewhere to run it.
    #[test]
    fn attaching_a_worker_starts_one_scan_per_root() {
        let (repos, mut args) = two_repositories();
        args.repos_dirs
            .push(repos.path().join("elsewhere").display().to_string());

        let fetcher: github::PrFetcher = std::sync::Arc::new(|_| eyre::bail!("unused"));
        let mut app = App::with_args(args, fetcher);
        let (sender, _results) = mpsc::channel();
        app.attach_worker(Worker::with_threads(sender, 1));

        assert_eq!(app.scans.len(), 2, "one scan per configured repos dir");
        assert!(app.is_scanning());
    }

    /// `--run-fetch` still becomes one fetch per repository — it now hangs off
    /// the scan result rather than off the constructor, so the network work is
    /// behind the list rather than in front of the first frame.
    #[test]
    fn run_fetch_becomes_a_job_per_repository() {
        let (_repos, mut args) = two_repositories();
        args.run_fetch = true;

        let fetcher: github::PrFetcher = std::sync::Arc::new(|_| eyre::bail!("unused"));
        let mut app = App::with_args(args, fetcher);
        let (sender, results) = mpsc::channel();
        app.attach_worker(Worker::with_threads(sender, 1));
        // Only the scan so far: no remote has been talked to.
        assert_eq!(app.outstanding.len(), 1);

        app.handle_job(next_result(&results));

        assert!(!app.is_scanning(), "the scan never finished");
        assert_eq!(app.outstanding.len(), 2, "the fetches were never queued");
    }

    /// The list fills from the scan, and stops saying "scanning" when it lands.
    #[test]
    fn the_list_is_populated_by_the_scan() {
        let (_repos, args) = two_repositories();
        let fetcher: github::PrFetcher = std::sync::Arc::new(|_| eyre::bail!("unused"));
        let mut app = App::with_args(args, fetcher);
        let (sender, results) = mpsc::channel();
        app.attach_worker(Worker::with_threads(sender, 1));

        assert!(app.is_scanning(), "the spinner must be running");
        app.handle_job(next_result(&results));

        assert!(!app.is_scanning());
        assert_eq!(app.scan_found, 2, "both repositories should have arrived");
    }

    /// The staleness rule, applied to the scan: the answer to a question the app
    /// stopped asking must not put repositories on screen.
    #[test]
    fn a_scan_the_app_stopped_waiting_for_puts_nothing_on_screen() {
        let (_repos, args) = two_repositories();
        let fetcher: github::PrFetcher = std::sync::Arc::new(|_| eyre::bail!("unused"));
        let mut app = App::with_args(args, fetcher);
        let (sender, results) = mpsc::channel();
        app.attach_worker(Worker::with_threads(sender, 1));

        // Abandoned only after the result was produced — the case cancelling
        // cannot help with, and the one a second scan would create.
        let result = next_result(&results);
        let id = result.id;
        app.abandon(id);
        app.handle_job(result);

        assert_eq!(app.scan_found, 0, "a stale scan result was applied");
        assert!(!app.is_scanning());
    }

    /// A job the app is still waiting for reports its failure where the user
    /// will see it, rather than on a worker thread's way out.
    #[test]
    fn a_failed_job_becomes_a_message() {
        let (mut app, results) = app_with_worker();
        app.submit(Job::Custom(Box::new(|| Err(eyre::eyre!("no such remote")))));

        app.handle_job(next_result(&results));

        let shown = app
            .worktrees_component
            .last_error
            .as_deref()
            .expect("the failure is on screen");
        assert!(shown.contains("no such remote"), "{shown}");
    }

    /// The staleness rule: the user moved on, so the answer is no longer wanted
    /// and must not touch a thing.
    #[test]
    fn a_result_the_app_stopped_waiting_for_is_dropped() {
        let (mut app, results) = app_with_worker();
        let id = app
            .submit(Job::Custom(Box::new(|| Err(eyre::eyre!("far too late")))))
            .expect("a worker is attached");

        // Abandoned only *after* the result was produced — the case cancelling
        // cannot help with, and exactly the one the id exists for: the answer is
        // already on the channel when the user moves on.
        let result = next_result(&results);
        app.abandon(id);
        app.handle_job(result);

        assert!(
            app.worktrees_component.last_error.is_none(),
            "a stale result was applied"
        );
    }

    /// The list is re-read once every outstanding job has landed, not once per
    /// result — and never while one is still in flight.
    #[test]
    fn the_list_is_rebuilt_only_after_the_last_result() {
        let (mut app, results) = app_with_worker();
        app.submit(Job::Custom(Box::new(|| {
            Ok(Completion::Fetched {
                path: PathBuf::from("/somewhere"),
            })
        })));
        let second = app
            .submit(Job::Custom(Box::new(|| {
                Ok(Completion::Fetched {
                    path: PathBuf::from("/elsewhere"),
                })
            })))
            .expect("a worker is attached");

        app.handle_job(next_result(&results));
        app.on_tick();
        assert!(app.spaces_stale, "rebuilt while a job was still in flight");

        app.handle_job(next_result(&results));
        app.on_tick();
        assert!(!app.spaces_stale, "the rebuild never happened");
        assert!(app.outstanding.is_empty() && !app.outstanding.contains(&second));
    }
}
