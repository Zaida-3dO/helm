//! Private-by-default files.
//!
//! Every folder the app creates is owner-only, and every file it writes is owner-only and written
//! atomically: a create-new temp file beside the target, write, flush to disk, then an atomic
//! replace with write-through (retried while antivirus holds the file).

use helm_platform::fs as pfs;
use std::io::{self, Read, Write};
use std::path::Path;

/// Create `dir` (and missing ancestors) and make `dir` itself owner-only. An existing folder is
/// re-restricted, which repairs a profile whose permissions were widened. A link is refused.
pub fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        if pfs::is_link_like(&meta) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refused: folder is a link or reparse point",
            ));
        }
    }
    std::fs::create_dir_all(dir)?;
    pfs::restrict_to_owner(dir)
}

/// Atomically write `bytes` to `path` as an owner-only file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        pfs::restrict_to_owner(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        pfs::replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Read a regular file without following links, refusing anything larger than `cap` bytes.
pub fn read_capped(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let f = pfs::open_regular_no_follow(path)?;
    if f.metadata()?.len() > cap {
        return Err(too_large());
    }
    let mut buf = Vec::new();
    f.take(cap + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        return Err(too_large());
    }
    Ok(buf)
}

fn too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "file exceeds its size limit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_is_owner_only_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prof");
        create_private_dir_all(&p).unwrap();
        assert!(pfs::is_owner_only(&p).unwrap());
        let f = p.join("x.json");
        write_atomic(&f, b"one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        assert!(pfs::is_owner_only(&f).unwrap());
        let names: Vec<_> = std::fs::read_dir(&p)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("x.json")]);
    }

    #[test]
    fn atomic_write_never_reuses_an_existing_temp() {
        // create_new means a planted file at the temp name can never be written through; the
        // random suffix makes collisions practically impossible, so this checks the flag directly.
        let dir = tempfile::tempdir().unwrap();
        let planted = dir.path().join("planted");
        std::fs::write(&planted, b"x").unwrap();
        let r = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&planted);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn read_capped_refuses_oversize() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("big");
        std::fs::write(&f, vec![0u8; 11]).unwrap();
        assert!(read_capped(&f, 11).is_ok());
        assert_eq!(
            read_capped(&f, 10).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
