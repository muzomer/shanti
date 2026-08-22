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
use ratatui::style::{
    palette::tailwind::{BLUE, GREEN, SLATE},
    Modifier, Style,
};
pub use repositories::{RepositoriesComponent, RepositoriesModal};
pub use worktrees::WorktreesComponent;

/// Selected item: blue bg matching lazygit's selectedLineBgColor.
const SELECTED_STYLE: Style = Style::new().bg(BLUE.c800).add_modifier(Modifier::BOLD);
/// Muted border for the main panel — lazygit inactiveBorderColor is terminal default.
const BORDER_STYLE: Style = Style::new().fg(SLATE.c500);
/// Green bold border for popups — lazygit activeBorderColor is [green, bold].
const POPUP_BORDER_STYLE: Style = Style::new().fg(GREEN.c400).add_modifier(Modifier::BOLD);
/// Brighter green bold border for focused inputs — one step lighter than popup border.
const ACTIVE_BORDER_STYLE: Style = Style::new().fg(GREEN.c300).add_modifier(Modifier::BOLD);

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
