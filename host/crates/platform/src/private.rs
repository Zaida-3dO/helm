//! Private roots, refused by path and also by device and inode.
//! The app's own profile folder (data, sessions, backups, caches) is never read, listed,
//! previewed or opened by the panel.
//!
//! A path is refused when it is lexically inside a root, **or** when it or any of its ancestors is
//! the same directory as a root by file identity. On Windows the identity is the volume serial
//! number plus the file index (Windows' counterpart of device and inode), which catches 8.3 short
//! names, other casings and other spellings of the same folder.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    pub volume: u64,
    pub index: u64,
}

/// The identity of an existing file or directory, without following a final link.
pub fn file_id(path: &Path) -> std::io::Result<FileId> {
    imp::file_id(path)
}

#[derive(Debug, Clone, Default)]
pub struct PrivateRoots {
    paths: Vec<String>,
    ids: Vec<FileId>,
}

/// Case-insensitive, slash-normalised, without the verbatim prefix and trailing separator.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    s.trim_end_matches('\\').to_lowercase()
}

impl PrivateRoots {
    pub fn new(roots: &[PathBuf]) -> Self {
        let mut out = Self::default();
        for r in roots {
            out.paths.push(norm(r));
            if let Ok(id) = file_id(r) {
                out.ids.push(id);
            }
        }
        out
    }

    /// True when `path` is a private root or inside one. Reads identities of the path's existing
    /// ancestors (never their contents); call only after the lexical path check has passed.
    pub fn contains(&self, path: &Path) -> bool {
        let n = norm(path);
        if self
            .paths
            .iter()
            .any(|r| n == *r || n.starts_with(&format!("{r}\\")))
        {
            return true;
        }
        if self.ids.is_empty() {
            return false;
        }
        let mut cur = Some(path);
        while let Some(p) = cur {
            if let Ok(id) = file_id(p) {
                if self.ids.contains(&id) {
                    return true;
                }
            }
            cur = p.parent();
        }
        false
    }

    /// For listing a folder already admitted: is this child itself a root (by path or identity)?
    /// Checks only the child, not its ancestors, so a listing stays cheap.
    pub fn is_root(&self, child: &Path) -> bool {
        let n = norm(child);
        if self.paths.contains(&n) {
            return true;
        }
        !self.ids.is_empty()
            && file_id(child)
                .map(|id| self.ids.contains(&id))
                .unwrap_or(false)
    }
}

#[cfg(windows)]
mod imp {
    use super::FileId;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT,
    };

    pub fn file_id(path: &Path) -> std::io::Result<FileId> {
        // Opening with no access rights reads only metadata; backup semantics allow directories.
        let f = std::fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open for the duration of the call and `info` is a valid out-pointer.
        let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(FileId {
            volume: info.dwVolumeSerialNumber as u64,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        })
    }
}

#[cfg(unix)]
mod imp {
    use super::FileId;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    pub fn file_id(path: &Path) -> std::io::Result<FileId> {
        let m = std::fs::symlink_metadata(path)?;
        Ok(FileId {
            volume: m.dev(),
            index: m.ino(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_and_everything_under_it_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Helm");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        let roots = PrivateRoots::new(std::slice::from_ref(&root));
        assert!(roots.contains(&root));
        assert!(roots.contains(&root.join("sessions").join("x.jsonl")));
        assert!(!roots.contains(&dir.path().join("Helmet")));
        assert!(!roots.contains(dir.path()));
    }

    #[cfg(windows)]
    #[test]
    fn another_spelling_of_a_root_is_private_by_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("HelmProfileFolder");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("a.txt"), "x").unwrap();
        let roots = PrivateRoots::new(std::slice::from_ref(&root));
        // Another casing is caught lexically; the 8.3 short name (when the volume has them) only by
        // identity. Find the short name through the OS rather than guessing it.
        let upper = PathBuf::from(root.to_string_lossy().to_uppercase());
        assert!(roots.contains(&upper.join("sub").join("a.txt")));
        if let Some(short) = short_name(&root) {
            if norm(&short) != norm(&root) {
                assert!(
                    roots.contains(&short.join("sub").join("a.txt")),
                    "{short:?}"
                );
            }
        }
        // A sibling outside the root is not private.
        assert!(!roots.contains(&dir.path().join("elsewhere.txt")));
    }

    #[cfg(windows)]
    fn short_name(p: &Path) -> Option<PathBuf> {
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let wide: Vec<u16> = p
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: both buffers are valid for the lengths given.
        let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
            as usize;
        if n == 0 || n >= buf.len() {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    }

    #[test]
    fn identity_survives_a_lexically_different_path() {
        // Same directory reached through a path the lexical check does not recognise: the roots
        // hold only the identity, the path list is empty.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir_all(root.join("x")).unwrap();
        let mut roots = PrivateRoots::new(std::slice::from_ref(&root));
        roots.paths.clear();
        assert!(roots.contains(&root.join("x")));
        assert!(!roots.contains(dir.path()));
    }
}
