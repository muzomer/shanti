use std::path::{Path, PathBuf};

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
        worktrees_bindings, Action, AppContext, ConfirmComponent, EventState, HelpComponent, Modal,
        ModalFlow, PrWorktreeComponent, RepositoriesComponent, RepositoriesModal,
        WorktreesComponent,
    },
    github,
    keymap::{self, InputMode},
    vcs::{self, Consequence, DeletionRisk, Discovered, Space},
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
        let found = Self::discover_repositories(&args);
        let repositories = RepositoriesComponent::new(vcs::open_backends(&found, args.run_fetch));

        // Spaces are collected through the `Vcs` trait, so a jj repository shows
        // up here on exactly the same terms as a git one.
        let (spaces, failed) = repositories.collect_spaces();
        let mut worktrees_component = WorktreesComponent::new(spaces);
        worktrees_component.last_error = listing_failure_notice(&failed);

        Self {
            worktrees_component,
            repositories_component: repositories,
            modals: Vec::new(),
            args,
            pr_fetcher,
            mode: InputMode::Normal,
            selected_path: None,
        }
    }

    /// Walk every configured repos dir once, skipping the worktrees dir.
    ///
    /// The exclusion is what stops a worktrees dir nested inside a repos dir
    /// from having its spaces rediscovered as repositories in their own right.
    fn discover_repositories(args: &cli::Args) -> Vec<Discovered> {
        let excluded = vec![PathBuf::from(&args.worktrees_dir)];
        args.repos_dirs
            .iter()
            .flat_map(|dir| {
                debug!("Listing repositories in: {}", dir);
                vcs::discover(Path::new(dir), &excluded)
            })
            .collect()
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
            Action::Delete | Action::ForceDelete => {
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
        let risk = DeletionRisk::of(&space);
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
