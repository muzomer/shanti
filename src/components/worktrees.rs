use std::path::PathBuf;

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
        ScrollbarState, StatefulWidget, Wrap,
    },
    Frame,
};

use super::list::{Focus, ItemOrder, ListComponent};
use super::{
    filter::FilterComponent, Action, EventState, RepositoriesComponent, MIN_HEIGHT, MIN_WIDTH,
};
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
    /// Root of the repository on disk, as the owning backend reports it.
    ///
    /// Carried alongside the name so a row can tell whether it *is* the
    /// repository's own working copy — see [`SpaceEntry::is_default_space`].
    /// Taken from the backend rather than re-derived from the space's
    /// [`RepoId`](crate::vcs::RepoId) so the comparison stays a comparison of
    /// paths even if repository ids stop being paths.
    pub repo_path: PathBuf,
    pub space: Space,
}

impl SpaceEntry {
    /// Whether this space is the repository's own working copy rather than one
    /// shanti created beside it.
    ///
    /// Identified by path, never by the name `default`: `jj workspace rename`
    /// can move that name onto another workspace, while the repository root
    /// cannot move. This is the rule `JjBackend::is_default_space` applies
    /// before it refuses a deletion, restated here because the `Vcs` trait the
    /// UI is allowed to see does not expose it.
    ///
    /// Git needs no special case: only *linked* worktrees are listed as spaces,
    /// so no git row ever sits at the repository root and the answer is simply
    /// always `false` for one. That is also the right answer — a linked worktree
    /// carries a name the user chose, and that name is the only thing telling
    /// two rows of the same repository apart.
    fn is_default_space(&self) -> bool {
        self.space.path == self.repo_path
    }

    /// What this row spells out, in parts.
    fn row_label(&self) -> RowLabel {
        RowLabel {
            repo: self.repo_name.clone(),
            // A repository has exactly one default space, and it is called
            // `default` until someone renames it, so naming it says nothing
            // while costing half the row. Dropping it leaves the two things the
            // eye is after: which repository, and what state.
            space: (!self.is_default_space()).then(|| self.space.name.clone()),
        }
    }

    /// The text this row is filtered on, so that what the user sees is what they
    /// can type at. A suppressed space name is deliberately not part of the
    /// haystack: typing `default` must not match a row that never says it.
    ///
    /// The backend tag beside the label is an annotation, not part of the name,
    /// and is likewise not filterable: "git" would otherwise match every git
    /// space of a repository called `digit`.
    fn label(&self) -> String {
        self.row_label().text()
    }
}

/// A row's name, split into the parts it is drawn from.
///
/// Kept structured rather than pre-formatted because the renderer styles the
/// repository and the space differently, and recovering them by re-splitting a
/// formatted string breaks as soon as either part contains the separator — or,
/// as here, as soon as one of them is absent.
struct RowLabel {
    repo: String,
    /// The space's name, or `None` when the row is the repository's default
    /// space and naming it would add nothing.
    space: Option<String>,
}

