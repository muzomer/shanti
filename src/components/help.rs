use ratatui::{
    layout::{Alignment, Margin, Rect},
    style::{
        palette::tailwind::{BLUE, GREEN},
        Style,
    },
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
    Frame,
};

use super::{centered, Action, AppContext, EventState, Modal, ModalFlow};
use ratatui::layout::Constraint;

pub enum HelpEntry {
    Binding(&'static str, &'static str),
    Section(&'static str),
    Blank,
}

pub struct HelpComponent {
    pub entries: Vec<HelpEntry>,
}

impl HelpComponent {
    pub fn new(entries: Vec<HelpEntry>) -> Self {
        Self { entries }
    }

    /// Returns the (width, height) the popup needs, including borders and padding.
    pub fn dimensions(&self) -> (u16, u16) {
        let content_width = self
            .entries
            .iter()
            .map(|e| match e {
                HelpEntry::Binding(key, desc) => key.len().max(12) + desc.len(),
                HelpEntry::Section(title) => title.len(),
                HelpEntry::Blank => 0,
            })
            .max()
            .unwrap_or(0) as u16;
        // borders (2) + horizontal margin (2*2)
        let width = content_width + 6;
        // borders (2) + vertical margin (2*1) + one extra blank line per Section
        let section_count = self
            .entries
            .iter()
            .filter(|e| matches!(e, HelpEntry::Section(_)))
            .count() as u16;
        let height = self.entries.len() as u16 + section_count + 4;
        (width, height)
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        f.render_widget(Clear, area);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(super::POPUP_BORDER_STYLE)
            .title(Line::from(" Help ").style(Style::new().fg(GREEN.c300).bold()))
            .title_alignment(Alignment::Center);
        f.render_widget(block, area);

        let inner = area.inner(Margin {
            horizontal: 2,
            vertical: 1,
        });
        let rows: Vec<Line> = self
            .entries
            .iter()
            .flat_map(|e| match e {
                HelpEntry::Binding(key, desc) => vec![Line::from(vec![
                    Span::styled(format!("{:<12}", key), Style::new().fg(BLUE.c400).bold()),
                    Span::raw(*desc),
                ])],
                HelpEntry::Section(title) => vec![
                    Line::from(Span::styled(
                        *title,
                        Style::new().fg(GREEN.c400).bold().underlined(),
                    )),
                    Line::raw(""),
                ],
                HelpEntry::Blank => vec![Line::raw("")],
            })
            .collect();

        f.render_widget(Paragraph::new(rows), inner);
    }

    pub fn handle_action(&mut self, _action: Action) -> EventState {
        EventState::NotConsumed
    }
}

impl Modal for HelpComponent {
    fn area(&self, full: Rect) -> Rect {
        let (width, height) = self.dimensions();
        centered(full, Constraint::Length(width), Constraint::Length(height))
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        HelpComponent::draw(self, frame, area);
    }

    fn handle(&mut self, action: Action, _ctx: &mut AppContext) -> ModalFlow {
        match action {
            // Consuming `ShowHelp` by closing is what makes '?' a toggle.
            Action::ClosePopup | Action::ExitInsertMode | Action::ShowHelp => ModalFlow::Close,
            _ => self.handle_action(action).into(),
        }
    }
}

/// Keybindings for the worktree list, the one layer that is not a modal.
///
/// There is no Insert-mode variant: `?` is a literal character in Insert mode,
/// so help can only ever be opened from Normal mode. The old
/// `(Worktrees, Insert)` and `(Repositories, Insert)` tables were unreachable
/// for exactly that reason and have been removed rather than left as decoration.
pub fn worktrees_bindings() -> Vec<HelpEntry> {
    vec![
        HelpEntry::Section("Keybindings"),
        HelpEntry::Binding("j / ↓", "Move down"),
        HelpEntry::Binding("k / ↑", "Move up"),
        HelpEntry::Binding("g / Home", "Go to first"),
        HelpEntry::Binding("G / End", "Go to last"),
        HelpEntry::Binding("i / /", "Enter filter mode"),
        HelpEntry::Binding("Tab", "Toggle filter / list"),
        HelpEntry::Binding("n", "New worktree (pick repo)"),
        HelpEntry::Binding("p", "New worktree from PR URL"),
        HelpEntry::Binding("P", "New worktree from PR URL (auto-clone)"),
        HelpEntry::Binding("d", "Delete with confirmation"),
        HelpEntry::Binding("D", "Force delete"),
        HelpEntry::Binding("Enter", "Print path & exit"),
        HelpEntry::Binding("?", "Show this help"),
        HelpEntry::Binding("q / Ctrl+C", "Quit"),
        HelpEntry::Blank,
        HelpEntry::Section("Worktree State"),
        HelpEntry::Binding("✔", "Remote branch exists"),
        HelpEntry::Binding("✘", "Merged / deleted remotely"),
        HelpEntry::Binding("⬆", "Never pushed to remote"),
        HelpEntry::Binding("*", "Dirty working tree"),
    ]
}
