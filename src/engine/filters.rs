//! Decides which files a transfer picks up.

use std::collections::HashSet;
use std::fs::Metadata;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::cli::TransferArgs;

/// Why a particular file was left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reject {
    Extension,
    NotIncluded,
    Excluded,
    TooSmall,
    TooBig,
    TooOld,
    TooNew,
    Hidden,
}

impl Reject {
    pub fn describe(self) -> &'static str {
        match self {
            Reject::Extension => "extension not requested",
            Reject::NotIncluded => "did not match --include",
            Reject::Excluded => "matched --exclude",
            Reject::TooSmall => "smaller than --min-size",
            Reject::TooBig => "larger than --max-size",
            Reject::TooOld => "older than --newer-than",
            Reject::TooNew => "newer than --older-than",
            Reject::Hidden => "hidden or system file (--skip-hidden)",
        }
    }
}

/// A compiled set of file filters.
#[derive(Debug, Default)]
pub struct Filter {
    extensions: Option<HashSet<String>>,
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    newer_than: Option<SystemTime>,
    older_than: Option<SystemTime>,
    skip_hidden: bool,
}

impl Filter {
    /// Compile the filters described by the command line.
    pub fn from_args(args: &TransferArgs) -> Result<Self> {
        let extensions = if args.ext.is_empty() {
            None
        } else {
            Some(
                args.ext
                    .iter()
                    .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|e| !e.is_empty())
                    .collect::<HashSet<_>>(),
            )
        };

        Ok(Self {
            extensions,
            include: build_globs(&args.include).context("invalid --include glob")?,
            exclude: build_globs(&args.exclude).context("invalid --exclude glob")?,
            min_size: args.min_size,
            max_size: args.max_size,
            newer_than: args.newer_than,
            older_than: args.older_than,
            skip_hidden: args.skip_hidden,
        })
    }

    /// Build a filter that only looks at extensions, age and excludes.
    /// Used by `clean-downloads`, which has a smaller set of options.
    pub fn for_cleanup(
        ext: &[String],
        exclude: &[String],
        older_than: Option<SystemTime>,
    ) -> Result<Self> {
        let extensions = if ext.is_empty() {
            None
        } else {
            Some(
                ext.iter()
                    .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|e| !e.is_empty())
                    .collect::<HashSet<_>>(),
            )
        };
        Ok(Self {
            extensions,
            include: None,
            exclude: build_globs(exclude).context("invalid --exclude glob")?,
            min_size: None,
            max_size: None,
            newer_than: None,
            older_than,
            skip_hidden: false,
        })
    }

    /// `true` when this filter would accept every file it is shown.
    pub fn is_pass_through(&self) -> bool {
        self.extensions.is_none()
            && self.include.is_none()
            && self.exclude.is_none()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.newer_than.is_none()
            && self.older_than.is_none()
            && !self.skip_hidden
    }

    /// Test a file. `relative` is the path relative to the source root and is
    /// what globs are matched against; `metadata` must be the file's own.
    pub fn check(&self, relative: &Path, metadata: &Metadata) -> Result<(), Reject> {
        if self.skip_hidden && is_hidden(relative, metadata) {
            return Err(Reject::Hidden);
        }

        if let Some(wanted) = &self.extensions {
            let ext = relative
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if !wanted.contains(&ext) {
                return Err(Reject::Extension);
            }
        }

        let glob_path = glob_target(relative);
        if let Some(include) = &self.include {
            if !include.is_match(&glob_path) {
                return Err(Reject::NotIncluded);
            }
        }
        if let Some(exclude) = &self.exclude {
            if exclude.is_match(&glob_path) {
                return Err(Reject::Excluded);
            }
        }

        let size = metadata.len();
        if let Some(min) = self.min_size {
            if size < min {
                return Err(Reject::TooSmall);
            }
        }
        if let Some(max) = self.max_size {
            if size > max {
                return Err(Reject::TooBig);
            }
        }

        if self.newer_than.is_some() || self.older_than.is_some() {
            // A file whose mtime cannot be read is kept rather than silently
            // dropped: losing data is worse than moving one extra file.
            if let Ok(modified) = metadata.modified() {
                if let Some(bound) = self.newer_than {
                    if modified < bound {
                        return Err(Reject::TooOld);
                    }
                }
                if let Some(bound) = self.older_than {
                    if modified > bound {
                        return Err(Reject::TooNew);
                    }
                }
            }
        }

        Ok(())
    }

    /// Test a directory, so that an excluded directory is not descended into.
    pub fn excludes_directory(&self, relative: &Path) -> bool {
        match &self.exclude {
            Some(exclude) => {
                let target = glob_target(relative);
                exclude.is_match(&target) || exclude.is_match(format!("{target}/"))
            }
            None => false,
        }
    }
}

fn build_globs(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        // Accept Windows-style separators in patterns by normalising them.
        let normalised = pattern.replace('\\', "/");
        builder.add(
            Glob::new(&normalised).with_context(|| format!("`{pattern}` is not a valid glob"))?,
        );
        // `*.txt` should also match `sub/dir/file.txt`, which is what people
        // expect from a recursive mover, so register a `**/` variant too.
        if !normalised.contains('/') {
            builder.add(Glob::new(&format!("**/{normalised}")).expect("derived glob is valid"));
        }
    }
    Ok(Some(builder.build()?))
}

