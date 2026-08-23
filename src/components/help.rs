//! The keybinding reference, and the one popup whose size is dictated by its
//! own content rather than by a share of the frame.
//!
//! Because of that it is also the one popup that can want more rows than the
//! terminal has, so it is the one that scrolls. Everything else degrades by
//! giving up padding; this gives up nothing and moves the window instead.

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};

use super::{
    gutter, popup_area,
    prompt::{footer, FooterEntry},
    Action, AppContext, EventState, Extent, Modal, ModalFlow,
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

/// The name of the section every binding table opens with.
///
/// Named rather than spelled out at each call site because the footer picks its
/// entries *by section* — see [`footer_entries`].
pub const KEYS_SECTION: &str = "Keybindings";
/// The section of [`worktrees_bindings`] that applies while a filter is being
/// typed. The space list has two input modes but only one table, so the mode
/// chooses a section out of it.
pub const FILTER_SECTION: &str = "Filter mode";

/// One key and what it does, as the help table states it.
///
/// `hint` is the same binding as the always-visible footer shows it, and is
/// `Some` only for the handful worth that space. Keeping it here — beside the
/// long description rather than in a second list somewhere else — is what stops
/// the footer and the help popup from ever disagreeing: a binding that changes
/// changes in exactly one place.
pub struct Binding {
    keys: &'static str,
    description: &'static str,
    hint: Option<FooterEntry<'static>>,
    /// How readily the footer gives this hint up as the terminal narrows. See
    /// [`footer_entries`].
    rank: Rank,
}

/// What a footer hint is worth when the width runs out.
///
/// Three levels rather than a table position, because a help table is ordered
/// for *reading* — the action first, the way out last — and a footer sheds
/// entries in order of *importance*. Stating the rank on the binding keeps both
/// orders correct without either dictating the other.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// A convenience. First to go.
    Aside,
    /// What this screen is for.
    Ordinary,
    /// The way out, and the way to the full table. Last to go.
    Essential,
}

pub enum HelpEntry {
    Binding(Binding),
    Section(&'static str),
    Blank,
}

impl HelpEntry {
    /// A binding the help popup lists and the footer does not.
    pub fn bind(keys: &'static str, description: &'static str) -> Self {
        HelpEntry::Binding(Binding {
            keys,
            description,
            hint: None,
            rank: Rank::Ordinary,
        })
    }

    /// Also carry this binding in the always-visible footer, in short form.
    ///
    /// The footer gets its own key and verb because the two are read at
    /// different moments: `Delete (skips the question, not the guard)` is a
    /// sentence for someone who stopped to ask, `[D] force` a reminder for
    /// someone who did not.
    ///
    /// The builder methods here apply to a binding; on a section or a blank they
    /// do nothing, because nothing else is ever chained onto those.
    pub fn hint(mut self, key: &'static str, verb: &'static str) -> Self {
        if let HelpEntry::Binding(binding) = &mut self {
            binding.hint = Some((key, verb, theme::KEY));
        }
        self
    }

    /// Paints the hint's key in the destructive colour, so the footer doubles as
    /// a warning: what cannot be undone is what stands out in it.
    pub fn destructive(self) -> Self {
        self.restyle(theme::KEY_DESTRUCTIVE)
    }

    /// Paints the hint's key as the way safely back out.
    pub fn safe(self) -> Self {
        self.restyle(theme::KEY_SAFE)
    }

    /// Holds this hint back to the end of the footer, where [`footer_entries`]
    /// puts the bindings that survive longest on a narrow terminal.
    pub fn essential(self) -> Self {
        self.rank(Rank::Essential)
    }

    /// Marks this hint as the first the footer may drop — a convenience the
    /// screen still works without, such as the pointer to the help popup.
    pub fn aside(self) -> Self {
        self.rank(Rank::Aside)
    }

    fn rank(mut self, rank: Rank) -> Self {
        if let HelpEntry::Binding(binding) = &mut self {
            binding.rank = rank;
        }
        self
    }

