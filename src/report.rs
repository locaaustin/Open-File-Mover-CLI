//! What happened, in a form suitable for both humans and scripts.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::engine::plan::{Mode, Plan};
use crate::ui::Ui;
use crate::util;

/// One thing that went wrong.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
    pub message: String,
}

/// Counters produced by the executor.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Files that moved by rename (no data copied).
    pub renamed: u64,
    /// Files whose bytes were streamed to another volume.
    pub copied: u64,
    /// Whole directory trees moved with a single rename.
    pub trees: u64,
    pub bytes_moved: u64,
    pub directories_pruned: u64,
    pub failures: Vec<Failure>,
    pub elapsed: Duration,
    pub cancelled: bool,
    pub workers: usize,
}

impl Outcome {
    pub fn files_done(&self) -> u64 {
        self.renamed + self.copied
    }
}

/// The complete, serialisable result of one command.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub command: String,
    pub dry_run: bool,
    pub destination: Option<PathBuf>,
    pub files: u64,
    pub trees: u64,
    pub renamed: u64,
    pub copied: u64,
    pub bytes: u64,
    pub bytes_human: String,
    pub skipped: u64,
    pub failed: u64,
    pub directories_pruned: u64,
    pub elapsed_ms: u128,
    pub cancelled: bool,
    pub workers: usize,
    pub failures: Vec<Failure>,
    pub skipped_detail: Vec<SkippedEntry>,
}

#[derive(Debug, Serialize)]
pub struct SkippedEntry {
    pub path: PathBuf,
    pub reason: String,
}

impl Summary {
    pub fn from_transfer(mode: Mode, plan: &Plan, outcome: &Outcome, dry_run: bool) -> Self {
        Self {
            command: mode.verb().to_string(),
            dry_run,
            destination: Some(plan.destination_root.clone()),
            files: if dry_run {
                plan.file_count() as u64
            } else {
                outcome.files_done()
            },
            trees: if dry_run {
                plan.tree_count() as u64
            } else {
                outcome.trees
            },
            renamed: outcome.renamed,
            copied: outcome.copied,
            bytes: if dry_run {
                plan.total_bytes()
            } else {
                outcome.bytes_moved
            },
            bytes_human: util::format_bytes(if dry_run {
                plan.total_bytes()
            } else {
                outcome.bytes_moved
            }),
            skipped: plan.skipped.len() as u64,
            failed: outcome.failures.len() as u64,
            directories_pruned: outcome.directories_pruned,
            elapsed_ms: outcome.elapsed.as_millis(),
            cancelled: outcome.cancelled,
            workers: outcome.workers,
            failures: outcome.failures.clone(),
            skipped_detail: plan
                .skipped
                .iter()
                .map(|s| SkippedEntry {
                    path: s.path.clone(),
                    reason: s.reason.describe(),
                })
                .collect(),
        }
    }

    /// A summary for the commands that are not transfers.
    pub fn simple(command: &str, dry_run: bool) -> Self {
        Self {
            command: command.to_string(),
            dry_run,
            destination: None,
            files: 0,
            trees: 0,
            renamed: 0,
            copied: 0,
            bytes: 0,
            bytes_human: util::format_bytes(0),
            skipped: 0,
            failed: 0,
            directories_pruned: 0,
            elapsed_ms: 0,
            cancelled: false,
            workers: 1,
            failures: Vec::new(),
            skipped_detail: Vec::new(),
        }
    }

    /// The process exit code this result should produce.
    pub fn exit_code(&self) -> u8 {
        if self.cancelled {
            130
        } else if self.failed > 0 {
            1
        } else {
            0
        }
    }

