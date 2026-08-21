use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::components::Action;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
    Normal,
    Insert,
}

pub fn resolve(mode: InputMode, key: KeyEvent) -> Option<Action> {
    match mode {
        InputMode::Normal => resolve_normal(key),
        InputMode::Insert => resolve_insert(key),
    }
}

fn resolve_normal(key: KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, KeyModifiers::NONE) => {
            Some(Action::MoveDown)
        }
        (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, KeyModifiers::NONE) => {
            Some(Action::MoveUp)
        }
        (KeyCode::Char('g'), KeyModifiers::NONE) | (KeyCode::Home, KeyModifiers::NONE) => {
            Some(Action::GoFirst)
        }
        (KeyCode::Char('G'), KeyModifiers::NONE)
        | (KeyCode::Char('G'), KeyModifiers::SHIFT)
        | (KeyCode::End, KeyModifiers::NONE) => Some(Action::GoLast),
        (KeyCode::Enter, KeyModifiers::NONE) => Some(Action::Select),
        // Deletion is intentionally only reachable through 'd' (confirmed) and
        // 'D' (forced), both documented in the help popup. A bare 'x' used to
        // delete without confirmation and appeared in no help list.
        (KeyCode::Char('d'), KeyModifiers::NONE) => Some(Action::DeleteWithConfirmation),
        (KeyCode::Char('D'), KeyModifiers::NONE) | (KeyCode::Char('D'), KeyModifiers::SHIFT) => {
            Some(Action::ForceDelete)
        }
        (KeyCode::Char('n'), KeyModifiers::NONE) => Some(Action::OpenRepositories),
        (KeyCode::Char('p'), KeyModifiers::NONE) => Some(Action::OpenPrWorktree),
        (KeyCode::Char('P'), KeyModifiers::NONE) | (KeyCode::Char('P'), KeyModifiers::SHIFT) => {
            Some(Action::OpenPrWorktreeAutoClone)
        }
        (KeyCode::Esc, KeyModifiers::NONE) => Some(Action::ClosePopup),
        (KeyCode::Char('/'), KeyModifiers::NONE) | (KeyCode::Char('i'), KeyModifiers::NONE) => {
            Some(Action::EnterInsertMode)
        }
        (KeyCode::Tab, KeyModifiers::NONE) => Some(Action::FocusNext),
        (KeyCode::Char('?'), KeyModifiers::NONE) => Some(Action::ShowHelp),
        (KeyCode::Char('q'), KeyModifiers::NONE) => Some(Action::Quit),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(Action::Quit),
        _ => None,
    }
}

fn resolve_insert(key: KeyEvent) -> Option<Action> {
    if key.modifiers == KeyModifiers::CONTROL {
        return match key.code {
            KeyCode::Char('c') => Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Char('n') => Some(Action::MoveDown),
            KeyCode::Char('k') | KeyCode::Char('p') => Some(Action::MoveUp),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Esc => Some(Action::ExitInsertMode),
        KeyCode::Enter => Some(Action::Select),
        KeyCode::Tab => Some(Action::FocusNext),
        KeyCode::Backspace => Some(Action::DeleteChar),
        KeyCode::Char(c) => Some(Action::InsertChar(c)),
        KeyCode::Down => Some(Action::MoveDown),
        KeyCode::Up => Some(Action::MoveUp),
        _ => None,
    }
}
