//! The PR flow: a URL in, a space for that pull request out.
//!
//! Two of its steps are slow enough to be jobs — asking GitHub about the pull
//! request, and cloning a repository that is not here yet — and neither may run
//! on the key handler. So the flow is cut in half at every slow step:
//!
//! * the half *before* the job leaves a [`BackgroundWork::Routed`] on the
//!   context, leaves the modal in its waiting state and returns to the loop;
//! * the half *after* it is [`resume_pr_flow`], which the loop calls once the answer has
//!   landed, with the same [`AppContext`] a key handler would have had.
//!
//! Everything the second half needs travels in the [`PrStep`] handed over with
//! the job, so nothing about a flow in progress is held by `App` beyond the job
//! id it must be able to cancel.

use std::path::PathBuf;

use color_eyre::eyre;
use ratatui::{layout::Rect, Frame};

use super::{
    create_worktree::CreateWorktreeComponent,
    footer_entries, popup_area,
    prompt::{prompt_width, FooterEntry, Prompt, PROMPT_HEIGHT},
    select_directory::SelectDirectoryComponent,
    Action, AppContext, BackgroundWork, ConfirmCallback, ConfirmComponent, EventState, Extent,
    HelpEntry, Modal, ModalFlow, ModalKind, SelectCallback, KEYS_SECTION,
};
use crate::theme;
use crate::{
    components::notify::Severity,
    github::{self, PrFetcher, PrInfo, PrUrl},
    jobs::{Completion, Job},
    keymap::InputMode,
    vcs::{self, Backend},
};

/// The frames of the wait spinner.
///
/// Advanced one per *draw* rather than per tick: the loop redraws on every
/// event, a tick included, so counting draws is the same 10fps clock the scan
/// spinner uses — without a modal having to be reachable from the tick handler.
const SPINNER: [&str; 10] = [
    "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280f}",
];

/// The parts of a flow that outlive each of its steps.
///
/// Carried rather than looked up again because a step resumes on the far side of
/// a job: by then the modal that knew the answers is gone.
#[derive(Clone)]
struct Flow {
    /// Whether this is the `P` variant, which clones and creates without asking.
    auto: bool,
    /// What the user typed, so a failed lookup can put them back in front of it.
    typed: String,
    fetch: PrFetcher,
}

/// Which slow step a flow is waiting on, and what its answer is about.
enum Stage {
    Lookup { url: PrUrl },
    Clone { url: PrUrl, info: Box<PrInfo> },
}

/// One suspended PR flow: what is out, and everything needed to carry on.
pub struct PrStep {
    stage: Stage,
    flow: Flow,
}

/// Carries on a flow whose job has finished, or reports why it cannot.
///
/// A failure is handled where it can be acted on: a lookup that failed returns
/// the user to the prompt with the reason under the field they typed into, while
/// a clone that failed has no prompt to return to and reports on the list.
pub fn resume_pr_flow(
    ctx: &mut AppContext,
    step: PrStep,
    outcome: eyre::Result<Completion>,
) -> ModalFlow {
    let PrStep { stage, flow } = step;
    match (stage, outcome) {
        (Stage::Lookup { .. }, Err(error)) => ModalFlow::Replace(Box::new(
            PrWorktreeComponent::reopened(flow, format!("{:#}", error)),
        )),
        (Stage::Lookup { url }, Ok(Completion::PullRequest(info))) => {
            after_lookup(ctx, url, *info, flow)
        }
        (Stage::Clone { .. }, Err(error)) => {
            ctx.notify.error(format!("{:#}", error));
            ModalFlow::Close
        }
        (Stage::Clone { url, info }, Ok(Completion::Cloned { path })) => {
            after_clone(ctx, &url, *info, path, flow.auto)
        }
        // A completion of another shape means a result was routed to the wrong
        // waiter, which is a bug in the loop rather than something the user did.
        // It is still said out loud: a flow that stopped without a word would
        // read as shanti having forgotten the request.
        (_, Ok(other)) => {
            ctx.notify
                .error(format!("unexpected background result: {other:?}"));
            ModalFlow::Close
        }
    }
}

