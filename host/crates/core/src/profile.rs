//! The profile directory and the single-instance lock.
//!
//! The default profile is `%LOCALAPPDATA%\Helm` on Windows and
//! `$XDG_DATA_HOME/helm` (or `~/.local/share/helm`) on Linux. `HELM_PROFILE_DIR` selects another
//! profile; tests and parallel profiles use it.

use crate::privfs;
use helm_platform::instance::Endpoint;
use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

pub const PROFILE_ENV: &str = "HELM_PROFILE_DIR";
pub const LOCK_FILE: &str = "app.lock";
/// A fresh product, so a fresh database name.
pub const DB_FILE: &str = "helm.sqlite3";

/// The platform default profile folder.
pub fn default_profile_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))?;
        Ok(PathBuf::from(base).join("Helm"))
    }
    #[cfg(not(windows))]
    {
        if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(x).join("helm"));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("helm"))
    }
}

/// `HELM_PROFILE_DIR` if set (it must be absolute), otherwise the default.
pub fn resolve_profile_dir() -> io::Result<PathBuf> {
    match std::env::var_os(PROFILE_ENV).filter(|v| !v.is_empty()) {
        Some(v) => {
            let p = PathBuf::from(v);
            if !p.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{PROFILE_ENV} must be an absolute path"),
                ));
            }
            Ok(p)
        }
        None => default_profile_dir(),
    }
}

#[derive(Clone, Debug)]
pub struct Profile {
    /// Canonical path of the profile folder.
    pub dir: PathBuf,
    /// SHA-256 of the lowercased canonical path: names the instance pipe and the keychain account.
    pub hash: String,
}

/// Holding this keeps the profile exclusively ours; dropping it releases `app.lock`.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

pub enum LockOutcome {
    Acquired(InstanceLock),
    /// Another instance holds the lock: signal it and exit.
    AlreadyRunning,
}

impl Profile {
    /// Create (owner-only) and resolve the profile folder.
    pub fn open(dir: &Path) -> io::Result<Self> {
        privfs::create_private_dir_all(dir)?;
        let canon = std::fs::canonicalize(dir)?;
        let hash = crate::sha256_hex(canon.to_string_lossy().to_lowercase().as_bytes());
        Ok(Self { dir: canon, hash })
    }

    /// The folder for people to read: without Windows' verbatim (`\\?\`) prefix on drive paths.
    pub fn display_dir(&self) -> String {
        let s = self.dir.to_string_lossy();
        match s.strip_prefix(r"\\?\") {
            Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
            _ => s.into_owned(),
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    pub fn db_path(&self) -> PathBuf {
        self.path(DB_FILE)
    }

    /// Where a second launch signals this one.
    pub fn endpoint(&self) -> Endpoint {
        Endpoint::for_profile(&self.dir, &self.hash)
    }

    /// Take the exclusive single-instance lock on `app.lock`.
    pub fn lock(&self) -> io::Result<LockOutcome> {
        let path = self.path(LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {
                helm_platform::fs::restrict_to_owner(&path)?;
                Ok(LockOutcome::Acquired(InstanceLock { _file: file }))
            }
            Err(TryLockError::WouldBlock) => Ok(LockOutcome::AlreadyRunning),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_on_the_same_profile_is_refused_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::open(&dir.path().join("prof")).unwrap();
        let first = match p.lock().unwrap() {
            LockOutcome::Acquired(l) => l,
            LockOutcome::AlreadyRunning => panic!("first lock must succeed"),
        };
        assert!(matches!(p.lock().unwrap(), LockOutcome::AlreadyRunning));
        drop(first);
        assert!(matches!(p.lock().unwrap(), LockOutcome::Acquired(_)));
    }

    #[test]
    fn different_profiles_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let a = Profile::open(&dir.path().join("a")).unwrap();
        let b = Profile::open(&dir.path().join("b")).unwrap();
        let _la = a.lock().unwrap();
        assert!(matches!(b.lock().unwrap(), LockOutcome::Acquired(_)));
        assert_ne!(a.hash, b.hash);
        assert_ne!(a.endpoint(), b.endpoint());
    }

    #[test]
    fn display_dir_drops_the_verbatim_prefix_on_drive_paths_only() {
        let p = Profile {
            dir: PathBuf::from(r"\\?\C:\x\Helm"),
            hash: String::new(),
        };
        assert_eq!(p.display_dir(), r"C:\x\Helm");
        let unc = Profile {
            dir: PathBuf::from(r"\\?\UNC\srv\share"),
            hash: String::new(),
        };
        assert_eq!(unc.display_dir(), r"\\?\UNC\srv\share");
    }

    #[test]
    fn profile_folder_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::open(&dir.path().join("prof")).unwrap();
        assert!(helm_platform::fs::is_owner_only(&p.dir).unwrap());
    }

    #[test]
    fn env_var_selects_the_profile() {
        // The only test that touches HELM_PROFILE_DIR, so no other test races it.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(PROFILE_ENV, dir.path());
        assert_eq!(resolve_profile_dir().unwrap(), dir.path());
        std::env::set_var(PROFILE_ENV, "relative/path");
        assert!(resolve_profile_dir().is_err());
        std::env::remove_var(PROFILE_ENV);
        assert_eq!(
            resolve_profile_dir().unwrap(),
            default_profile_dir().unwrap()
        );
    }
}
