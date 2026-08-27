use crate::keymap::InputMode;
use ratatui::{layout::Rect, Frame};

use super::{
    footer_entries, popup_area,
    prompt::{prompt_width, Prompt, PROMPT_HEIGHT},
    Action, AppContext, EventState, Extent, HelpEntry, Modal, ModalFlow, ModalKind, KEYS_SECTION,
};
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
    /// The pull request this prompt was opened for, when it was.
    ///
    /// Carried through the prompt rather than recorded when the flow opened it,
    /// because the space does not exist until the user presses Enter here — and
    /// a PR the user then abandoned must not be remembered against a space that
    /// was never made.
    pub pr_url: Option<String>,
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
            pr_url: None,
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
            pr_url: None,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        // A name that is merely unfinished is not an error: only an outright
        // invalid one turns the box red.
        let valid =
            self.new_worktree_name.is_empty() || is_valid_branch_name(&self.new_worktree_name);

        // A colocated repository could take either backend, so the prompt says
        // out loud which one it picked rather than leaving the user to discover
        // it in the list afterwards.
        let aside = self.colocated.then(|| {
            (
                // Kept short so it survives the fit check on a narrow popup: this
                // is the one thing about a colocated repository the user cannot
                // work out from the rest of the prompt. `Tab` switches which
                // backend the space is made through.
                format!("colocated → {} · Tab to switch", self.backend.space_noun()),
                theme::warning_text(),
            )
        });

        // A warning about the branch outranks a note about where it will start.
        let status = match (&self.warning, &self.base_branch_hint) {
            (Some(warning), _) => Some((warning.clone(), theme::warning_text())),
            (None, Some(hint)) => Some((hint.clone(), theme::secondary())),
            (None, None) => None,
        };

        // Titled in the vocabulary of the backend that will do the work, so a jj
        // user is not offered a "worktree" they will never see.
        let title = format!("New {}", capitalised(self.backend.space_noun()));

        Prompt {
            title: &title,
            context: Some(format!("{} · {}", self.repo_name, self.backend)),
            label: match self.backend {
                Backend::Git => "Branch name",
                Backend::Jj => "Bookmark name",
            },
            aside,
            value: &self.new_worktree_name,
            placeholder: "type a name…",
            cursor: self.character_index,
            valid,
            status,
            // Read off the same table the help popup shows, so the prompt cannot
            // promise one thing here and another there.
            footer: footer_entries(&self.help(), KEYS_SECTION),
        }
        .render(frame, area);
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
    fn kind(&self) -> ModalKind {
        ModalKind::CreateWorktree
    }

    fn area(&self, full: Rect) -> Rect {
        // A branch name is short; the floor is set by the base-branch sentence
        // beneath it, which is the longest thing this popup ever says.
        popup_area(full, prompt_width(55, 36, 90), Extent::fixed(PROMPT_HEIGHT))
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
                    match ctx.create_space_via(&self.new_worktree_name, self.backend) {
                        // The new row is the success message; all that is left
                        // to do is retire a failure the user has since fixed.
                        Ok(path) => {
                            if let Some(url) = &self.pr_url {
                                ctx.remember_pr(&path, url);
                            }
                            ctx.notify.clear()
                        }
                        Err(e) => ctx.notify.error(format!("{:#}", e)),
                    }
                }
                ModalFlow::Close
            }
            Action::ClosePopup | Action::ExitInsertMode => ModalFlow::Close,
            // Tab switches the target backend, but only where there is a choice:
            // a colocated repository can take a git worktree or a jj workspace.
            // The base branch is backend-specific, so it is recomputed here.
            Action::FocusNext if self.colocated => {
                self.backend = match self.backend {
                    Backend::Jj => Backend::Git,
                    Backend::Git => Backend::Jj,
                };
                self.base_branch_hint = (!self.new_worktree_name.is_empty())
                    .then(|| {
                        ctx.repositories
                            .selected_backend(self.backend)
                            .map(|r| r.resolve_base(&self.new_worktree_name))
                    })
                    .flatten();
                ModalFlow::Consumed
            }
            _ => {
                let result = self.handle_action(action);
                if result == EventState::Consumed {
                    // The base branch is derived from the name, so it is
                    // recomputed on every accepted keystroke.
                    self.base_branch_hint = if self.new_worktree_name.is_empty() {
                        None
                    } else {
                        ctx.repositories
                            .selected_backend(self.backend)
                            .map(|r| r.resolve_base(&self.new_worktree_name))
                    };
                }
                result.into()
            }
        }
    }

    fn help(&self) -> Vec<HelpEntry> {
        let mut entries = vec![
            HelpEntry::Section(KEYS_SECTION),
            HelpEntry::bind("Enter", "Create worktree").hint("Enter", "create"),
            HelpEntry::bind("F1", "Show this help")
                .hint("F1", "help")
                .aside(),
            HelpEntry::bind("Esc", "Cancel")
                .hint("Esc", "cancel")
                .safe()
                .essential(),
            HelpEntry::bind("Backspace", "Delete character"),
            HelpEntry::bind("Ctrl+C", "Quit"),
        ];
        // Only a colocated repository has a backend to switch between.
        if self.colocated {
            entries
                .push(HelpEntry::bind("Tab", "Switch backend (git / jj)").hint("Tab", "backend"));
        }
        entries
    }
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
