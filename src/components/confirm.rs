use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{
        palette::tailwind::{GREEN, RED, SLATE},
        Style, Stylize,
    },
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Widget},
    Frame,
};

use super::{
    centered, Action, AppContext, ConfirmCallback, EventState, HelpEntry, Modal, ModalFlow,
};

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
            Span::styled(" ⚠ ", Style::new().fg(RED.c400).bold()),
            Span::styled(self.title.clone(), Style::new().fg(GREEN.c300).bold()),
            Span::raw(" "),
        ]);

        let outer_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(super::POPUP_BORDER_STYLE)
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
            .style(Style::new().fg(SLATE.c200).bold())
            .render(label_area, frame.buffer_mut());
        Paragraph::new(format!(" {} ", self.detail))
            .style(Style::new().fg(RED.c300).bg(RED.c950).bold())
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
        Span::styled("[Enter] ", Style::new().fg(RED.c400).bold()),
        Span::styled("confirm", Style::new().fg(SLATE.c500)),
        Span::styled("  [Esc] ", Style::new().fg(GREEN.c400).bold()),
        Span::styled("cancel ", Style::new().fg(SLATE.c500)),
    ])
    .right_aligned()
}
