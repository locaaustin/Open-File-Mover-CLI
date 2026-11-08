//! Turns a command line into an explicit, inspectable list of file
//! operations. Nothing here touches the file system destructively, which is
//! what makes `--dry-run` trustworthy: the plan a dry run prints is exactly
//! the plan the executor would run.

use std::collections::{BTreeSet, HashSet};
use std::fs::Metadata;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

use crate::cli::{Conflict, GroupBy, TransferArgs};
use crate::engine::filters::{Filter, Reject};
use crate::util;

/// Whether originals are removed once they have arrived.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Move,
    Copy,
}

impl Mode {
    pub fn verb(self) -> &'static str {
        match self {
            Mode::Move => "move",
            Mode::Copy => "copy",
        }
    }

    pub fn past_tense(self) -> &'static str {
        match self {
            Mode::Move => "moved",
            Mode::Copy => "copied",
        }
    }

    /// Spelled out rather than derived, because "move" + "ing" is not a word.
    pub fn present_participle(self) -> &'static str {
        match self {
            Mode::Move => "Moving",
            Mode::Copy => "Copying",
        }
    }
}

/// One unit of work.
#[derive(Debug, Clone)]
pub struct Job {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub bytes: u64,
    pub kind: JobKind,
    /// An existing destination must be removed first (`--on-conflict
    /// overwrite` or `newer`).
    pub replace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// A single file.
    File,
    /// A whole directory tree, moved with one rename. Only ever produced for
    /// `Mode::Move` on a single volume.
    Tree,
}

/// Something deliberately left behind, recorded so the summary can explain
/// itself rather than silently doing less than the user asked.
#[derive(Debug, Clone)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case", tag = "reason")]
pub enum SkipReason {
    Filtered { rule: Reject },
    DestinationExists,
    NotNewer,
    SameFile,
    UnsupportedKind,
}

impl SkipReason {
    pub fn describe(&self) -> String {
        match self {
            SkipReason::Filtered { rule } => rule.describe().to_string(),
            SkipReason::DestinationExists => "destination already exists".into(),
            SkipReason::NotNewer => "destination is the same age or newer".into(),
            SkipReason::SameFile => "source and destination are the same path".into(),
            SkipReason::UnsupportedKind => "symlink or junction (use --follow-links)".into(),
        }
    }
}

/// The complete set of operations for one invocation.
#[derive(Debug, Default)]
pub struct Plan {
    pub jobs: Vec<Job>,
    /// Destination directories to create before any worker starts.
    pub directories: BTreeSet<PathBuf>,
    /// Source directories that may be empty afterwards, deepest first.
    pub prunable: Vec<PathBuf>,
    pub skipped: Vec<Skipped>,
    /// Entries that could not even be inspected.
    pub unreadable: Vec<(PathBuf, String)>,
    pub destination_root: PathBuf,
    pub mode: Mode,
    pub used_fast_path: bool,
}

impl Plan {
    pub fn total_bytes(&self) -> u64 {
        self.jobs.iter().map(|j| j.bytes).sum()
    }

    pub fn file_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.kind == JobKind::File).count()
    }

    pub fn tree_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.kind == JobKind::Tree).count()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

/// Everything the planner needs that is not already in `TransferArgs`.
pub struct PlanRequest<'a> {
    pub sources: &'a [PathBuf],
    pub destination: &'a Path,
    pub args: &'a TransferArgs,
    pub mode: Mode,
}