/// Globs are always matched against forward-slash paths so that the same
/// pattern works on every platform.
fn glob_target(relative: &Path) -> String {
    relative.to_string_lossy().replace('\\', "/")
}

#[cfg(windows)]
fn is_hidden(_relative: &Path, metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    metadata.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

#[cfg(not(windows))]
fn is_hidden(relative: &Path, _metadata: &Metadata) -> bool {
    relative
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn write(dir: &Path, name: &str, bytes: usize) -> std::path::PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, vec![b'x'; bytes]).unwrap();
        path
    }

    fn filter(build: impl FnOnce(&mut TransferArgs)) -> Filter {
        let mut args = TransferArgs {
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
            on_conflict: crate::cli::Conflict::Rename,
            workers: None,
            verify: false,
            fail_fast: false,
            keep_empty_dirs: false,
            no_fast_path: false,
        };
        build(&mut args);
        Filter::from_args(&args).unwrap()
    }

    #[test]
    fn pass_through_filter_accepts_everything() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "a.txt", 10);
        let f = filter(|_| {});
        assert!(f.is_pass_through());
        assert!(f
            .check(Path::new("a.txt"), &fs::metadata(&file).unwrap())
            .is_ok());
    }

    #[test]
    fn extension_filter_is_case_insensitive_and_dot_tolerant() {
        let dir = tempfile::tempdir().unwrap();
        let zip = write(dir.path(), "archive.ZIP", 4);
        let txt = write(dir.path(), "notes.txt", 4);
        let f = filter(|a| a.ext = vec![".Zip".into()]);
        assert!(f
            .check(Path::new("archive.ZIP"), &fs::metadata(&zip).unwrap())
            .is_ok());
        assert_eq!(
            f.check(Path::new("notes.txt"), &fs::metadata(&txt).unwrap()),
            Err(Reject::Extension)
        );
    }

    #[test]
    fn files_without_an_extension_do_not_match_an_extension_filter() {
        let dir = tempfile::tempdir().unwrap();
        let plain = write(dir.path(), "noextension", 4);
        let f = filter(|a| a.ext = vec!["txt".into()]);
        assert_eq!(
            f.check(Path::new("noextension"), &fs::metadata(&plain).unwrap()),
            Err(Reject::Extension)
        );
    }

    #[test]
    fn bare_globs_match_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        let nested = write(dir.path(), "deep/inner/a.log", 4);
        let f = filter(|a| a.include = vec!["*.log".into()]);
        assert!(f
            .check(
                Path::new("deep/inner/a.log"),
                &fs::metadata(&nested).unwrap()
            )
            .is_ok());
    }

    #[test]
    fn excludes_beat_includes() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "node_modules/left-pad/index.js", 4);
        let f = filter(|a| {
            a.include = vec!["**/*.js".into()];
            a.exclude = vec!["**/node_modules/**".into()];
        });
        assert_eq!(
            f.check(
                Path::new("node_modules/left-pad/index.js"),
                &fs::metadata(&file).unwrap()
            ),
            Err(Reject::Excluded)
        );
        assert!(f.excludes_directory(Path::new("node_modules/left-pad")));
    }

    #[test]
    fn size_bounds_are_inclusive() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "a.bin", 1024);
        let meta = fs::metadata(&file).unwrap();
        assert!(filter(|a| a.min_size = Some(1024))
            .check(Path::new("a.bin"), &meta)
            .is_ok());
        assert_eq!(
            filter(|a| a.min_size = Some(1025)).check(Path::new("a.bin"), &meta),
            Err(Reject::TooSmall)
        );
        assert!(filter(|a| a.max_size = Some(1024))
            .check(Path::new("a.bin"), &meta)
            .is_ok());
        assert_eq!(
            filter(|a| a.max_size = Some(1023)).check(Path::new("a.bin"), &meta),
            Err(Reject::TooBig)
        );
    }

    #[test]
    fn age_bounds_select_recent_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "fresh.txt", 4);
        let meta = fs::metadata(&file).unwrap();
        let an_hour_ago = SystemTime::now() - Duration::from_secs(3_600);

        // The file was just written, so it is newer than an hour ago.
        assert!(filter(|a| a.newer_than = Some(an_hour_ago))
            .check(Path::new("fresh.txt"), &meta)
            .is_ok());
        assert_eq!(
            filter(|a| a.older_than = Some(an_hour_ago)).check(Path::new("fresh.txt"), &meta),
            Err(Reject::TooNew)
        );
    }

    #[test]
    fn hidden_files_are_taken_unless_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), ".secret", 4);
        let meta = fs::metadata(&file).unwrap();

        // The default must take everything, otherwise moving a folder would
        // silently leave part of it behind.
        let default = filter(|_| {});
        assert!(default.is_pass_through());
        assert!(default.check(Path::new(".secret"), &meta).is_ok());

        // On Windows a freshly written dotfile carries no hidden attribute,
        // so the dotfile rule is only asserted where it applies.
        if cfg!(not(windows)) {
            assert_eq!(
                filter(|a| a.skip_hidden = true).check(Path::new(".secret"), &meta),
                Err(Reject::Hidden)
            );
        }
        assert!(!filter(|a| a.skip_hidden = true).is_pass_through());
    }

    #[test]
    fn invalid_globs_are_reported() {
        let mut args_err = false;
        if build_globs(&["[".to_string()]).is_err() {
            args_err = true;
        }
        assert!(args_err, "an unterminated character class should fail");
    }
}
