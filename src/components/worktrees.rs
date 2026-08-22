use color_eyre::eyre::{self, eyre};
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, StatefulWidget,
    },
    Frame,
};

use super::list::{Focus, ItemOrder, ListComponent};
use super::{filter::FilterComponent, Action, EventState, RepositoriesComponent};
use crate::keymap::InputMode;
use crate::theme;
use crate::vcs::Space;

/// A space, plus the name of the repository it belongs to.
///
/// [`Space`] names its repository by an opaque [`RepoId`](crate::vcs::RepoId),
/// which is exactly right for identity and useless as a label. The backend that
/// produced the space is the one that knows the human name, so the two are
/// paired at the moment of collection — rather than having the list reach back
/// into the repository list every time it draws a row.
pub struct SpaceEntry {
    pub repo_name: String,
    pub space: Space,
}

impl SpaceEntry {
    /// The name this row is drawn from and filtered on, so that what the user
    /// sees is what they can type at. The backend tag beside it is an
    /// annotation, not part of the name, and is deliberately not filterable:
    /// "git" would otherwise match every git space of a repository called
    /// `digit`.
    fn label(&self) -> String {
        format!("{} / {}", self.repo_name, self.space.name)
    }
}

pub struct WorktreesComponent {
    spaces: Vec<SpaceEntry>,
    filter: FilterComponent,
    state: ListState,
    focus: Focus,
    selected_index: Option<usize>,
    pub last_error: Option<String>,
}

impl WorktreesComponent {
    pub fn new(spaces: Vec<SpaceEntry>) -> WorktreesComponent {
        let selected_index = if spaces.is_empty() { None } else { Some(0) };
        Self {
            filter: FilterComponent::new(),
            state: ListState::default().with_selected(selected_index),
            focus: Focus::Filter,
            selected_index,
            spaces,
            last_error: None,
        }
    }

    pub fn draw(&mut self, f: &mut Frame, rect: Rect, mode: InputMode, is_active: bool) {
        // Collect display data — ends the filtered_items() borrow before we need &self again.
        let display_data: Vec<(Space, String)> = {
            let filtered = self.filtered_items();
            filtered
                .iter()
                .map(|entry| (entry.space.clone(), entry.label()))
                .collect()
        };
        let total = display_data.len();
        let items: Vec<ListItem<'static>> = display_data
            .iter()
            .map(|(space, label)| space_to_list_item(space, label))
            .collect();

        // B: cap current to total so a stale selected_index never shows x > y in (x/y)
        let current = self.selected_index.map(|i| (i + 1).min(total)).unwrap_or(0);

        let mode_indicator = match mode {
            InputMode::Normal => Line::from(" NORMAL ").style(theme::SUCCESS_TEXT),
            InputMode::Insert => Line::from(" INSERT ").style(theme::WARNING_TEXT),
        };
        let bottom_left = match &self.last_error {
            Some(err) => Line::from(format!(" {} ", err)).style(theme::DANGER_TEXT),
            None => mode_indicator,
        };

        // When a filter is active in Normal mode, show it in the title so it's always visible.
        let title = {
            let mut spans = vec![
                Span::raw(" "),
                Span::styled("Worktrees", theme::TITLE),
                Span::styled(format!(" ({}/{}) ", current, total), theme::SECONDARY),
            ];
            if !self.filter.value.is_empty() && matches!(mode, InputMode::Normal) {
                spans.push(Span::styled(
                    format!("/{} ", self.filter.value),
                    theme::MUTED,
                ));
            }
            Line::from(spans)
        };

        let mut block = Block::bordered()
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(theme::BORDER)
            .style(theme::CANVAS)
            .title(title)
            .title_bottom(bottom_left);

        if matches!(mode, InputMode::Normal) {
            block = block.title_bottom(
                Line::from(vec![
                    Span::styled(" ? ", theme::KEY),
                    Span::styled("help ", theme::MUTED),
                ])
                .right_aligned(),
            );
        }

        // A: render the block frame first, then lay out filter + list inside its inner area
        let inner_area = block.inner(rect);
        f.render_widget(block, rect);

        let in_filter =
            is_active && matches!(mode, InputMode::Insert) && matches!(self.focus, Focus::Filter);

        let list_area = if in_filter {
            // Split: filter line / separator / list
            let [filter_line, sep_line, list_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .areas(inner_area);

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" / ", theme::KEY),
                    Span::styled(self.filter.value.clone(), theme::TEXT),
                ])),
                filter_line,
            );
            // " / " prefix is 3 chars wide
            f.set_cursor_position((
                filter_line.x + 3 + self.filter.cursor_pos() as u16,
                filter_line.y,
            ));
            f.render_widget(
                Paragraph::new("─".repeat(sep_line.width as usize)).style(theme::RULE),
                sep_line,
            );
            list_area
        } else {
            inner_area
        };

        let list = List::new(items)
            .style(theme::TEXT)
            .highlight_style(theme::SELECTED_ROW)
            .direction(ratatui::widgets::ListDirection::TopToBottom);
        StatefulWidget::render(list, list_area, f.buffer_mut(), &mut self.state);

        let mut scroll_state = ScrollbarState::new(total).position(self.state.offset());
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
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
            Action::Select => {
                if self.selected_worktree_path().is_some() {
                    EventState::Exit
                } else {
                    EventState::Consumed
                }
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

    /// Clears any active filter, finds the space matching the given branch name,
    /// and selects it. Returns `true` if found, `false` otherwise.
    pub fn select_worktree_by_branch(&mut self, branch: &str) -> bool {
        let exists = self.spaces.iter().any(|entry| entry.space.name == branch);
        if !exists {
            return false;
        }
        self.filter.clear();
        let index = self
            .filtered_items()
            .iter()
            .position(|entry| entry.space.name == branch);
        if let Some(idx) = index {
            self.selected_index = Some(idx);
            self.state.select(Some(idx));
            true
        } else {
            false
        }
    }

    pub fn add(&mut self, entry: SpaceEntry) {
        let path = entry.space.path.clone();
        self.spaces.push(entry);
        let index = self
            .filtered_items()
            .iter()
            .position(|entry| entry.space.path == path);

        self.state.select(index);
        self.selected_index = index;
    }

    /// Deletes the selected space through the backend that owns it.
    ///
    /// The backend comes from the repository list rather than from the space,
    /// because a [`Space`] is a snapshot with no way to act on itself — which is
    /// what lets the list hold spaces of both backends side by side.
    ///
    /// The row is dropped only when the deletion actually succeeded: a backend
    /// may refuse (jj will not forget a repository's own working copy), and a
    /// space that still exists must not vanish from the list.
    pub fn delete_selected_space(&mut self, repos: &RepositoriesComponent) -> eyre::Result<()> {
        let Some(path) = self.selected_worktree_path() else {
            return Ok(());
        };
        let Some(index) = self
            .spaces
            .iter()
            .position(|entry| entry.space.path.to_string_lossy() == path)
        else {
            return Ok(());
        };

        let space = &self.spaces[index].space;
        let backend = repos.backend_for(space).ok_or_else(|| {
            eyre!(
                "no open repository for the space {:?}; it cannot be deleted",
                space.name
            )
        })?;
        backend.delete_space(space)?;

        self.spaces.remove(index);
        Ok(())
    }

    pub fn selected_worktree_path(&mut self) -> Option<String> {
        self.selected_index.and_then(|index| {
            self.filtered_items()
                .get(index)
                .map(|entry| entry.space.path.to_string_lossy().into_owned())
        })
    }

    /// The backend-neutral status of the selected space, for callers that need
    /// to word a message in the vocabulary of whatever drives it.
    pub fn selected_space_backend(&mut self) -> Option<crate::vcs::Backend> {
        self.selected_index.and_then(|index| {
            self.filtered_items()
                .get(index)
                // The owner recorded on the space, not the one implied by its
                // status: the status is a probe result and may be `Unknown`,
                // while the owner is what the deletion will actually go through.
                .map(|entry| entry.space.backend)
        })
    }
}