impl RowLabel {
    /// The label as one string — what the fuzzy filter matches against.
    fn text(&self) -> String {
        match &self.space {
            Some(space) => format!("{} / {}", self.repo, space),
            None => self.repo.clone(),
        }
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
        // Below the supported floor there is no honest layout left: the border
        // alone would eat most of a row and a truncated space name is worse than
        // no space name. One sentence, and the popups above stay closed
        // (`popup_area` gives them an empty rect), so this is what is on screen.
        if !super::fits(rect) {
            draw_too_small(f, rect);
            return;
        }

        // Collect display data — ends the filtered_items() borrow before we need &self again.
        let display_data: Vec<(Space, RowLabel)> = {
            let filtered = self.filtered_items();
            filtered
                .iter()
                .map(|entry| (entry.space.clone(), entry.row_label()))
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
            // Split: filter line / separator / list. `Min(1)` on the list states
            // the floor the whole pane is built around — the list is the point
            // of the screen, so it is the last thing allowed to reach zero.
            let [filter_line, sep_line, list_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .areas(inner_area);

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" / ", theme::KEY),
                    Span::styled(self.filter.value.clone(), theme::TEXT),
                ])),
                filter_line,
            );
            // " / " prefix is 3 chars wide. Clamped: a filter longer than the
            // pane is wide would otherwise park the caret off the edge.
            super::place_cursor(
                f,
                filter_line,
                filter_line.x + 3 + self.filter.cursor_pos() as u16,
                filter_line.y,
            );
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

        // Only when there are rows off-screen: a full-height track beside a list
        // that already fits is chrome that says nothing, and on a short terminal
        // it is a whole column spent saying it.
        if total > list_area.height as usize {
            let mut scroll_state = ScrollbarState::new(total)
                .position(self.state.offset())
                .viewport_content_length(list_area.height as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(theme::RULE)
                .track_style(theme::RULE);
            f.render_stateful_widget(scrollbar, list_area, &mut scroll_state);
        }
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

    /// A copy of the selected space, for callers that have to decide something
    /// from its state — which backend owns it, what deleting it would cost.
    ///
    /// Cloned rather than borrowed because the list has to be filtered to know
    /// what "selected" means, and a [`Space`] is an owned snapshot anyway: the
    /// caller is meant to reason about it, not to act on it.
    pub fn selected_space(&mut self) -> Option<Space> {
        self.selected_index.and_then(|index| {
            self.filtered_items()
                .get(index)
                .map(|entry| entry.space.clone())
        })
    }
}

