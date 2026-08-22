//! The modal stack: the contract every popup implements.
//!
//! A modal owns its own state and, when it is done, returns a [`ModalFlow`]
//! describing what should happen to the stack. `App` never learns what any
//! individual modal is for — it only pushes, pops and draws. Adding a popup
//! therefore means adding a `Modal` implementation, not another field and
//! another match arm on `App`.

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    Frame,
};

use crate::{cli, keymap::InputMode};

use super::{Action, EventState, HelpEntry, RepositoriesComponent, WorktreesComponent};

/// The state that outlives any single popup, lent to a modal while it runs.
///
/// Anything a modal needs *only for itself* lives in the modal. What is here is
/// shared with the base worktree pane, or handed on to the next step of a flow.
pub struct AppContext<'a> {
    pub worktrees: &'a mut WorktreesComponent,
    pub repositories: &'a mut RepositoriesComponent,
    pub args: &'a cli::Args,
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

pub trait Modal {
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
