use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Widget, Wrap},
    Frame,
};

use super::{
    footer_entries, gutter, place_cursor, popup_area, prompt::footer, Action, AppContext,
    ConfirmCallback, EventState, Extent, HelpEntry, Modal, ModalFlow, KEYS_SECTION,
};
use crate::keymap::InputMode;
use crate::theme;

/// The one key in the program that means "destroy it anyway".
///
/// Shifted, and bound nowhere else, so it cannot be produced by the reflex that
/// dismisses an ordinary dialog. It reaches the dialog as an `InsertChar`,
/// because a guarded dialog is in insert mode — which is also what keeps `q`
/// from quitting with a destructive question on screen.
pub const OVERRIDE_KEY: char = 'X';

/// What the dialog demands before it will run its work.
///
/// The ceremony is a property of the *choice*, not of the key that opened it,
/// which is why it lives on the dialog: the same binding leads to a one-keypress
/// question or to a typed confirmation depending on what is about to be lost.
///
/// Neither guarded variant accepts Enter. Enter is the key the user presses
/// without reading — it dismisses every other dialog in the program — so a
/// destructive choice must not be reachable through the same reflex.
enum Gate {
    /// Enter is enough. For choices that destroy nothing.
    Enter,
    /// [`OVERRIDE_KEY`] must be pressed. Deliberate but cheap, for a loss the
    /// backend can undo.
    Override,
    /// The exact phrase must be typed first. As deliberate as a terminal gets,
    /// and reserved for losses nothing can bring back — typing the space's own
    /// name also forces the user to look at *which* space they are on.
    Phrase { expected: String, typed: String },
}

impl Gate {
    /// Whether the work may run now.
    fn is_open(&self) -> bool {
        match self {
            Gate::Enter => true,
            // Opened by its own key, never by the Enter that reaches this.
            Gate::Override => false,
            Gate::Phrase { expected, typed } => typed == expected,
        }
    }

    /// The dialog is a typing surface unless Enter alone decides it.
    fn mode(&self) -> InputMode {
        match self {
            Gate::Enter => InputMode::Normal,
            _ => InputMode::Insert,
        }
    }
}

/// A generic yes/no dialog. It knows nothing about what "yes" means: the caller
/// hands it the work to run on confirmation, so no `ConfirmAction` discriminant
/// has to be kept in sync anywhere else.
///
/// It also knows nothing about *why* a choice is dangerous — the caller supplies
/// the lines to show and the [`Gate`] to pass. That keeps the domain's reading of
/// a space (`vcs::DeletionRisk`) out of the widget and the widget's layout out of
/// the domain.
pub struct ConfirmComponent {
    pub title: String,
    pub label: String,
    pub detail: String,
    /// One line per thing this choice would destroy. Empty when it destroys
    /// nothing, which is what keeps the ordinary dialog as small as it was.
    losses: Vec<String>,
    /// Whether there is a way back, in one short sentence.
    aftermath: Option<String>,
    /// One line per thing that goes whether or not any work is at risk — the
    /// branch, the registration, the directory. Shown even on a dialog with no
    /// losses, because "the branch goes too" is the part a user cannot infer.
    removals: Vec<String>,
    /// One short sentence naming what deletion deliberately leaves behind.
    retained: Option<String>,
    gate: Gate,
    on_confirm: Option<ConfirmCallback>,
}

impl ConfirmComponent {
    pub fn new(title: String, label: String, detail: String, on_confirm: ConfirmCallback) -> Self {
        Self {
            title,
            label,
            detail,
            losses: Vec::new(),
            aftermath: None,
            removals: Vec::new(),
            retained: None,
            gate: Gate::Enter,
            on_confirm: Some(on_confirm),
        }
    }

    /// Demands [`OVERRIDE_KEY`] instead of Enter: deliberate, but one keypress.
    ///
    /// A builder rather than an exposed `Gate`, so the ceremonies a dialog can
    /// demand stay a closed set defined here — a caller cannot invent a fourth.
    pub fn require_override(mut self) -> Self {
        self.gate = Gate::Override;
        self
    }

    /// Demands that `expected` be typed out before Enter will do anything.
    pub fn require_phrase(mut self, expected: impl Into<String>) -> Self {
        self.gate = Gate::Phrase {
            expected: expected.into(),
            typed: String::new(),
        };
        self
    }

    /// Says what confirming would destroy, and whether it could be recovered.
    pub fn at_risk(mut self, losses: Vec<String>, aftermath: Option<String>) -> Self {
        self.losses = losses;
        self.aftermath = aftermath;
        self
    }