/// One row: the two status slots, the backend tag, then the label — `<repo> /
/// <space>`, or just `<repo>` when the space is the repository's default one.
///
/// The renderer is deliberately dumb about state — it asks the status for its
/// glyphs and maps tones to colours. Matching on the backend here is what would
/// force every new jj state to be taught to the UI as well.
fn space_to_list_item(space: &Space, label: &RowLabel) -> ListItem<'static> {
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

    match &label.space {
        // The space is what tells this row from its siblings, so it takes the
        // emphasis and the repository recedes to context.
        Some(name) => {
            spans.push(Span::styled(label.repo.clone(), theme::SECONDARY));
            spans.push(Span::styled(" / ", theme::MUTED));
            spans.push(Span::styled(
                name.clone(),
                theme::TEXT.add_modifier(Modifier::BOLD),
            ));
        }
        // Nothing follows it, so the repository name *is* the row's subject and
        // takes the emphasis rather than reading as a dimmed prefix to nothing.
        None => spans.push(Span::styled(
            label.repo.clone(),
            theme::TEXT.add_modifier(Modifier::BOLD),
        )),
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
/// The whole interface, when the terminal is below [`MIN_WIDTH`] × [`MIN_HEIGHT`].
///
/// No border and no block: at this size chrome is the problem, not the solution.
/// It says the number it wants, because "too small" without a target leaves the
/// user dragging the corner and guessing.
fn draw_too_small(f: &mut Frame, rect: Rect) {
    f.render_widget(Paragraph::new("").style(theme::CANVAS), rect);
    let message = Paragraph::new(vec![
        Line::from(Span::styled("Terminal too small", theme::WARNING_TEXT)),
        Line::from(Span::styled(
            format!("Need {}x{}", MIN_WIDTH, MIN_HEIGHT),
            theme::MUTED,
        )),
        Line::from(Span::styled(
            format!("Have {}x{}", rect.width, rect.height),
            theme::MUTED,
        )),
    ])
    .wrap(Wrap { trim: true })
    .centered();
    // Centre what fits; on a two-row terminal the first line is the one that
    // survives, and it is the one that matters.
    let [area] = Layout::vertical([Constraint::Length(3.min(rect.height))])
        .flex(ratatui::layout::Flex::Center)
        .areas(rect);
    f.render_widget(message, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::{Backend, RepoId, SpaceStatus};

    /// A row for the space `name` of a repository rooted at `/repos/<repo>`.
    ///
    /// The status is irrelevant to labelling, so every entry gets the unknown
    /// one: these tests are about what the row *says*, not what it shows.
    fn entry(repo: &str, backend: Backend, name: &str, path: &str) -> SpaceEntry {
        let root = PathBuf::from("/repos").join(repo);
        SpaceEntry {
            repo_name: repo.to_owned(),
            repo_path: root.clone(),
            space: Space::new(
                RepoId::from_path(&root),
                backend,
                name,
                PathBuf::from(path),
                SpaceStatus::unknown(backend),
            ),
        }
    }

    #[test]
    fn a_repositorys_default_space_is_labelled_by_the_repository_alone() {
        let row = entry("shanti", Backend::Jj, "default", "/repos/shanti");
        assert_eq!(row.row_label().space, None);
        assert_eq!(row.label(), "shanti");
    }

    #[test]
    fn a_space_the_user_created_is_still_named() {
        let row = entry("shanti", Backend::Jj, "feat-x", "/spaces/shanti/feat-x");
        assert_eq!(row.row_label().space.as_deref(), Some("feat-x"));
        assert_eq!(row.label(), "shanti / feat-x");
    }

    /// `jj workspace rename` can move the name `default` onto a workspace that
    /// is not the repository's working copy — and can leave the working copy
    /// under some other name. Only the path settles it.
    #[test]
    fn the_default_space_is_recognised_by_path_not_by_the_name_default() {
        let renamed = entry("shanti", Backend::Jj, "default", "/spaces/shanti/default");
        assert_eq!(renamed.label(), "shanti / default");

        let working_copy = entry("shanti", Backend::Jj, "main-copy", "/repos/shanti");
        assert_eq!(working_copy.label(), "shanti");
    }

    /// Git lists only linked worktrees, so no git row sits at the repository
    /// root and every one of them keeps the name the user chose.
    #[test]
    fn a_git_linked_worktree_keeps_its_name() {
        let row = entry("shanti", Backend::Git, "feat-x", "/spaces/shanti/feat-x");
        assert_eq!(row.label(), "shanti / feat-x");
    }

    /// A colocated repository contributes rows from both backends under one
    /// name; suppression applies per row, not per repository.
    #[test]
    fn a_colocated_repository_suppresses_only_its_default_row() {
        let mut component = WorktreesComponent::new(vec![
            entry("shanti", Backend::Jj, "default", "/repos/shanti"),
            entry("shanti", Backend::Git, "feat-x", "/spaces/shanti/feat-x"),
        ]);
        let labels: Vec<String> = component
            .filtered_items()
            .iter()
            .map(|entry| entry.label())
            .collect();
        assert_eq!(labels, vec!["shanti", "shanti / feat-x"]);
    }

    fn type_filter(component: &mut WorktreesComponent, query: &str) -> Vec<String> {
        for c in query.chars() {
            component.handle_action(Action::InsertChar(c));
        }
        component
            .filtered_items()
            .iter()
            .map(|entry| entry.label())
            .collect()
    }

    /// The haystack is what the row displays, so a name the row no longer shows
    /// is not something the user can type at.
    #[test]
    fn a_suppressed_name_cannot_be_filtered_for() {
        let rows = vec![
            entry("shanti", Backend::Jj, "default", "/repos/shanti"),
            entry("eclair", Backend::Jj, "default", "/repos/eclair"),
            entry("eclair", Backend::Jj, "feat-default", "/spaces/eclair/d"),
        ];

        let mut component = WorktreesComponent::new(rows);
        // Only the row that still spells "default" out matches it.
        assert_eq!(
            type_filter(&mut component, "default"),
            vec!["eclair / feat-default"]
        );
    }

    #[test]
    fn a_default_row_is_still_found_by_its_repository_name() {
        let rows = vec![
            entry("shanti", Backend::Jj, "default", "/repos/shanti"),
            entry("eclair", Backend::Jj, "default", "/repos/eclair"),
        ];
        let mut component = WorktreesComponent::new(rows);
        assert_eq!(type_filter(&mut component, "shanti"), vec!["shanti"]);
    }
}
