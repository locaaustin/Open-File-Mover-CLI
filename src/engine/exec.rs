//! Runs a [`Plan`]. Same-volume moves become renames; everything else is a
//! parallel copy with an optional byte-for-byte verify before the original is
//! removed.

use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::engine::plan::{Job, JobKind, Mode, Plan};
use crate::report::{Failure, Outcome};
use crate::ui::progress::Progress;
use crate::util;

const COMPARE_BUFFER: usize = 64 * 1024;

pub struct ExecOptions {
    pub workers: Option<usize>,
    pub verify: bool,
    pub fail_fast: bool,
    pub keep_empty_dirs: bool,
    pub verbose: bool,
}

/// How a single file got to its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    /// One rename syscall: the data never moved.
    Renamed,
    /// Bytes were streamed to a new volume.
    Copied,
}

/// Decide how many workers to use.
///
/// Renames are cheap metadata operations and scale with cores. Cross-volume
/// copies are bound by the slower of the two devices, and piling on threads
/// makes spinning disks seek rather than read, so the default stays low.
pub fn default_workers(plan: &Plan) -> usize {
    let available = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let cross_volume = plan
        .jobs
        .iter()
        .take(64)
        .any(|job| !util::same_volume(&job.source, &job.destination));

    let cap = if cross_volume { 4 } else { available };
    plan.jobs.len().clamp(1, cap.max(1)).min(cap.max(1))
}

/// Execute the plan, reporting progress as it goes.
pub fn run(
    plan: &Plan,
    options: &ExecOptions,
    progress: &Progress,
    cancelled: Arc<AtomicBool>,
) -> Result<Outcome> {
    let started = Instant::now();

    create_directories(plan)?;

    let workers = options
        .workers
        .filter(|n| *n > 0)
        .unwrap_or_else(|| default_workers(plan))
        .min(plan.jobs.len().max(1));

    let cursor = AtomicUsize::new(0);
    let renamed = AtomicU64::new(0);
    let copied = AtomicU64::new(0);
    let bytes_moved = AtomicU64::new(0);
    let trees = AtomicU64::new(0);
    let failures: Mutex<Vec<Failure>> = Mutex::new(Vec::new());

    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if cancelled.load(Ordering::Relaxed) {
                    break;
                }
                let index = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(job) = plan.jobs.get(index) else {
                    break;
                };

                progress.set_current(&job.source);

                match perform(job, plan.mode, options.verify) {
                    Ok(how) => {
                        match job.kind {
                            JobKind::Tree => {
                                trees.fetch_add(1, Ordering::Relaxed);
                            }
                            JobKind::File => match how {
                                How::Renamed => {
                                    renamed.fetch_add(1, Ordering::Relaxed);
                                }
                                How::Copied => {
                                    copied.fetch_add(1, Ordering::Relaxed);
                                    bytes_moved.fetch_add(job.bytes, Ordering::Relaxed);
                                }
                            },
                        }
                        if options.verbose {
                            progress.note(format!(
                                "{} {} -> {}",
                                if how == How::Renamed {
                                    "renamed"
                                } else {
                                    "copied "
                                },
                                job.source.display(),
                                job.destination.display()
                            ));
                        }
                    }
                    Err(error) => {
                        failures
                            .lock()
                            .expect("failure list poisoned")
                            .push(Failure {
                                path: job.source.clone(),
                                destination: Some(job.destination.clone()),
                                message: error.to_string(),
                            });
                        if options.fail_fast {
                            cancelled.store(true, Ordering::Relaxed);
                        }
                    }
                }

                progress.advance(job.bytes);
            });
        }
    });

    let mut failures = failures.into_inner().expect("failure list poisoned");
    for (path, message) in &plan.unreadable {
        failures.push(Failure {
            path: path.clone(),
            destination: None,
            message: message.clone(),
        });
    }

    let mut pruned = 0u64;
    if plan.mode == Mode::Move && !options.keep_empty_dirs && !cancelled.load(Ordering::Relaxed) {
        pruned = prune_empty(&plan.prunable);
    }

    Ok(Outcome {
        renamed: renamed.load(Ordering::Relaxed),
        copied: copied.load(Ordering::Relaxed),
        trees: trees.load(Ordering::Relaxed),
        bytes_moved: bytes_moved.load(Ordering::Relaxed),
        directories_pruned: pruned,
        failures,
        elapsed: started.elapsed(),
        cancelled: cancelled.load(Ordering::Relaxed),
        workers,
    })
}

