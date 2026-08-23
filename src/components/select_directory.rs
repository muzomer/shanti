use ratatui::{
    layout::{Alignment, Layout, Rect},
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
    Action, AppContext, EventState, Extent, HelpEntry, Modal, ModalFlow, SelectCallback,
    KEYS_SECTION,
};
use crate::theme;
use ratatui::layout::Constraint;

/// Picks one directory and hands it to the work supplied by the caller, the same
/// deferral [`super::ConfirmComponent`] uses.
pub struct SelectDirectoryComponent {
    pub dirs: Vec<String>,
    state: ListState,
    selected_index: usize,
    on_select: Option<SelectCallback<String>>,
}

impl SelectDirectoryComponent {
    pub fn new(dirs: Vec<String>, on_select: SelectCallback<String>) -> Self {
        Self {
            dirs,
            state: ListState::default().with_selected(Some(0)),
            selected_index: 0,
            on_select: Some(on_select),
        }
    }

    pub fn selected_dir(&self) -> &str {
        &self.dirs[self.selected_index]
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);

        let title = Line::from(vec![
            Span::styled(" ▸ ", theme::KEY),
            Span::styled("Select Clone Directory ", theme::TITLE),
        ])
        .alignment(Alignment::Center);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::BORDER_FOCUSED)
            .style(theme::POPUP_SURFACE)
            .title(title)
            // Read off the same table the help popup shows.
            .title_bottom(footer(
                &footer_entries(&self.help(), KEYS_SECTION),
                area.width,
            ));

        let inner_area = block.inner(area);
        frame.render_widget(block, area);

        // The prompt row says what the choice is *for*; the list below says what
        // the options are. Without it the popup is a bare column of paths.
        //
        // It names git on purpose. `github::clone_repository` always clones with
        // plain git — see the reasoning there — and this is the last screen
        // before that happens, so it is the last chance to make the choice one
        // the user sees rather than one they discover.
        let [prompt_area, list_area, hint_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(inner_area);
        Paragraph::new(Line::from(Span::styled(
            " Clone with git into:",
            theme::SECONDARY,
        )))
        .render(prompt_area, frame.buffer_mut());

        let total = self.dirs.len();
        let items: Vec<ListItem> = self.dirs.iter().map(|d| dir_row(d)).collect();
        let list = List::new(items)
            .style(theme::TEXT)
            .highlight_style(theme::SELECTED_ROW)
            // A marker as well as the band: the band alone disappears on a
            // terminal that ignores background colours.
            .highlight_symbol("▸ ")
            .direction(ListDirection::TopToBottom);
        StatefulWidget::render(list, list_area, frame.buffer_mut(), &mut self.state);

        // Only when there is something off-screen: a full-height track beside a
        // two-item list is chrome that says nothing.
        if total > list_area.height as usize {
            let mut scroll_state = ScrollbarState::new(total)
                .position(self.state.offset())
                .viewport_content_length(list_area.height as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(theme::RULE)
                .track_style(theme::RULE);
            frame.render_stateful_widget(scrollbar, list_area, &mut scroll_state);
        }

        // The way out of the choice above, for the reader it matters to. Drawn
        // only when it fits whole, the same rule the prompt's aside follows: half
        // a command is worse than none, and the prompt row has already said git,
        // so nothing load-bearing is lost on a narrow terminal.
        const HINT: &str = " ↳ for jj: jj git init --colocate";
        if hint_area.width as usize >= HINT.chars().count() {
            Paragraph::new(Line::from(Span::styled(HINT, theme::MUTED)))
                .render(hint_area, frame.buffer_mut());
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

impl Modal for SelectDirectoryComponent {
    fn area(&self, full: Rect) -> Rect {
        // Grow with the list, but never past ten rows plus borders, prompt and
        // hint. `Extent` clips it to the frame from there, so the picker cannot
        // outgrow a short terminal even with a dozen configured directories.
        let rows = (self.dirs.len() as u16).min(10).saturating_add(5);
        popup_area(
            full,
            // Wide enough for an absolute path, which is the whole content.
            Extent::share(60, 38, 110),
            Extent::fixed(rows),
        )
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        SelectDirectoryComponent::draw(self, frame, area);
    }

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select => {
                let dir = self.selected_dir().to_string();
                match self.on_select.take() {
                    Some(work) => work(ctx, dir),
                    None => ModalFlow::Close,
                }
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
            HelpEntry::bind("Enter", "Clone to selected directory").hint("Enter", "select"),
            HelpEntry::bind("? / F1", "Show this help")
                .hint("?", "help")
                .aside(),
            HelpEntry::bind("Esc", "Cancel")
                .hint("Esc", "cancel")
                .safe()
                .essential(),
            HelpEntry::bind("q / Ctrl+C", "Quit"),
        ]
    }
}

/// One row of the picker: the parent path dimmed, the directory itself in full
/// weight.
///
/// Configured repos dirs commonly share a long prefix (`~/src/work`,
/// `~/src/personal`), and the part that differs is the part being chosen. Fading
/// the shared head puts the eye on the tail without hiding anything — the row is
/// still the whole path, character for character.
fn dir_row(dir: &str) -> ListItem<'static> {
    match dir.rsplit_once('/') {
        // A trailing slash leaves nothing to emphasise; show it plainly.
        Some((_, "")) | None => ListItem::new(Span::styled(dir.to_string(), theme::TEXT)),
        Some((parent, name)) => ListItem::new(Line::from(vec![
            Span::styled(format!("{}/", parent), theme::MUTED),
            Span::styled(name.to_string(), theme::TEXT.bold()),
        ])),
    }
}

impl ListComponent<String> for SelectDirectoryComponent {
    fn filtered_items(&mut self) -> Vec<&String> {
        self.dirs.iter().collect()
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
    use ratatui::{backend::TestBackend, Terminal};

    fn screen_at(width: u16, height: u16, dirs: &[&str]) -> String {
        let mut picker = SelectDirectoryComponent::new(
            dirs.iter().map(|d| d.to_string()).collect(),
            Box::new(|_, _| ModalFlow::Close),
        );
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("terminal should init");
        terminal
            .draw(|frame| {
                let area = Modal::area(&picker, frame.area());
                SelectDirectoryComponent::draw(&mut picker, frame, area);
            })
            .expect("draw should succeed");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The clone is plain git (see [`crate::github::clone_repository`]), and this
    /// is the last screen before it runs. Both halves of that — what it does, and
    /// how to move to jj afterwards — have to survive the smallest frame shanti
    /// draws an interface in, or the decision stays one the user only discovers.
    #[test]
    fn the_picker_names_git_and_the_jj_escape_hatch_at_the_size_floor() {
        let screen = screen_at(40, 10, &["/home/u/src/work", "/home/u/src/play"]);
        assert!(
            screen.contains("Clone with git into"),
            "the picker should say the clone is a git clone:\n{}",
            screen
        );
        assert!(
            screen.contains("jj git init --colocate"),
            "the picker should say how to adopt jj afterwards:\n{}",
            screen
        );
    }
}
