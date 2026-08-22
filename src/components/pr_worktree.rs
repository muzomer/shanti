use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{
        palette::tailwind::{GREEN, RED, SLATE},
        Style,
    },
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget},
    Frame,
};

use super::{
    centered, create_worktree::CreateWorktreeComponent, select_directory::SelectDirectoryComponent,
    Action, AppContext, ConfirmCallback, ConfirmComponent, EventState, HelpEntry, Modal, ModalFlow,
    SelectCallback,
};
use crate::{
    github::{self, PrFetcher},
    keymap::InputMode,
    vcs,
};

pub struct PrWorktreeComponent {
    character_index: usize,
    pub input: String,
    pub error: Option<String>,
    pub auto_clone: bool,
    /// Where PR data comes from. Injected so the steps that only exist after a
    /// successful fetch can be exercised without talking to GitHub.
    fetch: PrFetcher,
}

impl PrWorktreeComponent {
    /// `auto_clone` skips both the "clone this repo?" prompt and the branch-name
    /// prompt, going straight from a PR URL to a created worktree.
    pub fn new(auto_clone: bool, fetch: PrFetcher) -> Self {
        Self {
            character_index: 0,
            input: String::new(),
            error: None,
            auto_clone,
            fetch,
        }
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);

        let outer_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(super::POPUP_BORDER_STYLE)
            .title(Line::from(" Worktree from PR ").style(Style::new().fg(GREEN.c300).bold()))
            .title_bottom(keybinding_hint());

        let inner_area = outer_block.inner(area);
        outer_block.render(area, frame.buffer_mut());

        let [_, label_area, input_area, status_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .horizontal_margin(4)
        .areas(inner_area);

        Paragraph::new("GitHub PR URL:")
            .style(Style::new().fg(SLATE.c300))
            .render(label_area, frame.buffer_mut());

        Paragraph::new(self.input.as_str())
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(super::ACTIVE_BORDER_STYLE)
                    .padding(Padding::horizontal(1)),
            )
            .render(input_area, frame.buffer_mut());

        if let Some(err) = &self.error {
            Paragraph::new(err.as_str())
                .style(Style::new().fg(RED.c400))
                .render(status_area, frame.buffer_mut());
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

    fn enter_char(&mut self, c: char) {
        let index = self.byte_index();
        self.input.insert(index, c);
        self.move_cursor_right();
        self.error = None;
    }

    fn delete_char(&mut self) {
        if self.character_index != 0 {
            let current_index = self.character_index;
            let before = self.input.chars().take(current_index - 1);
            let after = self.input.chars().skip(current_index);
            self.input = before.chain(after).collect();
            self.move_cursor_left();
            self.error = None;
        }
    }

    fn byte_index(&self) -> usize {
        self.input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.character_index)
            .unwrap_or(self.input.len())
    }

    fn move_cursor_right(&mut self) {
        let moved = self.character_index.saturating_add(1);
        self.character_index = moved.clamp(0, self.input.chars().count());
    }

    fn move_cursor_left(&mut self) {
        let moved = self.character_index.saturating_sub(1);
        self.character_index = moved.clamp(0, self.input.chars().count());
    }
}

impl Modal for PrWorktreeComponent {
    fn area(&self, full: Rect) -> Rect {
        centered(full, Constraint::Percentage(70), Constraint::Length(9))
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        PrWorktreeComponent::draw(self, frame, area);
    }

    fn mode(&self) -> InputMode {
        InputMode::Insert
    }

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select => self.submit(ctx),
            Action::ClosePopup | Action::ExitInsertMode => ModalFlow::Close,
            _ => self.handle_action(action).into(),
        }
    }

    fn help(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry::Section("Keybindings"),
            HelpEntry::Binding("Enter", "Fetch PR and open worktree"),
            HelpEntry::Binding("Esc", "Cancel"),
            HelpEntry::Binding("Backspace", "Delete character"),
            HelpEntry::Binding("Ctrl+C", "Quit"),
        ]
    }
}

