use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Widget},
    Frame,
};

use super::{
    centered, Action, AppContext, ConfirmCallback, EventState, HelpEntry, Modal, ModalFlow,
};
use crate::theme;

/// A generic yes/no dialog. It knows nothing about what "yes" means: the caller
/// hands it the work to run on confirmation, so no `ConfirmAction` discriminant
/// has to be kept in sync anywhere else.
pub struct ConfirmComponent {
    pub title: String,
    pub label: String,
    pub detail: String,
    on_confirm: Option<ConfirmCallback>,
}

impl ConfirmComponent {
    pub fn new(title: String, label: String, detail: String, on_confirm: ConfirmCallback) -> Self {
        Self {
            title,
            label,
            detail,
            on_confirm: Some(on_confirm),
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);

        let title = Line::from(vec![
            Span::styled(" ⚠ ", theme::KEY_DESTRUCTIVE),
            Span::styled(self.title.clone(), theme::TITLE),
            Span::raw(" "),
        ]);

        let outer_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::BORDER_DESTRUCTIVE)
            .style(theme::POPUP_SURFACE)
            .title(title)
            .title_bottom(keybinding_hint());

        let inner_area = outer_block.inner(area);
        outer_block.render(area, frame.buffer_mut());

        let [_, label_area, _, detail_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .horizontal_margin(4)
        .areas(inner_area);

        Paragraph::new(self.label.as_str())
            .style(theme::TEXT.bold())
            .render(label_area, frame.buffer_mut());
        Paragraph::new(format!(" {} ", self.detail))
            .style(
                Style::new()
                    .fg(theme::DANGER)
                    .bg(theme::DESTRUCTIVE_SURFACE)
                    .bold(),
            )
            .render(detail_area, frame.buffer_mut());
    }

    pub fn handle_action(&mut self, _action: Action) -> EventState {
        EventState::NotConsumed
    }
}

impl Modal for ConfirmComponent {
    fn area(&self, full: Rect) -> Rect {
        centered(full, Constraint::Percentage(55), Constraint::Length(8))
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        ConfirmComponent::draw(self, frame, area);
    }

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select => match self.on_confirm.take() {
                // The confirmed work decides where the flow goes next.
                Some(work) => work(ctx),
                None => ModalFlow::Close,
            },
            Action::ClosePopup | Action::ExitInsertMode => ModalFlow::Close,
            _ => self.handle_action(action).into(),
        }
    }

    fn help(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry::Section("Keybindings"),
            HelpEntry::Binding("Enter", "Confirm"),
            HelpEntry::Binding("Esc", "Cancel"),
            HelpEntry::Binding("q / Ctrl+C", "Quit"),
        ]
    }
}

fn keybinding_hint() -> Line<'static> {
    Line::from(vec![
        Span::styled("[Enter] ", theme::KEY_DESTRUCTIVE),
        Span::styled("confirm", theme::MUTED),
        Span::styled("  [Esc] ", theme::KEY_SAFE),
        Span::styled("cancel ", theme::MUTED),
    ])
    .right_aligned()
}
