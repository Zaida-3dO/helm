//! Building one archive: a plain folder copy you can open and read, plus a consistent database
//! copy made with SQLite's own online backup API (never a byte copy of the live file).

use crate::restore::{FAILED_FILE, PENDING_FILE, SAFETY_PREFIX};
use crate::{inside, BackupError};
use helm_core::profile::{DB_FILE, LOCK_FILE};
use rusqlite::{backup::Backup, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The archive format this build understands.
pub const FORMAT: u32 = 1;
pub const ARCHIVE_PREFIX: &str = "helm-backup-";
pub const MANIFEST_FILE: &str = "manifest.json";
/// The profile folder holding the credential envelopes.
pub const CREDENTIALS_DIR: &str = "credentials";

/// Top-level profile folders Helm rebuilds on its own: per-run staging, caches and transient editor
/// selections. They are not worth carrying, and a restore must not put old copies back.
const REBUILT_DIRS: [&str; 6] = [
    "runtime",
    "agent-bin",
    "workers",
    "thumbs",
    "catalogue",
    "ide-context",
];

/// The web view's own data folder. It holds the browser engine's sign-in and cookie stores, so it
/// never goes into an archive.
pub const WEBVIEW_DIR: &str = "webview";

/// A profile entry that must never travel into an archive — and, read back from a manifest, never
/// name something a restore may delete or replace. Everything credential-shaped is out by name
/// (the `.cred` rule also applies at every level, see [`copy_tree`]), and so are the web view's
/// data and the folders Helm rebuilds itself.
pub fn is_excluded(name: &str) -> bool {
    let n = name.to_lowercase();
    n == LOCK_FILE
        || n == DB_FILE
        || n == format!("{DB_FILE}-wal")
        || n == format!("{DB_FILE}-shm")
        || n == PENDING_FILE
        || n == FAILED_FILE
        || n == MANIFEST_FILE
        || n == CREDENTIALS_DIR
        || n == WEBVIEW_DIR
        || REBUILT_DIRS.contains(&n.as_str())
        || n.ends_with(".cred")
        || n.starts_with(SAFETY_PREFIX)
}

/// What an archive says it is. Anything unreadable or of another format is simply not a backup.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub app_version: String,
    /// The top-level profile entries carried besides the database, sorted.
    #[serde(default)]
    pub entries: Vec<String>,
}

pub fn read_manifest(archive: &Path) -> Result<Manifest, BackupError> {
    let text = std::fs::read_to_string(archive.join(MANIFEST_FILE))
        .map_err(|_| BackupError::NotABackup)?;
    let m: Manifest = serde_json::from_str(&text).map_err(|_| BackupError::NotABackup)?;
    if m.format != FORMAT || !archive.join(DB_FILE).is_file() {
        return Err(BackupError::NotABackup);
    }
    Ok(m)
}

pub struct Created {
    pub archive_dir: PathBuf,
    pub entries: i64,
    pub db_bytes: i64,
}

/// Write `<dest_root>/helm-backup-<stamp>` holding the manifest, a consistent copy of the
/// database, and every top-level profile entry the exclusion list lets through. The destination
/// may not be inside the profile itself (that would archive a backup beside the thing it guards).
pub fn create(
    db_path: &Path,
    profile_dir: &Path,
    dest_root: &Path,
    app_version: &str,
    now_ms: i64,
) -> Result<Created, BackupError> {
    if inside(profile_dir, dest_root) {
        return Err(BackupError::InsideProfile);
    }
    let archive = unique_dir(
        dest_root,
        &format!("{ARCHIVE_PREFIX}{}", crate::stamp(now_ms)),
    )?;
    {
        // The consistent database copy first, while Helm keeps answering around us.
        let src = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut dst = Connection::open(archive.join(DB_FILE))?;
        let backup = Backup::new(&src, &mut dst)?;
        backup.run_to_completion(100, Duration::from_millis(10), None)?;
    }
    let mut entries: Vec<String> = Vec::new();
    for e in std::fs::read_dir(profile_dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if is_excluded(&name) {
            continue;
        }
        copy_tree(&e.path(), &archive.join(&name))?;
        entries.push(name);
    }
    entries.sort();
    let db_bytes = std::fs::metadata(archive.join(DB_FILE))?.len() as i64;
    let manifest = Manifest {
        format: FORMAT,
        created_at: now_ms,
        app_version: app_version.to_owned(),
        entries,
    };
    let text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| BackupError::Io(std::io::Error::other(e)))?;
    std::fs::write(archive.join(MANIFEST_FILE), text)?;
    Ok(Created {
        archive_dir: archive,
        entries: manifest.entries.len() as i64,
        db_bytes,
    })
}

