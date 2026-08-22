use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};

use super::list::ItemOrder;
use crate::vcs::{BoxedVcs, Space, Vcs};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{
        palette::tailwind::{GREEN, RED, SLATE},
        Style,
    },
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, List, ListDirection, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, StatefulWidget,
    },
    Frame,
};

use super::{
    centered,
    create_worktree::CreateWorktreeComponent,
    filter::FilterComponent,
    list::{Focus, ListComponent},
    worktrees::SpaceEntry,
    Action, AppContext, EventState, HelpEntry, Modal, ModalFlow, SELECTED_STYLE,
};
use crate::keymap::InputMode;
use tracing::error;

/// The repositories shanti found, each behind the backend that drives it.
///
/// Storing [`BoxedVcs`] rather than a concrete backend is what makes the list
/// heterogeneous: git and jj repositories sit side by side and nothing above
/// this type asks which is which.
pub struct RepositoriesComponent {
    repositories: Vec<BoxedVcs>,
    filter: FilterComponent,
    state: ListState,
    selected_index: Option<usize>,
    focus: Focus,
}

impl RepositoriesComponent {
    pub fn new(repositories: Vec<BoxedVcs>) -> Self {
        Self {
            repositories,
            filter: FilterComponent::new(),
            state: ListState::default().with_selected(Some(0)),
            selected_index: Some(0),
            focus: Focus::Filter,
        }
    }