/// The PR is known. Either its repository is here, or it has to be cloned first.
fn after_lookup(ctx: &mut AppContext, url: PrUrl, info: PrInfo, flow: Flow) -> ModalFlow {
    if ctx.repositories.select_repository_by_name(&url.repo) {
        return open_worktree_for_pr(ctx, &url, info, flow.auto);
    }

    if flow.auto {
        return clone_flow(ctx, url, info, flow);
    }

    // Names the backend, because this dialog is where the repository's shape
    // is decided: `github::clone_repository` always clones with git, and a jj
    // user who is not told here finds out much later. One extra word rather
    // than a sentence — the picker that follows carries the jj escape hatch.
    let label = format!(
        "Repository '{}' not found. Clone from GitHub with git?",
        url.repo
    );
    let remote = format!("git@github.com:{}/{}.git", url.owner, url.repo);
    let on_confirm: ConfirmCallback = Box::new(move |ctx| clone_flow(ctx, url, info, flow));
    ModalFlow::Replace(Box::new(ConfirmComponent::new(
        "Clone Repository".to_string(),
        label,
        remote,
        on_confirm,
    )))
}

/// Cloning needs a destination: with several configured repo dirs the user picks
/// one, otherwise the single dir is used without asking.
fn clone_flow(ctx: &mut AppContext, url: PrUrl, info: PrInfo, flow: Flow) -> ModalFlow {
    if ctx.args.repos_dirs.len() > 1 {
        let on_select: SelectCallback<String> =
            Box::new(move |ctx, dir| clone_into(ctx, dir, url, info, flow));
        return ModalFlow::Replace(Box::new(SelectDirectoryComponent::new(
            ctx.args.repos_dirs.clone(),
            on_select,
        )));
    }
    let dir = ctx.args.repos_dirs[0].clone();
    clone_into(ctx, dir, url, info, flow)
}

/// Starts the clone and puts a modal on screen that says so.
///
/// The waiting modal is a `PrWorktreeComponent` rather than a popup of its own
/// because it is still the same question being answered — and because the user
/// must be able to walk away from a clone the same way they walk away from a
/// lookup, with Escape.
fn clone_into(
    ctx: &mut AppContext,
    repos_dir: String,
    url: PrUrl,
    info: PrInfo,
    flow: Flow,
) -> ModalFlow {
    let message = format!("Cloning {}/{} into {}", url.owner, url.repo, repos_dir);
    ctx.run_in_background(BackgroundWork::Routed {
        job: Job::CloneRepository {
            owner: url.owner.clone(),
            repo: url.repo.clone(),
            repos_dir,
        },
        step: PrStep {
            stage: Stage::Clone {
                url,
                info: Box::new(info),
            },
            flow: flow.clone(),
        },
    });
    ModalFlow::Replace(Box::new(PrWorktreeComponent::waiting(flow, message)))
}

