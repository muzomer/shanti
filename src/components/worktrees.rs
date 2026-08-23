use std::collections::HashSet;
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
    filter::FilterComponent, footer_entries, prompt::footer, worktrees_bindings, Action,
    EventState, RepositoriesComponent, FILTER_SECTION, KEYS_SECTION, MIN_HEIGHT, MIN_WIDTH,
};
use crate::keymap::InputMode;
use crate::theme;
use crate::vcs::{Backend, RepoId, Space};

/// The frames of the scan spinner, advanced one per app tick (10fps).
const SPINNER: [&str; 10] = [
    "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280f}",
];

/// Identifies the row the user is on, so a rebuild can put them back on it.
///
/// The path and the backend, never the name: a colocated repository can hold a
/// git worktree and a jj workspace of the same name, and only the pair tells
/// those two rows apart.
type RowKey = (Backend, PathBuf);

/// A repository scan in flight, as the list shows it.
struct ScanProgress {
    /// Repositories the scan has reported so far.
    found: usize,
    /// Which spinner frame is on screen. Advanced by [`WorktreesComponent::tick`]
    /// from the app's existing clock — the list owns no timer of its own.
    frame: usize,
}

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
    ///
    /// Deliberately the *unpadded* text: the column padding [`RowLayout`] adds
    /// is a property of the frame, not of the row, and a haystack that changed
    /// width with the terminal would make a query match or miss depending on
    /// how wide the window happened to be.
    fn text(&self) -> String {
        match &self.space {
            Some(space) => format!("{}{}{}", self.repo, SEPARATOR, space),
            None => self.repo.clone(),
        }
    }
}

/// Between the repository column and the space column. Also the separator in
/// the filter haystack, so what the user reads is what they can type at.
const SEPARATOR: &str = " / ";

/// Width of the backend tag column: the widest [`Backend::label`], fixed rather
/// than measured, so a list of only jj rows does not shift sideways the moment
/// a git row arrives.
const BACKEND_WIDTH: usize = 3;

/// Everything left of the repository name: two status cells, a gap, the backend
/// tag, a gap.
const PREFIX_WIDTH: usize = 2 + 1 + BACKEND_WIDTH + 1;

/// The width of `text` on screen.
fn text_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// `text` cut to `max` cells, keeping the head and marking the cut with `…`.
fn clip_end(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut width = 0;
    for c in text.chars() {
        let w = text_width(&c.to_string());
        if width + w > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        width += w;
    }
    out.push('\u{2026}');
    out
}

/// `text` cut to `max` cells, keeping the *tail*.
///
/// The repository column is right-aligned against the separator, so its last
/// characters are the ones sitting in the column; dropping the head keeps the
/// alignment honest and keeps the half of the name nearest the space it belongs
/// to.
fn clip_start(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_owned();
    }
    let mut tail = String::new();
    let mut width = 0;
    for c in text.chars().rev() {
        let w = text_width(&c.to_string());
        if width + w > max.saturating_sub(1) {
            break;
        }
        tail.push(c);
        width += w;
    }
    let mut out = String::from('\u{2026}');
    out.extend(tail.chars().rev());
    out
}

/// `text` pushed to the right of a `width`-wide column.
fn pad_start(text: &str, width: usize) -> String {
    let mut out = " ".repeat(width.saturating_sub(text_width(text)));
    out.push_str(text);
    out
}

/// The column widths this frame's rows are drawn in.
///
/// Measured once per frame from the rows that are actually on screen, rather
/// than fixed: a list of short names should not reserve a column for a long one
/// that is filtered out. Measured from *all* filtered rows rather than the ones
/// currently scrolled into view, so scrolling never shifts the columns
/// underneath the user.
#[derive(Debug, PartialEq, Eq)]
struct RowLayout {
    /// Cells for the repository name, right-aligned against the separator.
    repo: usize,
    /// Cells for the space name. Zero when no row on screen names a space —
    /// then there is no separator and no second column at all.
    space: usize,
}

