//! ZIP contents listing for the viewer: names, sizes and folders, nothing extracted.
//!
//! Only the end-of-central-directory record and the central directory are read, never an entry's
//! data. The directory is capped at 8 MiB, at most 10,000 entries are listed (the rest are only
//! counted and the listing is marked truncated), each name is at most 4,096 bytes and all listed
//! names together at most 1 MiB. ZIP64 archives and directories that do not add up are refused with
//! a plain message rather than guessed at.

use crate::private::PrivateRoots;
use crate::viewer::{self, ReadError, ViewKind};
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use ts_rs::TS;

pub const MAX_ENTRIES: usize = 10_000;
pub const MAX_DIRECTORY: u64 = 8 * 1024 * 1024;
pub const MAX_NAME: usize = 4096;
pub const MAX_NAMES_TOTAL: usize = 1024 * 1024;
const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const EOCD_LEN: usize = 22;
/// The end record sits within its fixed size plus a comment of at most 65,535 bytes.
const EOCD_SEARCH: u64 = EOCD_LEN as u64 + 65_535;
const CENTRAL_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
const CENTRAL_LEN: usize = 46;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveEntry {
    pub path: String,
    /// Uncompressed size; absent when the archive stores it only in a ZIP64 extra field.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub compressed_size: Option<u64>,
    pub dir: bool,
    pub encrypted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveView {
    pub path: String,
    #[ts(type = "number")]
    pub size: u64,
    pub entries: Vec<ArchiveEntry>,
    /// Every entry the directory declares, listed or not.
    #[ts(type = "number")]
    pub total: usize,
    pub truncated: bool,
}

const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ',
    'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕',
    '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐',
    '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

fn le16(b: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u64
}

fn le32(b: &[u8], at: usize) -> u64 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as u64
}

/// A name as the archive wrote it: UTF-8 when flagged, else the legacy DOS code page. Control
/// characters become the replacement character and backslashes become slashes.
fn decode_name(raw: &[u8], utf8: bool) -> String {
    let text: String = if utf8 {
        String::from_utf8_lossy(raw).into_owned()
    } else {
        raw.iter()
            .map(|&b| {
                if b < 0x80 {
                    b as char
                } else {
                    CP437_HIGH[(b - 0x80) as usize]
                }
            })
            .collect()
    };
    text.chars()
        .map(|c| {
            if c.is_control() {
                '\u{fffd}'
            } else if c == '\\' {
                '/'
            } else {
                c
            }
        })
        .collect()
}

fn malformed() -> ReadError {
    ReadError::Refused("This archive's directory is damaged, so it is not listed.".into())
}

fn size_field(v: u64) -> Option<u64> {
    (v != 0xFFFF_FFFF).then_some(v)
}