/// Build the plan.
pub fn build(request: PlanRequest<'_>) -> Result<Plan> {
    let args = request.args;
    let filter = Filter::from_args(args)?;
    let destination_root = util::absolutize(request.destination).with_context(|| {
        format!(
            "cannot resolve destination `{}`",
            request.destination.display()
        )
    })?;

    if args.flatten && args.group_by.is_some() {
        bail!("--flatten and --group-by do the same job; pick one");
    }

    let mut plan = Plan {
        destination_root: destination_root.clone(),
        mode: request.mode,
        ..Default::default()
    };
    // Destination paths already claimed by an earlier job, so that two
    // sources flattening into one folder cannot collide.
    let mut claimed: HashSet<String> = HashSet::new();

    for raw_source in request.sources {
        let source = util::absolutize(raw_source)
            .with_context(|| format!("cannot resolve source `{}`", raw_source.display()))?;

        let metadata = std::fs::symlink_metadata(&source)
            .with_context(|| format!("cannot read source `{}`", source.display()))?;

        if metadata.is_dir() && util::is_inside(&destination_root, &source) {
            bail!(
                "refusing to {} `{}` into `{}`: the destination is inside the source",
                request.mode.verb(),
                source.display(),
                destination_root.display()
            );
        }

        if metadata.is_symlink() && !args.follow_links {
            // A symlink pointing at a directory is still a single entry to
            // rename; only cross-volume transfers cannot handle it.
            plan_single_file(
                &source,
                &metadata,
                args,
                &destination_root,
                &mut claimed,
                &mut plan,
            )?;
            continue;
        }

        if metadata.is_file() {
            plan_single_file(
                &source,
                &metadata,
                args,
                &destination_root,
                &mut claimed,
                &mut plan,
            )?;
            continue;
        }

        if !metadata.is_dir() {
            plan.skipped.push(Skipped {
                path: source,
                reason: SkipReason::UnsupportedKind,
            });
            continue;
        }

        plan_directory(
            &source,
            args,
            &filter,
            &destination_root,
            request.mode,
            &mut claimed,
            &mut plan,
        )?;
    }

    // Deepest directories first so that pruning empties leaves before stems.
    plan.prunable
        .sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    plan.prunable.dedup();

    for job in &plan.jobs {
        if job.kind == JobKind::File {
            if let Some(parent) = job.destination.parent() {
                plan.directories.insert(parent.to_path_buf());
            }
        } else if let Some(parent) = job.destination.parent() {
            plan.directories.insert(parent.to_path_buf());
        }
    }

    Ok(plan)
}

fn plan_single_file(
    source: &Path,
    metadata: &Metadata,
    args: &TransferArgs,
    destination_root: &Path,
    claimed: &mut HashSet<String>,
    plan: &mut Plan,
) -> Result<()> {
    let name = source
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("unnamed"));
    let target = map_destination(destination_root, &name, metadata, args);

    match resolve_conflict(&target, metadata, args.on_conflict, claimed) {
        Resolution::Use { path, replace } => plan.jobs.push(Job {
            source: source.to_path_buf(),
            destination: path,
            bytes: metadata.len(),
            kind: JobKind::File,
            replace,
        }),
        Resolution::Skip(reason) => plan.skipped.push(Skipped {
            path: source.to_path_buf(),
            reason,
        }),
    }
    Ok(())
}

