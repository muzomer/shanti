use shanti::{app, cli, github, logs, run_app, Outcome};
use std::{io, process::ExitCode};

use color_eyre::eyre::{Result, WrapErr};
use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        event::{DisableBracketedPaste, EnableBracketedPaste},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    Terminal,
};

fn main() -> ExitCode {
    // `session` has already restored the terminal by the time it returns, so it is
    // safe to write here — both to stdout (the machine-readable channel that
    // `cd $(shanti)` consumes) and to stderr (everything human-facing).
    match session() {
        Ok(Outcome::Selected(path)) => {
            println!("{path}");
            ExitCode::SUCCESS
        }
        // A deliberate quit must leave the shell where it is, so print no path.
        Ok(Outcome::Quit) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shanti: {error:?}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one TUI session and always leaves the terminal usable again.
///
/// Teardown happens before this returns on every path, including the error one:
/// a message written while the alternate screen is still up is painted onto a
/// buffer that is about to be discarded, so the user would never see it.
fn session() -> Result<Outcome> {
    logs::initialize_logging().wrap_err("failed to initialize logging")?;

    let args = cli::Args::try_new().wrap_err("failed to resolve the configuration")?;
    // `--show-config` is an inspection command, not a session: answer it here,
    // before the alternate screen goes up, and leave the shell where it is.
    if args.show_config {
        print!("{}", args.report());
        return Ok(Outcome::Quit);
    }

    let mut app = app::App::with_args(args, github::live_fetcher());
    let mut terminal = setup_terminal().wrap_err("failed to set up the terminal")?;

    let outcome = run_app(&mut terminal, &mut app);

    // Restore first, then decide what to report: a failed restore must not hide
    // the event-loop error, which is the more useful of the two.
    let restored = restore_terminal(&mut terminal);
    let outcome = outcome?;
    restored.wrap_err("failed to restore the terminal")?;

    Ok(outcome)
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    // Bracketed paste makes the terminal deliver a paste as one event instead of
    // as one key press per character — the PR URL prompt depends on it.
    //
    // Raw mode is already on, so undo it if the rest of the setup fails —
    // otherwise the user is dropped back into an unusable shell.
    if let Err(error) = execute!(stderr, EnterAlternateScreen, EnableBracketedPaste) {
        let _ = disable_raw_mode();
        return Err(error);
    }
    let backend = CrosstermBackend::new(stderr);
    Terminal::new(backend)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    disable_raw_mode()?;
    // Bracketed paste is a mode of the user's terminal, not of ours: leaving it
    // on would make every later paste in that shell arrive wrapped in escapes.
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()
}