impl PrWorktreeComponent {
    /// A rejected URL keeps the prompt open with an error so it can be corrected.
    fn submit(&mut self, ctx: &mut AppContext) -> ModalFlow {
        let pr_url = match github::parse_pr_url(&self.input) {
            Ok(url) => url,
            Err(e) => {
                self.set_error(format!("{:#}", e));
                return ModalFlow::Consumed;
            }
        };

        let pr_info = match (self.fetch)(&pr_url) {
            Ok(info) => info,
            Err(e) => {
                self.set_error(format!("{:#}", e));
                return ModalFlow::Consumed;
            }
        };

        let auto = self.auto_clone;
        if ctx.repositories.select_repository_by_name(&pr_url.repo) {
            return open_worktree_for_pr(ctx, pr_info, auto);
        }

        if auto {
            return clone_flow(ctx, pr_url, pr_info, true);
        }

        let label = format!("Repository '{}' not found. Clone from GitHub?", pr_url.repo);
        let remote = format!("git@github.com:{}/{}.git", pr_url.owner, pr_url.repo);
        let on_confirm: ConfirmCallback =
            Box::new(move |ctx| clone_flow(ctx, pr_url, pr_info, false));
        ModalFlow::Replace(Box::new(ConfirmComponent::new(
            "Clone Repository".to_string(),
            label,
            remote,
            on_confirm,
        )))
    }
}

/// Cloning needs a destination: with several configured repo dirs the user picks
/// one, otherwise the single dir is used without asking.
fn clone_flow(
    ctx: &mut AppContext,
    pr_url: github::PrUrl,
    pr_info: github::PrInfo,
    auto: bool,
) -> ModalFlow {
    if ctx.args.repos_dirs.len() > 1 {
        let on_select: SelectCallback<String> =
            Box::new(move |ctx, dir| clone_into(ctx, dir, pr_url, pr_info, auto));
        return ModalFlow::Replace(Box::new(SelectDirectoryComponent::new(
            ctx.args.repos_dirs.clone(),
            on_select,
        )));
    }
    let dir = ctx.args.repos_dirs[0].clone();
    clone_into(ctx, dir, pr_url, pr_info, auto)
}

fn clone_into(
    ctx: &mut AppContext,
    repos_dir: String,
    pr_url: github::PrUrl,
    pr_info: github::PrInfo,
    auto: bool,
) -> ModalFlow {
    if let Err(e) = github::clone_repository(&pr_url.owner, &pr_url.repo, &repos_dir) {
        ctx.worktrees.last_error = Some(format!("{:#}", e));
        return ModalFlow::Close;
    }

    let repo_path = std::path::PathBuf::from(&repos_dir).join(&pr_url.repo);
    // Opened by layout rather than as "a git repo we just cloned": the clone may
    // land somewhere that is already colocated with jj, and one rule for picking
    // a backend is the whole point of the seam.
    match vcs::open_at(&repo_path, false) {
        Ok(repo) => {
            ctx.repositories.add_repository(repo);
            ctx.repositories.select_repository_by_name(&pr_url.repo);
        }
        Err(e) => {
            ctx.worktrees.last_error = Some(format!("Cloned but failed to load repo: {:#}", e));
            return ModalFlow::Close;
        }
    }

    open_worktree_for_pr(ctx, pr_info, auto)
}

/// Final step of the PR flow: select the existing worktree, create one outright
/// (auto mode), or hand over to the branch-name prompt.
fn open_worktree_for_pr(ctx: &mut AppContext, pr_info: github::PrInfo, auto: bool) -> ModalFlow {
    let branch = pr_info.branch_name.clone();

    if ctx.worktrees.select_worktree_by_branch(&branch) {
        if pr_info.is_merged {
            ctx.worktrees.last_error =
                Some("PR is merged — existing worktree selected".to_string());
        }
        return ModalFlow::Close;
    }

    let merged_warning = || "Warning: PR is merged, branch may be deleted on remote".to_string();

    if auto {
        match ctx.create_space(&branch) {
            Ok(()) => ctx.worktrees.last_error = pr_info.is_merged.then(merged_warning),
            Err(e) => ctx.worktrees.last_error = Some(format!("{:#}", e)),
        }
        return ModalFlow::Close;
    }

    let (repo_name, base_branch_hint) = match ctx.repositories.selected_repository() {
        Some(repo) => (repo.repo().name.clone(), Some(repo.resolve_base(&branch))),
        None => (String::new(), None),
    };

    let mut prompt = CreateWorktreeComponent::new_with_branch(
        repo_name,
        branch,
        pr_info.is_merged.then(merged_warning),
    );
    prompt.base_branch_hint = base_branch_hint;
    ModalFlow::Replace(Box::new(prompt))
}

fn keybinding_hint() -> Line<'static> {
    Line::from(vec![
        Span::styled("[Enter] ", Style::new().fg(GREEN.c400).bold()),
        Span::styled("open", Style::new().fg(SLATE.c500)),
        Span::styled("  [Esc] ", Style::new().fg(RED.c400).bold()),
        Span::styled("cancel ", Style::new().fg(SLATE.c500)),
    ])
    .right_aligned()
}