fn plan_directory(
    source: &Path,
    args: &TransferArgs,
    filter: &Filter,
    destination_root: &Path,
    mode: Mode,
    claimed: &mut HashSet<String>,
    plan: &mut Plan,
) -> Result<()> {
    let restructuring = args.flatten || args.group_by.is_some();
    let landing = if args.contents || restructuring {
        destination_root.to_path_buf()
    } else {
        let name = source.file_name().unwrap_or_default();
        destination_root.join(name)
    };

    // The single best trick available: a same-volume directory move is one
    // rename syscall regardless of how many files are inside.
    if can_rename_whole_tree(source, &landing, args, filter, mode, claimed) {
        claimed.insert(util::path_key(&landing));
        plan.jobs.push(Job {
            source: source.to_path_buf(),
            destination: landing,
            bytes: 0,
            kind: JobKind::Tree,
            replace: false,
        });
        plan.used_fast_path = true;
        return Ok(());
    }

    let mut walker = WalkDir::new(source)
        .follow_links(args.follow_links)
        .min_depth(1);
    if let Some(depth) = args.depth {
        walker = walker.max_depth(depth.saturating_add(1));
    }

    let mut iterator = walker.into_iter();
    while let Some(entry) = iterator.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = error
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| source.to_path_buf());
                plan.unreadable.push((path, error.to_string()));
                continue;
            }
        };

        let relative = match entry.path().strip_prefix(source) {
            Ok(relative) => relative.to_path_buf(),
            Err(_) => continue,
        };

        if entry.file_type().is_dir() {
            if filter.excludes_directory(&relative) {
                iterator.skip_current_dir();
                continue;
            }
            // Mirror empty directories only when nothing is being filtered or
            // restructured, so that `--ext jpg` does not litter the
            // destination with hundreds of empty folders.
            if !restructuring && filter.is_pass_through() {
                plan.directories.insert(landing.join(&relative));
            }
            if mode == Mode::Move {
                plan.prunable.push(entry.path().to_path_buf());
            }
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                plan.unreadable
                    .push((entry.path().to_path_buf(), error.to_string()));
                continue;
            }
        };

        if let Err(rule) = filter.check(&relative, &metadata) {
            plan.skipped.push(Skipped {
                path: entry.path().to_path_buf(),
                reason: SkipReason::Filtered { rule },
            });
            continue;
        }

        let target = map_destination(&landing, &relative, &metadata, args);
        match resolve_conflict(&target, &metadata, args.on_conflict, claimed) {
            Resolution::Use { path, replace } => plan.jobs.push(Job {
                source: entry.path().to_path_buf(),
                destination: path,
                bytes: metadata.len(),
                kind: JobKind::File,
                replace,
            }),
            Resolution::Skip(reason) => plan.skipped.push(Skipped {
                path: entry.path().to_path_buf(),
                reason,
            }),
        }
    }

    // The source root itself is prunable only when its contents were not
    // explicitly meant to stay put.
    if mode == Mode::Move && !args.contents {
        plan.prunable.push(source.to_path_buf());
    }

    Ok(())
}

/// Work out where one file lands, applying `--flatten` / `--group-by`.
fn map_destination(
    landing: &Path,
    relative: &Path,
    metadata: &Metadata,
    args: &TransferArgs,
) -> PathBuf {
    let file_name = relative.file_name().unwrap_or(relative.as_os_str());

    match args.group_by {
        Some(GroupBy::Ext) => {
            let bucket = relative
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| "no-extension".to_string());
            landing.join(bucket).join(file_name)
        }
        Some(GroupBy::Date) => landing.join(month_bucket(metadata)).join(file_name),
        Some(GroupBy::Year) => landing.join(year_bucket(metadata)).join(file_name),
        None if args.flatten => landing.join(file_name),
        None => landing.join(relative),
    }
}

fn month_bucket(metadata: &Metadata) -> String {
    match metadata.modified() {
        Ok(time) => chrono::DateTime::<chrono::Local>::from(time)
            .format("%Y-%m")
            .to_string(),
        Err(_) => "unknown-date".to_string(),
    }
}

fn year_bucket(metadata: &Metadata) -> String {
    match metadata.modified() {
        Ok(time) => chrono::DateTime::<chrono::Local>::from(time)
            .format("%Y")
            .to_string(),
        Err(_) => "unknown-date".to_string(),
    }
}

enum Resolution {
    Use { path: PathBuf, replace: bool },
    Skip(SkipReason),
}