/// The clone landed. Take the repository into the list and carry on.
fn after_clone(
    ctx: &mut AppContext,
    url: &PrUrl,
    info: PrInfo,
    path: PathBuf,
    auto: bool,
) -> ModalFlow {
    // Opened by layout rather than as "a git repo we just cloned": the clone may
    // land somewhere that is already colocated with jj, and one rule for picking
    // a backend is the whole point of the seam.
    match vcs::open_at(&path, false) {
        Ok(backends) => {
            // One entry per backend that drives the clone; the picker still
            // shows a colocated one once.
            for backend in backends {
                ctx.repositories.add_repository(backend);
            }
            ctx.repositories.select_repository_by_name(&url.repo);
        }
        Err(e) => {
            ctx.notify
                .error(format!("Cloned but failed to load repo: {:#}", e));
            return ModalFlow::Close;
        }
    }

    let flow = open_worktree_for_pr(ctx, url, info, auto);

    // The silent auto-clone path (`P` with a single configured repos dir) shows
    // neither the "clone with git?" confirm dialog nor the directory picker —
    // there is nothing to confirm and nothing to pick — so this is the only
    // chance to tell a jj user their brand-new repository is a plain git clone.
    // It lives here, in the clone's *result* handler, rather than beside the
    // key press that started it: the clone runs on a background worker, so its
    // outcome arrives as a job result. Only a handler on that path can know the
    // clone actually happened.
    if let Some(notice) = git_clone_notice(url, auto, &ctx.args.repos_dirs) {
        // One slot, newest wins: an advisory about the clone's shape must not
        // paint over a real problem raised while opening the worktree — a failed
        // `create_space` or a merged-branch warning is the more urgent news.
        let urgent = matches!(
            ctx.notify.current().map(|n| n.severity),
            Some(Severity::Warning | Severity::Error)
        );
        if !urgent {
            ctx.notify.info(notice);
        }
    }

    flow
}

/// The line shown after a clone lands, or `None` when the user was already told
/// the clone uses git.
///
/// Only the silent path needs it: with the confirm dialog and the directory
/// picker both naming git (and the picker carrying the `jj git init --colocate`
/// hint), the sole path that says nothing is `P` auto-clone into a single repos
/// dir, where neither prompt appears.
fn git_clone_notice(url: &PrUrl, auto: bool, repos_dirs: &[String]) -> Option<String> {
    (auto && repos_dirs.len() == 1).then(|| {
        format!(
            "Cloned {} with git — run `jj git init --colocate` in it for jj",
            url.repo
        )
    })
}

// ---------------------------------------------------------------------------
// The modal
// ---------------------------------------------------------------------------

/// What the modal is waiting for, while it waits.
///
/// The job's id is *not* here: only `App` can mint one and only `App` can cancel
/// one, so it keeps the id and this keeps the half the user can see. Closing the
/// modal leaves a [`BackgroundWork::Abandon`], which is how the two halves stay
/// in step.
struct Waiting {
    /// Said in the status row, so the wait names what it is waiting for.
    message: String,
    /// Which spinner frame is next.
    frame: usize,
}

pub struct PrWorktreeComponent {
    character_index: usize,
    pub input: String,
    pub error: Option<String>,
    pub auto_clone: bool,
    /// Where PR data comes from. Injected so the steps that only exist after a
    /// successful fetch can be exercised without talking to GitHub.
    fetch: PrFetcher,
    /// `Some` while a job is out: the field is frozen and a spinner turns.
    waiting: Option<Waiting>,
}

impl PrWorktreeComponent {
    /// `auto_clone` skips both the "clone this repo?" prompt and the branch-name
    /// prompt, going straight from a PR URL to a created worktree.
    ///
    /// Its background work is deferred through the [`AppContext`] every modal is
    /// handed, so there is nothing to wire up: the loop drains it once the stack
    /// settles.
    pub fn new(auto_clone: bool, fetch: PrFetcher) -> Self {
        Self {
            character_index: 0,
            input: String::new(),
            error: None,
            auto_clone,
            fetch,
            waiting: None,
        }
    }

    /// The prompt as it looked when a lookup was started, plus why it failed.
    fn reopened(flow: Flow, error: String) -> Self {
        let mut prompt = Self::from_flow(flow);
        prompt.error = Some(error);
        prompt
    }

    /// A modal that only waits: the URL is still readable, nothing is editable.
    fn waiting(flow: Flow, message: String) -> Self {
        let mut prompt = Self::from_flow(flow);
        prompt.waiting = Some(Waiting { message, frame: 0 });
        prompt
    }

    fn from_flow(flow: Flow) -> Self {
        Self {
            character_index: flow.typed.chars().count(),
            input: flow.typed,
            error: None,
            auto_clone: flow.auto,
            fetch: flow.fetch,
            waiting: None,
        }
    }