/// One row: the two status slots, then `<repo> / <space>`.
///
/// The renderer is deliberately dumb about state — it asks the status for its
/// glyphs and maps tones to colours. Matching on the backend here is what would
/// force every new jj state to be taught to the UI as well.
fn space_to_list_item(space: &Space, label: &str) -> ListItem<'static> {
    let mut spans: Vec<Span<'static>> = space
        .status
        .glyphs()
        .iter()
        .map(|glyph| {
            Span::styled(
                glyph.symbol.to_string(),
                Style::default()
                    .fg(theme::tone(glyph.tone))
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    spans.push(Span::raw(" "));

    // Which backend owns the row. A colocated repository contributes both its
    // git worktrees and its jj workspaces to this list under one name, so
    // without this the two are indistinguishable — and they behave differently
    // when deleted. Padded so the repo names still line up in a column.
    spans.push(Span::styled(
        format!("{:<3} ", space.backend.label()),
        theme::MUTED,
    ));

    match label.split_once(" / ") {
        Some((repo, name)) => {
            spans.push(Span::styled(repo.to_string(), theme::SECONDARY));
            spans.push(Span::styled(" / ", theme::MUTED));
            spans.push(Span::styled(
                name.to_string(),
                theme::TEXT.add_modifier(Modifier::BOLD),
            ));
        }
        None => spans.push(Span::from(label.to_string())),
    }

    ListItem::new(Line::from(spans))
}

impl ListComponent<SpaceEntry> for WorktreesComponent {
    fn filtered_items(&mut self) -> Vec<&SpaceEntry> {
        let query = self.filter.value.as_str();
        if query.is_empty() {
            let mut items: Vec<&SpaceEntry> = self.spaces.iter().collect();
            items.sort_by(|a, b| (&a.repo_name, &a.space.name).cmp(&(&b.repo_name, &b.space.name)));
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
        let mut scored: Vec<(&SpaceEntry, u32)> = self
            .spaces
            .iter()
            .filter_map(|entry| {
                let label = entry.label();
                let mut total = 0u32;
                for (pattern, min_score) in &patterns {
                    match pattern.score(Utf32Str::new(&label, &mut buf), &mut matcher) {
                        Some(s) if s >= *min_score => total += s,
                        _ => return None,
                    }
                }
                Some((entry, total))
            })
            .collect();
        // Highest fuzzy score first; sort_by_key keeps the stable order of equal scores.
        scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        scored.into_iter().map(|(entry, _)| entry).collect()
    }

    fn get_state(&mut self) -> &mut ListState {
        &mut self.state
    }

    fn update_selected_index(&mut self, index: usize) {
        self.selected_index = Some(index);
    }
}