fn resolve_conflict(
    target: &Path,
    source_metadata: &Metadata,
    policy: Conflict,
    claimed: &mut HashSet<String>,
) -> Resolution {
    let key = util::path_key(target);
    let on_disk = target.symlink_metadata().ok();
    let taken = on_disk.is_some() || claimed.contains(&key);

    if !taken {
        claimed.insert(key);
        return Resolution::Use {
            path: target.to_path_buf(),
            replace: false,
        };
    }

    match policy {
        Conflict::Skip => Resolution::Skip(SkipReason::DestinationExists),
        Conflict::Overwrite => {
            claimed.insert(key);
            Resolution::Use {
                path: target.to_path_buf(),
                replace: true,
            }
        }
        Conflict::Newer => {
            let source_time = source_metadata.modified().ok();
            let target_time = on_disk.as_ref().and_then(|m| m.modified().ok());
            match (source_time, target_time) {
                (Some(source_time), Some(target_time)) if source_time > target_time => {
                    claimed.insert(key);
                    Resolution::Use {
                        path: target.to_path_buf(),
                        replace: true,
                    }
                }
                // Unknown timestamps are treated as "do not touch", which is
                // the conservative reading of --on-conflict newer.
                _ => Resolution::Skip(SkipReason::NotNewer),
            }
        }
        Conflict::Rename => match next_free_name(target, claimed) {
            Some(path) => {
                claimed.insert(util::path_key(&path));
                Resolution::Use {
                    path,
                    replace: false,
                }
            }
            None => Resolution::Skip(SkipReason::DestinationExists),
        },
    }
}

/// `report.txt` -> `report (1).txt` -> `report (2).txt` ...
fn next_free_name(target: &Path, claimed: &HashSet<String>) -> Option<PathBuf> {
    let parent = target.parent().unwrap_or(Path::new(""));
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = target
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    for n in 1..=10_000u32 {
        let candidate = parent.join(format!("{stem} ({n}){extension}"));
        if !candidate.exists() && !claimed.contains(&util::path_key(&candidate)) {
            return Some(candidate);
        }
    }
    None
}

