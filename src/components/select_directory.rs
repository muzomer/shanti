use ratatui::{
    layout::{Alignment, Rect},
    style::{
        palette::tailwind::{GREEN, RED, SLATE},
        Style,
    },
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, List, ListDirection, ListItem, ListState, Scrollbar,
        ScrollbarOrientation, ScrollbarState, StatefulWidget,
    },
    Frame,
};

use super::{
    centered,
    list::{ItemOrder, ListComponent},
    Action, AppContext, EventState, HelpEntry, Modal, ModalFlow, SelectCallback,
    POPUP_BORDER_STYLE, SELECTED_STYLE,
};
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
        frame.render_widget(Clear, area);

        let title = Line::from(vec![Span::styled(
            " Select Clone Directory ",
            Style::new().fg(GREEN.c400).bold(),
        )])
        .alignment(Alignment::Center);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(POPUP_BORDER_STYLE)
            .title(title)
            .title_bottom(dir_keybinding_hint());

        let inner_area = block.inner(area);
        frame.render_widget(block, area);

        let total = self.dirs.len();
        let items: Vec<ListItem> = self
            .dirs
            .iter()
            .map(|d| ListItem::new(d.as_str()))
            .collect();
        let list = List::new(items)
            .style(Style::new().white())
            .highlight_style(SELECTED_STYLE)
            .direction(ListDirection::TopToBottom);
        StatefulWidget::render(list, inner_area, frame.buffer_mut(), &mut self.state);

        let mut scroll_state = ScrollbarState::new(total).position(self.state.offset());
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(Style::new().dark_gray())
            .track_style(Style::new().dark_gray());
        frame.render_stateful_widget(scrollbar, inner_area, &mut scroll_state);
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
        // Grow with the list, but never past ten rows plus borders and hint.
        let rows = (self.dirs.len() as u16).min(10) + 4;
        centered(full, Constraint::Percentage(60), Constraint::Length(rows))
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
            HelpEntry::Section("Keybindings"),
            HelpEntry::Binding("j / ↓", "Move down"),
            HelpEntry::Binding("k / ↑", "Move up"),
            HelpEntry::Binding("g / Home", "Go to first"),
            HelpEntry::Binding("G / End", "Go to last"),
            HelpEntry::Binding("Enter", "Clone to selected directory"),
            HelpEntry::Binding("Esc", "Cancel"),
            HelpEntry::Binding("q / Ctrl+C", "Quit"),
        ]
    }
}

fn dir_keybinding_hint() -> Line<'static> {
    Line::from(vec![
        Span::styled("[Enter] ", Style::new().fg(GREEN.c400).bold()),
        Span::styled("select", Style::new().fg(SLATE.c500)),
        Span::styled("  [Esc] ", Style::new().fg(RED.c400).bold()),
        Span::styled("cancel ", Style::new().fg(SLATE.c500)),
    ])
    .right_aligned()
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
