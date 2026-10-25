//! The host's lexical path check: the third layer of the link gate.
//!
//! Every path a panel op receives passes [`check`] **before any filesystem call**. On Windows, merely
//! opening or canonicalising a UNC path (`\\host\share`) makes the OS contact that SMB server and
//! offer the user's NTLM credentials, so shares and device namespaces are refused from the string
//! alone. So are relative and drive-relative paths, `.`/`..`, control characters, alternate data
//! streams and the reserved device names (`NUL`, `CON`, `COM1`…), which name devices in any folder.
//!
//! A drive letter mapped to a network share is the one remote form the string cannot show; on
//! Windows [`check`] asks the drive's type (a local query that contacts no server) and refuses
//! remote drives.

use std::path::PathBuf;

/// Paths longer than this are refused (paths are at most 8 KiB).
pub const MAX_PATH_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRefusal {
    Empty,
    TooLong,
    Control,
    Share,
    Device,
    NotAbsolute,
    DotSegment,
    ReservedName,
    Stream,
    RemoteDrive,
}

impl PathRefusal {
    /// The message the user sees.
    pub fn message(&self) -> &'static str {
        match self {
            Self::Empty => "No path was given.",
            Self::TooLong => "The path is too long.",
            Self::Control => "The path contains control characters.",
            Self::Share | Self::RemoteDrive => "Network shares are not opened by Helm.",
            Self::Device | Self::ReservedName => "Device paths are not opened by Helm.",
            Self::NotAbsolute => "The path is not a complete file path.",
            Self::DotSegment => "The path contains . or .. segments.",
            Self::Stream => "The path names an alternate data stream.",
        }
    }
}

impl std::fmt::Display for PathRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

/// `NUL`, `nul.txt`, `COM1 .log`, `LPT9`, `COM¹`… name devices whatever folder they sit in.
fn is_reserved_name(segment: &str) -> bool {
    // Windows ignores trailing dots and spaces, and everything from the first dot is an extension.
    let trimmed = segment.trim_end_matches(['.', ' ']);
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = upper.strip_prefix(prefix) {
            let digits = [
                "1", "2", "3", "4", "5", "6", "7", "8", "9", "\u{b9}", "\u{b2}", "\u{b3}",
            ];
            if digits.contains(&rest) {
                return true;
            }
        }
    }
    false
}

/// Checks a path from the renderer (or an agent's link) as text. On success returns it as a
/// `PathBuf` with forward slashes turned into backslashes; nothing has touched the disk.
pub fn check(raw: &str) -> Result<PathBuf, PathRefusal> {
    if raw.is_empty() {
        return Err(PathRefusal::Empty);
    }
    if raw.len() > MAX_PATH_BYTES {
        return Err(PathRefusal::TooLong);
    }
    if raw.chars().any(|c| c.is_control()) {
        return Err(PathRefusal::Control);
    }
    let s = raw.replace('/', "\\");
    // `\\host\share`, `\\?\…`, `\\.\…` (and their forward-slash and mixed spellings, normalised above).
    if let Some(rest) = s.strip_prefix("\\\\") {
        if rest.starts_with('?') || rest.starts_with('.') {
            return Err(PathRefusal::Device);
        }
        return Err(PathRefusal::Share);
    }
    if s.starts_with("\\??\\") {
        return Err(PathRefusal::Device);
    }
    let bytes = s.as_bytes();
    #[cfg(windows)]
    {
        // Only `X:\…` is absolute: `\x` is rooted on the current drive and `C:x` is drive-relative.
        let drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\';
        if !drive {
            return Err(PathRefusal::NotAbsolute);
        }
    }
    #[cfg(not(windows))]
    {
        if bytes.first() != Some(&b'\\') {
            return Err(PathRefusal::NotAbsolute);
        }
    }
    let after_root = if cfg!(windows) { &s[3..] } else { &s[1..] };
    for seg in after_root.split('\\') {
        if seg.is_empty() {
            // A trailing separator is fine; an empty segment in the middle (`a\\b`) is not.
            continue;
        }
        if seg == "." || seg == ".." {
            return Err(PathRefusal::DotSegment);
        }
        if cfg!(windows) {
            if seg.contains(':') {
                return Err(PathRefusal::Stream);
            }
            if is_reserved_name(seg) {
                return Err(PathRefusal::ReservedName);
            }
        }
    }
    if after_root.contains("\\\\") {
        return Err(PathRefusal::DotSegment);
    }
    #[cfg(windows)]
    if drive_is_remote(bytes[0] as char) {
        return Err(PathRefusal::RemoteDrive);
    }
    Ok(PathBuf::from(if cfg!(windows) {
        s
    } else {
        raw.to_owned()
    }))
}

/// `GetDriveTypeW` reads the local drive table; it does not contact a server.
#[cfg(windows)]
fn drive_is_remote(letter: char) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOTE: u32 = 4;
    let root: Vec<u16> = format!("{letter}:\\")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `root` is a NUL-terminated UTF-16 string that outlives the call.
    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn shares_and_device_namespaces_are_refused_in_every_spelling() {
        for p in [
            r"\\host\share\x.txt",
            "//host/share/x.txt",
            r"/\host\share",
            r"\/host/share",
            r"\\?\C:\x.txt",
            r"\\?\UNC\host\share",
            r"\\.\pipe\x",
            r"\\.\COM1",
            r"\??\C:\x",
            "//?/C:/x",
        ] {
            assert!(
                matches!(check(p), Err(PathRefusal::Share | PathRefusal::Device)),
                "{p}: {:?}",
                check(p)
            );
        }
    }

    #[test]
    fn only_drive_absolute_paths_pass() {
        assert_eq!(check(r"C:\x\y.txt").unwrap(), PathBuf::from(r"C:\x\y.txt"));
        assert_eq!(check("C:/x/y.txt").unwrap(), PathBuf::from(r"C:\x\y.txt"));
        assert_eq!(check(r"C:\").unwrap(), PathBuf::from(r"C:\"));
        for p in ["C:x", "C:javascript:alert(1)", r"\x\y", "x/y", "", "1:/x"] {
            assert!(check(p).is_err(), "{p}");
        }
    }

    #[test]
    fn dot_segments_streams_and_control_characters_are_refused() {
        assert_eq!(check(r"C:\a\..\b"), Err(PathRefusal::DotSegment));
        assert_eq!(check(r"C:\a\.\b"), Err(PathRefusal::DotSegment));
        assert_eq!(check(r"C:\a\\b"), Err(PathRefusal::DotSegment));
        assert_eq!(check(r"C:\a\b.txt:secret"), Err(PathRefusal::Stream));
        assert_eq!(check("C:\\a\u{7}b"), Err(PathRefusal::Control));
        assert_eq!(
            check(&format!("C:\\{}", "a".repeat(MAX_PATH_BYTES))),
            Err(PathRefusal::TooLong)
        );
    }

    #[test]
    fn reserved_device_names_are_refused_in_any_folder() {
        for p in [
            r"C:\NUL",
            r"C:\x\con",
            r"C:\x\nul.txt",
            r"C:\x\COM1.log",
            r"C:\x\lpt9",
            r"C:\x\AUX .txt",
            r"C:\x\CONOUT$",
            r"C:\x\nul.",
        ] {
            assert_eq!(check(p), Err(PathRefusal::ReservedName), "{p}");
        }
        for p in [
            r"C:\x\console.txt",
            r"C:\x\com10",
            r"C:\x\nullable.rs",
            r"C:\x\LPT",
        ] {
            assert!(check(p).is_ok(), "{p}");
        }
    }
}
