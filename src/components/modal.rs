//! The modal stack: the contract every popup implements.
//!
//! A modal owns its own state and, when it is done, returns a [`ModalFlow`]
//! describing what should happen to the stack. `App` never learns what any
//! individual modal is for — it only pushes, pops and draws. Adding a popup
//! therefore means adding a `Modal` implementation, not another field and
//! another match arm on `App`.

use color_eyre::eyre::{self, eyre};
use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    Frame,
};

use crate::{cli, hooks::HookPlan, keymap::InputMode, vcs};

use super::{
    notify::Notifications, worktrees::SpaceEntry, Action, EventState, HelpEntry,
    RepositoriesComponent, WorktreesComponent,
};

/// The state that outlives any single popup, lent to a modal while it runs.
///
/// Anything a modal needs *only for itself* lives in the modal. What is here is
/// shared with the base worktree pane, or handed on to the next step of a flow.
pub struct AppContext<'a> {
    pub worktrees: &'a mut WorktreesComponent,
    pub repositories: &'a mut RepositoriesComponent,
    /// Where a modal says what happened, graded by how loud it should be.
    ///
    /// Lent rather than owned by any component: a modal closes the moment it is
    /// done, and its news has to outlive it by the few seconds it takes to read.
    pub notify: &'a mut Notifications,
    pub args: &'a cli::Args,
    /// Where a created space leaves the setup work it cannot do itself.
    ///
    /// The same seam the PR flow uses for its jobs, in its smallest form: a
    /// modal must not reach the worker — running `npm install` on the key
    /// handler is exactly the freeze the job pool exists to prevent — so it
    /// leaves the plan here and `App` submits it as soon as the stack settles.
    pub pending_hooks: &'a mut Vec<HookPlan>,
}

impl AppContext<'_> {
    /// Creates a space named `name` in the selected repository and shows it.
    ///
    /// Lives on the context because two flows need it — the name prompt and the
    /// PR flow — and both must apply the same layout policy and go through the
    /// same backend. Reporting is left to the caller: only it knows whether
    /// success is worth a message of its own.
    pub fn create_space(&mut self, name: &str) -> eyre::Result<()> {
        let repo = self
            .repositories
            .selected_repository()
            .ok_or_else(|| eyre!("no repository is selected"))?;
        let repo_name = repo.repo().name.clone();
        let repo_path = repo.repo().path.clone();
        let dest = vcs::space_dest(&self.args.worktrees_dir, &repo_name, name);
        // Creating and planning happen together so no caller can create a space
        // and forget its setup. Planning is pure and cheap; only `HookPlan::run`
        // blocks, and that happens on a worker.
        let (space, plan) = vcs::create_space_with_hooks(repo, name, &dest, &self.args.hooks)?;
        // A user with no hooks configured pays nothing: no job, no id to track,
        // no spinner.
        if !plan.is_empty() {
            self.pending_hooks.push(plan);
        }

        self.worktrees.add(SpaceEntry {
            repo_name,
            repo_path,
            space,
        });
        Ok(())
    }
}

/// Work a modal defers to whoever confirms it. The confirming modal is generic;
/// the meaning of "yes" belongs to the code that opened it.
pub type ConfirmCallback = Box<dyn FnOnce(&mut AppContext) -> ModalFlow>;

/// Same idea, for a modal that yields a chosen value along with the confirmation.
pub type SelectCallback<T> = Box<dyn FnOnce(&mut AppContext, T) -> ModalFlow>;

/// What a modal asks the stack to do after it has handled an action.
pub enum ModalFlow {
    /// Handled; the modal stays open.
    Consumed,
    /// Not handled; the key means nothing here.
    Ignored,
    /// Pop this modal, revealing whatever is beneath it.
    Close,
    /// Pop this modal and push another in its place — one step of a flow.
    /// (Stacking rather than replacing is what `App` does for the help popup.)
    Replace(Box<dyn Modal>),
}

impl From<EventState> for ModalFlow {
    fn from(state: EventState) -> Self {
        match state {
            EventState::Consumed => ModalFlow::Consumed,
            // No inner component asks to quit; only the stack decides that.
            _ => ModalFlow::Ignored,
        }
    }
}

