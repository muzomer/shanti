use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};

use super::list::ItemOrder;
use crate::theme;
use crate::vcs::{Backend, BoxedVcs, RepoId, RepoStore, Vcs};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, List, ListDirection, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, StatefulWidget,
    },
    Frame,
};

use super::{
    create_worktree::CreateWorktreeComponent,
    filter::FilterComponent,
    footer_entries,
    list::{Focus, ListComponent},
    popup_area,
    prompt::footer,
    worktrees::SpaceEntry,
    Action, AppContext, EventState, Extent, HelpEntry, Modal, ModalFlow, ModalKind, FILTER_SECTION,
    KEYS_SECTION,
};
use crate::keymap::InputMode;
use tracing::error;

/// The repositories shanti found, each behind the backend that drives it.
///
/// Storing [`BoxedVcs`] rather than a concrete backend is what makes the list
/// heterogeneous: git and jj repositories sit side by side and nothing above
/// this type asks which is which.
pub struct RepositoriesComponent {
    /// Every open backend. The widget owns a store rather than a bare vector, so
    /// "which backend owns this?" is answered by [`RepoStore`] and this type is
    /// left with the cursor, the filter and the focus — see `src/vcs/store.rs`.
    store: RepoStore,
    filter: FilterComponent,
    state: ListState,
    selected_index: Option<usize>,
    focus: Focus,
}

/// How one drawing of the list is dressed: the same rows are painted as a
/// popup over the canvas or as the resting left pane, and only these three
/// values differ between them. Grouped so `render` describes *what* it draws
/// and this type carries *how* it looks.
struct Chrome<'a> {
    border: Style,
    surface: Style,
    bindings: &'a [HelpEntry],
}

impl RepositoriesComponent {
    pub fn new(repositories: Vec<BoxedVcs>) -> Self {
        Self {
            store: RepoStore::new(repositories),
            filter: FilterComponent::new(),
            state: ListState::default().with_selected(Some(0)),
            selected_index: Some(0),
            focus: Focus::Filter,
        }
    }

    /// The picker modal: a popup surface with an always-focused border, drawn
    /// over the list. This is the narrow-terminal fallback for creating a space.
    pub fn draw(&mut self, f: &mut Frame, rect: Rect, mode: InputMode) {
        self.render(
            f,
            rect,
            mode,
            // The picker is the active modal: it owns the keyboard, so its own
            // Insert mode may open the filter.
            true,
            Chrome {
                border: theme::border_focused(),
                surface: theme::popup_surface(),
                bindings: &repositories_bindings(),
            },
        );
    }

    /// The persistent left pane of the two-pane layout. Sits on the canvas like
    /// the spaces pane, and shows focus through its border — accented when it
    /// holds the keyboard, muted when the spaces pane does.
    pub fn draw_pane(&mut self, f: &mut Frame, rect: Rect, mode: InputMode, focused: bool) {
        let border = if focused {
            theme::border_focused()
        } else {
            theme::border()
        };
        self.render(
            f,
            rect,
            mode,
            // Only the focused pane may open its filter: the effective mode is
            // the top modal's, so without this an Insert-mode popup (the create
            // prompt) would make this unfocused pane draw its filter too.
            focused,
            Chrome {
                border,
                surface: theme::canvas(),
                bindings: &repositories_pane_bindings(),
            },
        );
    }

