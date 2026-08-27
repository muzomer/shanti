//! The cross-repository jump list.
//!
//! The base pane orders spaces by `(repo, name)` — alphabetical, so a name can
//! be found once its repository is known. That is the wrong order for "what was
//! I just working on": this modal exists to answer that question instead, by
//! showing every space shanti knows about, across every repository, newest
//! commit first, reachable without switching panes or scope first.

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, List, ListDirection, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget,
    },
    Frame,
};

use super::{
    footer_entries,
    list::{ItemOrder, ListComponent},
    popup_area,
    prompt::footer,
    worktrees::SpaceEntry,
    Action, AppContext, EventState, Extent, HelpEntry, Modal, ModalFlow, ModalKind, KEYS_SECTION,
};
use crate::{theme, vcs::now_seconds};

/// Picks one space out of every repository shanti has scanned, ordered by how
/// recently its last commit landed.
pub struct RecentSpacesModal {
    entries: Vec<SpaceEntry>,
    state: ListState,
    selected_index: usize,
}

impl RecentSpacesModal {
    /// Builds the list from a snapshot of every known space, sorting it by
    /// recency here rather than asking the caller to: recency is this modal's
    /// entire reason to exist, not a policy owned by whoever opens it.
    pub fn new(mut entries: Vec<SpaceEntry>) -> Self {
        // Newest commit first. A space with no readable tip — a brand-new
        // worktree on an unborn branch, a jj listing from before the field
        // existed — sorts after every space that has one, on the same
        // `(repo, name)` order the base pane uses, so it is still findable
        // rather than scattered.
        entries.sort_by(|a, b| match (&a.space.tip, &b.space.tip) {
            (Some(at), Some(bt)) => bt.committed_at.cmp(&at.committed_at),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => (&a.repo_name, &a.space.name).cmp(&(&b.repo_name, &b.space.name)),
        });
        let selected_index = 0;
        Self {
            state: ListState::default().with_selected((!entries.is_empty()).then_some(0)),
            entries,
            selected_index,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);

        let title = Line::from(vec![
            Span::styled(" ▸ ", theme::key()),
            Span::styled("Recent Spaces ", theme::title()),
        ])
        .alignment(Alignment::Center);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::border_focused())
            .style(theme::popup_surface())
            .title(title)
            .title_bottom(footer(
                &footer_entries(&self.help(), KEYS_SECTION),
                area.width,
            ));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let [prompt_area, list_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        Paragraph::new(Line::from(Span::styled(
            " Every repository, newest commit first",
            theme::secondary(),
        )))
        .render(prompt_area, frame.buffer_mut());

        if self.entries.is_empty() {
            Paragraph::new(Line::from(Span::styled(" No spaces yet", theme::muted())))
                .render(list_area, frame.buffer_mut());
            return;
        }

        let now = now_seconds();
        let total = self.entries.len();
        let items: Vec<ListItem> = self.entries.iter().map(|e| space_row(e, now)).collect();
        let list = List::new(items)
            .style(theme::text())
            .highlight_style(theme::selected_row())
            .highlight_symbol("▸ ")
            .direction(ListDirection::TopToBottom);
        StatefulWidget::render(list, list_area, frame.buffer_mut(), &mut self.state);

        if total > list_area.height as usize {
            let mut scroll_state = ScrollbarState::new(total)
                .position(self.state.offset())
                .viewport_content_length(list_area.height as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(theme::rule())
                .track_style(theme::rule());
            frame.render_stateful_widget(scrollbar, list_area, &mut scroll_state);
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
            _ => EventState::NotConsumed,
        }
    }
}

impl Modal for RecentSpacesModal {
    fn kind(&self) -> ModalKind {
        ModalKind::RecentSpaces
    }

    fn area(&self, full: Rect) -> Rect {
        // Grows with the list, capped well above the base pane's own floor: this
        // is meant to be scanned at a glance across many repositories, so it
        // earns more of the frame than a same-repository picker would.
        let rows = (self.entries.len() as u16).clamp(1, 16).saturating_add(4);
        popup_area(
            full,
            // Wide enough for `<repo> › <space>` plus a commit subject beside it.
            Extent::share(80, 50, 140),
            Extent::fixed(rows),
        )
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        RecentSpacesModal::draw(self, frame, area);
    }

    fn handle(&mut self, action: Action, _ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select => {
                if self.entries.is_empty() {
                    return ModalFlow::Consumed;
                }
                ModalFlow::SelectSpace(self.entries[self.selected_index].space.path.clone())
            }
            Action::ClosePopup | Action::ExitInsertMode => ModalFlow::Close,
            _ => self.handle_action(action).into(),
        }
    }