    /// Print either the JSON document or the human table.
    pub fn print(&self, ui: &Ui, mode: Option<Mode>) {
        if ui.json {
            match serde_json::to_string_pretty(self) {
                Ok(json) => println!("{json}"),
                Err(error) => ui.error(format!("could not render JSON: {error}")),
            }
            return;
        }

        if ui.quiet {
            return;
        }

        let verb = match (self.dry_run, mode) {
            (true, Some(mode)) => format!("would {}", mode.verb()),
            (true, None) => "would process".to_string(),
            (false, Some(mode)) => mode.past_tense().to_string(),
            (false, None) => "processed".to_string(),
        };

        println!();
        let headline = if self.trees > 0 && self.files == 0 {
            format!(
                "{} {} {}",
                ui.bold(&verb),
                ui.bold(&self.trees.to_string()),
                plural(self.trees, "directory", "directories")
            )
        } else {
            format!(
                "{} {} {}",
                ui.bold(&verb),
                ui.bold(&self.files.to_string()),
                plural(self.files, "file", "files")
            )
        };
        println!("{}", ui.green(&headline));

        let mut rows: Vec<(String, String)> = Vec::new();
        if let Some(destination) = &self.destination {
            rows.push(("destination".into(), destination.display().to_string()));
        }
        if self.trees > 0 && self.files > 0 {
            rows.push((
                "whole folders".into(),
                format!("{} (instant rename)", self.trees),
            ));
        }
        if !self.dry_run {
            if self.renamed > 0 {
                rows.push((
                    "renamed in place".into(),
                    format!("{} (no data copied)", self.renamed),
                ));
            }
            if self.copied > 0 {
                rows.push((
                    "streamed".into(),
                    format!("{} files, {}", self.copied, self.bytes_human),
                ));
            }
        } else if self.bytes > 0 {
            rows.push(("data involved".into(), self.bytes_human.clone()));
        }
        if self.skipped > 0 {
            rows.push(("skipped".into(), self.skipped.to_string()));
        }
        if self.directories_pruned > 0 {
            rows.push((
                "empty folders removed".into(),
                self.directories_pruned.to_string(),
            ));
        }
        if !self.dry_run {
            let elapsed = Duration::from_millis(self.elapsed_ms as u64);
            let mut timing = util::format_duration(elapsed);
            if self.bytes > 0 {
                timing = format!("{timing} ({})", util::format_rate(self.bytes, elapsed));
            }
            rows.push(("took".into(), timing));
            if self.workers > 1 {
                rows.push(("workers".into(), self.workers.to_string()));
            }
        }

        // Pad before colouring: ANSI escapes count towards a format width,
        // so colouring first silently destroys the alignment.
        let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (key, value) in rows {
            println!("  {}  {}", ui.dim(&format!("{key:<width$}")), value);
        }

        if self.skipped > 0 && ui.verbose {
            println!();
            println!("{}", ui.dim("skipped:"));
            for entry in &self.skipped_detail {
                println!("  {} - {}", entry.path.display(), entry.reason);
            }
        } else if self.skipped > 0 {
            println!(
                "  {}",
                ui.dim("re-run with --verbose to list what was skipped")
            );
        }

        if self.failed > 0 {
            println!();
            println!(
                "{}",
                ui.red(&format!(
                    "{} {} could not be handled:",
                    self.failed,
                    plural(self.failed, "item", "items")
                ))
            );
            for failure in self.failures.iter().take(20) {
                println!("  {} - {}", failure.path.display(), failure.message);
            }
            if self.failures.len() > 20 {
                println!(
                    "  {}",
                    ui.dim(&format!("... and {} more", self.failures.len() - 20))
                );
            }
        }

        if self.cancelled {
            println!();
            println!(
                "{}",
                ui.yellow("Cancelled. Files already handled are at the destination; the rest were left untouched.")
            );
        }
        println!();
    }
}

fn plural(count: u64, one: &str, many: &str) -> String {
    if count == 1 {
        one.to_string()
    } else {
        many.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_reflect_what_happened() {
        let mut summary = Summary::simple("move", false);
        assert_eq!(summary.exit_code(), 0);

        summary.failed = 2;
        assert_eq!(summary.exit_code(), 1);

        summary.cancelled = true;
        assert_eq!(summary.exit_code(), 130);
    }

    #[test]
    fn summary_serialises_to_json() {
        let summary = Summary::simple("empty-recycle", true);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"command\":\"empty-recycle\""));
        assert!(json.contains("\"dry_run\":true"));
    }

    #[test]
    fn pluralisation_is_sane() {
        assert_eq!(plural(1, "file", "files"), "file");
        assert_eq!(plural(0, "file", "files"), "files");
        assert_eq!(plural(7, "file", "files"), "files");
    }
}
