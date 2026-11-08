//! Shell integration. The Windows implementation talks to the shell API; the
//! fallback keeps the crate building and testable on other platforms.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(windows))]
mod fallback;
#[cfg(not(windows))]
pub use fallback::*;

/// What is currently sitting in the Recycle Bin.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct BinContents {
    pub items: u64,
    pub bytes: u64,
}