    fn flow(&self) -> Flow {
        Flow {
            auto: self.auto_clone,
            typed: self.input.clone(),
            fetch: self.fetch.clone(),
        }
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        // Three things can occupy the status row, and they are ranked by how
        // much the user needs them: what is happening now, why the last attempt
        // failed, and — with nothing else to say — the shape of a PR URL.
        // Short enough to survive the narrowest popup this can be: the shape is
        // only useful if it can be read whole.
        let status = match (&mut self.waiting, &self.error) {
            (Some(waiting), _) => {
                let glyph = SPINNER[waiting.frame % SPINNER.len()];
                // Advanced here so the spinner turns for as long as the frame
                // keeps being drawn — which is once per tick, and is the only
                // evidence the user has that a multi-minute clone is alive.
                waiting.frame = waiting.frame.wrapping_add(1);
                Some((format!("{glyph} {}…", waiting.message), theme::secondary()))
            }
            (None, Some(err)) => Some((err.clone(), theme::danger_text())),
            (None, None) => Some(("github.com/owner/repo/pull/123".to_string(), theme::muted())),
        };

        Prompt {
            title: "Worktree from PR",
            // The auto-clone variant does more than the plain one, and that
            // difference is invisible once the popup is open unless it is said.
            context: self.auto_clone.then(|| "auto-clone".to_string()),
            label: "GitHub PR URL",
            aside: None,
            value: &self.input,
            placeholder: "paste a PR URL…",
            cursor: self.character_index,
            // Only a rejected URL reddens the box; an unfinished one is not wrong
            // yet, and `submit` is the only thing that can decide.
            valid: self.error.is_none(),
            status,
            footer: self.footer(),
        }
        .render(frame, area);
    }

    /// Read off [`Modal::help`], which already knows that Enter means nothing
    /// while a job is out — so the footer stops offering it for the same reason
    /// and at the same moment as the help popup does.
    fn footer(&self) -> Vec<FooterEntry<'static>> {
        footer_entries(&self.help(), KEYS_SECTION)
    }

    pub fn handle_action(&mut self, action: Action) -> EventState {
        // The field is frozen while a job is out: a URL edited under a lookup
        // that is already running would describe something else entirely. The
        // keystroke is still swallowed — nothing below this modal may act on it.
        match action {
            Action::InsertChar(c) => {
                if self.waiting.is_none() {
                    self.enter_char(c);
                }
                EventState::Consumed
            }
            Action::DeleteChar => {
                if self.waiting.is_none() {
                    self.delete_char();
                }
                EventState::Consumed
            }
            _ => EventState::NotConsumed,
        }
    }

    fn enter_char(&mut self, c: char) {
        let index = self.byte_index();
        self.input.insert(index, c);
        self.move_cursor_right();
        self.error = None;
    }

    fn delete_char(&mut self) {
        if self.character_index != 0 {
            let current_index = self.character_index;
            let before = self.input.chars().take(current_index - 1);
            let after = self.input.chars().skip(current_index);
            self.input = before.chain(after).collect();
            self.move_cursor_left();
            self.error = None;
        }
    }

    fn byte_index(&self) -> usize {
        self.input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.character_index)
            .unwrap_or(self.input.len())
    }

    fn move_cursor_right(&mut self) {
        let moved = self.character_index.saturating_add(1);
        self.character_index = moved.clamp(0, self.input.chars().count());
    }

    fn move_cursor_left(&mut self) {
        let moved = self.character_index.saturating_sub(1);
        self.character_index = moved.clamp(0, self.input.chars().count());
    }

    /// Starts the lookup and hands the flow to the loop.
    ///
    /// A rejected URL keeps the prompt open with an error so it can be
    /// corrected; an accepted one freezes the field and starts spinning. Enter
    /// pressed a second time is ignored rather than starting a second lookup:
    /// the first one is what this modal describes, and two answers to one
    /// question would race for the same screen.
    fn submit(&mut self, ctx: &mut AppContext) -> ModalFlow {
        if self.waiting.is_some() {
            return ModalFlow::Consumed;
        }

        let url = match github::parse_pr_url(&self.input) {
            Ok(url) => url,
            Err(e) => {
                self.set_error(format!("{:#}", e));
                return ModalFlow::Consumed;
            }
        };

        let message = format!("Looking up {}/{} #{}", url.owner, url.repo, url.number);
        ctx.run_in_background(BackgroundWork::Routed {
            job: Job::FetchPullRequest {
                fetcher: self.fetch.clone(),
                url: url.clone(),
            },
            step: PrStep {
                stage: Stage::Lookup { url },
                flow: self.flow(),
            },
        });
        self.error = None;
        self.waiting = Some(Waiting { message, frame: 0 });
        ModalFlow::Consumed
    }
}

