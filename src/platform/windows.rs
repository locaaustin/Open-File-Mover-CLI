//! Windows shell integration: the Recycle Bin, the real Downloads folder and
//! the elevation state of the current process.

use std::ffi::OsStr;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use windows_sys::core::{GUID, PWSTR};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, S_OK};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_RecycleBinFolder, ILFree, SHChangeNotify, SHEmptyRecycleBinW, SHFileOperationW,
    SHGetKnownFolderIDList, SHGetKnownFolderPath, SHQueryRecycleBinW, FOF_ALLOWUNDO,
    FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE, SHCNE_UPDATEDIR,
    SHCNF_IDLIST, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHFILEOPSTRUCTW,
    SHQUERYRBINFO,
};

use super::BinContents;

/// `SHCNF_FLUSH`. windows-sys does not export it, and without it the shell
/// takes the notification asynchronously - which a process that exits
/// immediately afterwards cannot rely on.
const SHCNF_FLUSH: u32 = 0x1000;

/// FOLDERID_Downloads: {374DE290-123F-4565-9164-39C4925E467B}
const FOLDERID_DOWNLOADS: GUID = GUID {
    data1: 0x374D_E290,
    data2: 0x123F,
    data3: 0x4565,
    data4: [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B],
};

/// Encode a path as a NUL-terminated UTF-16 string for the shell API.
fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// `true` when this process is running elevated.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// The user's Downloads folder, as the shell knows it. This respects a
/// relocated Downloads folder, which `%USERPROFILE%/Downloads` does not.
pub fn downloads_dir() -> Result<PathBuf> {
    unsafe {
        let mut raw: PWSTR = std::ptr::null_mut();
        let result = SHGetKnownFolderPath(&FOLDERID_DOWNLOADS, 0, std::ptr::null_mut(), &mut raw);
        if result == S_OK && !raw.is_null() {
            let mut length = 0usize;
            while *raw.add(length) != 0 {
                length += 1;
            }
            let slice = std::slice::from_raw_parts(raw, length);
            let path = PathBuf::from(std::ffi::OsString::from_wide(slice));
            CoTaskMemFree(raw.cast());
            if !path.as_os_str().is_empty() {
                return Ok(path);
            }
        } else if !raw.is_null() {
            CoTaskMemFree(raw.cast());
        }
    }

    // Fall back to the conventional location rather than giving up.
    let profile = std::env::var_os("USERPROFILE")
        .context("neither the shell nor USERPROFILE could tell us where Downloads is")?;
    Ok(PathBuf::from(profile).join("Downloads"))
}

/// Ask the shell what is in the Recycle Bin. `drive` is `None` for all drives.
pub fn recycle_bin_contents(drive: Option<&str>) -> Result<BinContents> {
    let root = drive.map(normalise_drive).transpose()?;
    // The buffer must outlive the call, so it is bound before the unsafe
    // block rather than inside a match arm.
    let buffer = root.as_ref().map(|text| wide(OsStr::new(text)));
    unsafe {
        let mut info = SHQUERYRBINFO {
            cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
            i64Size: 0,
            i64NumItems: 0,
        };
        let root_ptr = match &buffer {
            Some(buffer) => buffer.as_ptr(),
            None => std::ptr::null(),
        };

        let result = SHQueryRecycleBinW(root_ptr, &mut info);
        if result != S_OK {
            bail!("the shell could not read the Recycle Bin (HRESULT {result:#x})");
        }
        Ok(BinContents {
            items: info.i64NumItems.max(0) as u64,
            bytes: info.i64Size.max(0) as u64,
        })
    }
}

/// Permanently empty the Recycle Bin.
pub fn empty_recycle_bin(drive: Option<&str>) -> Result<()> {
    let root = drive.map(normalise_drive).transpose()?;
    let buffer = root.as_ref().map(|text| wide(OsStr::new(text)));
    unsafe {
        let root_ptr = match &buffer {
            Some(buffer) => buffer.as_ptr(),
            None => std::ptr::null(),
        };
        let result = SHEmptyRecycleBinW(
            std::ptr::null_mut(),
            root_ptr,
            SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
        );
        if result != S_OK {
            bail!("the shell refused to empty the Recycle Bin (HRESULT {result:#x})");
        }
    }
    refresh_recycle_bin_view();
    Ok(())
}

