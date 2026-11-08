//! Command-line surface. Everything the user can ask for is declared here so
//! that `--help` stays the single source of truth.

use std::path::PathBuf;
use std::time::SystemTime;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::util;

pub const BANNER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(
    name = "ofm",
    version,
    about = "Open File Mover - move mountains of files from the command line.",
    long_about = "Open File Mover (ofm) moves, copies and tidies up large numbers of \
                  files quickly.\n\nMoves within the same volume are instant renames; \
                  moves across volumes stream in parallel with a live progress bar. \
                  Every destructive command supports --dry-run.",
    after_help = "EXAMPLES:\n  \
        ofm move D:/camera-dump E:/archive\n  \
        ofm move C:/inbox --to D:/sorted --ext jpg,png,raw --group-by date\n  \
        ofm move C:/build/out --to //nas/releases --contents --on-conflict rename\n  \
        ofm copy C:/project --to D:/backup --exclude '**/node_modules/**' --verify\n  \
        ofm clean-downloads --older-than 30d --dry-run\n  \
        ofm recycle\n  \
        ofm doctor",
    propagate_version = true,
    disable_help_subcommand = false
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub global: GlobalArgs,
}

/// Flags accepted both before and after the subcommand.
#[derive(Debug, Args, Clone)]
pub struct GlobalArgs {
    /// Only print warnings and errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Print every file as it is handled.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Print the machine-readable summary as JSON instead of a table.
    #[arg(long, global = true)]
    pub json: bool,

    /// Assume "yes" for every confirmation prompt.
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,

    /// Never draw progress bars.
    #[arg(long, global = true)]
    pub no_progress: bool,

    /// Hide the startup banner.
    #[arg(long, global = true)]
    pub no_banner: bool,

    /// When to use colour in the output.
    #[arg(long, global = true, value_enum, default_value_t = ColorMode::Auto)]
    pub color: ColorMode,

    /// Append activity to this log file instead of the default location.
    #[arg(long, global = true, value_name = "FILE")]
    pub log: Option<PathBuf>,

    /// Do not write an activity log at all.
    #[arg(long, global = true, conflicts_with = "log")]
    pub no_log: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorMode {
    /// Colour when writing to a terminal.
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Move files or folders, fast.
    #[command(visible_aliases = ["mv", "move-folder"])]
    Move(TransferArgs),

    /// Copy files or folders, leaving the originals in place.
    #[command(visible_alias = "cp")]
    Copy(TransferArgs),

    /// Send files or folders to the Recycle Bin (undoable).
    #[command(visible_alias = "rm")]
    Trash(TrashArgs),

    /// Permanently empty the Recycle Bin.
    #[command(visible_aliases = ["recycle", "er"])]
    EmptyRecycle(EmptyRecycleArgs),

    /// Clear out the Downloads folder.
    #[command(visible_aliases = ["ed", "empty-downloads"])]
    CleanDownloads(CleanDownloadsArgs),

    /// Show environment details and where the log lives.
    Doctor,

    /// Generate a shell completion script.
    Completions(CompletionsArgs),
}

/// Shared arguments for `move` and `copy`.
#[derive(Debug, Args, Clone)]
pub struct TransferArgs {
    /// Sources to transfer. With no --to, the last path is the destination.
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<PathBuf>,

    /// Destination directory.
    #[arg(short = 't', long = "to", value_name = "DIR")]
    pub to: Option<PathBuf>,

    /// Show what would happen without touching anything.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    // ---- what to pick up -------------------------------------------------
    /// Only files with these extensions (comma separated, no dots needed).
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub ext: Vec<String>,

    /// Only paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,

    /// Skip paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Only files at least this big (e.g. 10MB).
    #[arg(long, value_name = "SIZE", value_parser = parse_size_arg)]
    pub min_size: Option<u64>,

    /// Only files no bigger than this (e.g. 2GB).
    #[arg(long, value_name = "SIZE", value_parser = parse_size_arg)]
    pub max_size: Option<u64>,

