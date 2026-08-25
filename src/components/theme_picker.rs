//! The popup a colour scheme is actually chosen with.
//!
//! The rule this module exists to enforce: **a scheme is judged on the real
//! interface, not on a swatch**. Moving the cursor installs the highlighted
//! scheme process-wide, so the very next frame repaints the whole application —
//! panes, glyphs, borders, this popup included — in the scheme under the
//! cursor. That is the only honest preview: a palette is a relationship between
//! a dozen colours, and no sample row can show it.
//!
//! Because the preview is a real mutation, the way out has to restore what was
//! there. The modal remembers the [`Theme`] that was in force when it opened —
//! the value, not its name — so `Esc` puts back exactly that, even if the
//! palette came from somewhere the catalogue does not know about.
//!
//! `Enter` keeps what is on screen and writes the scheme's name to the user's
//! configuration file. A failed write is news, not a crash: the scheme stays
//! active for this run and the notification says why the next run will not
//! start with it.

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, List, ListDirection, ListItem, ListState, Paragraph,
        StatefulWidget, Widget,
    },
    Frame,
};

use super::{
    footer_entries,
    list::{ItemOrder, ListComponent},
    popup_area,
    prompt::footer,
    Action, AppContext, EventState, Extent, HelpEntry, Modal, ModalFlow, ModalKind, KEYS_SECTION,
};
use crate::{
    config,
    theme::{self, scheme, Scheme, Theme},
};

/// Picks one entry out of [`scheme::ALL`], previewing it live.
pub struct ThemeModal {
    /// The catalogue, borrowed rather than copied: it is `'static` data and the
    /// picker must offer exactly what the rest of the program accepts.
    schemes: &'static [Scheme],
    state: ListState,
    selected_index: usize,
    /// The palette in force when the popup opened, kept so `Esc` can restore it
    /// exactly.
    original: Theme,
}

impl Default for ThemeModal {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeModal {
    pub fn new() -> Self {
        let original = theme::current();
        // Open on the scheme the user is already looking at, found by comparing
        // palettes rather than by being told a name. The active scheme can have
        // been changed earlier in the session, and no caller carries that; the
        // installed `Theme` is the one thing that is always current.
        let selected_index = scheme::ALL
            .iter()
            .position(|s| s.theme() == original)
            .unwrap_or(0);
        Self {
            schemes: scheme::ALL,
            state: ListState::default().with_selected(Some(selected_index)),
            selected_index,
            original,
        }
    }

    fn selected_scheme(&self) -> &'static Scheme {
        &self.schemes[self.selected_index]
    }

    /// Moves the cursor and installs what it lands on. Movement *is* the
    /// preview, so the two never happen apart.
    fn move_to(&mut self, order: ItemOrder) -> EventState {
        self.select(order);
        theme::set(self.selected_scheme().theme());
        EventState::Consumed
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);

        let title = Line::from(vec![
            Span::styled(" ▸ ", theme::key()),
            Span::styled("Colour Scheme ", theme::title()),
        ])
        .alignment(Alignment::Center);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::border_focused())
            .style(theme::popup_surface())
            .title(title)
            // Read off the same table the help popup shows, so the two can
            // never disagree about what a key does.
            .title_bottom(footer(
                &footer_entries(&self.help(), KEYS_SECTION),
                area.width,
            ));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // The prompt row says what pressing Enter costs: the preview is free and
        // reversible, saving is neither.
        let [prompt_area, list_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        Paragraph::new(Line::from(Span::styled(
            " Preview as you move; Enter saves it",
            theme::secondary(),
        )))
        .render(prompt_area, frame.buffer_mut());

        let items: Vec<ListItem> = self.schemes.iter().map(scheme_row).collect();
        let list = List::new(items)
            .style(theme::text())
            .highlight_style(theme::selected_row())
            // A marker as well as the band: on a terminal that ignores
            // background colours — the very terminal a user is most likely to
            // be hunting for a readable scheme on — the band alone vanishes.
            .highlight_symbol("▸ ")
            .direction(ListDirection::TopToBottom);
        StatefulWidget::render(list, list_area, frame.buffer_mut(), &mut self.state);
    }

    pub fn handle_action(&mut self, action: Action) -> EventState {
        match action {
            Action::MoveDown => self.move_to(ItemOrder::Next),
            Action::MoveUp => self.move_to(ItemOrder::Previous),
            Action::GoFirst => self.move_to(ItemOrder::First),
            Action::GoLast => self.move_to(ItemOrder::Last),
            _ => EventState::NotConsumed,
        }
    }
}

