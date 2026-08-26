//! The detail pane: everything about the highlighted space that the row itself
//! has no room for.
//!
//! A row can carry two status glyphs and a name. Deciding whether a space is
//! still needed takes more than that — what was last done in it, how long ago,
//! how far it has drifted from the remote, where it actually lives — and
//! finding that out used to mean leaving shanti for a shell, which is the
//! context switch the tool exists to remove.
//!
//! Nothing here reads the disk or spawns anything. Every field comes out of the
//! [`Space`] snapshot the list is already holding, so moving the selection costs
//! one re-render and no I/O at all — which is the only way a pane that redraws
//! on every cursor move can be honest about "no perceptible delay".

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use super::{MIN_HEIGHT, MIN_WIDTH};
use crate::theme;
use crate::vcs::{RemoteState, Space};

/// How many field rows the pane draws, whether or not each has a value.
///
/// Fixed rather than counted from the fields present: the PR line is there for
/// some spaces and not others, and a pane that changed height as the cursor
/// moved would shove the list up and down under the reader's eyes.
const FIELD_ROWS: u16 = 5;

/// The pane's full height, borders included.
pub const HEIGHT: u16 = FIELD_ROWS + 2;

/// Whether `area` — the space the list and the pane must share — can hold both.
///
/// The pane is hidden rather than clipped below this: half a field list reads as
/// a rendering bug, while its absence is simply a smaller terminal. The list
/// keeps its own [`MIN_HEIGHT`] floor, so the pane only ever appears with room
/// left over for it.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT + HEIGHT
}

/// Draw the pane for `space`, or an empty frame when nothing is selected.
///
/// `now` is passed in rather than read here so the age is derived from one
/// clock reading per frame — and so a test can pin it.
pub fn draw(frame: &mut Frame, area: Rect, space: Option<&Space>, now: i64) {
    let block = Block::bordered()
        // Rounded, like the panes it sits under: the two borders meet, and one
        // square corner beside a round one reads as a different kind of widget.
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(theme::border())
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Detail", theme::title()),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(space) = space else {
        // An empty list still has a pane: leaving the frame drawn and blank says
        // "nothing is selected", where an absent border would say the pane went
        // away and invite the reader to wonder what happened to it.
        return;
    };

    let width = inner.width as usize;
    let lines: Vec<Line> = fields(space, now)
        .into_iter()
        .map(|(label, value, style)| field_line(label, value, style, width))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The pane's rows, in reading order: what was done, then how it stands against
/// the remote and against the working copy, then where it is.
///
/// Exactly [`FIELD_ROWS`] entries, always — a field with nothing to say renders
/// as a blank value rather than disappearing, so every row keeps its place.
fn fields(space: &Space, now: i64) -> Vec<(&'static str, String, ratatui::style::Style)> {
    let (latest, when) = match &space.tip {
        Some(tip) => {
            let age = tip.age(now);
            // "now ago" is not English. Every other age is a duration and takes
            // the suffix; this one is already a point in time.
            let when = if age == "now" {
                age
            } else {
                format!("{age} ago")
            };
            (tip.subject_or_placeholder().to_string(), when)
        }
        // Not read rather than not there: `·` is the same "not checked yet" mark
        // the status glyphs use, so the two halves of the UI say it the same way.
        None => ("·".to_string(), String::new()),
    };
    let local = space.status.glyphs()[1];
    vec![
        ("Latest", latest, theme::text()),
        ("When", when, theme::secondary()),
        ("Remote", remote_summary(space.status.remote), theme::text()),
        (
            "Local",
            local.meaning.to_string(),
            theme::tone_text(local.tone),
        ),
        (
            "Path",
            space.path.to_string_lossy().into_owned(),
            theme::secondary(),
        ),
    ]
}

/// The remote half in words rather than in a glyph.
///
/// The counts are the point: "ahead" alone is what the row already says, and the
/// question the pane answers is how much work is sitting here unpushed.
fn remote_summary(remote: RemoteState) -> String {
    match remote {
        RemoteState::Unknown => "not checked yet".to_string(),
        RemoteState::Tracked {
            ahead: 0,
            behind: 0,
        } => "in sync with upstream".to_string(),
        RemoteState::Tracked { ahead, behind: 0 } => format!("{ahead} ahead of upstream"),
        RemoteState::Tracked { ahead: 0, behind } => format!("{behind} behind upstream"),
        RemoteState::Tracked { ahead, behind } => {
            format!("{ahead} ahead, {behind} behind — diverged")
        }
        RemoteState::Gone => "upstream is gone".to_string(),
        RemoteState::Untracked => "no upstream — never pushed".to_string(),
    }
}