/// The archives in `dir`, newest first; a folder that is not a readable backup is left out.
pub fn list(dir: &Path) -> Result<Vec<crate::types::BackupEntry>, BackupError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(ARCHIVE_PREFIX) || !e.path().is_dir() {
            continue;
        }
        let Ok(m) = read_manifest(&e.path()) else {
            continue;
        };
        let db_bytes = e
            .path()
            .join(DB_FILE)
            .metadata()
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        out.push(crate::types::BackupEntry {
            name,
            path: e.path().to_string_lossy().into_owned(),
            created_at: m.created_at,
            db_bytes,
            entries: m.entries.len() as i64,
        });
    }
    out.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.name.cmp(&a.name))
    });
    Ok(out)
}

/// Copy a file tree, counting nothing and learning nothing: a profile never holds links to chase,
/// and anything credential-shaped is left out at every level, not just the top.
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<(), BackupError> {
    let md = std::fs::symlink_metadata(src)?;
    if md.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            let name = e.file_name();
            if name.to_string_lossy().to_lowercase().ends_with(".cred") {
                continue;
            }
            copy_tree(&e.path(), &dst.join(name))?;
        }
        Ok(())
    } else if md.is_file() {
        std::fs::copy(src, dst)?;
        Ok(())
    } else {
        // A socket or similar: skip it rather than fail the whole archive over a transient node.
        Ok(())
    }
}

fn unique_dir(root: &Path, base: &str) -> Result<PathBuf, BackupError> {
    let mut name = base.to_owned();
    for i in 2.. {
        let p = root.join(&name);
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                name = format!("{base}-{i}");
            }
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("the loop always returns or tries the next suffix")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    const T0: i64 = 1_760_000_000_000;

    #[test]
    fn an_archive_carries_the_data_and_never_anything_credential_shaped() {
        let f = testutil::profile("keep me");
        let dest = tempfile::tempdir().unwrap();
        let created = create(&f.db, f.dir.path(), dest.path(), "0.6.2", T0).unwrap();
        let files = testutil::tree(&created.archive_dir);

        // The guardrail, proved by the listing: nothing credential-shaped travelled.
        for p in &files {
            assert!(!p.contains("credentials"), "{p} in the archive");
            assert!(!p.ends_with(".cred"), "{p} in the archive");
        }
        // What is in: the plain folders, and a database that opens as one.
        assert!(files.contains(&"sessions/s1/log.jsonl".to_string()));
        assert!(files.contains(&"artifacts/a.json".to_string()));
        assert!(files.contains(&"attachments/".to_string()));
        assert!(files.contains(&"manifest.json".to_string()));
        assert!(files.contains(&"helm.sqlite3".to_string()));
        // What is not: lock, sidecars, bookkeeping, safety folders, loose envelopes.
        for bad in [
            "app.lock",
            "helm.sqlite3-wal",
            "token.cred",
            "restore-pending.json",
        ] {
            assert!(!files.contains(&bad.to_string()), "{bad} in the archive");
        }
        assert!(!files.iter().any(|p| p.starts_with("before-restore-")));
        // The web view's data (browser sign-in and cookie stores) and the folders Helm rebuilds.
        for bad in ["webview", "runtime"] {
            assert!(
                !files.iter().any(|p| p.starts_with(bad)),
                "{bad} in the archive"
            );
        }

        // The copied database is a real SQLite database holding the row as of the copy.
        let c = Connection::open(created.archive_dir.join(DB_FILE)).unwrap();
        let got: String = c
            .query_row("SELECT x FROM notes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(got, "keep me");

        assert_eq!(created.entries, 3); // sessions, artifacts, attachments
        assert!(created.db_bytes > 0);
        let m = read_manifest(&created.archive_dir).unwrap();
        assert_eq!(m.entries, ["artifacts", "attachments", "sessions"]);

        // A second archive in the same second lands beside it.
        let again = create(&f.db, f.dir.path(), dest.path(), "0.6.2", T0).unwrap();
        assert_ne!(again.archive_dir, created.archive_dir);

        // And a destination inside the profile is refused outright.
        let inside = create(
            &f.db,
            f.dir.path(),
            &f.dir.path().join("Helm Backups"),
            "0.6.2",
            T0,
        );
        assert!(matches!(inside, Err(BackupError::InsideProfile)));
    }
}
