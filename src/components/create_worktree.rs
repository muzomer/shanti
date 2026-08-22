use crate::keymap::InputMode;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{
        palette::tailwind::{AMBER, GREEN, RED, SLATE},
        Color, Style,
    },
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget},
    Frame,
};

use super::{centered, Action, AppContext, EventState, HelpEntry, Modal, ModalFlow};

pub struct CreateWorktreeComponent {
    character_index: usize,
    pub new_worktree_name: String,
    repo_name: String,
    pub base_branch_hint: Option<String>,
    pub warning: Option<String>,
}

impl CreateWorktreeComponent {
    pub fn new(repo_name: String) -> Self {
        Self {
            character_index: 0,
            new_worktree_name: String::new(),
            repo_name,
            base_branch_hint: None,
            warning: None,
        }
    }

    pub fn new_with_branch(
        repo_name: String,
        branch_name: String,
        warning: Option<String>,
    ) -> Self {
        let character_index = branch_name.chars().count();
        Self {
            character_index,
            new_worktree_name: branch_name,
            repo_name,
            base_branch_hint: None,
            warning,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);

        let input_border_style =
            if self.new_worktree_name.is_empty() || is_valid_branch_name(&self.new_worktree_name) {
                super::ACTIVE_BORDER_STYLE
            } else {
                Style::new().fg(Color::Red)
            };

        let outer_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(super::POPUP_BORDER_STYLE)
            .title(Line::from(" New Worktree ").style(Style::new().fg(GREEN.c300).bold()))
            .title_top(
                Line::from(format!(" repo: {} ", self.repo_name))
                    .style(Style::new().fg(SLATE.c400))
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

        Paragraph::new("Branch name:")
            .style(Style::new().fg(SLATE.c300))
            .render(label_area, frame.buffer_mut());

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
                .style(Style::new().fg(AMBER.c300))
                .render(hint_area, frame.buffer_mut());
        } else if let Some(hint) = &self.base_branch_hint {
            Paragraph::new(hint.as_str())
                .style(Style::new().fg(SLATE.c400))
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
                    if let Some(repository) = ctx.repositories.selected_repository() {
                        match repository
                            .create_new_worktree(&self.new_worktree_name, &ctx.args.worktrees_dir)
                        {
                            Ok(created) => {
                                ctx.worktrees.last_error = None;
                                ctx.worktrees.add(created);
                            }
                            Err(e) => ctx.worktrees.last_error = Some(format!("{:#}", e)),
                        }
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
        Span::styled("[Enter] ", Style::new().fg(GREEN.c400).bold()),
        Span::styled("confirm", Style::new().fg(SLATE.c500)),
        Span::styled("  [Esc] ", Style::new().fg(RED.c400).bold()),
        Span::styled("cancel ", Style::new().fg(SLATE.c500)),
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