    fn restyle(mut self, style: Style) -> Self {
        if let HelpEntry::Binding(Binding {
            hint: Some(hint), ..
        }) = &mut self
        {
            hint.2 = style;
        }
        self
    }
}

/// The footer form of a binding table, ordered so [`footer`] sheds the least
/// important entry first.
///
/// `footer` drops from the *front*, so this is importance ascending: [`Rank`]
/// order first, and the table's own order within a rank. The last entry is
/// therefore the one that survives a 40-column terminal.
///
/// `section` says which part of the table to read, because one table may cover
/// two modes and a footer only ever describes the mode the user is in.
pub fn footer_entries(entries: &[HelpEntry], section: &str) -> Vec<FooterEntry<'static>> {
    let mut hints: Vec<(Rank, FooterEntry<'static>)> = Vec::new();
    let mut inside = false;
    for entry in entries {
        match entry {
            HelpEntry::Section(title) => inside = *title == section,
            HelpEntry::Binding(binding) if inside => {
                if let Some(hint) = binding.hint {
                    hints.push((binding.rank, hint));
                }
            }
            _ => {}
        }
    }
    // Stable, so two hints of the same rank stay in the order the table gave
    // them.
    hints.sort_by_key(|(rank, _)| *rank);
    hints.into_iter().map(|(_, hint)| hint).collect()
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
                HelpEntry::Binding(b) => {
                    key_cell(b.keys).chars().count() + b.description.chars().count()
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
                HelpEntry::Binding(b) => vec![Line::from(vec![
                    Span::styled(key_cell(b.keys), theme::KEY),
                    Span::styled(b.description, theme::SECONDARY),
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
        HelpEntry::Section(KEYS_SECTION),
        // The footer hint hangs off the `j` row alone: `j/k` names both halves
        // of the pair, and one entry costs half the width of two.
        HelpEntry::bind("j / ↓", "Move down").hint("j/k", "move"),
        HelpEntry::bind("k / ↑", "Move up"),
        HelpEntry::bind("g / Home", "Go to first"),
        HelpEntry::bind("G / End", "Go to last"),
        HelpEntry::bind("i / /", "Enter filter mode").hint("i", "filter"),
        HelpEntry::bind("Tab", "Toggle filter / list"),
        HelpEntry::bind("n", "New worktree (pick repo)").hint("n", "new"),
        // `p` and `P` stay out of the footer: they are a whole flow rather than
        // a key, and the footer is not the place to learn a flow exists.
        HelpEntry::bind("p", "New worktree from PR URL"),
        HelpEntry::bind("P", "New worktree from PR URL (auto-clone)"),
        // The two halves of "the list is wrong, fix it", kept apart because
        // they cost differently: `r` re-reads the repositories already on
        // screen, `R` goes back to the repos dirs. Only `r` earns a footer
        // hint, and only as an aside — it is the one of the three a user
        // reaches for often enough to want reminding of.
        HelpEntry::bind("r", "Refresh spaces & status (no network)")
            .hint("r", "refresh")
            .aside(),
        HelpEntry::bind("R", "Rescan the repos dirs for new repositories"),
        HelpEntry::bind("f", "Fetch the selected repository's remotes"),
        HelpEntry::bind("d", "Delete with confirmation")
            .hint("d", "delete")
            .destructive(),
        HelpEntry::bind("D", "Delete (skips the question, not the guard)")
            .hint("D", "force")
            .destructive(),
        HelpEntry::bind("Enter", "Print path & exit").hint("Enter", "path"),
        // Both essential, and in this order, because the footer keeps its tail:
        // on the narrowest terminal `? help` is the one entry that leads to all
        // the others, so it is the one that outlives even `quit`.
        HelpEntry::bind("q / Ctrl+C", "Quit")
            .hint("q", "quit")
            .essential(),
        HelpEntry::bind("? / F1", "Show this help")
            .hint("?", "help")
            .essential(),
        HelpEntry::Blank,
        HelpEntry::Section(FILTER_SECTION),
        // Marked essential, so the way back to Normal mode is the hint that
        // outlives every other on a narrow terminal — in a mode the user may
        // have entered by accident, the exit is the promise worth keeping.
        HelpEntry::bind("Esc", "Leave filter mode")
            .hint("Esc", "normal")
            .safe()
            .essential(),
        HelpEntry::bind("Tab", "Toggle filter / list").hint("Tab", "list"),
        HelpEntry::bind("↑ / Ctrl+K / Ctrl+P", "Move up in list").hint("↑/↓", "move"),
        HelpEntry::bind("↓ / Ctrl+J / Ctrl+N", "Move down in list"),
        HelpEntry::bind("Backspace", "Delete filter character"),
        HelpEntry::bind("Enter", "Print path & exit").hint("Enter", "path"),
        HelpEntry::bind("F1", "Show this help")
            .hint("F1", "help")
            .aside(),
        HelpEntry::bind("Ctrl+C", "Quit"),
        HelpEntry::Blank,
        // Two slots per row: the first says how the space stands to its
        // upstream, the second what its own working state is. The wording
        // mirrors `SpaceStatus::glyphs`, which is what actually draws them.
        HelpEntry::Section("Space State — upstream"),
        HelpEntry::bind("✔", "In sync with upstream"),
        HelpEntry::bind("↑ / ↓ / ↕", "Ahead / behind / diverged"),
        HelpEntry::bind("✘", "Upstream is gone (merged or deleted)"),
        HelpEntry::bind("⬆", "Never pushed"),
        HelpEntry::Blank,
        HelpEntry::Section("Space State — local"),
        HelpEntry::bind("*", "Uncommitted changes (git)"),
        HelpEntry::bind("!", "Change has conflicts (jj)"),
        HelpEntry::bind("≠", "Change is divergent (jj)"),
        HelpEntry::bind("∅", "Working copy is empty (jj)"),
        HelpEntry::bind("·", "Not checked yet"),
    ]
}