    /// Only files modified more recently than this (e.g. 7d, 2026-01-31).
    #[arg(long, value_name = "AGE", value_parser = parse_age_arg)]
    pub newer_than: Option<SystemTime>,

    /// Only files last modified before this (e.g. 30d, 2026-01-31).
    #[arg(long, value_name = "AGE", value_parser = parse_age_arg)]
    pub older_than: Option<SystemTime>,

    /// Leave hidden and system files behind.
    ///
    /// Everything is taken by default: a mover that quietly skipped `.git` or
    /// `desktop.ini` would split a folder in half.
    #[arg(long)]
    pub skip_hidden: bool,

    /// Limit how deep to descend. 0 means the top level only.
    #[arg(long, value_name = "N")]
    pub depth: Option<usize>,

    /// Follow symlinks and directory junctions instead of skipping them.
    #[arg(long)]
    pub follow_links: bool,

    // ---- how it lands ----------------------------------------------------
    /// Move the contents of each source directory rather than the directory.
    #[arg(long)]
    pub contents: bool,

    /// Drop every file straight into the destination, discarding subfolders.
    #[arg(long)]
    pub flatten: bool,

    /// Sort files into destination subfolders as they land.
    #[arg(long, value_enum, value_name = "KEY")]
    pub group_by: Option<GroupBy>,

    /// What to do when the destination file already exists.
    #[arg(long, value_enum, default_value_t = Conflict::Rename, value_name = "POLICY")]
    pub on_conflict: Conflict,

    // ---- how it runs -----------------------------------------------------
    /// Number of parallel workers. Defaults to a value chosen per transfer.
    #[arg(short = 'j', long, value_name = "N")]
    pub workers: Option<usize>,

    /// Byte-for-byte compare each copy before the original is removed.
    #[arg(long)]
    pub verify: bool,

    /// Stop at the first error instead of carrying on.
    #[arg(long)]
    pub fail_fast: bool,

    /// Leave emptied source directories behind after a move.
    #[arg(long)]
    pub keep_empty_dirs: bool,

    /// Transfer every file individually, skipping the whole-folder rename.
    #[arg(long)]
    pub no_fast_path: bool,
}

#[derive(Debug, Args, Clone)]
pub struct TrashArgs {
    /// Files or folders to send to the Recycle Bin.
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<PathBuf>,

    /// Show what would be recycled without doing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone)]
pub struct EmptyRecycleArgs {
    /// Only empty the Recycle Bin of this drive (e.g. C:).
    #[arg(long, value_name = "DRIVE")]
    pub drive: Option<String>,

    /// Report what is in the bin without emptying it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone)]
pub struct CleanDownloadsArgs {
    /// Clean this folder instead of the detected Downloads folder.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Only remove entries older than this (e.g. 30d).
    #[arg(long, value_name = "AGE", value_parser = parse_age_arg)]
    pub older_than: Option<SystemTime>,

    /// Only entries with these extensions (comma separated).
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub ext: Vec<String>,

    /// Skip paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Delete permanently instead of sending to the Recycle Bin.
    #[arg(long)]
    pub permanent: bool,

    /// Move the entries into this folder instead of deleting them.
    #[arg(long, value_name = "DIR", conflicts_with = "permanent")]
    pub to: Option<PathBuf>,