fn create_directories(plan: &Plan) -> Result<()> {
    // `directories` is a BTreeSet, so parents are created before children.
    for directory in &plan.directories {
        fs::create_dir_all(directory)
            .with_context(|| format!("cannot create `{}`", directory.display()))?;
    }
    Ok(())
}

fn perform(job: &Job, mode: Mode, verify: bool) -> Result<How> {
    if job.replace {
        force_remove(&job.destination)
            .with_context(|| format!("cannot replace `{}`", job.destination.display()))?;
    }

    match job.kind {
        JobKind::Tree => {
            fs::rename(&job.source, &job.destination).with_context(|| {
                format!(
                    "cannot move directory `{}` to `{}`",
                    job.source.display(),
                    job.destination.display()
                )
            })?;
            Ok(How::Renamed)
        }
        JobKind::File => transfer_file(&job.source, &job.destination, mode, verify),
    }
}

fn transfer_file(source: &Path, destination: &Path, mode: Mode, verify: bool) -> Result<How> {
    if mode == Mode::Move {
        // The cheap path. A rename within a volume is instant no matter how
        // large the file is. Any failure (most often a cross-volume move)
        // falls through to copy + delete, and a genuine problem such as a
        // permission error surfaces from the copy below instead.
        if fs::rename(source, destination).is_ok() {
            return Ok(How::Renamed);
        }
    }

    copy_file(source, destination).with_context(|| {
        format!(
            "cannot copy `{}` to `{}`",
            source.display(),
            destination.display()
        )
    })?;

    if verify {
        same_contents(source, destination)
            .with_context(|| format!("cannot verify `{}`", destination.display()))?;
    }

    if mode == Mode::Move {
        force_remove(source)
            .with_context(|| format!("copied but could not remove `{}`", source.display()))?;
    }

    Ok(How::Copied)
}

fn copy_file(source: &Path, destination: &Path) -> io::Result<u64> {
    // std::fs::copy maps to CopyFileEx on Windows, which is both faster than
    // a hand-rolled loop and preserves file attributes.
    let bytes = fs::copy(source, destination)?;

    // CopyFileEx keeps timestamps, but other platforms do not, so make
    // mtime preservation explicit and uniform.
    if let Ok(modified) = fs::metadata(source).and_then(|m| m.modified()) {
        if let Ok(handle) = OpenOptions::new().write(true).open(destination) {
            let _ = handle.set_modified(modified);
        }
    }

    Ok(bytes)
}

/// Remove a file even if it is marked read-only, which Windows otherwise
/// refuses.
fn force_remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            if let Ok(metadata) = fs::metadata(path) {
                let mut permissions = metadata.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = fs::set_permissions(path, permissions);
            }
            fs::remove_file(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            // A directory where a file was expected.
            if path.is_dir() {
                fs::remove_dir_all(path)
            } else {
                Err(error)
            }
        }
    }
}