    pub fn draw(&mut self, f: &mut Frame, rect: Rect, mode: InputMode) {
        f.render_widget(Clear, rect);

        let total = self.filtered_items().len();
        let title = {
            let mut spans = vec![
                Span::raw(" "),
                Span::styled("Repositories", Style::new().fg(GREEN.c400).bold()),
                Span::styled(format!(" ({}) ", total), Style::new().fg(SLATE.c400)),
            ];
            if !self.filter.value.is_empty() && matches!(mode, InputMode::Normal) {
                spans.push(Span::styled(
                    format!("/{} ", self.filter.value),
                    Style::new().fg(SLATE.c500),
                ));
            }
            Line::from(spans).alignment(Alignment::Center)
        };

        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(super::POPUP_BORDER_STYLE)
            .title(title);
        if matches!(mode, InputMode::Normal) {
            block = block.title_bottom(repos_keybinding_hint());
        }

        let inner_area = block.inner(rect);
        f.render_widget(block, rect);

        let in_filter = matches!(mode, InputMode::Insert) && matches!(self.focus, Focus::Filter);

        let list_area = if in_filter {
            let [filter_line, sep_line, list_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .areas(inner_area);

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" / ", Style::new().fg(GREEN.c300).bold()),
                    Span::styled(self.filter.value.clone(), Style::new().white()),
                ])),
                filter_line,
            );
            f.set_cursor_position((
                filter_line.x + 3 + self.filter.cursor_pos() as u16,
                filter_line.y,
            ));
            f.render_widget(
                Paragraph::new("─".repeat(sep_line.width as usize))
                    .style(Style::new().fg(SLATE.c700)),
                sep_line,
            );
            list_area
        } else {
            inner_area
        };

        let items: Vec<ListItem> = self
            .filtered_items()
            .iter()
            .map(|r| ListItem::new(r.repo().name.clone()))
            .collect();
        let list = List::new(items)
            .style(Style::new().white())
            .highlight_style(SELECTED_STYLE)
            .direction(ListDirection::TopToBottom);
        StatefulWidget::render(list, list_area, f.buffer_mut(), &mut self.state);

        let mut scroll_state = ScrollbarState::new(total).position(self.state.offset());
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(Style::new().dark_gray())
            .track_style(Style::new().dark_gray());
        f.render_stateful_widget(scrollbar, list_area, &mut scroll_state);
    }

    pub fn handle_action(&mut self, action: Action) -> EventState {
        match action {
            Action::MoveDown => {
                self.select(ItemOrder::Next);
                EventState::Consumed
            }
            Action::MoveUp => {
                self.select(ItemOrder::Previous);
                EventState::Consumed
            }
            Action::GoFirst => {
                self.select(ItemOrder::First);
                EventState::Consumed
            }
            Action::GoLast => {
                self.select(ItemOrder::Last);
                EventState::Consumed
            }
            Action::InsertChar(c) => {
                self.filter.enter_char(c);
                self.select(ItemOrder::First);
                EventState::Consumed
            }
            Action::DeleteChar => {
                self.filter.delete_char();
                self.select(ItemOrder::First);
                EventState::Consumed
            }
            _ => EventState::NotConsumed,
        }
    }

    pub fn focus_filter(&mut self) {
        self.focus = Focus::Filter;
    }

    pub fn focus_list(&mut self) {
        self.focus = Focus::List;
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Filter => Focus::List,
            Focus::List => Focus::Filter,
        };
    }

    pub fn is_filter_focused(&self) -> bool {
        matches!(self.focus, Focus::Filter)
    }

    /// Clears any active filter, finds the repository by name, and selects it.
    /// Returns `true` if found, `false` otherwise.
    pub fn select_repository_by_name(&mut self, name: &str) -> bool {
        self.filter.clear();
        let index = self
            .filtered_items()
            .iter()
            .position(|r| r.repo().name == name);
        if let Some(idx) = index {
            self.selected_index = Some(idx);
            self.state.select(Some(idx));
            true
        } else {
            false
        }
    }

    pub fn add_repository(&mut self, repo: BoxedVcs) {
        self.repositories.push(repo);
    }

    pub fn selected_repository(&mut self) -> Option<&dyn Vcs> {
        let index = self.selected_index?;
        // Copy the borrow out of the temporary Vec: its elements already point
        // into `self`, so the `&dyn Vcs` outlives the filtered list itself.
        let selected: &BoxedVcs = *self.filtered_items().get(index)?;
        Some(selected.as_ref())
    }

    /// The backend that owns `space`, if it is still open.
    ///
    /// A [`Space`] is an inert snapshot; every action on one — deleting it above
    /// all — has to go back through the repository it came from, and this list is
    /// the only place those live.
    pub fn backend_for(&self, space: &Space) -> Option<&dyn Vcs> {
        self.repositories
            .iter()
            .find(|backend| backend.repo().id == space.repo)
            .map(|backend| backend.as_ref())
    }

    /// Every space of every repository, and the names of the repositories that
    /// could not be asked.
    ///
    /// Listing is per repository and each one can fail on its own (an unreadable
    /// worktree registration, a `jj` that will not run), so a failure is reported
    /// alongside the spaces that *did* list rather than replacing them.
    pub fn collect_spaces(&self) -> (Vec<SpaceEntry>, Vec<String>) {
        let mut spaces = Vec::new();
        let mut failed = Vec::new();
        for backend in &self.repositories {
            let repo_name = &backend.repo().name;
            match backend.spaces() {
                Ok(found) => spaces.extend(found.into_iter().map(|space| SpaceEntry {
                    repo_name: repo_name.clone(),
                    space,
                })),
                Err(error) => {
                    error!(repo = %repo_name, %error, "could not list the spaces");
                    failed.push(repo_name.clone());
                }
            }
        }
        (spaces, failed)
    }
}

fn repos_keybinding_hint() -> Line<'static> {
    Line::from(vec![
        Span::styled("[Enter] ", Style::new().fg(GREEN.c400).bold()),
        Span::styled("select", Style::new().fg(SLATE.c500)),
        Span::styled("  [Esc] ", Style::new().fg(RED.c400).bold()),
        Span::styled("close ", Style::new().fg(SLATE.c500)),
    ])
    .right_aligned()
}

