//! Interactive terminal viewer for persisted comparison results.

mod app;
mod view;

use anyhow::{bail, Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io::IsTerminal, path::Path, time::Duration};

pub(crate) fn run(result: &Path) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("the terminal UI requires an interactive stdin and stdout");
    }

    let mut app = app::App::load(result)?;
    let mut terminal = TerminalSession::start()?;
    loop {
        terminal.terminal.draw(|frame| view::render(frame, &app))?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if app.handle_key(key)? => break,
                Event::Mouse(mouse) => {
                    let (width, height) = crossterm::terminal::size()?;
                    app.handle_mouse(mouse, width, height);
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl TerminalSession {
    fn start() -> Result<Self> {
        enable_raw_mode().context("enabling terminal raw mode")?;
        let mut stdout = std::io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(error).context("entering alternate terminal screen");
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = std::io::stdout();
                let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
                return Err(error).context("initializing terminal");
            }
        };
        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}