/// Lists a ZIP archive from any seekable source holding `len` bytes.
pub fn list_reader<R: Read + Seek>(
    r: &mut R,
    len: u64,
    shown: String,
) -> Result<ArchiveView, ReadError> {
    let io = |e: std::io::Error| ReadError::Io(e.to_string());
    if len < EOCD_LEN as u64 {
        return Err(ReadError::Refused("This is not a ZIP archive.".into()));
    }
    let tail_len = len.min(EOCD_SEARCH);
    r.seek(SeekFrom::Start(len - tail_len)).map_err(io)?;
    let mut tail = vec![0u8; tail_len as usize];
    r.read_exact(&mut tail).map_err(io)?;
    let at = (0..=tail.len() - EOCD_LEN)
        .rev()
        .find(|&i| {
            tail[i..i + 4] == EOCD_SIG && {
                // The comment length must reach exactly to the end of the file.
                let c = le16(&tail, i + 20) as usize;
                i + EOCD_LEN + c == tail.len()
            }
        })
        .ok_or_else(|| ReadError::Refused("This is not a ZIP archive.".into()))?;
    let end = &tail[at..];
    let (disk, cd_disk) = (le16(end, 4), le16(end, 6));
    let (entries_here, total16) = (le16(end, 8), le16(end, 10));
    let (cd_size, cd_offset) = (le32(end, 12), le32(end, 16));
    if total16 == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
        return Err(ReadError::Refused(
            "ZIP64 archives are not listed here.".into(),
        ));
    }
    if disk != 0 || cd_disk != 0 || entries_here != total16 {
        return Err(ReadError::Refused(
            "Archives split across several files are not listed.".into(),
        ));
    }
    if cd_size > MAX_DIRECTORY {
        return Err(ReadError::TooLarge(
            "This archive's directory is too large to list.",
        ));
    }
    let eocd_at = len - (tail.len() - at) as u64;
    if cd_offset + cd_size > eocd_at || (total16 > 0 && cd_size < CENTRAL_LEN as u64) {
        return Err(malformed());
    }
    r.seek(SeekFrom::Start(cd_offset)).map_err(io)?;
    let mut dir = vec![0u8; cd_size as usize];
    r.read_exact(&mut dir).map_err(io)?;

    let total = total16 as usize;
    let mut entries = Vec::with_capacity(total.min(MAX_ENTRIES));
    let mut names_total = 0usize;
    let mut truncated = false;
    let mut pos = 0usize;
    for _ in 0..total {
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        if pos + CENTRAL_LEN > dir.len() || dir[pos..pos + 4] != CENTRAL_SIG {
            return Err(malformed());
        }
        let flags = le16(&dir, pos + 8);
        let compressed = le32(&dir, pos + 20);
        let size = le32(&dir, pos + 24);
        let name_len = le16(&dir, pos + 28) as usize;
        let extra_len = le16(&dir, pos + 30) as usize;
        let comment_len = le16(&dir, pos + 32) as usize;
        let name_at = pos + CENTRAL_LEN;
        let next = name_at + name_len + extra_len + comment_len;
        if name_len > MAX_NAME || next > dir.len() {
            return Err(malformed());
        }
        names_total += name_len;
        if names_total > MAX_NAMES_TOTAL {
            truncated = true;
            break;
        }
        let name = decode_name(&dir[name_at..name_at + name_len], flags & 0x800 != 0);
        entries.push(ArchiveEntry {
            dir: name.ends_with('/'),
            path: name,
            size: size_field(size),
            compressed_size: size_field(compressed),
            encrypted: flags & 1 != 0,
        });
        pos = next;
    }
    Ok(ArchiveView {
        path: shown,
        size: len,
        entries,
        total,
        truncated,
    })
}