/// Which modal is on top, as a value rather than a rendered title.
///
/// There is no `Focus` enum any more — focus is "whatever is on top of the modal
/// stack" — so this is what an observer (a test above all) names instead. It is
/// the stable identity of a modal, independent of what the modal paints, so an
/// assertion on it does not break when a title is reworded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    Repositories,
    CreateWorktree,
    Confirm,
    Help,
    PrWorktree,
    SelectReposDir,
}

pub trait Modal {
    /// This modal's stable identity, so the stack can be observed without
    /// reading what it draws. Every modal declares its own — there is no
    /// sensible default, and a new modal that forgets to is a compile error.
    fn kind(&self) -> ModalKind;

    /// Where this modal sits inside the full frame. Each modal sizes itself, so
    /// the stack can draw bottom-to-top without knowing any geometry.
    fn area(&self, full: Rect) -> Rect;

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &mut AppContext);

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow;

    /// The input mode in force while this modal is on top. Popping therefore
    /// restores the mode of the layer below with no bookkeeping.
    fn mode(&self) -> InputMode {
        InputMode::Normal
    }

    /// Keybindings shown when help is opened over this modal.
    fn help(&self) -> Vec<HelpEntry> {
        Vec::new()
    }
}

/// Centres `width` × `height` constraints inside `full`.
pub fn centered(full: Rect, width: Constraint, height: Constraint) -> Rect {
    let [area] = Layout::vertical([height]).flex(Flex::Center).areas(full);
    let [area] = Layout::horizontal([width]).flex(Flex::Center).areas(area);
    area
}

// --- The size floor ---------------------------------------------------------

/// The smallest terminal shanti will draw its interface into.
///
/// Chosen as the point below which the *content* stops being readable rather
/// than the point below which the code breaks: 40 columns is about what a
/// `git repo / space-name` row needs once the border and the two status glyphs
/// are paid for, and 10 rows leaves a title, a footer and a handful of spaces.
/// Below this the base pane draws one sentence instead of a shredded frame.
pub const MIN_WIDTH: u16 = 40;
/// See [`MIN_WIDTH`].
pub const MIN_HEIGHT: u16 = 10;

/// Whether `area` is at or above the supported floor.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
}

/// How wide, or how tall, a popup would like to be.
///
/// One type for both axes because the policy is the same on both: take a share
/// of the frame, but never shrink past the point where the content stops making
/// sense, and never grow past the point where a centred dialog becomes a wall.
/// The frame has the final word — see [`Extent::resolve`].
#[derive(Clone, Copy)]
pub struct Extent {
    percent: u16,
    min: u16,
    max: u16,
}

impl Extent {
    /// A share of the frame, held between a floor and a ceiling.
    pub const fn share(percent: u16, min: u16, max: u16) -> Self {
        Self { percent, min, max }
    }

    /// Exactly `n` cells, still clipped to the frame by [`Extent::resolve`].
    /// What a popup sized from its own content asks for.
    pub const fn fixed(n: u16) -> Self {
        Self {
            percent: 100,
            min: n,
            max: n,
        }
    }

    /// The size to actually use, given how much there is.
    ///
    /// The clamp to `available` comes *last* on purpose: `min` expresses a
    /// preference, not a guarantee, and a popup that honoured its floor over the
    /// frame would be the overflow this whole type exists to prevent. When the
    /// frame is the binding constraint the popup gets less than it asked for and
    /// has to degrade — which is why every popup below lays its body out from
    /// the area it is handed rather than from what it requested.
    pub(super) fn resolve(self, available: u16) -> u16 {
        // u32 throughout: `available * percent` overflows u16 past 655 cells.
        let want = (u32::from(available) * u32::from(self.percent) / 100) as u16;
        want.clamp(self.min.min(self.max), self.max).min(available)
    }
}

