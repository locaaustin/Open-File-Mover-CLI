//! The live progress bar. Wrapped in a type that also works when disabled, so
//! the executor never has to care whether there is a terminal attached.

use std::path::Path;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::util;

/// What the bar is counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Bytes,
    Files,
}

pub struct Progress {
    bar: Option<ProgressBar>,
    unit: Unit,
}

impl Progress {
    /// A no-op reporter, used with `--no-progress`, `--json`, `--quiet`, when
    /// output is piped, and in tests.
    pub fn disabled() -> Self {
        Self {
            bar: None,
            unit: Unit::Files,
        }
    }

    /// A bar measured in bytes, which is what matters for cross-volume copies.
    pub fn bytes(total: u64, label: &str) -> Self {
        Self {
            bar: Some(make_bar(
                total,
                label,
                "{spinner:.cyan} {msg}\n  [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
                 {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})",
            )),
            unit: Unit::Bytes,
        }
    }

    /// A bar measured in files, used when a transfer is all renames and so has
    /// no meaningful byte throughput.
    pub fn files(total: u64, label: &str) -> Self {
        Self {
            bar: Some(make_bar(
                total,
                label,
                "{spinner:.cyan} {msg}\n  [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
                 {pos}/{len} files ({per_sec}, eta {eta})",
            )),
            unit: Unit::Files,
        }
    }

    /// Pick a style: byte-based when there is real data to stream.
    pub fn for_transfer(
        enabled: bool,
        total_files: u64,
        total_bytes: u64,
        cross_volume: bool,
        label: &str,
    ) -> Self {
        if !enabled || total_files == 0 {
            return Self::disabled();
        }
        if cross_volume && total_bytes > 0 {
            Self::bytes(total_bytes, label)
        } else {
            Self::files(total_files, label)
        }
    }

    pub fn set_current(&self, path: &Path) {
        if let Some(bar) = &self.bar {
            bar.set_message(util::shorten(path, 70));
        }
    }

    /// Mark one file as finished.
    pub fn advance(&self, bytes: u64) {
        if let Some(bar) = &self.bar {
            match self.unit {
                Unit::Bytes => bar.inc(bytes),
                Unit::Files => bar.inc(1),
            }
        }
    }

    /// Print a line above the bar without corrupting it.
    pub fn note(&self, line: impl AsRef<str>) {
        match &self.bar {
            Some(bar) => bar.println(line.as_ref()),
            None => println!("{}", line.as_ref()),
        }
    }

    pub fn finish(&self) {
        if let Some(bar) = &self.bar {
            bar.finish_and_clear();
        }
    }
}

fn make_bar(total: u64, label: &str, template: &str) -> ProgressBar {
    let bar = ProgressBar::new(total);
    bar.set_style(
        ProgressStyle::with_template(template)
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("##-"),
    );
    bar.set_message(label.to_string());
    bar.enable_steady_tick(Duration::from_millis(120));
    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_progress_is_inert() {
        let progress = Progress::disabled();
        progress.set_current(Path::new("/tmp/x"));
        progress.advance(1024);
        progress.finish();
    }

    #[test]
    fn transfer_picks_bytes_only_when_streaming() {
        let bytes = Progress::for_transfer(true, 10, 2048, true, "copying");
        assert_eq!(bytes.unit, Unit::Bytes);

        let files = Progress::for_transfer(true, 10, 2048, false, "moving");
        assert_eq!(files.unit, Unit::Files);

        let off = Progress::for_transfer(false, 10, 2048, true, "moving");
        assert!(off.bar.is_none());

        let nothing = Progress::for_transfer(true, 0, 0, true, "moving");
        assert!(nothing.bar.is_none());
    }
}
