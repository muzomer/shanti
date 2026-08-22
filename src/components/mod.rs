mod confirm;
mod create_worktree;
mod filter;
mod help;
mod list;
mod modal;
mod pr_worktree;
mod repositories;
mod select_directory;
mod worktrees;

pub use confirm::ConfirmComponent;
pub use help::{worktrees_bindings, HelpComponent, HelpEntry};
pub use modal::{centered, AppContext, ConfirmCallback, Modal, ModalFlow, SelectCallback};
pub use pr_worktree::PrWorktreeComponent;
pub use repositories::{RepositoriesComponent, RepositoriesModal};
pub use worktrees::WorktreesComponent;

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
    Delete,
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
