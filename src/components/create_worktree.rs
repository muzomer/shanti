use crate::keymap::InputMode;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget},
    Frame,
};

use super::{centered, Action, AppContext, EventState, HelpEntry, Modal, ModalFlow};
use crate::theme;
use crate::vcs::Backend;

/// Title case for a backend's own word for a space ("worktree" -> "Worktree").
fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub struct CreateWorktreeComponent {
    character_index: usize,
    pub new_worktree_name: String,
    repo_name: String,
    /// The backend this prompt will create through.
    ///
    /// Carried rather than looked up at draw time because it is what the prompt
    /// *promises*: on a colocated repository "new space" is ambiguous, and a
    /// prompt that says "worktree" while creating a jj workspace would be a lie
    /// the user only discovers afterwards.
    backend: Backend,
    /// Whether the repository is driven by more than one backend. Purely so the
    /// prompt can explain why it picked one, instead of silently choosing.
    colocated: bool,
    pub base_branch_hint: Option<String>,
    pub warning: Option<String>,
}

impl CreateWorktreeComponent {
    pub fn new(repo_name: String, backend: Backend, colocated: bool) -> Self {
        Self {
            character_index: 0,
            new_worktree_name: String::new(),
            repo_name,
            backend,
            colocated,
            base_branch_hint: None,
            warning: None,
        }
    }

    pub fn new_with_branch(
        repo_name: String,
        backend: Backend,
        colocated: bool,
        branch_name: String,
        warning: Option<String>,
    ) -> Self {
        let character_index = branch_name.chars().count();
        Self {
            character_index,
            new_worktree_name: branch_name,
            repo_name,
            backend,
            colocated,
            base_branch_hint: None,
            warning,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);

        let input_border_style =
            if self.new_worktree_name.is_empty() || is_valid_branch_name(&self.new_worktree_name) {
                theme::BORDER_INPUT_FOCUSED
            } else {
                Style::new().fg(theme::DANGER)
            };

        let outer_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::BORDER_FOCUSED)
            .style(theme::POPUP_SURFACE)
            // Titled in the vocabulary of the backend that will do the work, so
            // a jj user is not offered a "worktree" they will never see.
            .title(
                Line::from(format!(" New {} ", capitalised(self.backend.space_noun())))
                    .style(theme::TITLE),
            )
            .title_top(
                Line::from(format!(" repo: {} · {} ", self.repo_name, self.backend))
                    .style(theme::SECONDARY)
                    .right_aligned(),
            )
            .title_bottom(keybinding_hint());

        let inner_area = outer_block.inner(area);
        outer_block.render(area, frame.buffer_mut());

        let [_, label_area, input_area, hint_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .horizontal_margin(4)
        .areas(inner_area);

        Paragraph::new(match self.backend {
            Backend::Git => "Branch name:",
            Backend::Jj => "Bookmark name:",
        })
        .style(theme::SECONDARY)
        .render(label_area, frame.buffer_mut());

        // A colocated repository could take either backend, so the default is
        // stated out loud rather than left for the user to discover in the list.
        if self.colocated {
            Paragraph::new(format!(
                "colocated repo — creating a {} {} ",
                self.backend,
                self.backend.space_noun()
            ))
            .style(Style::new().fg(theme::WARNING))
            .right_aligned()
            .render(label_area, frame.buffer_mut());
        }

        Paragraph::new(self.new_worktree_name.as_str())
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(input_border_style)
                    .padding(Padding::horizontal(1)),
            )
            .render(input_area, frame.buffer_mut());

        if let Some(warning) = &self.warning {
            Paragraph::new(warning.as_str())
                .style(Style::new().fg(theme::WARNING))
                .render(hint_area, frame.buffer_mut());
        } else if let Some(hint) = &self.base_branch_hint {
            Paragraph::new(hint.as_str())
                .style(theme::SECONDARY)
                .render(hint_area, frame.buffer_mut());
        }