/// Tell the shell that the Recycle Bin changed.
///
/// `SHEmptyRecycleBinW` called with no owner window and no progress UI leaves
/// an already-open Recycle Bin window - and the desktop icon - still showing
/// items that are no longer on disk. From the user's side a successful empty
/// then looks like it did nothing at all, so the notification is not optional.
pub fn refresh_recycle_bin_view() {
    unsafe {
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let result = SHGetKnownFolderIDList(
            &FOLDERID_RecycleBinFolder,
            0,
            std::ptr::null_mut(),
            &mut pidl,
        );
        if result != S_OK || pidl.is_null() {
            // Cosmetic only: a stale window is not worth failing the command.
            return;
        }
        SHChangeNotify(
            SHCNE_UPDATEDIR as i32,
            SHCNF_IDLIST | SHCNF_FLUSH,
            pidl.cast(),
            std::ptr::null(),
        );
        ILFree(pidl);
    }
}

/// Send paths to the Recycle Bin so they can be restored from Explorer.
///
/// The shell takes the whole batch in one call, which is both faster and more
/// atomic than deleting one path at a time.
pub fn send_to_recycle_bin(paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }

    // SHFileOperation wants a double-NUL-terminated list of plain (not
    // verbatim) absolute paths.
    let mut list: Vec<u16> = Vec::new();
    for path in paths {
        let absolute = crate::util::absolutize(path)
            .with_context(|| format!("cannot resolve `{}`", path.display()))?;
        let text = absolute.as_os_str();
        if text.encode_wide().any(|unit| unit == 0) {
            bail!("`{}` contains an invalid character", path.display());
        }
        list.extend(text.encode_wide());
        list.push(0);
    }
    list.push(0);

    let mut operation = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: list.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO
            | FOF_NOCONFIRMATION
            | FOF_NOERRORUI
            | FOF_SILENT
            | FOF_WANTNUKEWARNING) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };

    let result = unsafe { SHFileOperationW(&mut operation) };
    if result != 0 {
        bail!("the shell could not recycle these files (code {result})");
    }
    if operation.fAnyOperationsAborted != 0 {
        bail!("the operation was aborted before everything was recycled");
    }
    refresh_recycle_bin_view();
    Ok(())
}

/// `C:` / `c` / `C:\` all mean the same Recycle Bin root.
fn normalise_drive(drive: &str) -> Result<String> {
    let trimmed = drive.trim().trim_end_matches(['\\', '/']);
    let letter = trimmed
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .with_context(|| format!("`{drive}` is not a drive letter"))?;
    if trimmed.len() > 2 {
        anyhow::bail!("`{drive}` is not a drive letter, expected something like C:");
    }
    Ok(format!("{}:\\", letter.to_ascii_uppercase()))
}

/// Make the console interpret ANSI escapes so colour works in older hosts.
pub fn enable_ansi() {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_OUTPUT_HANDLE,
    };
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

/// Used by `doctor` to describe the running environment.
pub fn describe_platform() -> String {
    format!(
        "Windows (elevated: {})",
        if is_elevated() { "yes" } else { "no" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letters_are_normalised() {
        assert_eq!(normalise_drive("c").unwrap(), "C:\\");
        assert_eq!(normalise_drive("C:").unwrap(), "C:\\");
        assert_eq!(normalise_drive("d:\\").unwrap(), "D:\\");
        assert!(normalise_drive("").is_err());
        assert!(normalise_drive("9:").is_err());
        assert!(normalise_drive("C:\\Users").is_err());
    }

    #[test]
    fn downloads_folder_resolves() {
        let path = downloads_dir().unwrap();
        assert!(path.is_absolute(), "{path:?} should be absolute");
    }

    #[test]
    fn recycle_bin_can_be_queried() {
        // The counts vary by machine; only the call has to succeed.
        let all = recycle_bin_contents(None).unwrap();
        let _ = all.items;
        let _ = all.bytes;
    }

    #[test]
    fn elevation_check_does_not_panic() {
        let _ = is_elevated();
    }

    #[test]
    fn refreshing_the_bin_view_is_safe_to_call() {
        // It talks to the shell and returns nothing; the contract is only that
        // it never panics and never leaks the PIDL.
        refresh_recycle_bin_view();
        refresh_recycle_bin_view();
    }

    #[test]
    fn recycling_nothing_is_a_no_op() {
        assert!(send_to_recycle_bin(&[]).is_ok());
    }

    // Ignored by default: it leaves a real entry in whoever runs it's Recycle
    // Bin, and a stray `ofm-recycle-test` turning up after every `cargo test`
    // is exactly the kind of thing that makes people distrust the tool. Run it
    // deliberately with `cargo test -- --ignored`.
    #[test]
    #[ignore = "puts a real file in the Recycle Bin"]
    fn a_real_file_round_trips_to_the_bin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ofm-recycle-test.txt");
        std::fs::write(&path, b"recycle me").unwrap();
        send_to_recycle_bin(std::slice::from_ref(&path)).unwrap();
        assert!(!path.exists(), "the file should have left the folder");
    }
}
