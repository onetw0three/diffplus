mod classify;
mod cli;
mod core;
mod diff;
mod model;
mod native;
mod output;
mod process;
mod progress;
mod scan;
mod tui;

use clap::Parser;

fn main() {
    let args = cli::Args::parse();
    let analyzer_options = tui::AnalyzerOptions::from_args(&args);
    if let Some(result) = &args.view {
        if let Err(error) = tui::run(result, analyzer_options) {
            eprintln!("{error:#}");
            std::process::exit(2);
        }
        return;
    }
    let show_tui = args.tui;
    let output = args.output.clone();
    match core::run(args) {
        Ok(changed) => {
            if show_tui {
                if let Err(error) = tui::run(&output, analyzer_options) {
                    eprintln!("{error:#}");
                    std::process::exit(2);
                }
            }
            if changed {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(2);
        }
    }
}