        // input_area: border(1) + padding(1) = offset 2; y+1 skips top border row
        frame.set_cursor_position((
            input_area.x + 2 + self.character_index as u16,
            input_area.y + 1,
        ));
    }

    pub fn handle_action(&mut self, action: Action) -> EventState {
        match action {
            Action::InsertChar(c) => {
                self.enter_char(c);
                EventState::Consumed
            }
            Action::DeleteChar => {
                self.delete_char();
                EventState::Consumed
            }
            _ => EventState::NotConsumed,
        }
    }

    fn enter_char(&mut self, new_char: char) {
        let ch = if new_char == ' ' {
            '-'
        } else if is_valid_branch_char(new_char) {
            new_char
        } else {
            return;
        };
        let index = self.byte_index();
        self.new_worktree_name.insert(index, ch);
        self.move_cursor_right();
    }

    fn delete_char(&mut self) {
        let is_not_cursor_leftmost = self.character_index != 0;
        if is_not_cursor_leftmost {
            let current_index = self.character_index;
            let from_left_to_current_index = current_index - 1;
            let before_char_to_delete = self
                .new_worktree_name
                .chars()
                .take(from_left_to_current_index);
            let after_char_to_delete = self.new_worktree_name.chars().skip(current_index);
            self.new_worktree_name = before_char_to_delete.chain(after_char_to_delete).collect();
            self.move_cursor_left();
        }
    }

    fn byte_index(&self) -> usize {
        self.new_worktree_name
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.character_index)
            .unwrap_or(self.new_worktree_name.len())
    }

    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.character_index.saturating_add(1);
        self.character_index = self.clamp_cursor(cursor_moved_right);
    }

    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.character_index.saturating_sub(1);
        self.character_index = self.clamp_cursor(cursor_moved_left);
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.new_worktree_name.chars().count())
    }
}

impl Modal for CreateWorktreeComponent {
    fn area(&self, full: Rect) -> Rect {
        centered(full, Constraint::Percentage(55), Constraint::Length(9))
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        CreateWorktreeComponent::draw(self, frame, area);
    }

    fn mode(&self) -> InputMode {
        InputMode::Insert
    }

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select => {
                if !self.new_worktree_name.is_empty() {
                    match ctx.create_space(&self.new_worktree_name) {
                        Ok(()) => ctx.worktrees.last_error = None,
                        Err(e) => ctx.worktrees.last_error = Some(format!("{:#}", e)),
                    }
                }
                ModalFlow::Close
            }
            Action::ClosePopup | Action::ExitInsertMode => ModalFlow::Close,
            _ => {
                let result = self.handle_action(action);
                if result == EventState::Consumed {
                    // The base branch is derived from the name, so it is
                    // recomputed on every accepted keystroke.
                    self.base_branch_hint = if self.new_worktree_name.is_empty() {
                        None
                    } else {
                        ctx.repositories
                            .selected_repository()
                            .map(|r| r.resolve_base(&self.new_worktree_name))
                    };
                }
                result.into()
            }
        }
    }

    fn help(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry::Section("Keybindings"),
            HelpEntry::Binding("Enter", "Create worktree"),
            HelpEntry::Binding("Esc", "Cancel"),
            HelpEntry::Binding("Backspace", "Delete character"),
            HelpEntry::Binding("Ctrl+C", "Quit"),
        ]
    }
}

fn keybinding_hint() -> Line<'static> {
    Line::from(vec![
        Span::styled("[Enter] ", theme::KEY),
        Span::styled("confirm", theme::MUTED),
        Span::styled("  [Esc] ", theme::KEY_DESTRUCTIVE),
        Span::styled("cancel ", theme::MUTED),
    ])
    .right_aligned()
}

fn is_valid_branch_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')
}

fn is_valid_branch_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.starts_with('-') || name.starts_with('.') {
        return false;
    }
    if name.ends_with('.') || name.ends_with('/') {
        return false;
    }
    if name.contains("..") || name.contains("@{") {
        return false;
    }
    true
}