impl RowLayout {
    /// Splits `width` between the two name columns.
    ///
    /// Each column asks for what its longest name needs. When both fit, both get
    /// it. When they do not, whichever fits in half the space keeps its full
    /// width and the other takes the remainder — so one very long repository
    /// name costs the space names at most half the row, instead of pushing them
    /// off the edge as the old free-flowing layout did.
    fn measure<'a>(labels: impl Iterator<Item = &'a RowLabel> + Clone, width: u16) -> RowLayout {
        let budget = (width as usize).saturating_sub(PREFIX_WIDTH);
        let repo_natural = labels
            .clone()
            .map(|label| text_width(&label.repo))
            .max()
            .unwrap_or(0);
        let Some(space_natural) = labels
            .filter_map(|label| label.space.as_deref())
            .map(text_width)
            .max()
        else {
            // Every row is a repository's default space, so there is no second
            // column to line anything up against.
            return RowLayout {
                repo: budget,
                space: 0,
            };
        };

        let avail = budget.saturating_sub(text_width(SEPARATOR));
        let half = avail / 2;
        let repo = if repo_natural + space_natural <= avail || repo_natural <= half {
            repo_natural
        } else if space_natural <= avail - half {
            avail - space_natural
        } else {
            half
        };
        RowLayout {
            repo,
            space: avail - repo,
        }
    }
}

