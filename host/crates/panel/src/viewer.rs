//! File viewer reads: `file_read` and `dir_list`.
//!
//! Text is read up to 512 KiB (a longer file comes back as an excerpt, marked truncated), images up
//! to 8 MiB (as base64 bytes, shown by the renderer as an object URL), directories up to 1,000
//! entries. Every path passes the lexical [`crate::pathgate`] check, then the private-root check,
//! then a read that refuses links along the path. Nothing is ever executed or rendered here.

use crate::pathgate;
use crate::private::PrivateRoots;
use base64::Engine;
use serde::Serialize;
use std::io::Read;
use std::path::Path;

pub const TEXT_LIMIT: u64 = 512 * 1024;
pub const IMAGE_LIMIT: u64 = 8 * 1024 * 1024;
pub const DIR_LIMIT: usize = 1000;
/// Bytes inspected for a NUL to call a file binary.
const SNIFF: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewKind {
    Text,
    Image,
    Dir,
    Html,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    /// `dir`, `file` or `link`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    pub path: String,
    pub kind: ViewKind,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Base64 image bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<DirEntry>>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Refused(String),
    NotFound,
    TooLarge(&'static str),
    Io(String),
}

impl ReadError {
    pub fn message(&self) -> String {
        match self {
            Self::Refused(m) => m.clone(),
            Self::NotFound => "The file was not found.".into(),
            Self::TooLarge(m) => (*m).into(),
            Self::Io(m) => m.clone(),
        }
    }
}

pub const PRIVATE_MESSAGE: &str = "This folder belongs to Helm itself and is never shown.";

fn io_err(e: std::io::Error) -> ReadError {
    match e.kind() {
        std::io::ErrorKind::NotFound => ReadError::NotFound,
        std::io::ErrorKind::PermissionDenied => ReadError::Refused(e.to_string()),
        _ => ReadError::Io(e.to_string()),
    }
}

/// Checks a renderer-supplied path: the lexical gate, then no links along it (a no-follow stat per
/// component), then private roots. No contents are read.
pub fn admit(raw: &str, roots: &PrivateRoots) -> Result<std::path::PathBuf, ReadError> {
    let path = pathgate::check(raw).map_err(|r| ReadError::Refused(r.message().into()))?;
    // Links anywhere along the path are refused before anything opens or follows it: the identity
    // check below opens each ancestor, and a folder link could point at a network share.
    helm_platform::fs::refuse_links_along(&path).map_err(io_err)?;
    if roots.contains(&path) {
        return Err(ReadError::Refused(PRIVATE_MESSAGE.into()));
    }
    Ok(path)
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn image_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// A language id the renderer's highlighter knows, from the extension or the file name.
pub fn language_for(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name == "dockerfile" {
        return Some("dockerfile");
    }
    if name == "makefile" {
        return Some("makefile");
    }
    Some(match extension(path).as_str() {
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "json" | "jsonc" => "json",
        "md" | "markdown" => "markdown",
        "rs" => "rust",
        "py" => "python",
        "css" => "css",
        "scss" => "scss",
        "html" | "htm" => "xml",
        "xml" | "svg" | "csproj" | "props" => "xml",
        "yml" | "yaml" => "yaml",
        "toml" => "ini",
        "ini" | "cfg" | "conf" => "ini",
        "sh" | "bash" => "bash",
        "ps1" | "psm1" | "psd1" => "powershell",
        "bat" | "cmd" => "dos",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "cs" => "csharp",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" | "cxx" => "cpp",
        "sql" => "sql",
        "diff" | "patch" => "diff",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "lua" => "lua",
        _ => return None,
    })
}

/// Cuts at a UTF-8 boundary at or below `max` bytes.
fn cut_utf8(bytes: &[u8], max: usize) -> &[u8] {
    if bytes.len() <= max {
        return bytes;
    }
    let mut end = max;
    while end > 0 && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    &bytes[..end]
}

/// Reads a file or lists a directory for the viewer.
pub fn read(raw: &str, roots: &PrivateRoots) -> Result<FileView, ReadError> {
    let path = admit(raw, roots)?;
    let meta = std::fs::symlink_metadata(&path).map_err(io_err)?;
    if helm_platform::fs::is_link_like(&meta) {
        return Err(ReadError::Refused(
            "Links and junctions are not opened.".into(),
        ));
    }
    let shown = path.to_string_lossy().into_owned();
    if meta.is_dir() {
        let (entries, truncated) = list_dir(&path, roots)?;
        return Ok(FileView {
            path: shown,
            kind: ViewKind::Dir,
            size: 0,
            text: None,
            bytes: None,
            mime: None,
            entries: Some(entries),
            truncated,
            language: None,
        });
    }
    let size = meta.len();
    let ext = extension(&path);
    if let Some(mime) = image_mime(&ext) {
        if size > IMAGE_LIMIT {
            return Err(ReadError::TooLarge(
                "The image is larger than 8 MiB, so it is not previewed.",
            ));
        }
        let bytes = helm_core::privfs::read_capped(&path, IMAGE_LIMIT).map_err(io_err)?;
        return Ok(FileView {
            path: shown,
            kind: ViewKind::Image,
            size,
            text: None,
            bytes: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
            mime: Some(mime),
            entries: None,
            truncated: false,
            language: None,
        });
    }
    let f = helm_platform::fs::open_regular_no_follow(&path).map_err(io_err)?;
    let mut buf = Vec::new();
    f.take(TEXT_LIMIT + 4)
        .read_to_end(&mut buf)
        .map_err(io_err)?;
    let binary = buf[..buf.len().min(SNIFF)].contains(&0);
    if binary {
        return Ok(FileView {
            path: shown,
            kind: ViewKind::Binary,
            size,
            text: None,
            bytes: None,
            mime: None,
            entries: None,
            truncated: false,
            language: None,
        });
    }
    let truncated = size > TEXT_LIMIT || buf.len() as u64 > TEXT_LIMIT;
    let text = String::from_utf8_lossy(cut_utf8(&buf, TEXT_LIMIT as usize)).into_owned();
    let html = matches!(ext.as_str(), "html" | "htm");
    Ok(FileView {
        path: shown,
        kind: if html { ViewKind::Html } else { ViewKind::Text },
        size,
        text: Some(text),
        bytes: None,
        mime: None,
        entries: None,
        truncated,
        language: language_for(&path),
    })
}

/// Folders first, then case-insensitive name; at most 1,000. Private roots, devices, other types
/// and names with control characters or backslashes are left out.
pub fn list_dir(dir: &Path, roots: &PrivateRoots) -> Result<(Vec<DirEntry>, bool), ReadError> {
    helm_platform::fs::refuse_links_along(dir).map_err(io_err)?;
    let mut out = Vec::new();
    let mut truncated = false;
    for entry in std::fs::read_dir(dir).map_err(io_err)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.chars().any(|c| c.is_control() || c == '\\') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let kind = if helm_platform::fs::is_link_like(&meta) || ft.is_symlink() {
            "link"
        } else if ft.is_dir() {
            "dir"
        } else if ft.is_file() {
            "file"
        } else {
            continue;
        };
        if kind == "dir" && roots.is_root(&entry.path()) {
            continue;
        }
        if out.len() >= DIR_LIMIT {
            truncated = true;
            break;
        }
        out.push(DirEntry {
            name,
            kind,
            size: (kind == "file").then_some(meta.len()),
        });
    }
    out.sort_by(|a, b| {
        (a.kind != "dir")
            .cmp(&(b.kind != "dir"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((out, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn text_is_capped_at_512_kib_and_marked_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("a.rs");
        std::fs::write(&small, "fn main() {}\n").unwrap();
        let v = read(&s(&small), &PrivateRoots::default()).unwrap();
        assert_eq!(v.kind, ViewKind::Text);
        assert_eq!(v.text.as_deref(), Some("fn main() {}\n"));
        assert_eq!(v.language, Some("rust"));
        assert!(!v.truncated);

        let big = dir.path().join("big.txt");
        // Multi-byte characters straddle the cut, which must land on a character boundary.
        std::fs::write(&big, "é".repeat(300 * 1024)).unwrap();
        let v = read(&s(&big), &PrivateRoots::default()).unwrap();
        assert!(v.truncated);
        let text = v.text.unwrap();
        assert!(text.len() as u64 <= TEXT_LIMIT && text.len() as u64 > TEXT_LIMIT - 4);
        assert!(!text.contains('\u{fffd}'));
    }

    #[test]
    fn binary_files_are_not_shown_as_text_and_html_is_marked() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("x.dat");
        std::fs::write(&bin, [b'a', 0, b'b']).unwrap();
        let v = read(&s(&bin), &PrivateRoots::default()).unwrap();
        assert_eq!(v.kind, ViewKind::Binary);
        assert!(v.text.is_none());
        let html = dir.path().join("p.html");
        std::fs::write(&html, "<script>alert(1)</script>").unwrap();
        assert_eq!(
            read(&s(&html), &PrivateRoots::default()).unwrap().kind,
            ViewKind::Html
        );
    }

    #[test]
    fn images_come_back_as_bytes_up_to_8_mib() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("i.png");
        std::fs::write(&png, [0x89, b'P', b'N', b'G']).unwrap();
        let v = read(&s(&png), &PrivateRoots::default()).unwrap();
        assert_eq!(v.kind, ViewKind::Image);
        assert_eq!(v.mime, Some("image/png"));
        assert_eq!(v.bytes.as_deref(), Some("iVBORw=="));
        let huge = dir.path().join("h.jpg");
        let f = std::fs::File::create(&huge).unwrap();
        f.set_len(IMAGE_LIMIT + 1).unwrap();
        assert!(matches!(
            read(&s(&huge), &PrivateRoots::default()),
            Err(ReadError::TooLarge(_))
        ));
    }

    #[test]
    fn directories_list_folders_first_capped_at_1000_without_private_roots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("zeta")).unwrap();
        std::fs::create_dir(dir.path().join("Helm")).unwrap();
        std::fs::write(dir.path().join("Alpha.txt"), "x").unwrap();
        std::fs::write(dir.path().join("beta.txt"), "xy").unwrap();
        let roots = PrivateRoots::new(&[dir.path().join("Helm")]);
        let v = read(&s(dir.path()), &roots).unwrap();
        let names: Vec<_> = v.entries.unwrap().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["zeta", "Alpha.txt", "beta.txt"]);

        let many = tempfile::tempdir().unwrap();
        for i in 0..(DIR_LIMIT + 5) {
            std::fs::write(many.path().join(format!("f{i}")), "").unwrap();
        }
        let v = read(&s(many.path()), &PrivateRoots::default()).unwrap();
        assert!(v.truncated);
        assert_eq!(v.entries.unwrap().len(), DIR_LIMIT);
    }

    #[test]
    fn private_roots_and_gate_refusals_come_before_any_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Helm");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("helm.sqlite3"), "secret").unwrap();
        let roots = PrivateRoots::new(std::slice::from_ref(&root));
        assert_eq!(
            read(&s(&root.join("helm.sqlite3")), &roots).unwrap_err(),
            ReadError::Refused(PRIVATE_MESSAGE.into())
        );
        assert!(matches!(
            read(&s(&root), &roots),
            Err(ReadError::Refused(_))
        ));
        #[cfg(windows)]
        for p in [r"\\host\share\x.txt", r"\\?\C:\x", "C:x", r"C:\NUL"] {
            assert!(matches!(read(p, &roots), Err(ReadError::Refused(_))), "{p}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("a.txt"), "x").unwrap();
        let link = dir.path().join("link");
        let ok = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return;
        }
        assert!(matches!(
            read(&s(&link.join("a.txt")), &PrivateRoots::default()),
            Err(ReadError::Refused(_))
        ));
        assert!(matches!(
            read(&s(&link), &PrivateRoots::default()),
            Err(ReadError::Refused(_))
        ));
    }
}