/// Byte-for-byte comparison used by `--verify`.
fn same_contents(left: &Path, right: &Path) -> Result<()> {
    let left_size = fs::metadata(left)?.len();
    let right_size = fs::metadata(right)?.len();
    anyhow::ensure!(
        left_size == right_size,
        "size mismatch after copy: {} vs {}",
        util::format_bytes(left_size),
        util::format_bytes(right_size)
    );

    let mut left_file = fs::File::open(left)?;
    let mut right_file = fs::File::open(right)?;
    let mut left_buffer = vec![0u8; COMPARE_BUFFER];
    let mut right_buffer = vec![0u8; COMPARE_BUFFER];
    let mut offset = 0u64;

    loop {
        let read = read_full(&mut left_file, &mut left_buffer)?;
        let other = read_full(&mut right_file, &mut right_buffer)?;
        anyhow::ensure!(read == other, "the copy ended early at byte {offset}");
        if read == 0 {
            return Ok(());
        }
        anyhow::ensure!(
            left_buffer[..read] == right_buffer[..read],
            "contents differ near byte {offset}"
        );
        offset += read as u64;
    }
}

fn read_full(file: &mut fs::File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// Remove source directories that the move emptied. Already sorted deepest
/// first by the planner, and `remove_dir` refuses to touch a directory that
/// still has anything in it, so this can never delete data.
fn prune_empty(candidates: &[PathBuf]) -> u64 {
    let mut removed = 0;
    for directory in candidates {
        if fs::remove_dir(directory).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Conflict, TransferArgs};
    use crate::engine::plan::{self, PlanRequest};

    fn args() -> TransferArgs {
        TransferArgs {
            paths: vec![],
            to: None,
            dry_run: false,
            ext: vec![],
            include: vec![],
            exclude: vec![],
            min_size: None,
            max_size: None,
            newer_than: None,
            older_than: None,
            skip_hidden: false,
            depth: None,
            follow_links: false,
            contents: false,
            flatten: false,
            group_by: None,
            on_conflict: Conflict::Rename,
            workers: Some(2),
            verify: false,
            fail_fast: false,
            keep_empty_dirs: false,
            no_fast_path: true,
        }
    }

    fn options() -> ExecOptions {
        ExecOptions {
            workers: Some(2),
            verify: true,
            fail_fast: false,
            keep_empty_dirs: false,
            verbose: false,
        }
    }

    fn execute(root: &Path, mode: Mode, tweak: impl FnOnce(&mut TransferArgs)) -> Outcome {
        let mut a = args();
        tweak(&mut a);
        let sources = vec![root.join("src")];
        let destination = root.join("dest");
        let plan = plan::build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &a,
            mode,
        })
        .unwrap();
        run(
            &plan,
            &options(),
            &Progress::disabled(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
    }

    fn seed(root: &Path) {
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::write(root.join("src/a.txt"), b"alpha").unwrap();
        fs::write(root.join("src/b.jpg"), b"bravo!!").unwrap();
        fs::write(root.join("src/deep/c.txt"), b"charlie").unwrap();
    }

    #[test]
    fn moves_a_tree_file_by_file() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let outcome = execute(dir.path(), Mode::Move, |_| {});

        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(outcome.renamed + outcome.copied, 3);
        assert!(dir.path().join("dest/src/a.txt").exists());
        assert!(dir.path().join("dest/src/deep/c.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("dest/src/deep/c.txt")).unwrap(),
            "charlie"
        );
        assert!(!dir.path().join("src/a.txt").exists());
    }

    #[test]
    fn move_prunes_the_emptied_source() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        execute(dir.path(), Mode::Move, |_| {});
        assert!(!dir.path().join("src").exists(), "source should be gone");
    }

    #[test]
    fn keep_empty_dirs_leaves_the_skeleton() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut a = args();
        a.keep_empty_dirs = true;
        let sources = vec![dir.path().join("src")];
        let destination = dir.path().join("dest");
        let plan = plan::build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &a,
            mode: Mode::Move,
        })
        .unwrap();
        let mut o = options();
        o.keep_empty_dirs = true;
        run(
            &plan,
            &o,
            &Progress::disabled(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(dir.path().join("src").exists());
    }

    #[test]
    fn copy_leaves_the_originals_alone() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let outcome = execute(dir.path(), Mode::Copy, |_| {});
        assert!(outcome.failures.is_empty());
        assert!(dir.path().join("src/a.txt").exists());
        assert!(dir.path().join("dest/src/a.txt").exists());
        assert_eq!(outcome.copied, 3);
        assert_eq!(outcome.renamed, 0);
    }

    #[test]
    fn copy_preserves_contents_and_size() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let payload: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        fs::write(dir.path().join("src/big.bin"), &payload).unwrap();

        execute(dir.path(), Mode::Copy, |_| {});
        let landed = fs::read(dir.path().join("dest/src/big.bin")).unwrap();
        assert_eq!(landed, payload);
    }

    #[test]
    fn overwrite_replaces_a_read_only_destination() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        fs::create_dir_all(dir.path().join("dest/src")).unwrap();
        let victim = dir.path().join("dest/src/a.txt");
        fs::write(&victim, b"stale").unwrap();
        let mut permissions = fs::metadata(&victim).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&victim, permissions).unwrap();

        let outcome = execute(dir.path(), Mode::Copy, |a| {
            a.on_conflict = Conflict::Overwrite
        });
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "alpha");
    }

    #[test]
    fn verify_catches_nothing_on_a_good_copy() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let outcome = execute(dir.path(), Mode::Copy, |_| {});
        assert!(outcome.failures.is_empty());
    }

    #[test]
    fn content_comparison_detects_a_difference() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.bin");
        let right = dir.path().join("right.bin");
        fs::write(&left, vec![1u8; 100_000]).unwrap();
        fs::write(&right, vec![1u8; 100_000]).unwrap();
        assert!(same_contents(&left, &right).is_ok());

        let mut altered = vec![1u8; 100_000];
        altered[99_999] = 2;
        fs::write(&right, altered).unwrap();
        assert!(same_contents(&left, &right).is_err());

        fs::write(&right, vec![1u8; 99_999]).unwrap();
        assert!(same_contents(&left, &right).is_err());
    }

    #[test]
    fn cancellation_stops_early_and_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        for n in 0..50 {
            fs::write(dir.path().join(format!("src/f{n}.txt")), b"x").unwrap();
        }
        let a = args();
        let sources = vec![dir.path().join("src")];
        let destination = dir.path().join("dest");
        let plan = plan::build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &a,
            mode: Mode::Move,
        })
        .unwrap();

        let cancelled = Arc::new(AtomicBool::new(true));
        let outcome = run(&plan, &options(), &Progress::disabled(), cancelled).unwrap();
        assert!(outcome.cancelled);
        assert_eq!(outcome.renamed + outcome.copied, 0);
        assert!(dir.path().join("src/f0.txt").exists());
    }

    #[test]
    fn a_whole_tree_rename_is_one_job() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut a = args();
        a.no_fast_path = false;
        let sources = vec![dir.path().join("src")];
        let destination = dir.path().join("dest");
        let plan = plan::build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &a,
            mode: Mode::Move,
        })
        .unwrap();
        let outcome = run(
            &plan,
            &options(),
            &Progress::disabled(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(outcome.trees, 1);
        assert!(dir.path().join("dest/src/deep/c.txt").exists());
        assert!(!dir.path().join("src").exists());
    }

    #[test]
    fn missing_source_file_is_reported_not_panicked() {
        let dir = tempfile::tempdir().unwrap();
        let job = Job {
            source: dir.path().join("ghost.txt"),
            destination: dir.path().join("out.txt"),
            bytes: 0,
            kind: JobKind::File,
            replace: false,
        };
        assert!(perform(&job, Mode::Move, false).is_err());
    }

    #[test]
    fn force_remove_handles_read_only_and_absent_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.txt");
        fs::write(&path, b"x").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        assert!(force_remove(&path).is_ok());
        assert!(!path.exists());
        assert!(force_remove(&path).is_ok(), "absent file is not an error");
    }
}