    fn render(
        &mut self,
        f: &mut Frame,
        rect: Rect,
        mode: InputMode,
        filtering: bool,
        chrome: Chrome<'_>,
    ) {
        // Empty when the terminal is below the floor: the base pane's one-line
        // message is what the user gets, and clearing over it would leave a hole.
        if rect.is_empty() {
            return;
        }
        f.render_widget(Clear, rect);

        let total = self.filtered_items().len();
        let title = {
            let mut spans = vec![
                Span::raw(" "),
                Span::styled("Repositories", theme::title()),
                Span::styled(format!(" ({}) ", total), theme::secondary()),
            ];
            if !self.filter.value.is_empty() && matches!(mode, InputMode::Normal) {
                spans.push(Span::styled(
                    format!("/{} ", self.filter.value),
                    theme::muted(),
                ));
            }
            Line::from(spans).alignment(Alignment::Center)
        };

        // Drawn in both modes, not just Normal: filter mode is exactly where a
        // user is most likely to have forgotten which key gets them back out.
        let section = match mode {
            InputMode::Normal => KEYS_SECTION,
            InputMode::Insert => FILTER_SECTION,
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(chrome.border)
            .style(chrome.surface)
            .title(title)
            .title_bottom(footer(
                &footer_entries(chrome.bindings, section),
                rect.width,
            ));

        let inner_area = block.inner(rect);
        f.render_widget(block, rect);

        let in_filter =
            filtering && matches!(mode, InputMode::Insert) && matches!(self.focus, Focus::Filter);

        let list_area = if in_filter {
            // `Min(1)` on the list, not `Fill`: the list is the point of the
            // popup and is the last thing allowed to be squeezed to nothing.
            let [filter_line, sep_line, list_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .areas(inner_area);

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" / ", theme::key()),
                    Span::styled(self.filter.value.clone(), theme::text()),
                ])),
                filter_line,
            );
            // Clamped: a filter longer than the popup is wide would otherwise
            // park the caret past the border.
            super::place_cursor(
                f,
                filter_line,
                filter_line.x + 3 + self.filter.cursor_pos() as u16,
                filter_line.y,
            );
            f.render_widget(
                Paragraph::new("─".repeat(sep_line.width as usize)).style(theme::rule()),
                sep_line,
            );
            list_area
        } else {
            inner_area
        };

        let items: Vec<ListItem> = self
            .filtered_items()
            .iter()
            .map(|r| repo_row(&r.repo().name))
            .collect();
        let list = List::new(items)
            .style(theme::text())
            .highlight_style(theme::selected_row())
            // A marker as well as the band, for terminals that drop backgrounds.
            .highlight_symbol("▸ ")
            .direction(ListDirection::TopToBottom);
        StatefulWidget::render(list, list_area, f.buffer_mut(), &mut self.state);

        // Only when something is off-screen; see `select_directory`.
        if total > list_area.height as usize {
            let mut scroll_state = ScrollbarState::new(total)
                .position(self.state.offset())
                .viewport_content_length(list_area.height as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(theme::rule())
                .track_style(theme::rule());
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
        self.store.add(repo);
    }

    /// Swaps the whole backend set, keeping the user on the repository they had
    /// selected if it is still there.
    ///
    /// This is what a scan result needs and `add_repository` cannot do: the set
    /// is *replaced*, so restarting a scan cannot leave repositories behind that
    /// are no longer on disk.
    pub fn replace_backends(&mut self, repositories: Vec<BoxedVcs>) {
        let anchor = self.selected_id();
        self.store.replace(repositories);
        self.restore_selection(anchor);
    }

    /// Merges a batch of freshly opened backends into the list.
    ///
    /// A repository already in the list is replaced rather than added beside
    /// itself, so overlapping repos dirs — or a second scan — cannot show the
    /// same repository twice.
    pub fn add_repositories(&mut self, repositories: Vec<BoxedVcs>) {
        let anchor = self.selected_id();
        self.store.merge(repositories);
        self.restore_selection(anchor);
    }

    /// The selected repository's id, as something a rebuilt list can be
    /// searched for.
    fn selected_id(&mut self) -> Option<RepoId> {
        let index = self.selected_index?;
        self.filtered_items()
            .get(index)
            .map(|backend| backend.repo().id.clone())
    }

    /// Puts the selection back on `anchor`, or keeps the position it had.
    fn restore_selection(&mut self, anchor: Option<RepoId>) {
        let rows = self.filtered_items().len();
        if rows == 0 {
            self.selected_index = None;
            self.state.select(None);
            return;
        }

        let mut index = None;
        if let Some(id) = anchor {
            index = self
                .filtered_items()
                .iter()
                .position(|backend| backend.repo().id == id);
        }
        let index = index.unwrap_or_else(|| self.selected_index.unwrap_or(0).min(rows - 1));
        self.selected_index = Some(index);
        self.state.select(Some(index));
    }

    /// The highlighted repository's id, for scoping the spaces pane to it. A
    /// public view of the same anchor a rebuild uses to keep the selection.
    pub fn selected_repo_id(&mut self) -> Option<RepoId> {
        self.selected_id()
    }

    /// The selected repository driven by a specific `backend`, for a colocated
    /// repository where the create prompt lets the user choose git or jj. Falls
    /// back to whatever backend owns the id when the chosen one is not open on
    /// it, so a single-backend repository always resolves.
    pub fn selected_backend(&mut self, backend: Backend) -> Option<&dyn Vcs> {
        let id = self.selected_id()?;
        self.store.backend_of(&id, backend)
    }

    pub fn selected_repository(&mut self) -> Option<&dyn Vcs> {
        let index = self.selected_index?;
        // Copy the borrow out of the temporary Vec: its elements already point
        // into `self`, so the `&dyn Vcs` outlives the filtered list itself.
        let selected: &BoxedVcs = *self.filtered_items().get(index)?;
        Some(selected.as_ref())
    }

    /// The open repositories, for a caller that wants a domain answer rather
    /// than a row. The seam that stops a new view depending on this widget.
    pub fn store(&self) -> &RepoStore {
        &self.store
    }

    /// One entry per repository, for the picker.
    ///
    /// The picker asks "which repository?", not "which backend?", so a colocated
    /// repository must appear once rather than twice. The entry kept is the jj
    /// one, which is also what makes a new space on a colocated repo a jj
    /// workspace by default — shanti-12z.5's rule that jj owns such a repo.
    fn repository_choices(&self) -> Vec<&BoxedVcs> {
        self.store.choices()
    }

    /// Every space of every repository, and the names of the repositories that
    /// could not be asked.
    ///
    /// Listing is per repository and each one can fail on its own (an unreadable
    /// worktree registration, a `jj` that will not run), so a failure is reported
    /// alongside the spaces that *did* list rather than replacing them.
    ///
    /// A colocated repository is open once per backend, so its git worktrees and
    /// its jj workspaces both land here — merged by iterating the backend list,
    /// with no special case of its own. Each space carries the backend that owns
    /// it, which is what keeps the merged list actionable.
    ///
    /// Test-only now that nothing re-reads the whole list on the render thread:
    /// a refresh asks the *worker* for one repository at a time. Kept because it
    /// is still the clearest way to state, in a test, what the whole list should
    /// contain.
    #[cfg(test)]
    fn collect_spaces(&self) -> (Vec<SpaceEntry>, Vec<String>) {
        spaces_of(self.store.backends())
    }
}

