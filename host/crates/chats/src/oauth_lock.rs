//! Clearing a Claude Code sign-in lock that its holder left behind.
//!
//! Claude Code refreshes the sign-in its processes share under a lock in its config folder: a
//! folder named `.oauth_refresh.lock`, beside an owner file `.oauth_refresh.lock.owner` that names
//! the process holding it. A holder that ends halfway through a refresh leaves both behind, and
//! every Claude process then fails its own refresh until they are gone. Before Helm retries a chat
//! whose refresh failed, it looks: when the owner's process is no longer running, or there is no
//! owner file it can read and the lock is older than [`STALE_AFTER`], it removes the lock, the
//! owner file and any half-written owner files older than that.
//!
//! What it reads is narrow on purpose. From the owner file it takes only the `pid` (the file is
//! read only when it is at most [`OWNER_MAX_BYTES`], every other field is ignored, and nothing of
//! it is ever logged); otherwise it looks only at the names and modified times of these entries.
//! No other file in the folder is opened. A lock whose owner is running is never touched, even
//! when it is old.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A lock with no readable owner is left alone until it is this old; half-written owner files
/// too.
pub const STALE_AFTER: Duration = Duration::from_secs(3 * 60);
/// An owner file larger than this is not read.
pub const OWNER_MAX_BYTES: u64 = 4 * 1024;

const LOCK_DIR: &str = ".oauth_refresh.lock";
const OWNER_FILE: &str = ".oauth_refresh.lock.owner";
const OWNER_TMP_PREFIX: &str = ".oauth_refresh.lock.owner.tmp.";

/// The line logged, and the line the chat gets, when a lock is cleared.
pub const CLEARED_LOG: &str =
    "cleared a Claude Code sign-in lock left by a process that had exited";
pub const CLEARED_TEXT: &str =
    "Helm cleared a Claude Code sign-in lock left behind by a process that had exited, then tried again.";

/// The one field Helm takes from the owner file.
#[derive(Deserialize)]
struct Owner {
    pid: Option<u32>,
}

/// Claude Code's config folder: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".claude"))
        })
}

/// Whether a lock is abandoned: its owner (`owner_pid`, when the owner file could be read) is not
/// running (`pid_alive`), or there is no owner to ask and the lock is older than [`STALE_AFTER`].
pub fn stale(owner_pid: Option<u32>, lock_age: Duration, pid_alive: bool) -> bool {
    match owner_pid {
        Some(_) => !pid_alive,
        None => lock_age >= STALE_AFTER,
    }
}

/// The owner file's `pid`, or `None` when there is no such file, it is not a plain file, it is
/// too large, or it does not parse.
fn owner_pid(path: &Path) -> Option<u32> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > OWNER_MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(OWNER_MAX_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice::<Owner>(&bytes).ok()?.pid
}

fn age(meta: &std::fs::Metadata, now: SystemTime) -> Duration {
    meta.modified()
        .ok()
        .and_then(|m| now.duration_since(m).ok())
        .unwrap_or_default()
}

/// Remove the sign-in lock in `dir` if it is abandoned ([`stale`]). `pid_alive` says whether a
/// process is running. Answers whether a lock was removed.
pub fn heal(dir: &Path, now: SystemTime, pid_alive: &dyn Fn(u32) -> bool) -> std::io::Result<bool> {
    let lock = dir.join(LOCK_DIR);
    let Ok(meta) = std::fs::symlink_metadata(&lock) else {
        return Ok(false);
    };
    if !meta.is_dir() {
        return Ok(false);
    }
    let owner = dir.join(OWNER_FILE);
    let pid = owner_pid(&owner);
    if !stale(pid, age(&meta, now), pid.is_some_and(pid_alive)) {
        return Ok(false);
    }
    // Another process may have taken the lock since the look: only the same owner is removed.
    if owner_pid(&owner) != pid {
        return Ok(false);
    }
    std::fs::remove_dir_all(&lock)?;
    match std::fs::remove_file(&owner) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        let half_written = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(OWNER_TMP_PREFIX));
        let Ok(m) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if half_written && m.is_file() && age(&m, now) >= STALE_AFTER {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_stale_only_when_its_owner_is_gone_or_unknown_and_old() {
        let min = Duration::from_secs(60);
        for (pid, age, alive, want) in [
            (Some(42), Duration::ZERO, false, true),
            (Some(42), 10 * min, true, false),
            (Some(42), Duration::ZERO, true, false),
            (None, 2 * min, false, false),
            (None, 3 * min, false, true),
            (None, 30 * min, true, true),
        ] {
            assert_eq!(stale(pid, age, alive), want, "{pid:?} {age:?} {alive}");
        }
    }

    /// In a temp folder, never the real config folder: a live owner's lock stays, a dead
    /// owner's lock, its owner file and old half-written owner files go, an ownerless lock goes
    /// only once it is old, and nothing else in the folder is touched.
    #[test]
    fn an_abandoned_lock_is_removed_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let lock = dir.join(LOCK_DIR);
        let owner = dir.join(OWNER_FILE);
        let other = dir.join("settings.json");
        let now = SystemTime::now();
        let old = now - Duration::from_secs(10 * 60);
        let set_up = |pid: Option<u32>| {
            std::fs::create_dir_all(&lock).unwrap();
            match pid {
                Some(p) => std::fs::write(
                    &owner,
                    format!(r#"{{"pid": {p}, "procStart": "x", "other": "ignored"}}"#),
                )
                .unwrap(),
                None => {
                    let _ = std::fs::remove_file(&owner);
                }
            }
        };
        std::fs::write(&other, "{}").unwrap();
        let live = |p: u32| p == 7;
        // A running owner: untouched, however old.
        set_up(Some(7));
        assert!(!heal(dir, now + 60 * STALE_AFTER, &live).unwrap());
        assert!(lock.is_dir() && owner.is_file());
        // A gone owner: the lock, its owner file and an old half-written one go; a fresh
        // half-written one stays (a refresh may be writing it right now).
        set_up(Some(9));
        let tmp_old = dir.join(format!("{OWNER_TMP_PREFIX}1"));
        let tmp_new = dir.join(format!("{OWNER_TMP_PREFIX}2"));
        std::fs::write(&tmp_old, "{}").unwrap();
        std::fs::write(&tmp_new, "{}").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&tmp_old)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(heal(dir, now, &live).unwrap());
        assert!(!lock.exists() && !owner.exists() && !tmp_old.exists());
        assert!(tmp_new.exists());
        // No owner to ask: kept while young, removed once old. An owner file too large to read
        // counts as none.
        set_up(None);
        assert!(!heal(dir, now, &live).unwrap());
        assert!(heal(dir, now + 2 * STALE_AFTER, &live).unwrap());
        set_up(None);
        std::fs::write(&owner, " ".repeat(OWNER_MAX_BYTES as usize + 1)).unwrap();
        assert!(!heal(dir, now, &live).unwrap(), "unreadable and young");
        // No lock at all: nothing to do.
        let _ = std::fs::remove_dir_all(&lock);
        assert!(!heal(dir, now + 60 * STALE_AFTER, &live).unwrap());
        assert!(other.is_file(), "nothing else in the folder is touched");
    }
}
