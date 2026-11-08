//! The destructive conveniences: `trash`, `empty-recycle` and
//! `clean-downloads`.
//!
//! Every one of these can delete something the user wanted, so they all
//! default to the recoverable option (the Recycle Bin), state exactly what
//! they are about to do, and ask before doing it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::cli::{CleanDownloadsArgs, EmptyRecycleArgs, TrashArgs};
use crate::engine::filters::Filter;
use crate::log::Logger;
use crate::platform;
use crate::report::{Failure, Summary};
use crate::ui::Ui;
use crate::util;

/// `ofm trash` - send paths to the Recycle Bin.
pub fn trash(args: &TrashArgs, ui: &Ui, logger: &Logger) -> Result<Summary> {
    let mut targets = Vec::new();
    let mut summary = Summary::simple("trash", args.dry_run);

    for path in &args.paths {
        let absolute = util::absolutize(path)
            .with_context(|| format!("cannot resolve `{}`", path.display()))?;
        if !absolute.exists() {
            summary.failures.push(Failure {
                path: absolute,
                destination: None,
                message: "does not exist".into(),
            });
            continue;
        }
        targets.push(absolute);
    }
    summary.failed = summary.failures.len() as u64;

    if targets.is_empty() {
        ui.say("Nothing to recycle.");
        return Ok(summary);
    }

    ui.say(format!(
        "{} {} to the Recycle Bin:",
        if args.dry_run {
            "Would send"
        } else {
            "Sending"
        },
        count(targets.len(), "item", "items")
    ));
    for target in &targets {
        ui.say(format!("  {}", target.display()));
    }

    if args.dry_run {
        summary.files = targets.len() as u64;
        return Ok(summary);
    }

    if !ui.confirm(&format!(
        "Send {} to the Recycle Bin?",
        count(targets.len(), "item", "items")
    ))? {
        ui.say("Left alone.");
        return Ok(summary);
    }

    platform::send_to_recycle_bin(&targets)?;
    summary.files = targets.len() as u64;
    logger.write(format!("recycled {} items", targets.len()));
    ui.say(ui.green(&format!(
        "Sent {} to the Recycle Bin. Restore from Explorer if you need them back.",
        count(targets.len(), "item", "items")
    )));
    Ok(summary)
}

/// `ofm empty-recycle` - permanently empty the Recycle Bin.
pub fn empty_recycle(args: &EmptyRecycleArgs, ui: &Ui, logger: &Logger) -> Result<Summary> {
    let mut summary = Summary::simple("empty-recycle", args.dry_run);
    let contents = platform::recycle_bin_contents(args.drive.as_deref())?;

    let scope = match &args.drive {
        Some(drive) => format!(" on {drive}"),
        None => String::new(),
    };

    if contents.items == 0 {
        ui.say(format!("The Recycle Bin{scope} is already empty."));
        return Ok(summary);
    }

    ui.say(format!(
        "The Recycle Bin{scope} holds {} using {}.",
        count(contents.items as usize, "item", "items"),
        util::format_bytes(contents.bytes)
    ));

    if args.dry_run {
        summary.files = contents.items;
        summary.bytes = contents.bytes;
        summary.bytes_human = util::format_bytes(contents.bytes);
        return Ok(summary);
    }

    if !ui.confirm("Permanently delete everything in the Recycle Bin? This cannot be undone.")? {
        ui.say("Left alone.");
        return Ok(summary);
    }

    platform::empty_recycle_bin(args.drive.as_deref())?;
    summary.files = contents.items;
    summary.bytes = contents.bytes;
    summary.bytes_human = util::format_bytes(contents.bytes);
    logger.write(format!(
        "emptied the Recycle Bin{scope}: {} items, {}",
        contents.items,
        util::format_bytes(contents.bytes)
    ));
    ui.say(ui.green(&format!(
        "Emptied the Recycle Bin{scope} and reclaimed {}.",
        util::format_bytes(contents.bytes)
    )));
    Ok(summary)
}