    fn help(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry::Section(KEYS_SECTION),
            HelpEntry::bind("j / ↓", "Move down").hint("↑/↓", "move"),
            HelpEntry::bind("k / ↑", "Move up"),
            HelpEntry::bind("g / Home", "Go to first"),
            HelpEntry::bind("G / End", "Go to last"),
            HelpEntry::bind("Enter", "Jump to selected space").hint("Enter", "jump"),
            HelpEntry::bind("? / F1", "Show this help")
                .hint("?", "help")
                .aside(),
            HelpEntry::bind("Esc", "Close")
                .hint("Esc", "cancel")
                .safe()
                .essential(),
            HelpEntry::bind("q / Ctrl+C", "Quit"),
        ]
    }
}

/// One row: the age, which repository, which space in it — skipped when the
/// space *is* the repository's own working copy, the same rule the base pane's
/// row applies — and the commit it is sitting on.
fn space_row(entry: &SpaceEntry, now: i64) -> ListItem<'static> {
    let age = entry
        .space
        .tip
        .as_ref()
        .map(|tip| tip.age(now))
        .unwrap_or_else(|| "—".to_string());
    let subject = entry
        .space
        .tip
        .as_ref()
        .map(|tip| tip.subject_or_placeholder().to_string())
        .unwrap_or_else(|| "(no commits yet)".to_string());

    let mut spans = vec![
        Span::styled(format!("{:>3} ", age), theme::secondary()),
        Span::styled(entry.repo_name.clone(), theme::secondary()),
    ];
    if !entry.is_default_space() {
        spans.push(Span::styled(" › ", theme::muted()));
        spans.push(Span::styled(
            entry.space.name.clone(),
            theme::text().add_modifier(ratatui::style::Modifier::BOLD),
        ));
    }
    spans.push(Span::styled("  ", theme::muted()));
    spans.push(Span::styled(subject, theme::muted()));

    ListItem::new(Line::from(spans))
}

impl ListComponent<SpaceEntry> for RecentSpacesModal {
    fn filtered_items(&mut self) -> Vec<&SpaceEntry> {
        self.entries.iter().collect()
    }

    fn get_state(&mut self) -> &mut ListState {
        &mut self.state
    }

    fn update_selected_index(&mut self, index: usize) {
        self.selected_index = index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::{Backend, RepoId, Space, SpaceStatus, SpaceTip};
    use std::path::PathBuf;

    fn entry(repo: &str, name: &str, committed_at: Option<i64>) -> SpaceEntry {
        let repo_path = PathBuf::from(format!("/repos/{repo}"));
        let path = repo_path.join(name);
        let space = Space::new(
            RepoId::from_path(&repo_path),
            Backend::Git,
            name,
            path,
            SpaceStatus::unknown(Backend::Git),
        )
        .with_tip(committed_at.map(|at| SpaceTip::new("a commit", at)));
        SpaceEntry {
            repo_name: repo.to_string(),
            repo_path,
            space,
        }
    }

    /// The entire point of the modal: across repositories, most recent first.
    #[test]
    fn entries_are_ordered_by_recency_across_repositories() {
        let modal = RecentSpacesModal::new(vec![
            entry("alpha", "old", Some(100)),
            entry("beta", "new", Some(300)),
            entry("alpha", "mid", Some(200)),
        ]);
        let names: Vec<&str> = modal
            .entries
            .iter()
            .map(|e| e.space.name.as_str())
            .collect();
        assert_eq!(names, vec!["new", "mid", "old"]);
    }

    /// A space whose head could not be read must still be reachable — it just
    /// isn't the first thing shown.
    #[test]
    fn spaces_with_no_tip_sort_after_ones_with_a_tip() {
        let modal = RecentSpacesModal::new(vec![
            entry("alpha", "unborn", None),
            entry("alpha", "has-history", Some(50)),
        ]);
        let names: Vec<&str> = modal
            .entries
            .iter()
            .map(|e| e.space.name.as_str())
            .collect();
        assert_eq!(names, vec!["has-history", "unborn"]);
    }

    #[test]
    fn selecting_the_top_entry_yields_its_path() {
        let mut modal = RecentSpacesModal::new(vec![
            entry("alpha", "old", Some(1)),
            entry("beta", "new", Some(2)),
        ]);
        let mut worktrees = super::super::WorktreesComponent::new(Vec::new());
        let mut repositories = super::super::RepositoriesComponent::new(Vec::new());
        let mut notify = crate::components::Notifications::default();
        let args = crate::cli::Args::for_dirs("/tmp/spaces", vec!["/tmp/repos".to_string()]);
        let mut background = Vec::new();
        let mut meta = crate::space_meta::SpaceMeta::in_memory();
        let mut ctx = AppContext {
            worktrees: &mut worktrees,
            repositories: &mut repositories,
            notify: &mut notify,
            args: &args,
            background: &mut background,
            meta: &mut meta,
        };
        match Modal::handle(&mut modal, Action::Select, &mut ctx) {
            ModalFlow::SelectSpace(path) => {
                assert_eq!(path, PathBuf::from("/repos/beta/new"));
            }
            _ => panic!("expected a space to be selected"),
        }
    }
}