/// The fast path is only safe when the whole tree is wanted, unchanged, on the
/// same volume, and nothing is in the way at the destination.
fn can_rename_whole_tree(
    source: &Path,
    landing: &Path,
    args: &TransferArgs,
    filter: &Filter,
    mode: Mode,
    claimed: &HashSet<String>,
) -> bool {
    mode == Mode::Move
        && !args.no_fast_path
        && !args.contents
        && !args.flatten
        && args.group_by.is_none()
        && args.depth.is_none()
        && filter.is_pass_through()
        && util::same_volume(source, landing)
        && !landing.exists()
        && !claimed.contains(&util::path_key(landing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn args_for(paths: Vec<PathBuf>) -> TransferArgs {
        TransferArgs {
            paths,
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
            workers: None,
            verify: false,
            fail_fast: false,
            keep_empty_dirs: false,
            no_fast_path: false,
        }
    }

    fn seed(root: &Path) {
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::write(root.join("src/a.txt"), b"aaa").unwrap();
        fs::write(root.join("src/b.jpg"), b"bbbb").unwrap();
        fs::write(root.join("src/deep/c.txt"), b"ccccc").unwrap();
        fs::create_dir_all(root.join("dest")).unwrap();
    }

    fn plan_for(root: &Path, mode: Mode, tweak: impl FnOnce(&mut TransferArgs)) -> Plan {
        let mut args = args_for(vec![]);
        args.no_fast_path = true;
        tweak(&mut args);
        let sources = vec![root.join("src")];
        let destination = root.join("dest");
        build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &args,
            mode,
        })
        .unwrap()
    }

    #[test]
    fn mirrors_the_tree_by_default() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let plan = plan_for(dir.path(), Mode::Move, |_| {});
        assert_eq!(plan.file_count(), 3);
        let landed: Vec<_> = plan
            .jobs
            .iter()
            .map(|j| {
                j.destination
                    .strip_prefix(dir.path())
                    .unwrap()
                    .to_path_buf()
            })
            .collect();
        assert!(landed.contains(&PathBuf::from("dest").join("src").join("a.txt")));
        assert!(landed.contains(&PathBuf::from("dest").join("src").join("deep").join("c.txt")));
    }

    #[test]
    fn contents_drops_the_source_folder_name() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let plan = plan_for(dir.path(), Mode::Move, |a| a.contents = true);
        let landed: Vec<_> = plan
            .jobs
            .iter()
            .map(|j| {
                j.destination
                    .strip_prefix(dir.path())
                    .unwrap()
                    .to_path_buf()
            })
            .collect();
        assert!(landed.contains(&PathBuf::from("dest").join("a.txt")));
    }

    #[test]
    fn flatten_collapses_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let plan = plan_for(dir.path(), Mode::Move, |a| a.flatten = true);
        for job in &plan.jobs {
            assert_eq!(job.destination.parent().unwrap(), dir.path().join("dest"));
        }
    }

    #[test]
    fn flatten_renames_colliding_basenames() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/one")).unwrap();
        fs::create_dir_all(dir.path().join("src/two")).unwrap();
        fs::write(dir.path().join("src/one/same.txt"), b"1").unwrap();
        fs::write(dir.path().join("src/two/same.txt"), b"2").unwrap();
        fs::create_dir_all(dir.path().join("dest")).unwrap();

        let plan = plan_for(dir.path(), Mode::Move, |a| a.flatten = true);
        assert_eq!(plan.jobs.len(), 2);
        let names: HashSet<String> = plan
            .jobs
            .iter()
            .map(|j| {
                j.destination
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names.len(), 2, "colliding names must be disambiguated");
        assert!(names.contains("same.txt"));
        assert!(names.contains("same (1).txt"));
    }

    #[test]
    fn group_by_extension_creates_buckets() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let plan = plan_for(dir.path(), Mode::Move, |a| a.group_by = Some(GroupBy::Ext));
        let landed: Vec<_> = plan
            .jobs
            .iter()
            .map(|j| {
                j.destination
                    .strip_prefix(dir.path())
                    .unwrap()
                    .to_path_buf()
            })
            .collect();
        assert!(landed.contains(&PathBuf::from("dest").join("txt").join("a.txt")));
        assert!(landed.contains(&PathBuf::from("dest").join("jpg").join("b.jpg")));
    }

    #[test]
    fn extension_filter_narrows_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let plan = plan_for(dir.path(), Mode::Move, |a| a.ext = vec!["txt".into()]);
        assert_eq!(plan.file_count(), 2);
        assert_eq!(plan.skipped.len(), 1);
    }

    #[test]
    fn depth_zero_stays_at_the_top_level() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let plan = plan_for(dir.path(), Mode::Move, |a| a.depth = Some(0));
        assert_eq!(plan.file_count(), 2);
    }

    #[test]
    fn conflict_skip_leaves_the_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        fs::create_dir_all(dir.path().join("dest/src")).unwrap();
        fs::write(dir.path().join("dest/src/a.txt"), b"old").unwrap();

        let plan = plan_for(dir.path(), Mode::Move, |a| a.on_conflict = Conflict::Skip);
        assert_eq!(plan.file_count(), 2);
        assert!(plan
            .skipped
            .iter()
            .any(|s| s.reason == SkipReason::DestinationExists));
    }

    #[test]
    fn conflict_overwrite_marks_the_job_for_replacement() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        fs::create_dir_all(dir.path().join("dest/src")).unwrap();
        fs::write(dir.path().join("dest/src/a.txt"), b"old").unwrap();

        let plan = plan_for(dir.path(), Mode::Move, |a| {
            a.on_conflict = Conflict::Overwrite
        });
        let job = plan
            .jobs
            .iter()
            .find(|j| j.destination.ends_with("a.txt"))
            .unwrap();
        assert!(job.replace);
    }

    #[test]
    fn conflict_rename_picks_a_numbered_name() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        fs::create_dir_all(dir.path().join("dest/src")).unwrap();
        fs::write(dir.path().join("dest/src/a.txt"), b"old").unwrap();

        let plan = plan_for(dir.path(), Mode::Move, |a| a.on_conflict = Conflict::Rename);
        assert!(plan
            .jobs
            .iter()
            .any(|j| j.destination.ends_with("a (1).txt")));
    }

    #[test]
    fn refuses_to_move_a_folder_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let args = args_for(vec![]);
        let sources = vec![dir.path().join("src")];
        let destination = dir.path().join("src").join("inner");
        let error = build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &args,
            mode: Mode::Move,
        })
        .unwrap_err();
        assert!(error.to_string().contains("inside the source"));
    }

    #[test]
    fn copy_mode_never_prunes_or_fast_paths() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut args = args_for(vec![]);
        args.no_fast_path = false;
        let sources = vec![dir.path().join("src")];
        let destination = dir.path().join("dest");
        let plan = build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &args,
            mode: Mode::Copy,
        })
        .unwrap();
        assert!(plan.prunable.is_empty());
        assert!(!plan.used_fast_path);
        assert_eq!(plan.file_count(), 3);
    }

    #[test]
    fn same_volume_move_uses_one_tree_job() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut args = args_for(vec![]);
        args.no_fast_path = false;
        let sources = vec![dir.path().join("src")];
        let destination = dir.path().join("dest");
        let plan = build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &args,
            mode: Mode::Move,
        })
        .unwrap();
        assert!(plan.used_fast_path);
        assert_eq!(plan.tree_count(), 1);
        assert_eq!(plan.file_count(), 0);
    }

    /// A plain `ofm move folder dest` must take the one-rename path. This
    /// regressed once because the default filter excluded hidden files, which
    /// made every transfer look filtered and turned an instant move of 20,000
    /// files into 20,000 separate renames.
    #[test]
    fn a_default_move_is_not_accidentally_filtered() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let args = args_for(vec![]);
        let filter = Filter::from_args(&args).unwrap();
        assert!(
            filter.is_pass_through(),
            "the default filter must accept everything so the fast path stays reachable"
        );

        let sources = vec![dir.path().join("src")];
        let destination = dir.path().join("dest");
        let plan = build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &args,
            mode: Mode::Move,
        })
        .unwrap();
        assert!(plan.used_fast_path);
        assert_eq!(plan.jobs.len(), 1, "one rename, not one job per file");
    }

    #[test]
    fn hidden_files_are_included_in_a_default_move() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        fs::write(dir.path().join("src/.hidden-config"), b"keep me").unwrap();

        let plan = plan_for(dir.path(), Mode::Move, |_| {});
        assert!(
            plan.jobs
                .iter()
                .any(|j| j.source.ends_with(".hidden-config")),
            "a mover must not leave hidden files behind"
        );
    }

    #[test]
    fn excluded_directories_are_not_descended() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        fs::create_dir_all(dir.path().join("src/node_modules/pkg")).unwrap();
        fs::write(dir.path().join("src/node_modules/pkg/index.js"), b"x").unwrap();

        let plan = plan_for(dir.path(), Mode::Move, |a| {
            a.exclude = vec!["**/node_modules/**".into()]
        });
        assert!(!plan
            .jobs
            .iter()
            .any(|j| j.source.to_string_lossy().contains("node_modules")));
    }

    #[test]
    fn flatten_and_group_by_together_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut args = args_for(vec![]);
        args.flatten = true;
        args.group_by = Some(GroupBy::Ext);
        let sources = vec![dir.path().join("src")];
        let destination = dir.path().join("dest");
        assert!(build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &args,
            mode: Mode::Move,
        })
        .is_err());
    }

    #[test]
    fn a_missing_source_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let args = args_for(vec![]);
        let sources = vec![dir.path().join("nope")];
        let destination = dir.path().join("dest");
        assert!(build(PlanRequest {
            sources: &sources,
            destination: &destination,
            args: &args,
            mode: Mode::Move,
        })
        .is_err());
    }
}