impl Modal for ThemeModal {
    fn kind(&self) -> ModalKind {
        ModalKind::Theme
    }

    fn area(&self, full: Rect) -> Rect {
        // The catalogue is short and fixed, so the popup is sized from it:
        // every scheme visible at once, which is what makes moving through them
        // a comparison rather than a search. `Extent` clips it to the frame.
        let rows = (self.schemes.len() as u16).saturating_add(3);
        popup_area(
            full,
            // Wide enough for the longest label beside its appearance word.
            Extent::share(45, 34, 60),
            Extent::fixed(rows),
        )
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        ThemeModal::draw(self, frame, area);
    }

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select => {
                // The scheme is already installed — Enter only makes it outlive
                // the process. A write that fails therefore costs the *next*
                // run, not this one, and the message says exactly that.
                let name = self.selected_scheme().name;
                if let Err(error) = config::persist_theme(&ctx.args.config_path, name) {
                    ctx.notify
                        .error(format!("{name} is active but could not be saved: {error}"));
                }
                ModalFlow::Close
            }
            // Both ways out of a popup mean the same thing here: undo the
            // preview. Nothing else in the modal has state to discard.
            Action::ClosePopup | Action::ExitInsertMode => {
                theme::set(self.original);
                ModalFlow::Close
            }
            _ => self.handle_action(action).into(),
        }
    }

    fn help(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry::Section(KEYS_SECTION),
            HelpEntry::bind("j / ↓", "Move down (previews the scheme)").hint("j/k", "preview"),
            HelpEntry::bind("k / ↑", "Move up (previews the scheme)"),
            HelpEntry::bind("g / Home", "Go to first"),
            HelpEntry::bind("G / End", "Go to last"),
            HelpEntry::bind("Enter", "Keep it and save it to the config file")
                .hint("Enter", "save"),
            HelpEntry::bind("? / F1", "Show this help")
                .hint("?", "help")
                .aside(),
            HelpEntry::bind("Esc", "Cancel and restore the previous scheme")
                .hint("Esc", "cancel")
                .safe()
                .essential(),
            HelpEntry::bind("q / Ctrl+C", "Quit"),
        ]
    }
}

/// One row: the human label, then whether the scheme is light or dark.
///
/// The appearance is the one property a user needs *before* looking — a dark
/// scheme previewed on a light terminal is a moment of unreadable screen — so
/// it is on the row rather than left to the preview to reveal.
fn scheme_row(scheme: &Scheme) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::styled(scheme.label.to_string(), theme::text()),
        Span::styled(format!("  {}", scheme.appearance.label()), theme::muted()),
    ]))
}

impl ListComponent<Scheme> for ThemeModal {
    fn filtered_items(&mut self) -> Vec<&Scheme> {
        self.schemes.iter().collect()
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

    /// The popup opens on the scheme that is actually installed, whatever put
    /// it there — that is what makes the first `j` a step *away* from the
    /// current look rather than a jump to the top of the list.
    #[test]
    fn it_opens_on_the_active_scheme() {
        let _guard = theme::test_lock();
        let gruvbox = scheme::find("gruvbox-dark").expect("catalogue entry");
        theme::set(gruvbox.theme());

        let picker = ThemeModal::new();
        assert_eq!(picker.selected_scheme().name, "gruvbox-dark");

        theme::set(Theme::default());
    }

    /// Moving is previewing: the palette the whole process draws with follows
    /// the cursor, with no extra key.
    #[test]
    fn moving_installs_the_highlighted_scheme() {
        let _guard = theme::test_lock();
        theme::set(Theme::default());
        let mut picker = ThemeModal::new();

        picker.handle_action(Action::GoLast);
        let last = scheme::ALL.last().expect("a non-empty catalogue");
        assert_eq!(theme::current(), last.theme());

        picker.handle_action(Action::GoFirst);
        assert_eq!(theme::current(), scheme::ALL[0].theme());

        theme::set(Theme::default());
    }
}
