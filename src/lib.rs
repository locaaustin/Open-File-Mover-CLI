//! Open File Mover - a fast bulk file mover for the command line.
//!
//! The binary is a thin wrapper around [`run`]. Everything is exposed as a
//! library so that the engine can be tested without spawning processes.

pub mod cli;
pub mod commands;
pub mod engine;
pub mod log;
pub mod platform;
pub mod report;
pub mod ui;
pub mod util;

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use clap::{CommandFactory, Parser};

use crate::cli::{Cli, Command};
use crate::engine::plan::Mode;
use crate::log::Logger;
use crate::report::Summary;
use crate::ui::Ui;

/// Parse the command line and carry it out, returning the process exit code.
pub fn main_with_args<I, T>(args: I) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            // clap already formats help and version nicely; let it print.
            let _ = error.print();
            return if error.use_stderr() { 2 } else { 0 };
        }
    };

    platform::enable_ansi();
    let ui = Ui::from_args(&cli.global);
    let logger = Logger::new(cli.global.log.clone(), cli.global.no_log);

    let Some(command) = cli.command else {
        ui::banner(&ui);
        let _ = Cli::command().print_help();
        let _ = std::io::stdout().flush();
        return 0;
    };

    // Ctrl-C asks the workers to stop at the next file boundary rather than
    // killing the process mid-copy, so a cancelled run never leaves a
    // half-written file at the destination.
    let cancelled = Arc::new(AtomicBool::new(false));
    install_cancel_handler(Arc::clone(&cancelled), ui);

    match dispatch(command, &ui, &logger, cancelled) {
        Ok(Handled::Summarise(summary, mode)) => {
            summary.print(&ui, mode);
            summary.exit_code()
        }
        // `doctor` and `completions` write their own output. Appending a
        // summary table would add a second JSON document to `doctor --json`
        // and corrupt a completion script that is meant to be eval'd.
        Ok(Handled::SelfReported(summary)) => summary.exit_code(),
        // The cleanup commands narrate themselves as they go, so the table
        // would only repeat them. Scripts still get their JSON.
        Ok(Handled::JsonOnly(summary)) => {
            if ui.json {
                summary.print(&ui, None);
            }
            summary.exit_code()
        }
        Err(error) => {
            ui.error(format!("{error:#}"));
            logger.write(format!("error: {error:#}"));
            2
        }
    }
}

/// How a command's result should be presented.
enum Handled {
    /// Print the standard summary for this result.
    Summarise(Summary, Option<Mode>),
    /// The command has already produced all of its output.
    SelfReported(Summary),
    /// The command narrated itself; only `--json` needs the summary.
    JsonOnly(Summary),
}

fn dispatch(
    command: Command,
    ui: &Ui,
    logger: &Logger,
    cancelled: Arc<AtomicBool>,
) -> Result<Handled> {
    match command {
        Command::Move(args) => {
            let summary = commands::transfer::run(&args, Mode::Move, ui, logger, cancelled)?;
            Ok(Handled::Summarise(summary, Some(Mode::Move)))
        }
        Command::Copy(args) => {
            let summary = commands::transfer::run(&args, Mode::Copy, ui, logger, cancelled)?;
            Ok(Handled::Summarise(summary, Some(Mode::Copy)))
        }
        Command::Trash(args) => Ok(Handled::JsonOnly(commands::cleanup::trash(
            &args, ui, logger,
        )?)),
        Command::EmptyRecycle(args) => Ok(Handled::JsonOnly(commands::cleanup::empty_recycle(
            &args, ui, logger,
        )?)),
        Command::CleanDownloads(args) => Ok(Handled::JsonOnly(commands::cleanup::clean_downloads(
            &args, ui, logger, cancelled,
        )?)),
        Command::Doctor => Ok(Handled::SelfReported(commands::doctor::run(ui, logger)?)),
        Command::Completions(args) => {
            let mut command = Cli::command();
            let name = command.get_name().to_string();
            clap_complete::generate(args.shell, &mut command, name, &mut std::io::stdout());
            Ok(Handled::SelfReported(Summary::simple("completions", false)))
        }
    }
}

fn install_cancel_handler(cancelled: Arc<AtomicBool>, ui: Ui) {
    let result = ctrlc::set_handler(move || {
        if cancelled.swap(true, Ordering::SeqCst) {
            // A second Ctrl-C means the user is done being patient.
            std::process::exit(130);
        }
        eprintln!();
        ui.warn("stopping after the current file - press Ctrl-C again to quit immediately");
    });
    // A missing handler is not worth failing over; the tool still works.
    let _ = result;
}
