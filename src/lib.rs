pub mod app;
mod cli;
mod components;
pub mod config;
mod dirs;
pub mod events;
pub mod github;
pub mod keymap;
pub mod logs;
pub mod vcs;

use color_eyre::eyre::{Result, WrapErr};

use components::EventState;
use events::{AppEvent, EventSource};
use ratatui::{backend::Backend, Terminal};
use tracing::debug;

/// How a TUI session ended.
///
/// The caller has to tell a deliberate quit apart from a selection, because only
/// a selection may write a path to stdout — `cd $(shanti)` consumes that stream
/// verbatim, so a path printed after a quit would move the user's shell against
/// their intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The user picked a worktree; the payload is its absolute path.
    Selected(String),
    /// The user left without picking anything.
    Quit,
}

/// Draws, then waits for the next [`AppEvent`], for as long as the session lasts.
///
/// Every event redraws: input changes the state, a tick lets anything
/// time-dependent advance, and a finished job brings in new data. Waiting on one
/// channel instead of on the keyboard is what allows the last two to exist at all.
pub fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut app::App) -> Result<Outcome> {
    // Dropping this at any exit — including the `?` below — stops the producer
    // threads and waits for them, so none outlives the session.
    let events = EventSource::new();

    loop {
        terminal
            .draw(|f| app.draw(f))
            .wrap_err("failed to draw a frame")?;

        match events.next().wrap_err("the event producers stopped")? {
            AppEvent::Key(key) => {
                if app.handle_key(key) == EventState::Exit {
                    break Ok(match app.selected_path.take() {
                        Some(path) => Outcome::Selected(path),
                        None => Outcome::Quit,
                    });
                }
            }
            AppEvent::Paste(text) => app.handle_paste(&text),
            // Redrawing is all a tick owes anyone today; animations and timeouts
            // hang off this arm later.
            AppEvent::Tick => {}
            // PLACEHOLDER until shanti-hml.2 adds the workers that send these.
            AppEvent::Job(result) => debug!(job = %result.name, "a background job finished"),
        }
    }
}