pub struct WorktreesComponent {
    spaces: Vec<SpaceEntry>,
    filter: FilterComponent,
    state: ListState,
    focus: Focus,
    selected_index: Option<usize>,
    /// `Some` while repositories are still being discovered.
    scan: Option<ScanProgress>,
    /// Whether a scan has ever reported to this list.
    ///
    /// Without it an empty list at startup is indistinguishable from an empty
    /// list after a completed scan, and only the second one is entitled to say
    /// that nothing was found.
    scanned: bool,
    /// How many repositories the last scan reported. The one thing that tells
    /// "your configuration found nothing" apart from "your repositories have no
    /// spaces yet".
    repos_seen: usize,
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
            scan: None,
            scanned: false,
            repos_seen: 0,
            last_error: None,
        }
    }

    /// Replaces every row, keeping the user where they were.
    ///
    /// The filter text, the cursor in it and the focused pane are all left
    /// alone, because none of them is about the *data*: rebuilding the whole
    /// component instead — which is what this replaces — silently threw away a
    /// filter the user had typed, and could do so at any moment, since the
    /// rebuild is triggered by a background job rather than by a keystroke.
    pub fn set_spaces(&mut self, spaces: Vec<SpaceEntry>) {
        let anchor = self.selected_key();
        self.spaces = spaces;
        self.restore_selection(anchor);
    }

    /// Adds the rows of a batch of repositories, replacing whatever those same
    /// repositories had contributed before.
    ///
    /// Per repository rather than a plain append so the operation is
    /// idempotent: two repos dirs may overlap, and the same repository arriving
    /// from a second scan must update its rows, not double them.
    pub fn extend(&mut self, entries: Vec<SpaceEntry>) {
        let anchor = self.selected_key();
        let arriving: HashSet<RepoId> = entries.iter().map(|e| e.space.repo.clone()).collect();
        self.spaces.retain(|e| !arriving.contains(&e.space.repo));
        self.spaces.extend(entries);
        self.restore_selection(anchor);
    }

    /// Says a scan is running and how many repositories it has found; `None`
    /// means it is over and the spinner goes away.
    pub fn set_scan(&mut self, found: Option<usize>) {
        // Taken as reported rather than accumulated: the count is cumulative
        // within a scan and starts again at zero when a new one begins, so a
        // rescan that now finds nothing must be able to say so. The last count
        // survives the scan ending, which is what the empty state reads.
        if let Some(found) = found {
            self.scanned = true;
            self.repos_seen = found;
        }
        match (found, &mut self.scan) {
            (Some(found), Some(scan)) => scan.found = found,
            (Some(found), slot) => *slot = Some(ScanProgress { found, frame: 0 }),
            (None, slot) => *slot = None,
        }
    }

    /// Advances anything time-dependent by one frame of the app's clock.
    ///
    /// Driven by the tick the event loop already emits: a second timer would be
    /// a second thing to stop, and a spinner that outlived its scan.
    pub fn tick(&mut self) {
        if let Some(scan) = &mut self.scan {
            scan.frame = scan.frame.wrapping_add(1);
        }
    }

    /// Why the pane is empty, in the order the answers become knowable: a scan
    /// in flight has not finished being wrong yet, a filter is the user's own
    /// doing, and only a completed scan may speak about repositories at all.
    fn empty_state(&self) -> EmptyState {
        if self.scan.is_some() {
            EmptyState::Scanning
        } else if !self.filter.value.is_empty() {
            EmptyState::Filtered
        } else if !self.scanned {
            EmptyState::Unknown
        } else if self.repos_seen == 0 {
            EmptyState::NoRepositories
        } else {
            EmptyState::NoSpaces {
                repos: self.repos_seen,
            }
        }
    }

    /// The row the user is on, as something a rebuilt list can be searched for.
    fn selected_key(&mut self) -> Option<RowKey> {
        let index = self.selected_index?;
        self.filtered_items()
            .get(index)
            .map(|entry| (entry.space.backend, entry.space.path.clone()))
    }

    /// Puts the selection back on `anchor` if that row survived the rebuild.
    ///
    /// If it did not, the *position* is kept instead (clamped): rows arriving
    /// underneath the user must never scroll them somewhere else, and a row
    /// that was deleted leaves the cursor where the list continues.
    fn restore_selection(&mut self, anchor: Option<RowKey>) {
        let rows = self.filtered_items().len();
        if rows == 0 {
            self.selected_index = None;
            self.state.select(None);
            return;
        }

        let mut index = None;
        if let Some(key) = anchor {
            index = self
                .filtered_items()
                .iter()
                .position(|entry| (entry.space.backend, entry.space.path.clone()) == key);
        }
        let index = index.unwrap_or_else(|| self.selected_index.unwrap_or(0).min(rows - 1));
        self.selected_index = Some(index);
        self.state.select(Some(index));
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

        // B: cap current to total so a stale selected_index never shows x > y in (x/y)
        let current = self.selected_index.map(|i| (i + 1).min(total)).unwrap_or(0);

        let in_filter =
            is_active && matches!(mode, InputMode::Insert) && matches!(self.focus, Focus::Filter);

        // The bottom border has two zones, and this is where that is decided
        // once: the left says *what is going on* — the input mode, or the newest
        // thing that went wrong — and the right says *what the keys do*. They
        // cannot collide, because the status zone is measured first and capped
        // at half the border, and the footer is fitted into what is left.
        let (status, status_width) = self.status_zone(mode, rect.width);
        // Which half of the table applies is the mode's business, but only while
        // the pane actually owns the keyboard: with a popup on top the pane is
        // showing the keys it will offer again once the popup closes.
        let bindings = worktrees_bindings();
        let section = if in_filter {
            FILTER_SECTION
        } else {
            KEYS_SECTION
        };
        let keys = footer(
            &footer_entries(&bindings, section),
            rect.width.saturating_sub(status_width),
        );

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
            // The one thing on screen that says shanti is still working. The
            // count is what makes it more than decoration: a scan that has found
            // nothing for ten seconds looks different from one that is streaming.
            if let Some(scan) = &self.scan {
                spans.push(Span::styled(
                    format!("{} ", SPINNER[scan.frame % SPINNER.len()]),
                    theme::TITLE,
                ));
                spans.push(Span::styled(
                    format!("scanning\u{2026} {} repos ", scan.found),
                    theme::SECONDARY,
                ));
            }
            Line::from(spans)
        };

        let block = Block::bordered()
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(theme::BORDER)
            .style(theme::CANVAS)
            .title(title)
            .title_bottom(status)
            .title_bottom(keys);

        // A: render the block frame first, then lay out filter + list inside its inner area
        let inner_area = block.inner(rect);
        f.render_widget(block, rect);

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

        // The scrollbar is drawn *over* the right-hand column of the list, so
        // the row has to give it up before the columns are measured — otherwise
        // the last character of the longest space name disappears under the
        // track exactly when the list is long enough to need one.
        let scrolls = total > list_area.height as usize;
        let row_width = list_area.width.saturating_sub(u16::from(scrolls));
        let cols = RowLayout::measure(display_data.iter().map(|(_, label)| label), row_width);
        let items: Vec<ListItem<'static>> = display_data
            .iter()
            .map(|(space, label)| space_to_list_item(space, label, &cols))
            .collect();

        let list = List::new(items)
            .style(theme::TEXT)
            .highlight_style(theme::SELECTED_ROW)
            .direction(ratatui::widgets::ListDirection::TopToBottom);
        StatefulWidget::render(list, list_area, f.buffer_mut(), &mut self.state);

        // An empty pane is exactly what a hung program looks like, and during a
        // scan the pane *is* empty for a moment. One line says which of the two
        // this is.
        if total == 0 {
            let lines = self.empty_state().lines();
            let height = (lines.len() as u16).min(list_area.height);
            let [area] = Layout::vertical([Constraint::Length(height)])
                .flex(ratatui::layout::Flex::Center)
                .areas(list_area);
            f.render_widget(
                Paragraph::new(lines).centered().wrap(Wrap { trim: true }),
                area,
            );
        }

        // Only when there are rows off-screen: a full-height track beside a list
        // that already fits is chrome that says nothing, and on a short terminal
        // it is a whole column spent saying it.
        if scrolls {
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

    /// The left zone of the bottom border, and how many columns it claims.
    ///
    /// One line, one voice: the input mode is what there is to say when nothing
    /// has gone wrong, and an error takes the slot while it is the newer news.
    /// `shanti-nbt.3` replaces `last_error` with a notification queue — it
    /// inherits this zone and this cap rather than opening a third one, because
    /// the bottom border only has two ends and the right one is spoken for.
    ///
    /// The cap is half the border. A message is never a reason to stop telling
    /// the user which key gets them out of the situation it is reporting, so the
    /// footer keeps its half and a long error is elided instead.
    fn status_zone(&self, mode: InputMode, width: u16) -> (Line<'static>, u16) {
        let Some(error) = &self.last_error else {
            return match mode {
                InputMode::Normal => (Line::from(" NORMAL ").style(theme::SUCCESS_TEXT), 8),
                InputMode::Insert => (Line::from(" INSERT ").style(theme::WARNING_TEXT), 8),
            };
        };
        // Two for the spaces that keep the text off the border's corners.
        let text = elide(error, (width / 2).saturating_sub(2) as usize);
        let claimed = text.chars().count() as u16 + 2;
        (
            Line::from(format!(" {} ", text)).style(theme::DANGER_TEXT),
            claimed,
        )
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

/// One row of the table: the two status slots, the backend tag, then the label
/// — `<repo> / <space>`, or just `<repo>` when the space is the repository's
/// default one.
///
/// Everything left of the space name sits in a fixed-width column, and the
/// repository name is right-aligned inside its own, so ` / ` falls in the same
/// place on every row and the eye can run down either name column. Right
/// alignment rather than left is what makes that possible without a ragged gap
/// before the separator.
///
/// The renderer is deliberately dumb about state — it asks the status for its
/// glyphs and maps tones to colours. Matching on the backend here is what would
/// force every new jj state to be taught to the UI as well.
fn space_to_list_item(space: &Space, label: &RowLabel, cols: &RowLayout) -> ListItem<'static> {
    ListItem::new(space_row(space, label, cols))
}

/// The row itself, as styled spans. Split from [`space_to_list_item`] so a test
/// can read back what a row says without going through a terminal.
fn space_row(space: &Space, label: &RowLabel, cols: &RowLayout) -> Line<'static> {
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
        format!("{:<width$} ", space.backend.label(), width = BACKEND_WIDTH),
        theme::MUTED,
    ));

    match (&label.space, cols.space) {
        // The space is what tells this row from its siblings, so it takes the
        // emphasis and the repository recedes to context.
        (Some(name), space_width) if space_width > 0 => {
            spans.push(Span::styled(
                pad_start(&clip_start(&label.repo, cols.repo), cols.repo),
                theme::SECONDARY,
            ));
            spans.push(Span::styled(SEPARATOR, theme::MUTED));
            spans.push(Span::styled(
                clip_end(name, space_width),
                theme::TEXT.add_modifier(Modifier::BOLD),
            ));
        }
        // Nothing follows it, so the repository name *is* the row's subject and
        // takes the emphasis rather than reading as a dimmed prefix to nothing.
        //
        // Padded to the repository column so its right edge lands on the same
        // spine as the separators around it, but allowed to run past that spine
        // when it is longer: the rest of the row is empty on a row like this, so
        // clipping the name to the column would throw away characters to line up
        // with nothing.
        (_, space_width) => {
            let room = cols.repo
                + if space_width > 0 {
                    text_width(SEPARATOR) + space_width
                } else {
                    0
                };
            let name = clip_end(&label.repo, room);
            // With no space column there is no spine to align against, so the
            // names start at the left instead of floating away from the tag.
            let name = if space_width > 0 {
                pad_start(&name, cols.repo)
            } else {
                name
            };
            spans.push(Span::styled(name, theme::TEXT.add_modifier(Modifier::BOLD)));
        }
    }

    Line::from(spans)
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
/// Why the list is empty — which decides what the pane says instead of rows.
///
/// An empty pane is an instruction, not a void: each of these is a different
/// situation with a different next step, and the old single "no spaces yet"
/// answered for all of them at once.
#[derive(Debug, PartialEq, Eq)]
enum EmptyState {
    /// Repositories are still being discovered; anything else would be a lie
    /// told one frame too early.
    Scanning,
    /// There are rows, but the filter excludes all of them.
    Filtered,
    /// Nothing has been scanned yet, so nothing can be claimed.
    Unknown,
    /// The scan finished and found no repository at all — which almost always
    /// means the repos dir is not where shanti was told to look.
    NoRepositories,
    /// Repositories were found and none of them has a space yet. Everything
    /// works; the user simply has not made one.
    NoSpaces { repos: usize },
}

