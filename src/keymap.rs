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
        // Two keys rather than one, because the two cost wildly different
        // amounts: 'r' re-reads what shanti already knows about (disk only,
        // proportional to the repositories on screen), while 'R' walks the
        // repos dirs again from scratch — the startup cost, paid again. Putting
        // both behind one key would either make the common case slow or make
        // the rare one unreachable.
        (KeyCode::Char('r'), KeyModifiers::NONE) => Some(Action::Refresh),
        (KeyCode::Char('R'), KeyModifiers::NONE) | (KeyCode::Char('R'), KeyModifiers::SHIFT) => {
            Some(Action::Rescan)
        }
        (KeyCode::Char('f'), KeyModifiers::NONE) => Some(Action::FetchSelected),
        (KeyCode::Esc, KeyModifiers::NONE) => Some(Action::ClosePopup),
        (KeyCode::Char('/'), KeyModifiers::NONE) | (KeyCode::Char('i'), KeyModifiers::NONE) => {
            Some(Action::EnterInsertMode)
        }
        (KeyCode::Tab, KeyModifiers::NONE) => Some(Action::FocusNext),
        (KeyCode::Char('?'), KeyModifiers::NONE) | (KeyCode::F(1), KeyModifiers::NONE) => {
            Some(Action::ShowHelp)
        }
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
        // Insert mode's help key. It cannot be '?', which is a literal character
        // here — and a legitimate one inside a PR URL's query string. F1 is the
        // one key every terminal already agrees means "help", it carries no
        // modifier a text field could want, and nothing else in this table
        // claims it, so it is safe to bind in both modes: help then answers to
        // the same key everywhere, whether or not a text field has focus.
        KeyCode::F(1) => Some(Action::ShowHelp),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn f1_opens_help_in_both_modes() {
        assert_eq!(
            resolve(InputMode::Normal, key(KeyCode::F(1))),
            Some(Action::ShowHelp)
        );
        assert_eq!(
            resolve(InputMode::Insert, key(KeyCode::F(1))),
            Some(Action::ShowHelp)
        );
    }

    /// The cheap refresh and the expensive rescan are one shifted key apart, and
    /// must not resolve to the same action however the terminal reports the
    /// shift.
    #[test]
    fn refresh_and_rescan_are_different_keys() {
        assert_eq!(
            resolve(InputMode::Normal, key(KeyCode::Char('r'))),
            Some(Action::Refresh)
        );
        assert_eq!(
            resolve(InputMode::Normal, key(KeyCode::Char('R'))),
            Some(Action::Rescan)
        );
        assert_eq!(
            resolve(
                InputMode::Normal,
                KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT)
            ),
            Some(Action::Rescan),
            "a terminal that reports the shift must not lose the binding"
        );
        assert_eq!(
            resolve(InputMode::Normal, key(KeyCode::Char('f'))),
            Some(Action::FetchSelected)
        );
    }

    /// None of the three may steal a character from a filter being typed.
    #[test]
    fn refresh_and_fetch_stay_literal_in_insert_mode() {
        for c in ['r', 'R', 'f'] {
            assert_eq!(
                resolve(InputMode::Insert, key(KeyCode::Char(c))),
                Some(Action::InsertChar(c)),
                "{c} was stolen from the filter"
            );
        }
    }

    /// Why Insert mode needs a help key of its own: `?` belongs to the text
    /// field there — it is a legitimate character inside a PR URL.
    #[test]
    fn question_mark_stays_a_literal_in_insert_mode() {
        assert_eq!(
            resolve(InputMode::Insert, key(KeyCode::Char('?'))),
            Some(Action::InsertChar('?'))
        );
        assert_eq!(
            resolve(InputMode::Normal, key(KeyCode::Char('?'))),
            Some(Action::ShowHelp)
        );
    }
}
