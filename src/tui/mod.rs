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
use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub(crate) struct AnalyzerOptions {
    pub(crate) jadx_path: PathBuf,
    pub(crate) ilspy_path: PathBuf,
    pub(crate) ida_path: Option<PathBuf>,
    pub(crate) diaphora_script: Option<PathBuf>,
    pub(crate) diaphora_path: Option<PathBuf>,
    pub(crate) python_path: PathBuf,
    pub(crate) cache_dir: Option<PathBuf>,
    pub(crate) no_cache: bool,
}

impl AnalyzerOptions {
    pub(crate) fn from_args(args: &crate::cli::Args) -> Self {
        Self {
            jadx_path: args.jadx_path.clone(),
            ilspy_path: args.ilspy_path.clone(),
            ida_path: args.ida_path.clone(),
            diaphora_script: args.diaphora_script.clone(),
            diaphora_path: args.diaphora_path.clone(),
            python_path: args.python_path.clone(),
            cache_dir: args.cache_dir.clone(),
            no_cache: args.no_cache,
        }
    }
}

pub(crate) fn run(result: &Path, options: AnalyzerOptions) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("the terminal UI requires an interactive stdin and stdout");
    }

    let mut app = app::App::load(result)?;
    let mut terminal = TerminalSession::start()?;
    let mut analysis_job: Option<(
        app::AnalysisRequest,
        std::thread::JoinHandle<anyhow::Result<()>>,
    )> = None;
    loop {
        if analysis_job
            .as_ref()
            .is_some_and(|(_, handle)| handle.is_finished())
        {
            let (request, handle) = analysis_job.take().expect("finished analyzer job exists");
            let result = handle
                .join()
                .map_err(|_| anyhow::anyhow!("analyzer thread panicked"))
                .and_then(|result| result);
            crate::progress::set_enabled(true);
            app.finish_analysis(request, result);
        }
        terminal.terminal.draw(|frame| view::render(frame, &app))?;
        if analysis_job.is_none() {
            if let Some(request) = app.take_analysis_request() {
                let old_blob = request.old_blob.clone();
                let new_blob = request.new_blob.clone();
                let old_name = request.old_name.clone();
                let new_name = request.new_name.clone();
                let output = request.output.clone();
                let kind = request.kind;
                let options = options.clone();
                crate::progress::set_enabled(false);
                let handle = std::thread::spawn(move || match kind {
                    app::AnalyzerKind::Text => crate::core::run_text_diff(
                        &old_blob, &new_blob, &old_name, &new_name, &output,
                    ),
                    app::AnalyzerKind::Jadx => crate::core::run_jadx_diff(
                        &old_blob,
                        &new_blob,
                        &old_name,
                        &new_name,
                        &output,
                        &options.jadx_path,
                    ),
                    app::AnalyzerKind::Ilspy => crate::core::run_dotnet_diff(
                        &old_blob,
                        &new_blob,
                        &old_name,
                        &new_name,
                        &output,
                        &options.ilspy_path,
                    ),
                    app::AnalyzerKind::Ida => crate::core::run_native_diff(
                        &old_blob,
                        &new_blob,
                        &old_name,
                        &new_name,
                        &output,
                        options
                            .ida_path
                            .as_deref()
                            .context("--ida-path is required for on-demand native analysis")?,
                        &options.python_path,
                        options.diaphora_script.as_deref().context(
                            "--diaphora-script is required for on-demand native analysis",
                        )?,
                        options
                            .diaphora_path
                            .as_deref()
                            .context("--diaphora-path is required for on-demand native analysis")?,
                        options.cache_dir.as_deref(),
                        options.no_cache,
                    ),
                });
                analysis_job = Some((request, handle));
                continue;
            }
        }
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