impl Modal for PrWorktreeComponent {
    fn kind(&self) -> ModalKind {
        ModalKind::PrWorktree
    }

    fn area(&self, full: Rect) -> Rect {
        // Wider than the name prompt: a pull request URL is roughly 45 columns
        // before anyone's repository is named, and an input the user cannot read
        // back is an input they cannot correct.
        popup_area(
            full,
            prompt_width(70, 38, 110),
            Extent::fixed(PROMPT_HEIGHT),
        )
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        PrWorktreeComponent::draw(self, frame, area);
    }

    /// Insert even while waiting, where nothing can be typed: Normal mode would
    /// make every letter a list command — 'q' would quit in the middle of a
    /// clone — and the keys that matter here, Escape and Ctrl+C, work in both.
    fn mode(&self) -> InputMode {
        InputMode::Insert
    }

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select => self.submit(ctx),
            Action::ClosePopup | Action::ExitInsertMode => {
                // Leaving is what cancels: the loop cannot see that this modal
                // is gone, so it is told before it goes.
                if self.waiting.is_some() {
                    ctx.run_in_background(BackgroundWork::Abandon);
                }
                ModalFlow::Close
            }
            _ => self.handle_action(action).into(),
        }
    }

    fn help(&self) -> Vec<HelpEntry> {
        if self.waiting.is_some() {
            return vec![
                HelpEntry::Section(KEYS_SECTION),
                HelpEntry::bind("Esc", "Stop waiting and cancel")
                    .hint("Esc", "cancel")
                    .safe()
                    .essential(),
                HelpEntry::bind("Ctrl+C", "Quit"),
            ];
        }
        vec![
            HelpEntry::Section(KEYS_SECTION),
            HelpEntry::bind("Enter", "Fetch PR and open worktree").hint("Enter", "open"),
            HelpEntry::bind("F1", "Show this help")
                .hint("F1", "help")
                .aside(),
            HelpEntry::bind("Esc", "Cancel")
                .hint("Esc", "cancel")
                .safe()
                .essential(),
            HelpEntry::bind("Backspace", "Delete character"),
            HelpEntry::bind("Ctrl+C", "Quit"),
        ]
    }
}

/// Said in both places a space is created from a merged PR — the name prompt,
/// which shows it in the warning style, and the notification raised after an
/// automatic creation, which is a `Severity::Warning`. One string so the two
/// cannot drift apart, and no "Warning:" prefix in it: both places already say
/// that with colour.
const MERGED_BRANCH_WARNING: &str = "PR is merged, branch may be deleted on remote";