/// `ofm clean-downloads` - clear out the Downloads folder.
///
/// Unlike the tool this replaces, the default is to recycle rather than
/// permanently delete, and nothing happens without a confirmation.
pub fn clean_downloads(args: &CleanDownloadsArgs, ui: &Ui, logger: &Logger) -> Result<Summary> {
    let folder = match &args.dir {
        Some(dir) => util::absolutize(dir)?,
        None => platform::downloads_dir()?,
    };

    let mut summary = Summary::simple("clean-downloads", args.dry_run);
    summary.destination = args.to.clone();

    if !folder.is_dir() {
        bail!("`{}` is not a folder", folder.display());
    }

    let filter = Filter::for_cleanup(&args.ext, &args.exclude, args.older_than)?;
    let mut victims: Vec<(PathBuf, u64, bool)> = Vec::new();
    let mut total = 0u64;

    for entry in
        fs::read_dir(&folder).with_context(|| format!("cannot read `{}`", folder.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                summary.failures.push(Failure {
                    path: folder.clone(),
                    destination: None,
                    message: error.to_string(),
                });
                continue;
            }
        };
        let path = entry.path();
        let name = PathBuf::from(entry.file_name());
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                summary.failures.push(Failure {
                    path: path.clone(),
                    destination: None,
                    message: error.to_string(),
                });
                continue;
            }
        };

        let is_dir = metadata.is_dir();
        // Directories cannot be filtered by extension, so an extension
        // filter means "files only".
        if is_dir && !args.ext.is_empty() {
            continue;
        }
        if filter.check(&name, &metadata).is_err() {
            continue;
        }

        let size = if is_dir {
            directory_size(&path)
        } else {
            metadata.len()
        };
        total += size;
        victims.push((path, size, is_dir));
    }

    if victims.is_empty() {
        ui.say(format!("Nothing to clear in {}.", folder.display()));
        return Ok(summary);
    }

    let fate = if args.to.is_some() {
        Fate::Move
    } else if args.permanent {
        Fate::Delete
    } else {
        Fate::Recycle
    };

    ui.say(format!(
        "{} {} ({}) from {}{}",
        if args.dry_run { "Would" } else { "About to" },
        fate_phrase(fate, victims.len()),
        util::format_bytes(total),
        folder.display(),
        if args.dry_run {
            " - nothing will change"
        } else {
            ""
        }
    ));
    let preview = 20;
    for (path, size, is_dir) in victims.iter().take(preview) {
        ui.say(format!(
            "  {}{}  {}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            if *is_dir { "/" } else { "" },
            ui.dim(&util::format_bytes(*size))
        ));
    }
    if victims.len() > preview {
        ui.say(ui.dim(&format!("  ... and {} more", victims.len() - preview)));
    }

    summary.bytes = total;
    summary.bytes_human = util::format_bytes(total);

    if args.dry_run {
        summary.files = victims.len() as u64;
        return Ok(summary);
    }

    let question = match fate {
        Fate::Delete => format!(
            "Permanently delete {}? This cannot be undone.",
            count(victims.len(), "entry", "entries")
        ),
        Fate::Recycle => format!(
            "Send {} to the Recycle Bin?",
            count(victims.len(), "entry", "entries")
        ),
        Fate::Move => format!("Move {}?", count(victims.len(), "entry", "entries")),
    };
    if !ui.confirm(&question)? {
        ui.say("Left alone.");
        return Ok(summary);
    }

    let started = Instant::now();
    let paths: Vec<PathBuf> = victims.iter().map(|(p, _, _)| p.clone()).collect();

    if let Some(destination) = &args.to {
        let destination = util::absolutize(destination)?;
        fs::create_dir_all(&destination)
            .with_context(|| format!("cannot create `{}`", destination.display()))?;
        let mut moved = 0u64;
        for path in &paths {
            let target = destination.join(path.file_name().unwrap_or_default());
            match fs::rename(path, &target) {
                Ok(()) => moved += 1,
                Err(error) => summary.failures.push(Failure {
                    path: path.clone(),
                    destination: Some(target),
                    message: error.to_string(),
                }),
            }
        }
        summary.files = moved;
        ui.say(ui.green(&format!(
            "Moved {} into {}.",
            count(moved as usize, "entry", "entries"),
            destination.display()
        )));
    } else if args.permanent {
        let mut removed = 0u64;
        for (path, _, is_dir) in &victims {
            let result = if *is_dir {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            match result {
                Ok(()) => removed += 1,
                Err(error) => summary.failures.push(Failure {
                    path: path.clone(),
                    destination: None,
                    message: error.to_string(),
                }),
            }
        }
        summary.files = removed;
        ui.say(ui.green(&format!(
            "Permanently deleted {} and reclaimed {}.",
            count(removed as usize, "entry", "entries"),
            util::format_bytes(total)
        )));
    } else {
        platform::send_to_recycle_bin(&paths)?;
        summary.files = paths.len() as u64;
        ui.say(ui.green(&format!(
            "Sent {} to the Recycle Bin. Restore from Explorer if you need them back.",
            count(paths.len(), "entry", "entries")
        )));
    }

    summary.failed = summary.failures.len() as u64;
    summary.elapsed_ms = started.elapsed().as_millis();
    logger.write(format!(
        "cleaned {}: {} entries, {} ({})",
        folder.display(),
        summary.files,
        util::format_bytes(total),
        fate.verb()
    ));

    Ok(summary)
}

/// Total size of a directory tree, for the preview. Unreadable entries are
/// treated as zero rather than failing the whole listing.
fn directory_size(root: &Path) -> u64 {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What `clean-downloads` is about to do with the entries it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Send to the Recycle Bin (the default, because it is undoable).
    Recycle,
    /// Delete outright.
    Delete,
    /// Relocate to another folder.
    Move,
}

impl Fate {
    fn verb(self) -> &'static str {
        match self {
            Fate::Recycle => "recycled",
            Fate::Delete => "deleted",
            Fate::Move => "moved",
        }
    }
}

/// "send 3 entries to the Recycle Bin", reading naturally after
/// "Would" or "About to".
fn fate_phrase(fate: Fate, entries: usize) -> String {
    let entries = count(entries, "entry", "entries");
    match fate {
        Fate::Recycle => format!("send {entries} to the Recycle Bin"),
        Fate::Delete => format!("permanently delete {entries}"),
        Fate::Move => format!("move {entries}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_naturally() {
        assert_eq!(count(1, "entry", "entries"), "1 entry");
        assert_eq!(count(3, "entry", "entries"), "3 entries");
        assert_eq!(count(0, "item", "items"), "0 items");
    }

    #[test]
    fn fate_phrases_read_as_english() {
        assert_eq!(
            format!("Would {}", fate_phrase(Fate::Recycle, 352)),
            "Would send 352 entries to the Recycle Bin"
        );
        assert_eq!(
            format!("About to {}", fate_phrase(Fate::Delete, 1)),
            "About to permanently delete 1 entry"
        );
        assert_eq!(
            format!("Would {}", fate_phrase(Fate::Move, 2)),
            "Would move 2 entries"
        );
    }

    #[test]
    fn directory_size_sums_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b")).unwrap();
        fs::write(dir.path().join("a/one.bin"), vec![0u8; 100]).unwrap();
        fs::write(dir.path().join("a/b/two.bin"), vec![0u8; 250]).unwrap();
        assert_eq!(directory_size(dir.path()), 350);
    }

    #[test]
    fn directory_size_of_nothing_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(directory_size(&dir.path().join("missing")), 0);
    }
}
