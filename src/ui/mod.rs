//! Terminal presentation: colour handling, the banner, and confirmations.

pub mod progress;

use std::io::{self, IsTerminal, Write};

use crate::cli::{ColorMode, GlobalArgs};

/// Shared output settings, derived once from the command line.
#[derive(Debug, Clone, Copy)]
pub struct Ui {
    pub color: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub json: bool,
    pub assume_yes: bool,
    pub progress: bool,
}

impl Ui {
    pub fn from_args(args: &GlobalArgs) -> Self {
        let tty = io::stdout().is_terminal();
        let color = match args.color {
            ColorMode::Always => true,
            ColorMode::Never => false,
            // Respect the de facto standard for opting out of colour.
            ColorMode::Auto => tty && std::env::var_os("NO_COLOR").is_none(),
        };
        Self {
            color,
            quiet: args.quiet,
            verbose: args.verbose,
            json: args.json,
            assume_yes: args.yes,
            progress: !args.no_progress && tty && !args.quiet && !args.json,
        }
    }

    pub fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn green(&self, text: &str) -> String {
        self.paint("92", text)
    }

    pub fn yellow(&self, text: &str) -> String {
        self.paint("93", text)
    }

    pub fn red(&self, text: &str) -> String {
        self.paint("91", text)
    }

    pub fn cyan(&self, text: &str) -> String {
        self.paint("96", text)
    }

    /// Print unless `--quiet` or `--json` is in force.
    pub fn say(&self, text: impl AsRef<str>) {
        if !self.quiet && !self.json {
            println!("{}", text.as_ref());
        }
    }

    pub fn warn(&self, text: impl AsRef<str>) {
        eprintln!("{} {}", self.yellow("warning:"), text.as_ref());
    }

    pub fn error(&self, text: impl AsRef<str>) {
        eprintln!("{} {}", self.red("error:"), text.as_ref());
    }

    /// Ask a yes/no question. Returns `true` only on an explicit yes.
    ///
    /// Without a terminal to ask, the answer is no unless `--yes` was passed:
    /// a script that pipes `ofm` somewhere should never have files deleted
    /// out from under it because nobody was there to say no.
    pub fn confirm(&self, question: &str) -> io::Result<bool> {
        if self.assume_yes {
            return Ok(true);
        }
        if !io::stdin().is_terminal() {
            self.error(format!(
                "{question} - refusing to guess. Re-run with --yes to confirm, or --dry-run to preview."
            ));
            return Ok(false);
        }

        loop {
            print!("{} {} ", self.bold(question), self.dim("[y/N]"));
            io::stdout().flush()?;
            let mut answer = String::new();
            if io::stdin().read_line(&mut answer)? == 0 {
                println!();
                return Ok(false);
            }
            match answer.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => return Ok(true),
                "" | "n" | "no" => return Ok(false),
                _ => self.say("Please answer y or n."),
            }
        }
    }
}

/// The banner, kept from the original tool because it was a nice touch.
pub fn banner(ui: &Ui) {
    if ui.quiet || ui.json {
        return;
    }
    let version = crate::cli::BANNER_VERSION;
    let lines = [
        "  ___                   ___ _ _        __  __                   ".to_string(),
        " / _ \\ _ __  ___ _ _   | __(_) |___   |  \\/  |___ _  _ ___ _ _  ".to_string(),
        "| (_) | '_ \\/ -_) ' \\  | _|| | / -_)  | |\\/| / _ \\ V / -_) '_|  ".to_string(),
        " \\___/| .__/\\___|_||_| |_| |_|_\\___|  |_|  |_\\___/\\_/\\___|_|    ".to_string(),
        "      |_|                                                       ".to_string(),
    ];
    println!();
    for line in lines {
        println!("{}", ui.cyan(&line));
    }
    println!(
        "  {} {}   {}",
        ui.dim("v"),
        ui.bold(version),
        ui.dim("move mountains of files - try `ofm --help`")
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(color: bool) -> Ui {
        Ui {
            color,
            quiet: false,
            verbose: false,
            json: false,
            assume_yes: false,
            progress: false,
        }
    }

    #[test]
    fn colour_can_be_turned_off() {
        assert_eq!(ui(false).green("ok"), "ok");
        assert!(ui(true).green("ok").contains("\x1b["));
    }

    #[test]
    fn yes_flag_short_circuits_confirmation() {
        let mut u = ui(false);
        u.assume_yes = true;
        assert!(u.confirm("Delete everything?").unwrap());
    }
}
