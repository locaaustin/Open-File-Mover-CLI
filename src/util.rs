//! Small helpers shared across the crate: size/age parsing, human-readable
//! formatting, path normalisation and volume comparison.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Parse a human-written byte size such as `512`, `10k`, `4MB`, `1.5GiB`.
///
/// Both decimal (kB = 1000) and binary (KiB = 1024) suffixes are accepted.
/// A bare `k`/`m`/`g` suffix is treated as binary, matching what people
/// usually mean when they type `--min-size 10M`.
pub fn parse_size(input: &str) -> Result<u64, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("expected a size such as 10MB".into());
    }

    let digits_end = raw
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '_')
        .unwrap_or(raw.len());
    let (number, suffix) = raw.split_at(digits_end);
    let number: f64 = number
        .replace('_', "")
        .parse()
        .map_err(|_| format!("`{raw}` is not a valid size"))?;
    if number < 0.0 {
        return Err("size cannot be negative".into());
    }

    let multiplier: f64 = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kib" => 1024.0,
        "kb" => 1_000.0,
        "m" | "mib" => 1024.0 * 1024.0,
        "mb" => 1_000_000.0,
        "g" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "gb" => 1_000_000_000.0,
        "t" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "tb" => 1_000_000_000_000.0,
        other => return Err(format!("unknown size unit `{other}`")),
    };

    Ok((number * multiplier) as u64)
}

/// Parse an age (`30s`, `15m`, `6h`, `7d`, `2w`, `3mo`, `1y`) or an absolute
/// `YYYY-MM-DD` date, returning the instant it refers to.
pub fn parse_age(input: &str) -> Result<SystemTime, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("expected an age such as 7d or a date such as 2026-01-31".into());
    }

    // Absolute date first: 2026-01-31
    if raw.len() == 10 && raw.as_bytes()[4] == b'-' && raw.as_bytes()[7] == b'-' {
        let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .map_err(|_| format!("`{raw}` is not a valid YYYY-MM-DD date"))?;
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| "invalid date".to_string())?;
        let secs = naive.and_utc().timestamp();
        return if secs >= 0 {
            Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64))
        } else {
            Err("dates before 1970 are not supported".into())
        };
    }

    let digits_end = raw
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(raw.len());
    let (number, suffix) = raw.split_at(digits_end);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("`{raw}` is not a valid age"))?;

    let seconds: f64 = match suffix.trim().to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" => number,
        "" | "m" | "min" | "mins" => number * 60.0,
        "h" | "hr" | "hrs" => number * 3_600.0,
        "d" | "day" | "days" => number * 86_400.0,
        "w" | "week" | "weeks" => number * 604_800.0,
        "mo" | "month" | "months" => number * 2_592_000.0,
        "y" | "year" | "years" => number * 31_536_000.0,
        other => return Err(format!("unknown time unit `{other}`")),
    };

    SystemTime::now()
        .checked_sub(Duration::from_secs_f64(seconds))
        .ok_or_else(|| format!("`{raw}` reaches too far into the past"))
}

/// Format a byte count the way a human would read it.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Format a duration compactly: `840ms`, `3.2s`, `1m 04s`, `2h 11m`.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        return format!("{}ms", d.as_millis());
    }
    if secs < 60.0 {
        return format!("{secs:.1}s");
    }
    let total = d.as_secs();
    if total < 3_600 {
        format!("{}m {:02}s", total / 60, total % 60)
    } else {
        format!("{}h {:02}m", total / 3_600, (total % 3_600) / 60)
    }
}

/// Format a transfer rate in bytes per second.
pub fn format_rate(bytes: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 || bytes == 0 {
        return "-".into();
    }
    format!("{}/s", format_bytes((bytes as f64 / secs) as u64))
}

/// Turn a user-supplied path into an absolute path without requiring that it
/// exists (unlike [`std::fs::canonicalize`], which also adds verbatim
/// prefixes that look alarming in output).
pub fn absolutize(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    Ok(normalize_dots(&absolute))
}