impl ListComponent<BoxedVcs> for RepositoriesComponent {
    fn filtered_items(&mut self) -> Vec<&BoxedVcs> {
        let query = self.filter.value.as_str();
        if query.is_empty() {
            let mut items: Vec<&BoxedVcs> = self.repositories.iter().collect();
            items.sort_by(|a, b| a.repo().name.cmp(&b.repo().name));
            return items;
        }
        let mut matcher = Matcher::new(Config::DEFAULT);
        // Pair each word with its per-word minimum score threshold.
        // Short words (1-2 chars) have low scores due to gap penalties on
        // longer haystacks, so we accept any match for them.
        let patterns: Vec<(Pattern, u32)> = query
            .split_whitespace()
            .map(|w| {
                let min = if w.len() >= 3 { 70 } else { 1 };
                (
                    Pattern::parse(w, CaseMatching::Ignore, Normalization::Smart),
                    min,
                )
            })
            .collect();
        let mut buf = Vec::new();
        let mut scored: Vec<(&BoxedVcs, u32)> = self
            .repositories
            .iter()
            .filter_map(|r| {
                let name = &r.repo().name;
                let mut total = 0u32;
                for (pattern, min_score) in &patterns {
                    match pattern.score(Utf32Str::new(name, &mut buf), &mut matcher) {
                        Some(s) if s >= *min_score => total += s,
                        _ => return None,
                    }
                }
                Some((r, total))
            })
            .collect();
        // Highest fuzzy score first; sort_by_key keeps the stable order of equal scores.
        scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        scored.into_iter().map(|(r, _)| r).collect()
    }

    fn get_state(&mut self) -> &mut ListState {
        &mut self.state
    }

    fn update_selected_index(&mut self, index: usize) {
        self.selected_index = Some(index);
    }
}

/// The repository picker.
///
/// The repository list itself is long-lived shared state (the PR clone flow adds
/// to it, and the create-worktree step reads the selection back), so it stays in
/// [`AppContext`]; the modal owns only what belongs to this popup — its mode.
pub struct RepositoriesModal {
    mode: InputMode,
}

impl RepositoriesModal {
    pub fn new() -> Self {
        Self {
            mode: InputMode::Normal,
        }
    }
}

impl Default for RepositoriesModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for RepositoriesModal {
    fn area(&self, full: Rect) -> Rect {
        centered(full, Constraint::Percentage(50), Constraint::Percentage(50))
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &mut AppContext) {
        ctx.repositories.draw(frame, area, self.mode);
    }

    fn mode(&self) -> InputMode {
        self.mode
    }

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select => {
                let repo_name = ctx
                    .repositories
                    .selected_repository()
                    .map(|r| r.repo().name.clone())
                    .unwrap_or_default();
                // Replace, not stack: cancelling the name prompt returns to the
                // worktree list, it does not re-open the picker.
                ModalFlow::Replace(Box::new(CreateWorktreeComponent::new(repo_name)))
            }
            Action::ClosePopup => ModalFlow::Close,
            Action::EnterInsertMode => {
                self.mode = InputMode::Insert;
                ctx.repositories.focus_filter();
                ModalFlow::Consumed
            }
            Action::ExitInsertMode => {
                self.mode = InputMode::Normal;
                ctx.repositories.focus_list();
                ModalFlow::Consumed
            }
            Action::FocusNext => {
                ctx.repositories.toggle_focus();
                self.mode = if ctx.repositories.is_filter_focused() {
                    InputMode::Insert
                } else {
                    InputMode::Normal
                };
                ModalFlow::Consumed
            }
            _ => ctx.repositories.handle_action(action).into(),
        }
    }

    fn help(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry::Section("Keybindings"),
            HelpEntry::Binding("j / ↓", "Move down"),
            HelpEntry::Binding("k / ↑", "Move up"),
            HelpEntry::Binding("g / Home", "Go to first"),
            HelpEntry::Binding("G / End", "Go to last"),
            HelpEntry::Binding("i", "Enter filter mode"),
            HelpEntry::Binding("Tab", "Toggle filter / list"),
            HelpEntry::Binding("Enter", "Select repository"),
            HelpEntry::Binding("?", "Show this help"),
            HelpEntry::Binding("Esc", "Close popup"),
            HelpEntry::Binding("q / Ctrl+C", "Quit"),
        ]
    }
}