/// The same listing, over backends nobody owns yet.
///
/// Free-standing so a scan result can be turned into rows *before* it is handed
/// to the list — which is what keeps a streaming update to the cost of the batch
/// that arrived, rather than re-listing every repository already on screen.
pub fn spaces_of(backends: &[BoxedVcs]) -> (Vec<SpaceEntry>, Vec<String>) {
    let mut spaces = Vec::new();
    let mut failed = Vec::new();
    for backend in backends {
        let repo_name = &backend.repo().name;
        let repo_path = &backend.repo().path;
        match backend.spaces() {
            Ok(found) => spaces.extend(found.into_iter().map(|space| SpaceEntry {
                repo_name: repo_name.clone(),
                repo_path: repo_path.clone(),
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

/// The picker's footer, taken from the same table its help popup shows and cut
/// to the section the current mode is in.
/// Footer bindings for the left pane of the two-pane layout. Unlike the picker
/// (`repositories_bindings`), Enter does not pick a repository and Esc does not
/// close a popup: the pane is always on screen. `n` makes a space in the
/// highlighted repository, and `Tab` moves to the spaces pane beside it.
pub fn repositories_pane_bindings() -> Vec<HelpEntry> {
    vec![
        HelpEntry::Section(KEYS_SECTION),
        HelpEntry::bind("j / ↓", "Move down").hint("j/k", "move"),
        HelpEntry::bind("k / ↑", "Move up"),
        HelpEntry::bind("n", "New space here").hint("n", "new"),
        HelpEntry::bind("i", "Enter filter mode").hint("i", "filter"),
        HelpEntry::bind("Tab", "Focus spaces").hint("Tab", "spaces"),
        HelpEntry::bind("t", "Choose a colour scheme")
            .hint("t", "theme")
            .aside(),
        HelpEntry::bind("? / F1", "Show this help")
            .hint("?", "help")
            .aside(),
        HelpEntry::bind("q / Ctrl+C", "Quit"),
        HelpEntry::Blank,
        HelpEntry::Section(FILTER_SECTION),
        HelpEntry::bind("Esc", "Leave filter mode")
            .hint("Esc", "list")
            .safe()
            .essential(),
        HelpEntry::bind("↑ / ↓ / Ctrl+K / Ctrl+J", "Move in list").hint("↑/↓", "move"),
    ]
}

/// One row: the repository name, nothing else.
///
/// The backend is not named here on purpose. A repository is one thing however
/// it is driven, and each row is now one repository (a colocated repo is
/// deduplicated to its jj owner), so a per-row backend tag only added noise. The
/// backend is still visible where it decides something: per space in the
/// Worktrees pane, and in the create prompt, which names the backend it uses.
fn repo_row(name: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(name.to_string(), theme::text())))
}

impl ListComponent<BoxedVcs> for RepositoriesComponent {
    fn filtered_items(&mut self) -> Vec<&BoxedVcs> {
        let query = self.filter.value.as_str();
        let choices = self.repository_choices();
        if query.is_empty() {
            let mut items = choices;
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
        let mut scored: Vec<(&BoxedVcs, u32)> = choices
            .into_iter()
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
    fn kind(&self) -> ModalKind {
        ModalKind::Repositories
    }

    fn area(&self, full: Rect) -> Rect {
        popup_area(
            full,
            // Half the frame, but never so narrow that a repository name is
            // truncated to nothing, nor so wide that a column of short names
            // sprawls across a large terminal.
            Extent::share(50, 34, 80),
            Extent::share(50, 8, 30),
        )
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
                let selected = ctx
                    .repositories
                    .selected_repository()
                    .map(|r| (r.repo().name.clone(), r.repo().id.clone(), r.backend()));
                let (repo_name, backend, colocated) = match selected {
                    // The picker offers one entry per repository, and that entry
                    // is the owner — so what it hands over here *is* the default
                    // backend for a new space.
                    Some((name, id, backend)) => {
                        let colocated = ctx.repositories.store().backends_of(&id).len() > 1;
                        (name, backend, colocated)
                    }
                    None => (String::new(), Backend::Git, false),
                };
                // Replace, not stack: cancelling the name prompt returns to the
                // worktree list, it does not re-open the picker.
                ModalFlow::Replace(Box::new(CreateWorktreeComponent::new(
                    repo_name, backend, colocated,
                )))
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
        repositories_bindings()
    }
}

/// Keybindings for the repository picker, and the source its footer is read
/// from — the popup has two input modes, so the table has a section for each.
pub fn repositories_bindings() -> Vec<HelpEntry> {
    vec![
        HelpEntry::Section(KEYS_SECTION),
        HelpEntry::bind("j / ↓", "Move down").hint("j/k", "move"),
        HelpEntry::bind("k / ↑", "Move up"),
        HelpEntry::bind("g / Home", "Go to first"),
        HelpEntry::bind("G / End", "Go to last"),
        HelpEntry::bind("i", "Enter filter mode").hint("i", "filter"),
        HelpEntry::bind("Tab", "Toggle filter / list"),
        HelpEntry::bind("Enter", "Select repository").hint("Enter", "select"),
        HelpEntry::bind("? / F1", "Show this help")
            .hint("?", "help")
            .aside(),
        HelpEntry::bind("Esc", "Close popup")
            .hint("Esc", "close")
            .safe()
            .essential(),
        HelpEntry::bind("q / Ctrl+C", "Quit"),
        HelpEntry::Blank,
        // Deliberately the short version: what differs while typing a filter,
        // and nothing that the section above already says in the same words. The
        // help popup sizes itself from this table and is centred over the picker
        // it was opened from, so a second full-length section would grow past the
        // picker and bury the title telling the reader what they are reading
        // about.
        HelpEntry::Section(FILTER_SECTION),
        HelpEntry::bind("Esc", "Leave filter mode")
            .hint("Esc", "list")
            .safe()
            .essential(),
        HelpEntry::bind("↑ / ↓ / Ctrl+K / Ctrl+J", "Move in list").hint("↑/↓", "move"),
        HelpEntry::bind("Enter", "Select repository").hint("Enter", "select"),
        HelpEntry::bind("F1", "Show this help")
            .hint("F1", "help")
            .aside(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::{Repo, Space, SpaceStatus};
    use color_eyre::eyre;
    use std::path::{Path, PathBuf};

    /// A backend that answers from a fixed list.
    ///
    /// The behaviour under test is routing, not version control: what matters is
    /// that two backends can share one repository — which is what a colocated
    /// repo *is* — and still be told apart. A stub also keeps these tests free
    /// of a `jj` binary, which the machine running them may not have.
    struct StubVcs {
        repo: Repo,
        spaces: Vec<Space>,
    }

    impl StubVcs {
        /// A backend on `path` holding one space per name in `spaces`.
        fn new(path: &str, backend: Backend, spaces: &[&str]) -> Self {
            let repo = Repo::new("shanti", path, backend);
            let spaces = spaces
                .iter()
                .map(|name| {
                    Space::new(
                        repo.id.clone(),
                        backend,
                        *name,
                        PathBuf::from(path).join(name),
                        SpaceStatus::unknown(backend),
                    )
                })
                .collect();
            Self { repo, spaces }
        }

        fn boxed(self) -> BoxedVcs {
            Box::new(self)
        }
    }

    impl Vcs for StubVcs {
        fn repo(&self) -> &Repo {
            &self.repo
        }
        fn spaces(&self) -> eyre::Result<Vec<Space>> {
            Ok(self.spaces.clone())
        }
        fn create_space(&self, _name: &str, _dest: &Path) -> eyre::Result<Space> {
            unimplemented!("not exercised by these tests")
        }
        fn delete_space(&self, _space: &Space) -> eyre::Result<()> {
            Ok(())
        }
        fn fetch(&self) -> eyre::Result<()> {
            Ok(())
        }
        fn resolve_base(&self, _name: &str) -> String {
            String::new()
        }
    }

    /// The two backends a colocated repository is opened as: jj owns it, git
    /// still holds the worktrees made before `jj init`.
    fn colocated() -> RepositoriesComponent {
        RepositoriesComponent::new(vec![
            StubVcs::new("/repos/shanti", Backend::Jj, &["default"]).boxed(),
            StubVcs::new("/repos/shanti", Backend::Git, &["feature-a", "feature-b"]).boxed(),
        ])
    }

    /// The bug this change fixes: the git worktrees of a colocated repo were
    /// listed by nobody, because only the owning backend was ever opened.
    #[test]
    fn a_colocated_repository_lists_the_spaces_of_both_backends() {
        let (spaces, failed) = colocated().collect_spaces();

        assert!(
            failed.is_empty(),
            "nothing should have failed: {:?}",
            failed
        );
        let names: Vec<&str> = spaces.iter().map(|e| e.space.name.as_str()).collect();
        assert_eq!(names, vec!["default", "feature-a", "feature-b"]);
        let backends: Vec<Backend> = spaces.iter().map(|e| e.space.backend).collect();
        assert_eq!(
            backends,
            vec![Backend::Jj, Backend::Git, Backend::Git],
            "every space must remember which backend produced it"
        );
    }

    /// The crux: both backends share a repo id, so the id alone cannot say who
    /// to route to. Getting this wrong hands a git worktree to jj.
    #[test]
    fn a_space_is_routed_to_the_backend_that_owns_it() {
        let repos = colocated();
        let (spaces, _) = repos.collect_spaces();

        for entry in &spaces {
            let backend = repos
                .store()
                .backend_for(&entry.space)
                .unwrap_or_else(|| panic!("no backend for the space {:?}", entry.space.name));
            assert_eq!(
                backend.backend(),
                entry.space.backend,
                "the space {:?} was routed to the wrong backend",
                entry.space.name
            );
        }
    }

    /// A space whose backend is not open must not be quietly handed to the other
    /// backend of the same repository; saying "no" is what makes the caller
    /// report it instead.
    #[test]
    fn a_space_of_a_backend_that_is_not_open_routes_nowhere() {
        let repos = RepositoriesComponent::new(vec![StubVcs::new(
            "/repos/shanti",
            Backend::Jj,
            &["default"],
        )
        .boxed()]);
        let orphan = Space::new(
            RepoId::from_path("/repos/shanti"),
            Backend::Git,
            "feature-a",
            "/repos/shanti/feature-a",
            SpaceStatus::unknown(Backend::Git),
        );

        assert!(repos.store().backend_for(&orphan).is_none());
    }

    /// The picker asks which *repository*, so a colocated one appears once — as
    /// jj, which is also the default a new space is created through.
    #[test]
    fn the_picker_shows_a_colocated_repository_once_as_its_owner() {
        let mut repos = colocated();

        let listed: Vec<Backend> = repos.filtered_items().iter().map(|r| r.backend()).collect();
        assert_eq!(listed, vec![Backend::Jj]);
        assert_eq!(
            repos.selected_repository().map(|r| r.backend()),
            Some(Backend::Jj),
            "creating on a colocated repo defaults to jj"
        );
    }

    /// A filtered picker must dedupe too, or typing a name brings the second
    /// copy back.
    #[test]
    fn filtering_the_picker_still_shows_a_colocated_repository_once() {
        let mut repos = colocated();
        repos.filter.value = "shan".to_string();

        assert_eq!(repos.filtered_items().len(), 1);
    }

    /// What the create prompt uses to decide whether to explain its choice.
    #[test]
    fn backends_of_reports_every_backend_open_on_a_repository() {
        assert_eq!(
            colocated()
                .store()
                .backends_of(&RepoId::from_path("/repos/shanti")),
            vec![Backend::Jj, Backend::Git]
        );
    }

    /// A scan result swaps the whole set; anything no longer on disk goes with it.
    #[test]
    fn replacing_the_backends_swaps_the_whole_set() {
        let mut repos = colocated();
        repos.replace_backends(vec![
            StubVcs::new("/repos/eclair", Backend::Git, &["main"]).boxed()
        ]);

        let listed: Vec<String> = repos
            .filtered_items()
            .iter()
            .map(|r| r.repo().name.clone())
            .collect();
        assert_eq!(listed.len(), 1);
        assert!(repos
            .store()
            .backends_of(&RepoId::from_path("/repos/shanti"))
            .is_empty());
    }

    /// The same repository arriving from two overlapping repos dirs is one row,
    /// not two.
    #[test]
    fn a_repository_that_arrives_twice_is_listed_once() {
        let mut repos = RepositoriesComponent::new(Vec::new());
        for _ in 0..2 {
            repos.add_repositories(vec![
                StubVcs::new("/repos/shanti", Backend::Jj, &["default"]).boxed(),
                StubVcs::new("/repos/shanti", Backend::Git, &["feature-a"]).boxed(),
            ]);
        }

        assert_eq!(repos.filtered_items().len(), 1, "the picker doubled a repo");
        assert_eq!(
            repos
                .store()
                .backends_of(&RepoId::from_path("/repos/shanti"))
                .len(),
            2,
            "each backend should be open exactly once"
        );
        assert_eq!(repos.collect_spaces().0.len(), 2, "spaces were doubled");
    }
}