/// Final step of the PR flow: select the existing worktree, create one outright
/// (auto mode), or hand over to the branch-name prompt.
fn open_worktree_for_pr(
    ctx: &mut AppContext,
    url: &github::PrUrl,
    pr_info: github::PrInfo,
    auto: bool,
) -> ModalFlow {
    // The PR branch may have been pushed since the last fetch; without this it is
    // invisible to the backend and the space is silently based on trunk instead.
    // A failed refresh only costs a stale view of the remotes, so it is not fatal.
    if let Some(repo) = ctx.repositories.selected_repository() {
        vcs::refresh(repo);
    }

    let branch = pr_info.branch_name.clone();

    if ctx.worktrees.select_worktree_by_branch(&branch) {
        if pr_info.is_merged {
            // Informational, and this is the message the issue names: nothing
            // failed, nothing is owed. The user asked for a PR's space and got
            // one — the news is only that the PR has already landed.
            ctx.notify.info("PR is merged — existing worktree selected");
        }
        return ModalFlow::Close;
    }

    if auto {
        match ctx.create_space(&branch) {
            // A warning, not an error: the space exists and is usable, but it
            // was based on a branch the remote may already have deleted, so it
            // is not the space the user was picturing. The word "Warning:" is
            // gone from the text — the severity now says that in colour, and
            // repeating it cost characters the half-width status zone does not
            // have.
            Ok(path) => {
                // Remembered here and not before: this is the first moment a
                // space exists for the URL to belong to.
                ctx.remember_pr(&path, &url.to_url());
                if pr_info.is_merged {
                    ctx.notify.warn(MERGED_BRANCH_WARNING)
                } else {
                    ctx.notify.clear()
                }
            }
            Err(e) => ctx.notify.error(format!("{:#}", e)),
        }
        return ModalFlow::Close;
    }

    let selected = ctx.repositories.selected_repository().map(|repo| {
        (
            repo.repo().name.clone(),
            repo.repo().id.clone(),
            repo.backend(),
            repo.resolve_base(&branch),
        )
    });
    let (repo_name, backend, colocated, base_branch_hint) = match selected {
        Some((name, id, backend, base)) => {
            let colocated = ctx.repositories.store().backends_of(&id).len() > 1;
            (name, backend, colocated, Some(base))
        }
        None => (String::new(), Backend::Git, false, None),
    };

    let mut prompt = CreateWorktreeComponent::new_with_branch(
        repo_name,
        backend,
        colocated,
        branch,
        pr_info.is_merged.then(|| MERGED_BRANCH_WARNING.to_string()),
    );
    prompt.base_branch_hint = base_branch_hint;
    // The prompt records the PR itself, once the user confirms: a URL the user
    // typed and then escaped out of belongs to no space.
    prompt.pr_url = Some(url.to_url());
    ModalFlow::Replace(Box::new(prompt))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::components::{Notifications, RepositoriesComponent, WorktreesComponent};
    use crate::jobs::JobKind;

    fn a_flow() -> Flow {
        Flow {
            auto: false,
            typed: "https://github.com/acme/widget/pull/7".to_string(),
            fetch: Arc::new(|_| unreachable!("no lookup in these tests")),
        }
    }

    fn a_url() -> PrUrl {
        PrUrl {
            owner: "acme".to_string(),
            repo: "widget".to_string(),
            number: 7,
        }
    }

    fn an_info() -> PrInfo {
        PrInfo {
            branch_name: "feature-from-pr".to_string(),
            is_merged: false,
        }
    }

    fn an_args() -> crate::cli::Args {
        crate::cli::Args::for_dirs("/tmp/spaces", vec!["/tmp/repos".to_string()])
    }

    /// Renders a modal at a comfortable size and returns the screen as text.
    fn screen_of(modal: &mut dyn Modal) -> String {
        let mut worktrees = WorktreesComponent::new(Vec::new());
        let mut repositories = RepositoriesComponent::new(Vec::new());
        let args = an_args();
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal init");
        terminal
            .draw(|frame| {
                let area = modal.area(frame.area());
                let mut ctx = AppContext {
                    worktrees: &mut worktrees,
                    repositories: &mut repositories,
                    notify: &mut Notifications::default(),
                    args: &args,
                    background: &mut Vec::new(),
                    meta: &mut crate::space_meta::SpaceMeta::in_memory(),
                };
                modal.draw(frame, area, &mut ctx);
            })
            .expect("draw failed");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The clone is the worst wait in the app, and the one the key handler must
    /// never take: choosing a destination only *starts* it.
    ///
    /// Driven here rather than through the key handler because the step after
    /// this one shells out to `git clone`, which the suite has no business doing.
    #[test]
    fn choosing_a_destination_starts_a_clone_job_and_says_so() {
        let mut worktrees = WorktreesComponent::new(Vec::new());
        let mut repositories = RepositoriesComponent::new(Vec::new());
        let args = an_args();
        let mut background = Vec::new();
        let next = {
            let mut ctx = AppContext {
                worktrees: &mut worktrees,
                repositories: &mut repositories,
                notify: &mut Notifications::default(),
                args: &args,
                background: &mut background,
                meta: &mut crate::space_meta::SpaceMeta::in_memory(),
            };
            clone_into(
                &mut ctx,
                "/tmp/repos".to_string(),
                a_url(),
                an_info(),
                a_flow(),
            )
        };

        let raised = background.pop().expect("the clone must have been raised");
        let BackgroundWork::Routed { job, .. } = raised else {
            panic!("cloning should raise routed work, not an abandon");
        };
        assert_eq!(job.kind(), JobKind::CloneRepository);

        let ModalFlow::Replace(mut modal) = next else {
            panic!("the picker should be replaced by something that waits");
        };
        let screen = screen_of(modal.as_mut());
        assert!(
            screen.contains("Cloning acme/widget into /tmp/repos"),
            "the wait must name the clone it is waiting on:\n{screen}"
        );
        // Minutes of clone are unreadable without something moving.
        assert_ne!(screen, screen_of(modal.as_mut()), "the spinner must turn");
    }

    /// The silent path — `P` auto-clone into the one configured repos dir —
    /// shows no confirm dialog and no picker, so the clone's result handler is
    /// where the user finally learns the repository is a plain git clone and how
    /// to colocate it.
    #[test]
    fn a_silent_auto_clone_announces_the_git_clone_and_the_jj_escape_hatch() {
        let notice = git_clone_notice(&a_url(), true, &["/tmp/repos".to_string()])
            .expect("the silent path must say something");
        assert!(
            notice.contains("widget") && notice.contains("git"),
            "the notice must name the repository and that git was used:\n{notice}"
        );
        assert!(
            notice.contains("jj git init --colocate"),
            "the notice must tell a jj user how to colocate:\n{notice}"
        );
    }

    /// Every other path already tells the user: the confirm dialog and the
    /// picker both name git, so a second notice would be noise.
    #[test]
    fn the_paths_that_prompt_stay_silent_afterwards() {
        // Not auto: the confirm dialog was shown.
        assert!(git_clone_notice(&a_url(), false, &["/tmp/repos".to_string()]).is_none());
        // Auto but several dirs: the picker was shown.
        assert!(git_clone_notice(
            &a_url(),
            true,
            &["/tmp/one".to_string(), "/tmp/two".to_string()],
        )
        .is_none());
    }

    /// Escape during a clone is the only way out, so it has to reach the loop:
    /// closing the modal alone would leave the job running with nobody waiting.
    #[test]
    fn escaping_a_wait_asks_the_loop_to_abandon_the_job() {
        let mut modal = PrWorktreeComponent::waiting(a_flow(), "Cloning acme/widget".to_string());

        let mut worktrees = WorktreesComponent::new(Vec::new());
        let mut repositories = RepositoriesComponent::new(Vec::new());
        let args = an_args();
        let mut background = Vec::new();
        {
            let mut ctx = AppContext {
                worktrees: &mut worktrees,
                repositories: &mut repositories,
                notify: &mut Notifications::default(),
                args: &args,
                background: &mut background,
                meta: &mut crate::space_meta::SpaceMeta::in_memory(),
            };

            assert!(matches!(
                modal.handle(Action::ClosePopup, &mut ctx),
                ModalFlow::Close
            ));
        }
        assert!(matches!(background.pop(), Some(BackgroundWork::Abandon)));
    }
}