/// Resolve `.` and `..` components lexically.
fn normalize_dots(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A comparison key for paths that respects the host file system's case
/// sensitivity. Windows paths are compared case-insensitively.
pub fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.to_lowercase().replace('/', "\\")
    } else {
        text
    }
}

/// Best-effort check for whether two paths live on the same volume, used to
/// decide whether a move can be a cheap rename and how many workers to use.
///
/// This is only a hint: the transfer code always tries a rename first and
/// falls back to copy + delete if the rename is rejected.
pub fn same_volume(a: &Path, b: &Path) -> bool {
    volume_of(a) == volume_of(b)
}

fn volume_of(path: &Path) -> String {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                (letter as char).to_ascii_lowercase().to_string()
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => format!(
                "//{}/{}",
                server.to_string_lossy().to_lowercase(),
                share.to_string_lossy().to_lowercase()
            ),
            Prefix::DeviceNS(name) => name.to_string_lossy().to_lowercase(),
            Prefix::Verbatim(name) => name.to_string_lossy().to_lowercase(),
        },
        // On Unix everything is treated as one volume; rename-then-fallback
        // still keeps us correct across real mount points.
        _ => "/".to_string(),
    }
}

/// `true` when `child` is `ancestor` itself or sits inside it.
pub fn is_inside(child: &Path, ancestor: &Path) -> bool {
    let child = path_key(child);
    let ancestor = path_key(ancestor);
    if child == ancestor {
        return true;
    }
    let separator = if cfg!(windows) { '\\' } else { '/' };
    let with_sep = if ancestor.ends_with(separator) {
        ancestor
    } else {
        format!("{ancestor}{separator}")
    };
    child.starts_with(&with_sep)
}

/// Shorten a path for single-line progress output.
pub fn shorten(path: &Path, max: usize) -> String {
    let text = path.to_string_lossy();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.into_owned();
    }
    let start = chars.len().saturating_sub(max.saturating_sub(1));
    let tail: String = chars[start..].iter().collect();
    format!("...{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("1k").unwrap(), 1024);
        assert_eq!(parse_size("1kb").unwrap(), 1000);
        assert_eq!(parse_size("1KiB").unwrap(), 1024);
        assert_eq!(parse_size("1.5M").unwrap(), 1_572_864);
        assert_eq!(parse_size(" 2 GB ").unwrap(), 2_000_000_000);
        assert!(parse_size("bogus").is_err());
        assert!(parse_size("10 quux").is_err());
    }

    #[test]
    fn parses_ages() {
        let now = SystemTime::now();
        let hour_ago = parse_age("1h").unwrap();
        let delta = now.duration_since(hour_ago).unwrap();
        assert!(delta.as_secs() >= 3_599 && delta.as_secs() <= 3_601);
        assert!(parse_age("2026-01-31").is_ok());
        assert!(parse_age("2026-13-31").is_err());
        assert!(parse_age("7 fortnights").is_err());
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
        assert_eq!(format_bytes(1024 * 1024 * 3 / 2), "1.50 MiB");
    }

    #[test]
    fn detects_containment() {
        assert!(is_inside(Path::new("/a/b/c"), Path::new("/a")));
        assert!(is_inside(Path::new("/a"), Path::new("/a")));
        assert!(!is_inside(Path::new("/ab"), Path::new("/a")));
    }

    #[test]
    fn compares_volumes() {
        if cfg!(windows) {
            assert!(same_volume(Path::new("C:/x"), Path::new("c:/y/z")));
            assert!(!same_volume(Path::new("C:/x"), Path::new("D:/x")));
        }
    }

    #[test]
    fn normalizes_dot_segments() {
        let p = normalize_dots(Path::new("/a/./b/../c"));
        assert_eq!(path_key(&p), path_key(Path::new("/a/c")));
    }
}
