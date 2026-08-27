pub mod app;
pub mod cli;
mod components;
pub mod config;
pub(crate) mod dirs;
pub mod events;
pub mod github;
pub mod hooks;
pub mod jobs;
pub mod keymap;
pub mod logs;
pub mod space_meta;
pub mod theme;
pub mod vcs;

use color_eyre::eyre::{Result, WrapErr};

// Re-exported at the crate root so an integration test can name the type
// `App::handle_key` returns, and the modal-stack identity `App::top_modal`
// yields, without `mod components` being public. `InputMode` rides along from
// `keymap` for the same reason.
pub use components::{EventState, ModalKind};
pub use keymap::InputMode;

use events::{AppEvent, EventSource};
use jobs::Worker;
use ratatui::{backend::Backend, Terminal};

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
///
/// Ratatui 0.30 gave every backend its own error type instead of always failing
/// with `io::Error`, so the bound below is what lets those failures keep flowing
/// into `eyre`.
pub fn run_app<B>(terminal: &mut Terminal<B>, app: &mut app::App) -> Result<Outcome>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // Dropping this at any exit — including the `?` below — stops the producer
    // threads and waits for them, so none outlives the session.
    let events = EventSource::new();

    // The pool is attached here rather than built in `App::with_args` because
    // the channel it reports on belongs to the loop, not to the app: an `App`
    // driven by a test has no loop, and must therefore have no worker either —
    // it runs the same code with every job left unsubmitted.
    app.attach_worker(Worker::new(events.job_sender()));

    let outcome = loop {
        terminal
            .draw(|f| app.draw(f))
            .wrap_err("failed to draw a frame")?;

        match events.next().wrap_err("the event producers stopped")? {
            AppEvent::Key(key) => {
                if app.handle_key(key) == EventState::Exit {
                    break match app.selected_path.take() {
                        Some(path) => Outcome::Selected(path),
                        None => Outcome::Quit,
                    };
                }
            }
            AppEvent::Paste(text) => app.handle_paste(&text),
            // The tick is also where work that arrived in bursts is applied, so
            // fifty results landing in a second cost one rebuild, not fifty.
            AppEvent::Tick => app.on_tick(),
            AppEvent::Job(result) => app.handle_job(result),
        }
    };

    // Stop the pool while the terminal is still ours: whatever is queued is
    // dropped now rather than after the caller has restored the screen.
    app.detach_worker();
    Ok(outcome)
}
