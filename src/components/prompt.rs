//! The shape every single-field popup in shanti takes.
//!
//! Two popups ask the user for one string — a space name and a PR URL — and
//! before this they each hand-rolled a block, a four-row layout and a cursor
//! calculation. Identical code drifts, and the drift showed as inconsistent
//! hierarchy: the same kind of information rendered at a different weight
//! depending on which popup you were in.
//!
//! So the hierarchy is stated once, here, as four distinct levels:
//!
//! 1. **What this is** — the accented title, with dim context beside it.
//! 2. **What it is asking for** — the field label, secondary.
//! 3. **What the value currently is** — inside its own box, the loudest text on
//!    screen, with a muted placeholder when it is empty.
//! 4. **What a keystroke will do** — the footer, keys accented, verbs muted.
//!
//! A caller supplies the words and the severity; it does not get to choose the
//! weights.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget, Wrap},
    Frame,
};

use super::{gutter, place_cursor, Extent};
use crate::theme;

/// Rows a prompt needs: two borders, a padding row, the label, the three-row
/// input box, and the status row beneath it.
pub const PROMPT_HEIGHT: u16 = 9;

/// The width policy shared by both prompts, differing only in how much of the
/// frame they ask for — a URL is longer than a branch name.
pub const fn prompt_width(percent: u16, min: u16, max: u16) -> Extent {
    Extent::share(percent, min, max)
}

/// One labelled text field in a popup. Built fresh each frame from the owning
/// component's state; it holds no state of its own.
pub struct Prompt<'a> {
    /// Level 1: what this popup is.
    pub title: &'a str,
    /// Level 1, quieter: which repository, which backend — the standing context
    /// the question is being asked in.
    pub context: Option<String>,
    /// Level 2: the name of the field.
    pub label: &'a str,
    /// Level 2, off to the right: a caveat about the field itself.
    pub aside: Option<(String, Style)>,
    /// Level 3: what the user has typed, and what to show when they have not.
    pub value: &'a str,
    pub placeholder: &'a str,
    pub cursor: usize,
    /// Whether the value is currently acceptable. Drives the input border alone,
    /// so "not valid yet" reads as a state of the field rather than an error.
    pub valid: bool,
    /// Beneath the input: a hint, a warning, or a failure — the caller picks the
    /// severity because only it knows which of the three this is.
    pub status: Option<(String, Style)>,
    /// Level 4: the keybinding footer, which is never optional. Held as entries
    /// rather than a finished line because only [`Prompt::render`] knows how
    /// much of the bottom border there is to fit them into.
    pub footer: Vec<FooterEntry<'static>>,
}

impl Prompt<'_> {
    pub fn render(self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);

        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::BORDER_FOCUSED)
            .style(theme::POPUP_SURFACE)
            .title(
                Line::from(vec![
                    Span::styled(" ▏", theme::KEY),
                    Span::styled(self.title, theme::TITLE),
                    Span::raw(" "),
                ])
                .left_aligned(),
            )
            .title_bottom(footer(&self.footer, area.width));
        if let Some(context) = &self.context {
            // Dropped rather than overlapped on a narrow popup: the title is the
            // half that has to survive.
            if area.width >= self.title.chars().count() as u16 + context.chars().count() as u16 + 8
            {
                block = block.title_top(
                    Line::from(format!(" {} ", context))
                        .style(theme::MUTED)
                        .right_aligned(),
                );
            }
        }

        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());

        // The input box is the only row that must keep its full three rows; the
        // padding above it and the status below it are what a short popup gives
        // up, in that order.
        let [_, label_area, input_area, status_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .horizontal_margin(gutter(inner.width))
        .areas(inner);

        Paragraph::new(self.label)
            .style(theme::SECONDARY)
            .render(label_area, frame.buffer_mut());

        // The aside shares its row with the label, so it is drawn only when both
        // fit whole. A truncated caveat ("creating a git workt") is worse than
        // none: the reader cannot tell what was cut off.
        if let Some((aside, style)) = &self.aside {
            let needed = self.label.chars().count() + aside.chars().count() + 2;
            if label_area.width as usize >= needed {
                Paragraph::new(aside.as_str())
                    .style(*style)
                    .right_aligned()
                    .render(label_area, frame.buffer_mut());
            }
        }

        let (text, text_style) = if self.value.is_empty() {
            (self.placeholder, theme::MUTED.italic())
        } else {
            (self.value, theme::TEXT.bold())
        };
        Paragraph::new(Span::styled(text, text_style))
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(if self.valid {
                        theme::BORDER_INPUT_FOCUSED
                    } else {
                        theme::BORDER_DESTRUCTIVE
                    })
                    .padding(Padding::horizontal(1)),
            )
            .render(input_area, frame.buffer_mut());

        // Wrapped, not truncated: this row carries the base branch a space will
        // be cut from and the reason a lookup failed, and half of either is
        // misleading rather than merely terse.
        if let Some((status, style)) = &self.status {
            Paragraph::new(Line::from(vec![
                Span::styled("↳ ", theme::RULE),
                Span::styled(status.clone(), *style),
            ]))
            .wrap(Wrap { trim: false })
            .render(status_area, frame.buffer_mut());
        }

        // input_area: border(1) + padding(1) = offset 2; y+1 skips the top border.
        // Clamped, because typing past the right edge otherwise parks the caret
        // outside the box it belongs to.
        place_cursor(
            frame,
            input_area,
            input_area.x + 2 + self.cursor as u16,
            input_area.y + 1,
        );
    }
}

/// One entry of a keybinding footer: the key, what it does, and how loud the key
/// is. The caller picks the style because only it knows whether a key confirms,
/// cancels or destroys.
pub type FooterEntry<'a> = (&'a str, &'a str, Style);

/// The vim-style keybinding footer every popup carries, fitted to `width`.
///
/// Drawn into the bottom border, so it has a hard ceiling and no way to wrap.
/// Rather than let the border slice a hint in half — `Enter] once typed` is
/// worse than no hint at all — entries are dropped until the rest fit. They go
/// from the left, because the footer is right-aligned and the leftmost entry is
/// the one the border would have eaten anyway; put the way *out* of the popup
/// last and it is the hint that always survives.
pub fn footer(entries: &[FooterEntry<'_>], width: u16) -> Line<'static> {
    // Two corners of the border, plus the trailing space.
    let budget = width.saturating_sub(3) as usize;
    let cost = |(key, verb, _): &FooterEntry| key.chars().count() + verb.chars().count() + 4;

    let mut keep = 0;
    let mut used = 0;
    for entry in entries.iter().rev() {
        used += cost(entry);
        if used > budget {
            break;
        }
        keep += 1;
    }

    let mut spans = Vec::new();
    for (key, verb, style) in entries.iter().skip(entries.len() - keep) {
        spans.push(Span::styled(format!(" [{}] ", key), *style));
        spans.push(Span::styled(verb.to_string(), theme::MUTED));
    }
    spans.push(Span::raw(" "));
    Line::from(spans).right_aligned()
}

/// The two entries every prompt ends with: confirm, and the way out.
pub fn confirm_and_cancel(verb: &'static str) -> [FooterEntry<'static>; 2] {
    [
        ("Enter", verb, theme::KEY),
        ("Esc", "cancel", theme::KEY_SAFE),
    ]
}