/// One row: a muted label in a fixed column, then the value.
///
/// The label column is what makes the pane scan vertically instead of reading as
/// prose, and it is the reason the value is truncated from the *left*: the end
/// of a path — the space's own directory — identifies it, while the repos-dir
/// prefix it shares with every other space does not.
fn field_line(
    label: &'static str,
    value: String,
    style: ratatui::style::Style,
    width: usize,
) -> Line<'static> {
    const LABEL_WIDTH: usize = 8;
    let room = width.saturating_sub(LABEL_WIDTH + 1);
    Line::from(vec![
        Span::styled(" ", theme::muted()),
        Span::styled(
            format!("{label:<width$}", width = LABEL_WIDTH - 1),
            theme::muted(),
        ),
        Span::styled(truncate_left(&value, room), style),
    ])
}

/// Keep the last `room` characters of `value`, marking what was dropped.
fn truncate_left(value: &str, room: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= room || room == 0 {
        return value.to_string();
    }
    // One cell of the room goes to the ellipsis, so the result is exactly `room`
    // wide and the pane never wraps a field onto a second row.
    let kept: String = chars[chars.len() - room.saturating_sub(1)..]
        .iter()
        .collect();
    format!("…{kept}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::{Backend, RepoId, SpaceStatus, SpaceTip};
    use pretty_assertions::assert_eq;
    use ratatui::{backend::TestBackend, Terminal};

    fn a_space(status: SpaceStatus, tip: Option<SpaceTip>) -> Space {
        Space::new(
            RepoId::new("shanti"),
            status.backend(),
            "feature",
            "/w/shanti/feature",
            status,
        )
        .with_tip(tip)
    }

    fn screen(space: Option<&Space>, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), space, 1_000_000))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(60)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_field_renders_for_a_git_space() {
        let space = a_space(
            SpaceStatus::git(
                RemoteState::Tracked {
                    ahead: 2,
                    behind: 1,
                },
                true,
            ),
            Some(SpaceTip::new("teach the pane to draw", 1_000_000 - 7200)),
        );
        let screen = screen(Some(&space), HEIGHT);

        assert!(screen.contains("teach the pane to draw"), "{screen}");
        assert!(screen.contains("2h ago"), "{screen}");
        assert!(screen.contains("2 ahead, 1 behind"), "{screen}");
        assert!(screen.contains("uncommitted changes"), "{screen}");
        assert!(screen.contains("feature"), "{screen}");
    }

    /// The pane is backend-neutral by construction, and this is what proves it:
    /// the same five rows, filled from a jj space whose local half git has no
    /// word for.
    #[test]
    fn every_field_renders_for_a_jj_space() {
        let space = a_space(
            SpaceStatus::jj(
                RemoteState::Untracked,
                crate::vcs::JjLocal {
                    empty: false,
                    conflicted: true,
                    divergent: false,
                },
            ),
            Some(SpaceTip::new("", 1_000_000 - 60 * 60 * 24 * 3)),
        );
        let screen = screen(Some(&space), HEIGHT);

        assert!(screen.contains("(no description)"), "{screen}");
        assert!(screen.contains("3d ago"), "{screen}");
        assert!(screen.contains("never pushed"), "{screen}");
        assert!(screen.contains("conflict"), "{screen}");
    }

    #[test]
    fn a_space_whose_head_was_not_read_says_so_without_a_date() {
        let space = a_space(SpaceStatus::unknown(Backend::Git), None);
        let screen = screen(Some(&space), HEIGHT);
        assert!(screen.contains("Latest"), "{screen}");
        assert!(!screen.contains("ago"), "{screen}");
    }

    /// With nothing selected the pane is still a pane: a border and no fields.
    #[test]
    fn an_empty_selection_draws_the_frame_and_nothing_else() {
        let screen = screen(None, HEIGHT);
        assert!(screen.contains("Detail"), "{screen}");
        assert!(!screen.contains("Path"), "{screen}");
    }

    /// A long path must lose its head, not its tail: the tail is the part that
    /// says *which* space this is.
    #[test]
    fn a_path_too_long_for_the_pane_keeps_its_end() {
        assert_eq!(truncate_left("/a/very/long/path", 8), "…ng/path");
        assert_eq!(truncate_left("/short", 8), "/short");
    }

    #[test]
    fn the_pane_is_hidden_rather_than_clipped_when_the_list_needs_the_room() {
        assert!(fits(Rect::new(0, 0, MIN_WIDTH, MIN_HEIGHT + HEIGHT)));
        assert!(!fits(Rect::new(0, 0, MIN_WIDTH, MIN_HEIGHT + HEIGHT - 1)));
        assert!(!fits(Rect::new(0, 0, MIN_WIDTH - 1, MIN_HEIGHT + HEIGHT)));
    }
}
