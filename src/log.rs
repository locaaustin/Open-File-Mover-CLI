//! The activity log. Appending to it must never be able to fail a transfer,
//! so every operation here is best-effort and silently degrades.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Rotate once the log passes this size so it cannot grow without bound.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct Logger {
    path: Option<PathBuf>,
}

impl Logger {
    /// Build a logger from the command line. `None` means logging is off.
    pub fn new(explicit: Option<PathBuf>, disabled: bool) -> Self {
        if disabled {
            return Self { path: None };
        }
        let path = explicit.or_else(default_log_path);
        Self { path }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Append one timestamped line.
    pub fn write(&self, line: impl AsRef<str>) {
        let Some(path) = &self.path else {
            return;
        };
        let _ = self.append(path, line.as_ref());
    }

    fn append(&self, path: &Path, line: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        rotate_if_needed(path);

        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "[{stamp}] {line}")
    }
}

fn rotate_if_needed(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        if metadata.len() > MAX_BYTES {
            let backup = path.with_extension("log.1");
            let _ = fs::remove_file(&backup);
            let _ = fs::rename(path, &backup);
        }
    }
}

/// `%LOCALAPPDATA%/open-file-mover/activity.log` on Windows, and an
/// XDG-ish location elsewhere. The log lives outside the repository so that
/// running the tool never dirties a working tree.
fn default_log_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })
    }?;
    Some(base.join("open-file-mover").join("activity.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_logger_writes_nothing() {
        let logger = Logger::new(None, true);
        assert!(logger.path().is_none());
        logger.write("ignored");
    }

    #[test]
    fn explicit_path_is_created_and_appended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("activity.log");
        let logger = Logger::new(Some(path.clone()), false);
        logger.write("first entry");
        logger.write("second entry");

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("first entry"));
        assert!(contents.contains("second entry"));
        assert_eq!(contents.lines().count(), 2);
        // Every line carries a timestamp.
        assert!(contents.starts_with('['));
    }

    #[test]
    fn oversized_logs_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.log");
        fs::write(&path, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();

        Logger::new(Some(path.clone()), false).write("after rotation");

        assert!(path.with_extension("log.1").exists());
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("after rotation"));
        assert_eq!(contents.lines().count(), 1);
    }

    #[test]
    fn a_default_path_is_available() {
        // The host always provides at least one of the base directories this
        // falls back to.
        assert!(default_log_path().is_some());
    }
}