/// The rectangle a popup gets: centred, sized by its [`Extent`]s, and never
/// larger than the frame.
///
/// Returns an empty rect when the terminal is below the floor, which makes every
/// popup a no-op there — nothing clears, so the one message the base pane draws
/// stays legible instead of being overpainted by a dialog that cannot fit.
pub fn popup_area(full: Rect, width: Extent, height: Extent) -> Rect {
    if !fits(full) {
        return Rect::ZERO;
    }
    centered(
        full,
        Constraint::Length(width.resolve(full.width)),
        Constraint::Length(height.resolve(full.height)),
    )
}

/// Horizontal breathing room inside a popup, surrendered a step at a time as the
/// popup narrows. Padding is the first thing to go: at 40 columns the content is
/// worth more than the margin around it.
pub fn gutter(width: u16) -> u16 {
    match width {
        0..=31 => 0,
        32..=51 => 1,
        52..=79 => 2,
        _ => 4,
    }
}

/// Puts the caret at `(col, row)` unless that falls outside `area`.
///
/// Typing runs past the right edge of a narrow input long before the input
/// scrolls, and a caret parked outside its own widget lands on whatever happens
/// to be there. Pinning it to the last cell keeps it inside the box it belongs
/// to.
pub fn place_cursor(frame: &mut Frame, area: Rect, col: u16, row: u16) {
    if area.is_empty() {
        return;
    }
    frame.set_cursor_position((
        col.min(area.right().saturating_sub(1)),
        row.min(area.bottom().saturating_sub(1)),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    /// A ladder from absurd to comfortable. The interesting sizes are the ones
    /// on either side of the floor: `39x9` is the last frame that gets the
    /// message, `40x10` the first that gets the interface.
    const SIZES: [(u16, u16); 8] = [
        (1, 1),
        (3, 2),
        (10, 4),
        (39, 9),
        (40, 10),
        (60, 16),
        (140, 50),
        (400, 120),
    ];

    fn frame_at(size: (u16, u16), draw: impl FnOnce(&mut Frame, Rect)) -> Rect {
        let mut terminal =
            Terminal::new(TestBackend::new(size.0, size.1)).expect("terminal should init");
        let mut used = Rect::ZERO;
        terminal
            .draw(|frame| {
                used = frame.area();
                draw(frame, used);
            })
            .expect("drawing must not fail at any terminal size");
        used
    }

    /// The contract of [`popup_area`], at every size: inside the frame, or empty.
    #[test]
    fn a_popup_never_escapes_the_frame_at_any_size() {
        let extents = [
            (Extent::share(60, 34, 100), Extent::fixed(22)),
            (
                Extent::share(70, 38, 110),
                Extent::fixed(PROMPT_HEIGHT_FOR_TEST),
            ),
            (Extent::share(50, 34, 80), Extent::share(50, 8, 30)),
            // A popup asking for more than any terminal has, twice over.
            (Extent::fixed(500), Extent::fixed(500)),
        ];
        for (w, h) in extents {
            for (width, height) in SIZES {
                let full = Rect::new(0, 0, width, height);
                let area = popup_area(full, w, h);
                if !fits(full) {
                    assert!(
                        area.is_empty(),
                        "below the floor a popup must not draw at all, got {area:?} in {full:?}"
                    );
                    continue;
                }
                assert!(
                    area.right() <= full.right() && area.bottom() <= full.bottom(),
                    "popup {area:?} escaped the frame {full:?}"
                );
            }
        }
    }

    const PROMPT_HEIGHT_FOR_TEST: u16 = 9;

    /// Every real popup, drawn at every size on the ladder.
    ///
    /// The point is not what it looks like — that is `shanti-b03.2`'s job — but
    /// that no size on the way down produces a panic, and that below the floor
    /// each popup declines to draw so the base pane's message survives.
    #[test]
    fn every_popup_draws_at_every_size() {
        use super::super::{
            create_worktree::CreateWorktreeComponent, help::worktrees_bindings,
            select_directory::SelectDirectoryComponent, ConfirmComponent, HelpComponent,
            PrWorktreeComponent, WorktreesComponent,
        };
        use crate::keymap::InputMode;
        use crate::vcs::Backend;

        for size in SIZES {
            // A dialog with every optional section filled, so the tallest shape
            // is the one under test.
            let mut confirm = ConfirmComponent::new(
                "Delete Space".into(),
                "This cannot be undone.".into(),
                "acme/widget / feature-one".into(),
                Box::new(|_| ModalFlow::Close),
            )
            .at_risk(
                vec![
                    "3 uncommitted files (modified, staged or untracked)".into(),
                    "a branch that was never pushed".into(),
                ],
                Some("Deleting it cannot be undone.".into()),
            )
            .removing(
                vec![
                    "the branch it has checked out".into(),
                    "the worktree directory and its registration".into(),
                ],
                Some("Commits already pushed are kept.".into()),
            )
            .require_override();
            frame_at(size, |frame, full| {
                let area = confirm.area(full);
                confirm.draw(frame, area);
            });

            let mut create = CreateWorktreeComponent::new("acme-widget".into(), Backend::Git, true);
            create.base_branch_hint = Some("Will be created from main (default branch)".into());
            frame_at(size, |frame, full| {
                let area = create.area(full);
                create.draw(frame, area);
            });

            let mut pr = PrWorktreeComponent::new(
                true,
                std::sync::Arc::new(|_: &crate::github::PrUrl| unreachable!("no lookup in draw")),
            );
            pr.set_error("GitHub auth failed".into());
            frame_at(size, |frame, full| {
                let area = pr.area(full);
                pr.draw(frame, area);
            });

            let mut picker = SelectDirectoryComponent::new(
                (0..12).map(|i| format!("/home/dev/src/dir-{i}")).collect(),
                Box::new(|_, _| ModalFlow::Close),
            );
            frame_at(size, |frame, full| {
                let area = picker.area(full);
                picker.draw(frame, area);
            });

            let mut help = HelpComponent::new(worktrees_bindings());
            frame_at(size, |frame, full| {
                let area = help.area(full);
                help.draw(frame, area);
            });

            let mut spaces = WorktreesComponent::new(Vec::new());
            frame_at(size, |frame, full| {
                spaces.draw(frame, full, InputMode::Insert, true, false, None);
            });
        }
    }

    /// The help popup is the one that can want more rows than the terminal has,
    /// so it is the one that must scroll rather than lose them.
    #[test]
    fn the_help_popup_scrolls_instead_of_overflowing() {
        use super::super::{help::worktrees_bindings, HelpComponent};

        let mut help = HelpComponent::new(worktrees_bindings());
        let (_, wanted) = help.dimensions();
        let short = (60u16, 20u16);
        assert!(
            wanted > short.1,
            "the table should be taller than the frame under test"
        );

        let area = frame_at(short, |frame, full| {
            let area = help.area(full);
            help.draw(frame, area);
            assert!(area.height <= full.height, "help must not exceed the frame");
        });
        assert_eq!(area.height, short.1);

        // The rows the frame could not show are reachable, and going past the
        // end stops at the end rather than running away.
        assert_eq!(help.handle_action(Action::MoveUp), EventState::Consumed);
        assert_eq!(help.handle_action(Action::GoLast), EventState::Consumed);
        let bottom = help.scroll;
        assert!(bottom > 0, "a clipped table must be scrollable");
        help.handle_action(Action::MoveDown);
        assert_eq!(help.scroll, bottom, "scrolling stops at the last row");
    }

    /// Rendering at an absurd size must be a no-op, never a panic.
    ///
    /// Ratatui's layout solver and every widget below tolerate a zero-sized
    /// rect; this pins that down for the geometry *we* compute, which is where a
    /// subtraction on a `u16` would otherwise wrap.
    #[test]
    fn drawing_into_an_empty_or_tiny_rect_is_a_no_op() {
        for size in SIZES {
            frame_at(size, |frame, full| {
                let area = popup_area(full, Extent::share(60, 34, 100), Extent::fixed(22));
                // Whatever geometry a popup derives from its area has to survive
                // an empty one, including the caret.
                place_cursor(frame, area, u16::MAX, u16::MAX);
                let _ = gutter(area.width);
            });
        }
    }
}