    /// Show what would be removed without removing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone)]
pub struct CompletionsArgs {
    /// Shell to generate a completion script for.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

/// What to do when a destination file is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Conflict {
    /// Give the incoming file a numbered name: `report (1).txt`.
    Rename,
    /// Leave the existing file alone and skip the source.
    Skip,
    /// Replace the existing file.
    Overwrite,
    /// Replace it only when the source is newer.
    Newer,
}

/// Destination subfolder layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupBy {
    /// One folder per extension: `jpg/`, `pdf/`, `no-extension/`.
    Ext,
    /// One folder per month of last modification: `2026-11/`.
    Date,
    /// One folder per year of last modification: `2026/`.
    Year,
}

fn parse_size_arg(raw: &str) -> Result<u64, String> {
    util::parse_size(raw)
}

fn parse_age_arg(raw: &str) -> Result<SystemTime, String> {
    util::parse_age(raw)
}

impl TransferArgs {
    /// Split `paths` into sources plus a destination, honouring `--to`.
    pub fn split_destination(&self) -> anyhow::Result<(Vec<PathBuf>, PathBuf)> {
        if let Some(dest) = &self.to {
            return Ok((self.paths.clone(), dest.clone()));
        }
        if self.paths.len() < 2 {
            anyhow::bail!("need a destination: either pass it as the last path or use --to <DIR>");
        }
        let mut sources = self.paths.clone();
        let dest = sources.pop().expect("checked length above");
        Ok((sources, dest))
    }

    /// `true` when no content filter would exclude anything.
    pub fn has_filters(&self) -> bool {
        !self.ext.is_empty()
            || !self.include.is_empty()
            || !self.exclude.is_empty()
            || self.min_size.is_some()
            || self.max_size.is_some()
            || self.newer_than.is_some()
            || self.older_than.is_some()
            || self.depth.is_some()
            || self.skip_hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn trailing_path_is_the_destination() {
        let cli = Cli::try_parse_from(["ofm", "move", "a", "b", "dest"]).unwrap();
        let Some(Command::Move(args)) = cli.command else {
            panic!("expected a move command");
        };
        let (sources, dest) = args.split_destination().unwrap();
        assert_eq!(sources, vec![PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(dest, PathBuf::from("dest"));
    }

    #[test]
    fn explicit_to_keeps_every_path_as_a_source() {
        let cli = Cli::try_parse_from(["ofm", "move", "a", "b", "--to", "dest"]).unwrap();
        let Some(Command::Move(args)) = cli.command else {
            panic!("expected a move command");
        };
        let (sources, dest) = args.split_destination().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(dest, PathBuf::from("dest"));
    }

    #[test]
    fn a_single_path_without_to_is_an_error() {
        let cli = Cli::try_parse_from(["ofm", "move", "only-one"]).unwrap();
        let Some(Command::Move(args)) = cli.command else {
            panic!("expected a move command");
        };
        assert!(args.split_destination().is_err());
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli = Cli::try_parse_from(["ofm", "move", "a", "b", "--json", "-y"]).unwrap();
        assert!(cli.global.json);
        assert!(cli.global.yes);
    }

    #[test]
    fn legacy_aliases_still_resolve() {
        for alias in ["mv", "move-folder"] {
            let cli = Cli::try_parse_from(["ofm", alias, "a", "b"]).unwrap();
            assert!(matches!(cli.command, Some(Command::Move(_))));
        }
        for alias in ["recycle", "er"] {
            let cli = Cli::try_parse_from(["ofm", alias]).unwrap();
            assert!(
                matches!(cli.command, Some(Command::EmptyRecycle(_))),
                "`{alias}` should empty the Recycle Bin"
            );
        }
        // `trash` needs paths, so a bare `rm` is a usage error rather than a
        // successful parse; what matters is that it is still the trash command.
        let cli = Cli::try_parse_from(["ofm", "rm", "a"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Trash(_))));
        assert!(matches!(
            Cli::try_parse_from(["ofm", "ed"]).unwrap().command,
            Some(Command::CleanDownloads(_))
        ));
    }

    #[test]
    fn sizes_and_ages_are_parsed_by_clap() {
        let cli = Cli::try_parse_from([
            "ofm",
            "move",
            "a",
            "b",
            "--min-size",
            "10MB",
            "--older-than",
            "7d",
        ])
        .unwrap();
        let Some(Command::Move(args)) = cli.command else {
            panic!("expected a move command");
        };
        assert_eq!(args.min_size, Some(10_000_000));
        assert!(args.older_than.is_some());
    }

    #[test]
    fn bad_size_is_rejected() {
        assert!(Cli::try_parse_from(["ofm", "move", "a", "b", "--min-size", "ten"]).is_err());
    }
}
