//! `ofm move` and `ofm copy`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;

use crate::cli::TransferArgs;
use crate::engine::exec::{self, ExecOptions};
use crate::engine::plan::{self, JobKind, Mode, Plan, PlanRequest};
use crate::log::Logger;
use crate::report::{Outcome, Summary};
use crate::ui::progress::Progress;
use crate::ui::Ui;
use crate::util;

pub fn run(
    args: &TransferArgs,
    mode: Mode,
    ui: &Ui,
    logger: &Logger,
    cancelled: Arc<AtomicBool>,
) -> Result<Summary> {
    let (sources, destination) = args.split_destination()?;

    let plan = plan::build(PlanRequest {
        sources: &sources,
        destination: &destination,
        args,
        mode,
    })?;

    if plan.is_empty() {
        report_nothing_to_do(&plan, ui, mode);
        return Ok(Summary::from_transfer(
            mode,
            &plan,
            &Outcome::default(),
            args.dry_run,
        ));
    }

    describe_plan(&plan, ui, mode, args.dry_run);

    if args.dry_run {
        let summary = Summary::from_transfer(mode, &plan, &Outcome::default(), true);
        logger.write(format!(
            "dry-run {}: {} files, {} -> {}",
            mode.verb(),
            plan.file_count(),
            util::format_bytes(plan.total_bytes()),
            plan.destination_root.display()
        ));
        return Ok(summary);
    }

    let cross_volume = plan
        .jobs
        .iter()
        .any(|job| !util::same_volume(&job.source, &job.destination));

    let progress = Progress::for_transfer(
        ui.progress,
        plan.jobs.len() as u64,
        plan.total_bytes(),
        cross_volume,
        &format!("{}...", mode.present_participle()),
    );

    let options = ExecOptions {
        workers: args.workers,
        verify: args.verify,
        fail_fast: args.fail_fast,
        keep_empty_dirs: args.keep_empty_dirs,
        verbose: ui.verbose,
    };

    let outcome = exec::run(&plan, &options, &progress, cancelled)?;
    progress.finish();

    logger.write(format!(
        "{} {} files ({}) to {} in {}{}",
        mode.past_tense(),
        outcome.files_done() + outcome.trees,
        util::format_bytes(outcome.bytes_moved),
        plan.destination_root.display(),
        util::format_duration(outcome.elapsed),
        if outcome.failures.is_empty() {
            String::new()
        } else {
            format!(" ({} failed)", outcome.failures.len())
        }
    ));

    Ok(Summary::from_transfer(mode, &plan, &outcome, false))
}

fn report_nothing_to_do(plan: &Plan, ui: &Ui, mode: Mode) {
    if plan.skipped.is_empty() && plan.unreadable.is_empty() {
        ui.say(format!("Nothing to {}: no files matched.", mode.verb()));
    } else {
        ui.say(format!(
            "Nothing to {}: {} {} filtered out or already present.",
            mode.verb(),
            plan.skipped.len(),
            if plan.skipped.len() == 1 {
                "item was"
            } else {
                "items were"
            }
        ));
        if !ui.verbose && !plan.skipped.is_empty() {
            ui.say(ui.dim("  re-run with --verbose to see why"));
        }
        if ui.verbose {
            for entry in &plan.skipped {
                ui.say(format!(
                    "  {} - {}",
                    entry.path.display(),
                    entry.reason.describe()
                ));
            }
        }
    }
}

/// Tell the user what is about to happen before it happens. For a dry run
/// this is the whole output; otherwise it is a one-line preamble.
fn describe_plan(plan: &Plan, ui: &Ui, mode: Mode, dry_run: bool) {
    if ui.json || ui.quiet {
        return;
    }

    if dry_run {
        ui.say(format!(
            "{} {} {}",
            ui.bold("Dry run:"),
            ui.dim("nothing will be changed."),
            ui.dim(&format!("Destination: {}", plan.destination_root.display()))
        ));
        ui.say("");

        let shown = 50;
        for job in plan.jobs.iter().take(shown) {
            let arrow = match job.kind {
                JobKind::Tree => ui.cyan("=> (whole folder, instant rename)"),
                JobKind::File => ui.dim("->"),
            };
            let note = if job.replace {
                ui.yellow(" [replaces existing]")
            } else {
                String::new()
            };
            ui.say(format!(
                "  {} {} {}{}",
                job.source.display(),
                arrow,
                job.destination.display(),
                note
            ));
        }
        if plan.jobs.len() > shown {
            ui.say(ui.dim(&format!("  ... and {} more", plan.jobs.len() - shown)));
        }
        return;
    }

    if plan.used_fast_path && plan.file_count() == 0 {
        ui.say(format!(
            "{} {} on the same volume: moving with a single rename.",
            ui.bold(&plan.tree_count().to_string()),
            if plan.tree_count() == 1 {
                "folder"
            } else {
                "folders"
            }
        ));
    } else {
        ui.say(format!(
            "{} {} {} ({}) to {}",
            ui.bold(mode.present_participle()),
            ui.bold(&plan.file_count().to_string()),
            if plan.file_count() == 1 {
                "file"
            } else {
                "files"
            },
            util::format_bytes(plan.total_bytes()),
            plan.destination_root.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::plan::Mode;

    #[test]
    fn participles_are_real_words() {
        assert_eq!(Mode::Move.present_participle(), "Moving");
        assert_eq!(Mode::Copy.present_participle(), "Copying");
    }
}