impl EmptyState {
    /// The notice, as an accented headline followed by muted detail.
    ///
    /// Every line is kept short enough to sit inside the 40-column floor
    /// unwrapped, because the wrap that would otherwise save them costs a row
    /// the centring has not reserved.
    fn lines(&self) -> Vec<Line<'static>> {
        let headline = |text: &'static str| Line::from(Span::styled(text, theme::TITLE));
        let detail = |text: String| Line::from(Span::styled(text, theme::MUTED));

        match self {
            EmptyState::Scanning => vec![detail("scanning for repositories\u{2026}".to_owned())],
            EmptyState::Filtered => vec![
                headline("nothing matches the filter"),
                detail("press / to change it".to_owned()),
            ],
            EmptyState::Unknown => vec![detail("no spaces yet".to_owned())],
            // The paths themselves would say more than the names of the
            // settings, and `cli::Args` holds them along with the `Origin` each
            // came from — but the list is never handed the arguments, and
            // resolving the repos dir a second time here would put the
            // precedence rules in two places. Naming the three inputs, in the
            // order that wins, is the honest fallback until `App` passes them in.
            EmptyState::NoRepositories => vec![
                headline("no repositories found"),
                detail("nothing was found in the repos dir".to_owned()),
                detail("set it with --repos-dir,".to_owned()),
                detail("SHANTI_REPOS_DIR or config.toml".to_owned()),
            ],
            EmptyState::NoSpaces { repos } => vec![
                headline("no spaces yet"),
                detail(format!(
                    "{} {}, none with a space",
                    repos,
                    if *repos == 1 {
                        "repository"
                    } else {
                        "repositories"
                    }
                )),
                detail("press n to create one".to_owned()),
            ],
        }
    }
}

