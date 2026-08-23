//! The keybinding reference, and the one popup whose size is dictated by its
//! own content rather than by a share of the frame.
//!
//! Because of that it is also the one popup that can want more rows than the
//! terminal has, so it is the one that scrolls. Everything else degrades by
//! giving up padding; this gives up nothing and moves the window instead.

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};

use super::{
    gutter, popup_area, prompt::footer, Action, AppContext, EventState, Extent, Modal, ModalFlow,
};
use crate::theme;

/// Width of the key column. Wide enough for `q / Ctrl+C`, and fixed so every
/// description starts on the same column — that alignment is what makes the
/// popup scannable rather than a list of sentences.
const KEY_COLUMN: usize = 12;

/// A key padded out to the gutter, always with at least one space after it.
///
/// A binding wider than the gutter (`↑ / Ctrl+K / Ctrl+P`) would otherwise run
/// straight into its own description. Counted in characters, not bytes, so the
/// arrow glyphs do not shift the column.
fn key_cell(key: &str) -> String {
    let pad = KEY_COLUMN.saturating_sub(key.chars().count()).max(1);
    format!("{}{}", key, " ".repeat(pad))
}

pub enum HelpEntry {
    Binding(&'static str, &'static str),
    Section(&'static str),
    Blank,
}

pub struct HelpComponent {
    pub entries: Vec<HelpEntry>,
    /// First visible row. Only ever non-zero on a terminal too short for the
    /// whole table.
    pub(super) scroll: u16,
    /// Rows the last frame could show, and rows it had. Remembered because
    /// scrolling is clamped when the key is pressed, and only the draw knows how
    /// much room there was.
    viewport: u16,
    content: u16,
}

impl HelpComponent {
    pub fn new(entries: Vec<HelpEntry>) -> Self {
        Self {
            entries,
            scroll: 0,
            viewport: 0,
            content: 0,
        }
    }

    /// The (width, height) the popup would like, borders and padding included.
    ///
    /// A *request*, not a promise: [`popup_area`] clips it to the frame, and
    /// what that clipping hides is what scrolling recovers.
    pub fn dimensions(&self) -> (u16, u16) {
        let content_width = self
            .entries
            .iter()
            .map(|e| match e {
                HelpEntry::Binding(key, desc) => {
                    key_cell(key).chars().count() + desc.chars().count()
                }
                HelpEntry::Section(title) => title.chars().count(),
                HelpEntry::Blank => 0,
            })
            .max()
            .unwrap_or(0) as u16;
        // borders (2) + horizontal padding (2*2)
        let width = content_width.saturating_add(6);
        // borders (2) + vertical padding (2*1) + one blank line per section rule
        let section_count = self
            .entries
            .iter()
            .filter(|e| matches!(e, HelpEntry::Section(_)))
            .count() as u16;
        let height = (self.entries.len() as u16)
            .saturating_add(section_count)
            .saturating_add(4);
        (width, height)
    }

    /// The table, one styled [`Line`] per row.
    ///
    /// Three levels, deliberately: a section heading is an accented rule, a key
    /// is the accent again but held in its own column, and the sentence
    /// explaining it is secondary text. Nothing here is an unstyled paragraph.
    fn rows(&self, width: u16) -> Vec<Line<'static>> {
        self.entries
            .iter()
            .flat_map(|e| match e {
                HelpEntry::Binding(key, desc) => vec![Line::from(vec![
                    Span::styled(key_cell(key), theme::KEY),
                    Span::styled(*desc, theme::SECONDARY),
                ])],
                HelpEntry::Section(title) => vec![
                    Line::from(vec![
                        Span::styled(*title, theme::TITLE),
                        Span::raw(" "),
                        // A rule out to the edge, so a heading reads as a band
                        // across the popup rather than as one more row.
                        Span::styled(
                            "─".repeat((width as usize).saturating_sub(title.chars().count() + 1)),
                            theme::RULE,
                        ),
                    ]),
                    Line::raw(""),
                ],
                HelpEntry::Blank => vec![Line::raw("")],
            })
            .collect()
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        f.render_widget(Clear, area);

        // Measured before the block is built, because the footer has to know
        // whether scrolling is possible — and the row count does not depend on
        // the width, so nothing here needs the inner area yet.
        self.content = self.row_count();
        self.viewport = area
            .height
            .saturating_sub(2 + 2 * u16::from(area.height >= 7));
        self.scroll = self.scroll.min(self.max_scroll());

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::BORDER_FOCUSED)
            .style(theme::POPUP_SURFACE)
            .title(
                Line::from(vec![
                    Span::styled(" ? ", theme::KEY),
                    Span::styled("Help ", theme::TITLE),
                ])
                .alignment(Alignment::Center),
            )
            .title_bottom(self.footer(area.width));
        let outer = block.inner(area);
        f.render_widget(block, area);

        // Padding is the first thing surrendered as the popup shrinks. The
        // vertical margin must agree with what was assumed above, so the footer
        // and the viewport cannot disagree about how many rows there are.
        let pad = gutter(outer.width).min(2);
        let [inner] = Layout::horizontal([Constraint::Min(1)])
            .horizontal_margin(pad)
            .areas(outer);
        let [inner] = Layout::vertical([Constraint::Min(1)])
            .vertical_margin(u16::from(area.height >= 7))
            .areas(inner);

