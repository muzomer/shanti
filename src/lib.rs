pub mod app;
mod cli;
mod components;
pub mod config;
mod dirs;
mod git;
mod github;
pub mod keymap;
pub mod logs;
pub mod vcs;

use color_eyre::eyre::{Result, WrapErr};

use components::EventState;
use ratatui::{
    backend::Backend,
    crossterm::event::{self, Event},
    Terminal,
};

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

pub fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut app::App) -> Result<Outcome> {
    loop {
        terminal
            .draw(|f| app.draw(f))
            .wrap_err("failed to draw a frame")?;

        if let Event::Key(key) = event::read().wrap_err("failed to read a terminal event")? {
            if app.handle_key(key) == EventState::Exit {
                break Ok(match app.selected_path.take() {
                    Some(path) => Outcome::Selected(path),
                    None => Outcome::Quit,
                });
            }
        };
    }
}