    /// Says what confirming removes regardless of risk, and what it spares.
    ///
    /// Separate from [`ConfirmComponent::at_risk`] because it is shown in cases
    /// that have no losses at all: a clean, pushed space destroys nothing and
    /// still has its branch deleted.
    pub fn removing(mut self, removals: Vec<String>, retained: Option<String>) -> Self {
        self.removals = removals;
        self.retained = retained;
        self
    }

    /// The body, top to bottom. Built once per frame and also used to size the
    /// popup, so what is drawn and what is reserved can never disagree.
    fn body(&self) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(Span::styled(self.label.clone(), theme::TEXT.bold())),
            Line::from(Span::styled(
                format!(" {} ", self.detail),
                Style::new()
                    .fg(theme::DANGER)
                    .bg(theme::DESTRUCTIVE_SURFACE)
                    .bold(),
            )),
        ];

        if !self.losses.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "This would destroy:",
                theme::MUTED,
            )));
            for loss in &self.losses {
                lines.push(Line::from(vec![
                    Span::styled("  • ", theme::MUTED),
                    Span::styled(loss.clone(), theme::WARNING_TEXT),
                ]));
            }
        }

        if !self.removals.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "Deleting also removes:",
                theme::MUTED,
            )));
            for removal in &self.removals {
                lines.push(Line::from(vec![
                    Span::styled("  • ", theme::MUTED),
                    // Dimmer than a loss: this is a statement of fact about what
                    // deletion does, not a warning about work disappearing.
                    Span::styled(removal.clone(), theme::SECONDARY),
                ]));
            }
            if let Some(retained) = &self.retained {
                lines.push(Line::from(Span::styled(retained.clone(), theme::MUTED)));
            }
        }

        if let Some(aftermath) = &self.aftermath {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                aftermath.clone(),
                Style::new().fg(theme::DESTRUCTIVE).bold(),
            )));
        }

        // A gate line is drawn in the reserved row below the body; keep it from
        // sitting flush against the warning it answers.
        if self.gate_line().is_some() {
            lines.push(Line::default());
        }

        lines
    }

    /// The line that tells the user how to get past the gate, and — for a typed
    /// gate — what they have typed so far.
    ///
    /// Returns the cursor's column offset within the line as well, so the caret
    /// lands after the last character the user typed.
    fn gate_line(&self) -> Option<(Line<'static>, u16)> {
        match &self.gate {
            Gate::Enter => None,
            Gate::Override => Some((
                Line::from(vec![
                    Span::styled(format!("Press {} ", OVERRIDE_KEY), theme::KEY_DESTRUCTIVE),
                    Span::styled("to delete it anyway.", theme::SECONDARY),
                ]),
                0,
            )),
            Gate::Phrase { expected, typed } => {
                let prompt = format!("Type {} to confirm: ", expected);
                let offset = prompt.chars().count() as u16 + typed.chars().count() as u16;
                Some((
                    Line::from(vec![
                        Span::styled(prompt, theme::SECONDARY),
                        Span::styled(typed.clone(), theme::TEXT.bold()),
                    ]),
                    offset,
                ))
            }
        }
    }

    /// How tall the popup has to be at `content_width` columns: two borders, a
    /// row of padding above the body, the body itself, and the gate row below.
    ///
    /// The gate row is reserved even when there is no gate line, where it reads
    /// as bottom padding — one layout for every dialog, so a body line can never
    /// be clipped by a variant nobody re-measured.
    ///
    /// It is measured *at a width* because the body wraps: on a narrow terminal
    /// a single loss becomes two rows, and a height counted in unwrapped lines
    /// would push the last of them under the border.
    fn height(&self, content_width: u16) -> u16 {
        let rows: u16 = self
            .body()
            .iter()
            .map(|line| wrapped_rows(line, content_width))
            .sum();
        rows.saturating_add(4)
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        // Empty below the size floor; the base pane's message stands instead of
        // a dialog with no room to say what it would destroy.
        if area.is_empty() {
            return;
        }
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
            .title_bottom(self.keybinding_hint(area.width));

        let inner_area = outer_block.inner(area);
        outer_block.render(area, frame.buffer_mut());

        // One blank row of padding above the body; the border supplies the rest.
        // The body takes the slack, so a short frame eats into the explanation
        // and never into the row that says how to get past the gate.
        let [_, body_area, gate_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .horizontal_margin(gutter(inner_area.width))
        .areas(inner_area);

        Paragraph::new(self.body())
            .wrap(Wrap { trim: false })
            .render(body_area, frame.buffer_mut());

        if let Some((line, cursor_offset)) = self.gate_line() {
            Paragraph::new(line).render(gate_area, frame.buffer_mut());
            if matches!(self.gate, Gate::Phrase { .. }) {
                place_cursor(frame, gate_area, gate_area.x + cursor_offset, gate_area.y);
            }
        }
    }

    pub fn handle_action(&mut self, _action: Action) -> EventState {
        EventState::NotConsumed
    }

    /// Runs the deferred work, or does nothing if it has already run.
    fn confirm(&mut self, ctx: &mut AppContext) -> ModalFlow {
        match self.on_confirm.take() {
            // The confirmed work decides where the flow goes next.
            Some(work) => work(ctx),
            None => ModalFlow::Close,
        }
    }

    /// The dialog's own footer, fitted to `width`.
    ///
    /// Read straight off [`Modal::help`], so the dialog cannot offer one word
    /// here and another in the help popup. The confirming key is coloured as
    /// destructive and the escape as safe, so which of the two ends badly is
    /// legible before either word is read; cancel is marked essential, because
    /// on the narrowest dialog the way *out* is the hint worth keeping.
    fn keybinding_hint(&self, width: u16) -> Line<'static> {
        footer(&footer_entries(&self.help(), KEYS_SECTION), width)
    }
}

