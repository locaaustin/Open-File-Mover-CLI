//! Non-Windows stand-ins. `move` and `copy` work everywhere; the shell
//! commands report that they need Windows rather than pretending to work.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use super::BinContents;

pub fn is_elevated() -> bool {
    false
}

pub fn downloads_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set, so Downloads cannot be found")?;
    Ok(PathBuf::from(home).join("Downloads"))
}

pub fn recycle_bin_contents(_drive: Option<&str>) -> Result<BinContents> {
    bail!("the Recycle Bin is a Windows feature")
}

pub fn empty_recycle_bin(_drive: Option<&str>) -> Result<()> {
    bail!("the Recycle Bin is a Windows feature")
}

pub fn send_to_recycle_bin(_paths: &[PathBuf]) -> Result<()> {
    bail!("the Recycle Bin is a Windows feature; use `ofm move` to relocate files instead")
}

pub fn enable_ansi() {}

pub fn describe_platform() -> String {
    format!("{} (non-Windows build)", std::env::consts::OS)
}