/// The `archive_list` op's read: the viewer's checks, then the listing.
pub fn list(raw: &str, roots: &PrivateRoots) -> Result<ArchiveView, ReadError> {
    let path = viewer::admit(raw, roots)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(viewer::stream_kind(&ext), Some((ViewKind::Archive, _))) {
        return Err(ReadError::Refused("Only ZIP archives are listed.".into()));
    }
    let mut f = helm_platform::fs::open_regular_no_follow(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ReadError::NotFound,
        std::io::ErrorKind::PermissionDenied => ReadError::Refused(e.to_string()),
        _ => ReadError::Io(e.to_string()),
    })?;
    let len = f
        .metadata()
        .map_err(|e| ReadError::Io(e.to_string()))?
        .len();
    list_reader(&mut f, len, path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A ZIP with the given `(name, flags, size)` entries and no entry data (the listing never
    /// reads it), built by hand so the tests hold no archive fixture.
    fn zip(entries: &[(&[u8], u16, u32)], comment: &[u8]) -> Vec<u8> {
        let mut dir = Vec::new();
        for (name, flags, size) in entries {
            dir.extend_from_slice(&CENTRAL_SIG);
            dir.extend_from_slice(&[20, 0, 20, 0]);
            dir.extend_from_slice(&flags.to_le_bytes());
            dir.extend_from_slice(&[0; 2 + 4 + 4]); // method, time, crc
            dir.extend_from_slice(&(size / 2).to_le_bytes());
            dir.extend_from_slice(&size.to_le_bytes());
            dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
            dir.extend_from_slice(&[0; 2 + 2 + 2 + 2 + 4 + 4]);
            dir.extend_from_slice(name);
        }
        let mut out = vec![0u8; 10]; // stand-in for the entries' data
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(&EOCD_SIG);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
        out.extend_from_slice(comment);
        out
    }

    fn run(bytes: Vec<u8>) -> Result<ArchiveView, ReadError> {
        let len = bytes.len() as u64;
        list_reader(&mut Cursor::new(bytes), len, "x.zip".into())
    }

    #[test]
    fn the_central_directory_is_listed_and_odd_archives_are_refused_gracefully() {
        let v = run(zip(
            &[
                (b"docs/", 0, 0),
                (b"docs/a.txt", 0, 120),
                (b"caf\x82.txt", 0, 5),
                ("é.txt".as_bytes(), 0x800, 7),
                (b"secret.bin", 1, 9),
                (b"bad\x07\\name", 0, 1),
            ],
            b"a comment PK\x05\x06 inside",
        ))
        .unwrap();
        assert_eq!((v.total, v.truncated, v.entries.len()), (6, false, 6));
        let names: Vec<_> = v.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "docs/",
                "docs/a.txt",
                "café.txt",
                "é.txt",
                "secret.bin",
                "bad\u{fffd}/name"
            ]
        );
        assert!(v.entries[0].dir && !v.entries[1].dir);
        assert_eq!(v.entries[1].size, Some(120));
        assert_eq!(v.entries[1].compressed_size, Some(60));
        assert!(v.entries[4].encrypted && !v.entries[0].encrypted);

        // More entries than the cap: the first 10,000 are listed, the count is the real one.
        let many: Vec<(&[u8], u16, u32)> =
            (0..MAX_ENTRIES + 5).map(|_| (&b"f"[..], 0, 1)).collect();
        let v = run(zip(&many, b"")).unwrap();
        assert_eq!(
            (v.entries.len(), v.total, v.truncated),
            (MAX_ENTRIES, MAX_ENTRIES + 5, true)
        );

        // Refusals: not a zip, a directory that overruns, an oversized name, ZIP64.
        assert!(matches!(
            run(b"just text, no archive here".to_vec()),
            Err(ReadError::Refused(_))
        ));
        assert!(matches!(run(vec![]), Err(ReadError::Refused(_))));
        let mut cut = zip(&[(b"a", 0, 1)], b"");
        cut[10 + 28] = 200; // a name length past the directory's end
        assert!(matches!(run(cut), Err(ReadError::Refused(_))));
        let long = vec![b'n'; MAX_NAME + 1];
        assert!(matches!(
            run(zip(&[(&long, 0, 1)], b"")),
            Err(ReadError::Refused(_))
        ));
        let mut z64 = zip(&[(b"a", 0, 1)], b"");
        let n = z64.len();
        z64[n - 12..n - 8].copy_from_slice(&[0xFF; 4]); // directory size
        assert!(matches!(run(z64), Err(ReadError::Refused(m)) if m.contains("ZIP64")));
    }

    #[test]
    fn the_op_read_checks_the_path_and_the_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.zip");
        std::fs::write(&p, zip(&[(b"x.txt", 0, 3)], b"")).unwrap();
        let v = list(&p.to_string_lossy(), &PrivateRoots::default()).unwrap();
        assert_eq!(v.entries[0].path, "x.txt");
        let t = dir.path().join("a.txt");
        std::fs::write(&t, "x").unwrap();
        assert!(list(&t.to_string_lossy(), &PrivateRoots::default()).is_err());
        assert_eq!(
            list(
                &dir.path().join("no.zip").to_string_lossy(),
                &PrivateRoots::default()
            )
            .unwrap_err(),
            ReadError::NotFound
        );
    }
}