/// `text` cut to `budget` characters, with an ellipsis standing for what was
/// cut.
///
/// Counted in characters rather than bytes, so a path with an accent in it does
/// not slice a `char` in half. A budget of nothing yields nothing: at that point
/// the border has no room for a message at all.
fn elide(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_owned();
    }
    match budget {
        0 => String::new(),
        _ => text
            .chars()
            .take(budget - 1)
            .chain(std::iter::once('\u{2026}'))
            .collect(),
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

    /// The row as plain text, so a test can see where the columns fall.
    fn render(entry: &SpaceEntry, cols: &RowLayout) -> String {
        space_row(&entry.space, &entry.row_label(), cols)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn layout(rows: &[SpaceEntry], width: u16) -> RowLayout {
        let labels: Vec<RowLabel> = rows.iter().map(|entry| entry.row_label()).collect();
        RowLayout::measure(labels.iter(), width)
    }

    /// Which cell the separator sits in — cells, not bytes, since the status
    /// glyphs left of it are not ASCII.
    fn spine(row: &str) -> Option<usize> {
        row.find(SEPARATOR).map(|at| text_width(&row[..at]))
    }

    /// The point of the table: the separator falls in the same cell on every
    /// row, however long the names either side of it are.
    #[test]
    fn the_separator_falls_in_one_column_on_every_row() {
        let rows = vec![
            entry("a", Backend::Git, "one", "/spaces/a/one"),
            entry("much-longer-name", Backend::Jj, "two", "/spaces/m/two"),
        ];
        let cols = layout(&rows, 80);
        let first = spine(&render(&rows[0], &cols));
        let second = spine(&render(&rows[1], &cols));
        assert!(first.is_some(), "the separator is missing");
        assert_eq!(first, second, "the rows do not line up");
    }

    /// A row that names no space still ends on the spine, so a mixed list reads
    /// as one table rather than two.
    #[test]
    fn a_default_row_ends_where_the_separator_would_start() {
        let rows = vec![
            entry("alpha", Backend::Jj, "default", "/repos/alpha"),
            entry("beta", Backend::Git, "feat-x", "/spaces/beta/feat-x"),
        ];
        let cols = layout(&rows, 80);
        let default_row = render(&rows[0], &cols);
        let named_row = render(&rows[1], &cols);
        assert!(
            default_row.ends_with("alpha"),
            "the default row should say the repository and stop: {default_row:?}"
        );
        assert_eq!(
            text_width(&default_row),
            spine(&named_row).expect("a separator"),
            "the default row does not end on the spine"
        );
    }

    /// The shape the app-level tests read: the backend tag comes first, and the
    /// label still reads `<repo> / <space>` with the separator against the
    /// repository name.
    #[test]
    fn a_row_names_its_backend_before_its_label() {
        let rows = vec![entry(
            "alpha",
            Backend::Git,
            "feat-x",
            "/spaces/alpha/feat-x",
        )];
        let row = render(&rows[0], &layout(&rows, 80));
        let (before, after) = row.split_once("alpha /").expect("the label is intact");
        assert!(
            before.contains("git"),
            "the backend tag is missing: {row:?}"
        );
        assert_eq!(after.trim(), "feat-x");
    }

    /// Right alignment is what lets the repository column be padded without
    /// opening a ragged gap in front of the separator.
    #[test]
    fn a_short_repository_name_is_pushed_up_against_the_separator() {
        let rows = vec![
            entry("a", Backend::Git, "one", "/spaces/a/one"),
            entry("bbbb", Backend::Git, "two", "/spaces/b/two"),
        ];
        let cols = layout(&rows, 80);
        assert_eq!(cols.repo, 4, "the column should fit the longest name");
        let short = render(&rows[0], &cols);
        assert!(
            short.contains("   a / one"),
            "the short name should be right-aligned: {short:?}"
        );
    }

    /// Both columns fit, so neither is trimmed and the rest of the row is left
    /// to the space names.
    #[test]
    fn columns_take_only_what_the_names_need() {
        let rows = vec![entry("alpha", Backend::Git, "feature-one", "/s/a/one")];
        let cols = layout(&rows, 80);
        assert_eq!(cols.repo, "alpha".len());
        assert_eq!(
            cols.space,
            80 - PREFIX_WIDTH - text_width(SEPARATOR) - cols.repo
        );
    }

    /// At the 40-column floor a very long repository name costs the space names
    /// at most half the room, and both cuts are marked rather than silent.
    #[test]
    fn at_the_minimum_width_one_long_name_cannot_crowd_the_other_out() {
        // The 40-column floor, less the two border cells the pane draws.
        let width = MIN_WIDTH - 2;
        let rows = vec![entry(
            "an-extremely-long-repository-name",
            Backend::Git,
            "an-extremely-long-space-name",
            "/s/x/y",
        )];
        let cols = layout(&rows, width);
        let avail = width as usize - PREFIX_WIDTH - text_width(SEPARATOR);
        assert_eq!(
            cols.repo,
            avail / 2,
            "the repository column took more than half"
        );
        assert!(cols.space >= avail / 2, "the space column was crowded out");

        let row = render(&rows[0], &cols);
        assert_eq!(
            row.matches('\u{2026}').count(),
            2,
            "both names should be marked as cut: {row:?}"
        );
        assert!(
            text_width(&row) <= width as usize,
            "the row overflows the pane: {row:?}"
        );
    }

    /// The head of a repository name is what gets dropped, because the tail is
    /// the half sitting against the space it belongs to.
    #[test]
    fn a_clipped_name_says_which_end_was_cut() {
        assert_eq!(clip_start("abcdefgh", 4), "\u{2026}fgh");
        assert_eq!(clip_end("abcdefgh", 4), "abc\u{2026}");
        assert_eq!(clip_end("abc", 4), "abc", "a name that fits is left alone");
    }

    /// With no space column there is no spine, so the names start at the left
    /// instead of drifting away from the backend tag.
    #[test]
    fn a_list_of_only_default_spaces_is_left_aligned() {
        let rows = vec![
            entry("alpha", Backend::Jj, "default", "/repos/alpha"),
            entry("b", Backend::Jj, "main", "/repos/b"),
        ];
        let cols = layout(&rows, 80);
        assert_eq!(cols.space, 0, "there is no second column to line up");
        for row in &rows {
            let text = render(row, &cols);
            assert!(
                text.ends_with(&row.repo_name),
                "the name should not be padded: {text:?}"
            );
        }
    }

    /// The haystack is the label, never the padded row: a filter that matched or
    /// missed depending on the width of the window would be unusable.
    #[test]
    fn the_filter_matches_the_label_not_the_padding() {
        let rows = vec![entry("a", Backend::Git, "one", "/spaces/a/one")];
        let padded = render(&rows[0], &layout(&rows, 80));
        assert_eq!(rows[0].label(), "a / one");
        assert!(padded.contains("a / one"), "{padded:?}");
        assert_ne!(padded.trim(), rows[0].label(), "the row is padded");
    }

    /// Nothing found and nothing created are different situations with
    /// different fixes, and one message cannot answer for both.
    #[test]
    fn an_empty_list_says_which_kind_of_empty_it_is() {
        let mut component = WorktreesComponent::new(Vec::new());
        assert_eq!(
            component.empty_state(),
            EmptyState::Unknown,
            "nothing has been scanned, so nothing may be claimed"
        );

        component.set_scan(Some(0));
        assert_eq!(component.empty_state(), EmptyState::Scanning);

        // The scan ended having found nothing: the configuration is the suspect.
        component.set_scan(None);
        assert_eq!(component.empty_state(), EmptyState::NoRepositories);

        // A second scan finds repositories, none of which has a space.
        component.set_scan(Some(3));
        component.set_scan(None);
        assert_eq!(component.empty_state(), EmptyState::NoSpaces { repos: 3 });
    }

    /// A filter the user typed is their own doing, and says so rather than
    /// blaming the configuration.
    #[test]
    fn a_filter_that_matches_nothing_is_not_reported_as_a_missing_repository() {
        let mut component =
            WorktreesComponent::new(vec![entry("alpha", Backend::Git, "one", "/s/a/one")]);
        component.set_scan(Some(1));
        component.set_scan(None);
        type_filter(&mut component, "zzz");
        assert_eq!(component.empty_state(), EmptyState::Filtered);
    }

    /// The no-repos notice names the settings that decide where shanti looks;
    /// the no-spaces notice names the key that makes one.
    #[test]
    fn each_empty_state_says_what_to_do_next() {
        let text = |state: EmptyState| {
            state
                .lines()
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let no_repos = text(EmptyState::NoRepositories);
        assert!(no_repos.contains("no repositories found"), "{no_repos}");
        assert!(no_repos.contains("--repos-dir"), "{no_repos}");
        assert!(no_repos.contains("SHANTI_REPOS_DIR"), "{no_repos}");

        let no_spaces = text(EmptyState::NoSpaces { repos: 1 });
        assert!(no_spaces.contains("no spaces yet"), "{no_spaces}");
        assert!(no_spaces.contains("1 repository,"), "{no_spaces}");
        assert!(no_spaces.contains("press n to create one"), "{no_spaces}");

        assert!(text(EmptyState::NoSpaces { repos: 2 }).contains("2 repositories,"));
    }

    /// Every notice has to fit the 40-column floor unwrapped, because the
    /// centring reserves exactly as many rows as there are lines.
    #[test]
    fn every_empty_notice_fits_the_minimum_size() {
        let inner = (MIN_WIDTH - 2) as usize;
        for state in [
            EmptyState::Scanning,
            EmptyState::Filtered,
            EmptyState::Unknown,
            EmptyState::NoRepositories,
            EmptyState::NoSpaces { repos: 12 },
        ] {
            let lines = state.lines();
            assert!(
                lines.len() <= (MIN_HEIGHT - 2) as usize,
                "{state:?} is taller than the pane"
            );
            for line in lines {
                assert!(
                    line.width() <= inner,
                    "{state:?} has a line wider than {inner} cells: {line:?}"
                );
            }
        }
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

    /// The wart this replaces: rebuilding the component threw the filter away.
    /// A background job may rebuild at any moment, so it must cost the user
    /// nothing they typed.
    #[test]
    fn replacing_the_rows_keeps_the_filter_and_the_selected_row() {
        let mut component = WorktreesComponent::new(vec![
            entry("alpha", Backend::Git, "feature-one", "/spaces/alpha/one"),
            entry("beta", Backend::Git, "feature-two", "/spaces/beta/two"),
        ]);
        type_filter(&mut component, "feature");
        component.handle_action(Action::MoveDown);
        let before = component.selected_space().expect("a row is selected").path;

        component.set_spaces(vec![
            entry("alpha", Backend::Git, "feature-one", "/spaces/alpha/one"),
            entry("beta", Backend::Git, "feature-two", "/spaces/beta/two"),
            entry(
                "gamma",
                Backend::Git,
                "feature-three",
                "/spaces/gamma/three",
            ),
        ]);

        assert_eq!(component.filter.value, "feature", "the filter was lost");
        assert_eq!(
            component.filtered_items().len(),
            3,
            "the new row is missing"
        );
        assert_eq!(
            component.selected_space().expect("still selected").path,
            before,
            "the user was moved off their row"
        );
    }

    /// A row that is gone leaves the cursor where the list continues, rather
    /// than jumping back to the top.
    #[test]
    fn a_row_that_vanished_leaves_the_cursor_in_place() {
        let mut component = WorktreesComponent::new(vec![
            entry("alpha", Backend::Git, "one", "/spaces/alpha/one"),
            entry("beta", Backend::Git, "two", "/spaces/beta/two"),
            entry("gamma", Backend::Git, "three", "/spaces/gamma/three"),
        ]);
        component.handle_action(Action::MoveDown);

        component.set_spaces(vec![
            entry("alpha", Backend::Git, "one", "/spaces/alpha/one"),
            entry("gamma", Backend::Git, "three", "/spaces/gamma/three"),
        ]);

        assert_eq!(
            component.selected_space().expect("still selected").name,
            "three",
            "the cursor should hold its position, not reset"
        );
    }

    /// Streaming: a batch adds its repositories' rows and replaces only those.
    #[test]
    fn a_batch_replaces_the_rows_of_its_own_repositories_only() {
        let mut component = WorktreesComponent::new(vec![entry(
            "alpha",
            Backend::Git,
            "one",
            "/spaces/alpha/one",
        )]);

        component.extend(vec![entry("beta", Backend::Git, "two", "/spaces/beta/two")]);
        assert_eq!(component.filtered_items().len(), 2);

        // The same repository arriving again — overlapping repos dirs, or a
        // second scan — updates its rows instead of doubling them.
        component.extend(vec![
            entry("alpha", Backend::Git, "one", "/spaces/alpha/one"),
            entry("alpha", Backend::Git, "three", "/spaces/alpha/three"),
        ]);
        let labels: Vec<String> = component
            .filtered_items()
            .iter()
            .map(|entry| entry.label())
            .collect();
        assert_eq!(
            labels,
            vec!["alpha / one", "alpha / three", "beta / two"],
            "a repository was listed twice"
        );
    }
}