        // The scrollbar draws over the last column of `outer`, so a section rule
        // run out to the full width would be overpainted by its track.
        let rule_width = inner
            .width
            .saturating_sub(if self.max_scroll() > 0 { 1 } else { 0 });
        f.render_widget(
            Paragraph::new(self.rows(rule_width)).scroll((self.scroll, 0)),
            inner,
        );

        if self.max_scroll() > 0 {
            let mut state = ScrollbarState::new(self.content as usize)
                .position(self.scroll as usize)
                .viewport_content_length(self.viewport as usize);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_style(theme::RULE)
                    .track_style(theme::RULE),
                outer,
                &mut state,
            );
        }
    }

    /// Rows hidden below the fold. Zero whenever the whole table is on screen,
    /// which is also what decides whether the scrollbar and its hint appear.
    fn max_scroll(&self) -> u16 {
        self.content.saturating_sub(self.viewport)
    }

    /// The always-visible keybinding footer. It gains a scroll hint only when
    /// there is something to scroll, so it never advertises a key that does
    /// nothing.
    fn footer(&self, width: u16) -> Line<'static> {
        let mut entries = Vec::new();
        if self.max_scroll() > 0 {
            entries.push(("j/k", "scroll", theme::KEY));
        }
        entries.push(("Esc", "close", theme::KEY_SAFE));
        footer(&entries, width)
    }

    /// Rows the table occupies — the entries plus the blank line each section
    /// heading brings with it. Independent of the width, which is what lets the
    /// footer know about scrolling before the layout is done.
    fn row_count(&self) -> u16 {
        self.entries
            .iter()
            .map(|e| match e {
                HelpEntry::Section(_) => 2,
                _ => 1,
            })
            .sum()
    }

    pub fn handle_action(&mut self, action: Action) -> EventState {
        match action {
            Action::MoveDown => {
                self.scroll = self.scroll.saturating_add(1).min(self.max_scroll());
                EventState::Consumed
            }
            Action::MoveUp => {
                self.scroll = self.scroll.saturating_sub(1);
                EventState::Consumed
            }
            Action::GoFirst => {
                self.scroll = 0;
                EventState::Consumed
            }
            Action::GoLast => {
                self.scroll = self.max_scroll();
                EventState::Consumed
            }
            _ => EventState::NotConsumed,
        }
    }
}

impl Modal for HelpComponent {
    fn area(&self, full: Rect) -> Rect {
        let (width, height) = self.dimensions();
        // `fixed` asks for exactly what the table needs; the frame trims it, and
        // the trimmed rows stay reachable by scrolling rather than being lost.
        popup_area(full, Extent::fixed(width), Extent::fixed(height))
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
/// Filter mode gets a section rather than a table of its own. Help is now
/// reachable from Insert mode too (`F1`, since `?` is a literal there), so the
/// bindings that only apply while filtering have to be documented again — but
/// as one scrollable table, not two mode-dependent ones: the reader is already
/// in filter mode when they ask, and the surrounding list keys are the other
/// half of what they need.
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
        HelpEntry::Binding("D", "Delete (skips the question, not the guard)"),
        HelpEntry::Binding("Enter", "Print path & exit"),
        HelpEntry::Binding("? / F1", "Show this help"),
        HelpEntry::Binding("q / Ctrl+C", "Quit"),
        HelpEntry::Blank,
        HelpEntry::Section("Filter mode"),
        HelpEntry::Binding("Esc", "Leave filter mode"),
        HelpEntry::Binding("Tab", "Toggle filter / list"),
        HelpEntry::Binding("↑ / Ctrl+K / Ctrl+P", "Move up in list"),
        HelpEntry::Binding("↓ / Ctrl+J / Ctrl+N", "Move down in list"),
        HelpEntry::Binding("Backspace", "Delete filter character"),
        HelpEntry::Binding("Enter", "Print path & exit"),
        HelpEntry::Binding("F1", "Show this help"),
        HelpEntry::Binding("Ctrl+C", "Quit"),
        HelpEntry::Blank,
        // Two slots per row: the first says how the space stands to its
        // upstream, the second what its own working state is. The wording
        // mirrors `SpaceStatus::glyphs`, which is what actually draws them.
        HelpEntry::Section("Space State — upstream"),
        HelpEntry::Binding("✔", "In sync with upstream"),
        HelpEntry::Binding("↑ / ↓ / ↕", "Ahead / behind / diverged"),
        HelpEntry::Binding("✘", "Upstream is gone (merged or deleted)"),
        HelpEntry::Binding("⬆", "Never pushed"),
        HelpEntry::Blank,
        HelpEntry::Section("Space State — local"),
        HelpEntry::Binding("*", "Uncommitted changes (git)"),
        HelpEntry::Binding("!", "Change has conflicts (jj)"),
        HelpEntry::Binding("≠", "Change is divergent (jj)"),
        HelpEntry::Binding("∅", "Working copy is empty (jj)"),
        HelpEntry::Binding("·", "Not checked yet"),
    ]
}
