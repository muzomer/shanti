pub mod app;
mod cli;
mod components;
mod dirs;
mod git;
mod github;
pub mod keymap;
pub mod logs;
pub mod vcs;

use std::io;

use components::EventState;
use ratatui::{
    backend::Backend,
    crossterm::event::{self, Event},
    Terminal,
};

pub fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut app::App,
) -> io::Result<Option<String>> {
    loop {
        terminal.draw(|f| app.draw(f))?;

        if let Event::Key(key) = event::read()? {
            if app.handle_key(key) == EventState::Exit {
                break Ok(app.selected_path.take());
            }
        };
    }
}