impl Modal for ConfirmComponent {
    fn area(&self, full: Rect) -> Rect {
        // Wide enough that a remote URL or a repository path stays on one line,
        // capped so a two-word question is not spread across a wall.
        let width = Extent::share(60, 34, 100);
        // The height depends on the width, so the width is settled first and the
        // body measured against the room it will actually get. The two
        // subtractions mirror `draw` exactly — the borders, then the same gutter
        // taken from the same inner width — so what is reserved and what is
        // drawn cannot disagree.
        let inner_width = width.resolve(full.width).saturating_sub(2);
        let content_width = inner_width.saturating_sub(2 * gutter(inner_width));
        popup_area(
            full,
            width,
            // Two border rows and one padding row around whatever the body needs,
            // so a dialog listing four losses is not silently clipped. Only a
            // frame shorter than the dialog can trim it now, and it trims the
            // explanation rather than the gate.
            Extent::fixed(self.height(content_width)),
        )
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, _ctx: &mut AppContext) {
        ConfirmComponent::draw(self, frame, area);
    }

    fn handle(&mut self, action: Action, ctx: &mut AppContext) -> ModalFlow {
        match action {
            Action::Select if self.gate.is_open() => self.confirm(ctx),
            // Enter with the gate shut is not an error and not a cancel: the
            // dialog stays up saying what it still wants.
            Action::Select => ModalFlow::Consumed,
            Action::ClosePopup | Action::ExitInsertMode => ModalFlow::Close,
            Action::InsertChar(c) => match &mut self.gate {
                Gate::Override if c == OVERRIDE_KEY => self.confirm(ctx),
                Gate::Phrase { typed, .. } => {
                    typed.push(c);
                    ModalFlow::Consumed
                }
                // A guarded dialog swallows stray typing rather than letting it
                // reach the list underneath.
                _ => ModalFlow::Consumed,
            },
            Action::DeleteChar => {
                if let Gate::Phrase { typed, .. } = &mut self.gate {
                    typed.pop();
                }
                ModalFlow::Consumed
            }
            _ => self.handle_action(action).into(),
        }
    }

    fn mode(&self) -> InputMode {
        self.gate.mode()
    }

    fn help(&self) -> Vec<HelpEntry> {
        let confirm = match &self.gate {
            Gate::Enter => HelpEntry::bind("Enter", "Confirm").hint("Enter", "confirm"),
            Gate::Override => HelpEntry::bind("X", "Delete anyway").hint("X", "delete anyway"),
            Gate::Phrase { .. } => HelpEntry::bind("Enter", "Confirm, once the name is typed")
                .hint("Enter", "once typed"),
        };
        vec![
            HelpEntry::Section(KEYS_SECTION),
            confirm.destructive(),
            HelpEntry::bind("F1", "Show this help")
                .hint("F1", "help")
                .aside(),
            HelpEntry::bind("Esc", "Cancel")
                .hint("Esc", "cancel")
                .safe()
                .essential(),
            HelpEntry::bind("Ctrl+C", "Quit"),
        ]
    }
}

/// How many rows `line` occupies once wrapped to `width`.
///
/// Greedy word wrap, matching what `Paragraph`'s `Wrap` does closely enough to
/// reserve space for: it only ever has to agree on the *count*, and erring long
/// costs a blank row while erring short clips a warning.
fn wrapped_rows(line: &Line<'_>, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if text.trim().is_empty() {
        return 1;
    }
    let mut rows = 1u16;
    let mut used = 0usize;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        let needed = if used == 0 { len } else { used + 1 + len };
        if needed > width && used > 0 {
            rows = rows.saturating_add(1);
            used = len;
        } else {
            used = needed;
        }
        // A word longer than the line takes rows of its own.
        while used > width {
            rows = rows.saturating_add(1);
            used -= width;
        }
    }
    rows
}
