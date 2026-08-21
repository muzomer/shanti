use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};

use super::list::ItemOrder;
use crate::git::Repository;
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{
        palette::tailwind::{GREEN, RED, SLATE},
        Style, Stylize,
    },
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, List, ListDirection, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, StatefulWidget,
    },
    Frame,
};

use super::{
    filter::FilterComponent,
    list::{Focus, ListComponent},
    Action, EventState, SELECTED_STYLE,
};
use crate::keymap::InputMode;

pub struct RepositoriesComponent {
    repositories: Vec<Repository>,
    filter: FilterComponent,
    state: ListState,
    selected_index: Option<usize>,
    focus: Focus,
}

impl RepositoriesComponent {
    pub fn new(repositories: Vec<Repository>) -> Self {
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
            .map(|r| ListItem::new(r.name()))
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
        let index = self.filtered_items().iter().position(|r| r.name() == name);
        if let Some(idx) = index {
            self.selected_index = Some(idx);
            self.state.select(Some(idx));
            true
        } else {
            false
        }
    }

    pub fn add_repository(&mut self, repo: Repository) {
        self.repositories.push(repo);
    }

    pub fn selected_repository(&mut self) -> Option<&Repository> {
        match self.selected_index {
            Some(index) => {
                let filtered_repositories = self.filtered_items();
                match filtered_repositories.get(index) {
                    Some(selected_repository) => Some(selected_repository),
                    None => None,
                }
            }
            None => None,
        }
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

impl ListComponent<Repository> for RepositoriesComponent {
    fn filtered_items(&mut self) -> Vec<&Repository> {
        let query = self.filter.value.as_str();
        if query.is_empty() {
            let mut items: Vec<&Repository> = self.repositories.iter().collect();
            items.sort_by_key(|a| a.name());
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
        let mut scored: Vec<(&Repository, u32)> = self
            .repositories
            .iter()
            .filter_map(|r| {
                let name = r.name();
                let mut total = 0u32;
                for (pattern, min_score) in &patterns {
                    match pattern.score(Utf32Str::new(&name, &mut buf), &mut matcher) {
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
