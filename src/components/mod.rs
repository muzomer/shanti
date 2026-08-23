mod confirm;
mod create_worktree;
mod filter;
mod help;
mod list;
mod modal;
mod pr_worktree;
mod prompt;
mod repositories;
mod select_directory;
mod worktrees;

pub use confirm::ConfirmComponent;
pub use help::{
    footer_entries, worktrees_bindings, HelpComponent, HelpEntry, FILTER_SECTION, KEYS_SECTION,
};
pub use modal::{
    fits, gutter, place_cursor, popup_area, AppContext, ConfirmCallback, Extent, Modal, ModalFlow,
    SelectCallback, MIN_HEIGHT, MIN_WIDTH,
};
pub use pr_worktree::{resume_pr_flow, PrCommand, PrRequests, PrStep, PrWorktreeComponent};
pub use repositories::{spaces_of, RepositoriesComponent, RepositoriesModal};
pub use worktrees::{SpaceEntry, WorktreesComponent};

// Components carry no palette of their own: every colour they draw comes from
// `crate::theme`, by the meaning it has rather than by its hue.

#[derive(PartialEq, Debug)]
pub enum EventState {
    Consumed,
    NotConsumed,
    Exit,
}

#[derive(PartialEq, Debug)]
pub enum Action {
    MoveDown,
    MoveUp,
    GoFirst,
    GoLast,
    Select,
    DeleteWithConfirmation,
    ForceDelete,
    OpenRepositories,
    OpenPrWorktree,
    OpenPrWorktreeAutoClone,
    ClosePopup,
    EnterInsertMode,
    ExitInsertMode,
    InsertChar(char),
    DeleteChar,
    FocusNext,
    ShowHelp,
    Quit,
}
