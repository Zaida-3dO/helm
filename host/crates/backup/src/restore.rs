//! Queuing a restore, and applying it at the next start.
//!
//! A restore replaces the chats, the settings and the approvals, so it is refuse-by-default: the
//! caller must type the confirmation phrase, and staging changes nothing but one small note in the
//! profile. [`apply_pending`] runs during start-up, after the profile lock is held and before the
//! database is opened, and it keeps the outgoing database in a dated safety folder.

use crate::archive::{copy_tree, is_excluded, read_manifest};
use crate::{inside, stamp, BackupError};
use helm_core::profile::DB_FILE;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The phrase the host demands, and the UI puts on the confirm button. Compared trimmed and
/// lower-cased; it must name what is being replaced, in plain words.
pub const CONFIRM_TEXT: &str = "replace chats and settings";
/// The note a staged restore leaves in the profile.
pub const PENDING_FILE: &str = "restore-pending.json";
/// Where a note that could not be applied is set aside, for a person to read.
pub const FAILED_FILE: &str = "restore-failed.json";
/// Prefix of the dated folder that keeps the outgoing database.
pub const SAFETY_PREFIX: &str = "before-restore-";

#[derive(Serialize, Deserialize)]
struct Pending {
    archive_dir: String,
    #[allow(dead_code)]
    staged_at: i64,
}

/// Validate the request and leave the note. The archive must be a readable backup, outside the
/// profile; the confirmation must match; and no restore may already wait.
pub fn prepare(profile_dir: &Path, archive_dir: &Path, confirm: &str) -> Result<(), BackupError> {
    if confirm.trim().to_lowercase() != CONFIRM_TEXT {
        return Err(BackupError::WrongConfirm);
    }
    read_manifest(archive_dir)?;
    if inside(profile_dir, archive_dir) {
        return Err(BackupError::InsideProfile);
    }
    let pending_path = profile_dir.join(PENDING_FILE);
    if pending_path.exists() {
        return Err(BackupError::AlreadyStaged);
    }
    let note = Pending {
        archive_dir: archive_dir.to_string_lossy().into_owned(),
        staged_at: helm_core::now_ms(),
    };
    let text = serde_json::to_vec(&note).map_err(|e| BackupError::Io(std::io::Error::other(e)))?;
    std::fs::write(&pending_path, text)?;
    log::info!(
        "a restore is staged from {}; it applies the next time Helm starts",
        archive_dir.display()
    );
    Ok(())
}

/// The archive a pending restore names, for the list to show that one waits.
pub fn pending_archive(profile_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(profile_dir.join(PENDING_FILE)).ok()?;
    serde_json::from_str::<Pending>(&text)
        .ok()
        .filter(|p| !p.archive_dir.is_empty())
        .map(|p| p.archive_dir)
}

pub struct Applied {
    pub archive: String,
    pub safety_dir: PathBuf,
    pub entries: i64,
}

/// The start-up half: if a restore waits, do it now, before anything has opened the profile.
/// An unreadable note or a missing archive is set aside to [`FAILED_FILE`] and start proceeds
/// normally; a failure part-way through is an error, because half a profile must not start.
pub fn apply_pending(profile_dir: &Path) -> Result<Option<Applied>, BackupError> {
    let pending_path = profile_dir.join(PENDING_FILE);
    let Ok(text) = std::fs::read_to_string(&pending_path) else {
        return Ok(None);
    };
    let archive = match serde_json::from_str::<Pending>(&text) {
        Ok(p) if !p.archive_dir.is_empty() => p.archive_dir,
        _ => {
            set_aside(profile_dir);
            log::error!(
                "the staged restore note could not be read; it was set aside as {FAILED_FILE}"
            );
            return Ok(None);
        }
    };
    let archive_dir = PathBuf::from(&archive);
    let manifest = match read_manifest(&archive_dir) {
        Ok(m) => m,
        Err(_) => {
            set_aside(profile_dir);
            log::error!(
                "the staged restore archive {} is not a readable backup; the note was set aside",
                archive_dir.display()
            );
            return Ok(None);
        }
    };

    // The database first: today's file and its sidecars go into a dated folder together, and every
    // other entry the restore replaces is moved in beside them below, so a restore can be undone by
    // hand whatever happens after this point.
    let safety_dir = profile_dir.join(format!("{}{}", SAFETY_PREFIX, stamp(helm_core::now_ms())));
    std::fs::create_dir_all(&safety_dir)?;
    for side in [
        DB_FILE.to_owned(),
        format!("{DB_FILE}-wal"),
        format!("{DB_FILE}-shm"),
    ] {
        let from = profile_dir.join(&side);
        if from.exists() {
            std::fs::rename(&from, safety_dir.join(&side))?;
        }
    }

    // Then the rest, exactly as the manifest names it — the exclusion list outranks the manifest,
    // so not even a crafted one can name the credential folder or a bookkeeping file, and an
    // entry with a path separator is skipped rather than followed.
    let mut entries = 0_i64;
    for name in &manifest.entries {
        if is_excluded(name) || name.contains('/') || name.contains('\\') || name == ".." {
            log::warn!("a staged restore skipped the entry {name:?}");
            continue;
        }
        let target = profile_dir.join(name);
        set_aside_entry(&target, &safety_dir.join(name))?;
        let source = archive_dir.join(name);
        if source.exists() {
            copy_tree(&source, &target)?;
        }
        entries += 1;
    }

    std::fs::copy(archive_dir.join(DB_FILE), profile_dir.join(DB_FILE))?;
    std::fs::remove_file(&pending_path)?;
    log::info!(
        "the staged restore from {archive} was applied; what it replaced is in {}",
        safety_dir.display()
    );
    Ok(Some(Applied {
        archive,
        safety_dir,
        entries,
    }))
}

fn set_aside(profile_dir: &Path) {
    let _ = std::fs::remove_file(profile_dir.join(FAILED_FILE));
    let _ = std::fs::rename(
        profile_dir.join(PENDING_FILE),
        profile_dir.join(FAILED_FILE),
    );
}

/// Move a replaced entry into the safety folder (same volume, so it is a rename and cheap). Only if
/// the rename fails is the entry deleted, and that is logged: the restore must still go through.
fn set_aside_entry(from: &Path, to: &Path) -> Result<(), BackupError> {
    if std::fs::symlink_metadata(from).is_err() {
        return Ok(());
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) => {
            log::warn!(
                "could not keep {} in the safety folder ({e}); it is being deleted instead",
                from.display()
            );
            remove_target(from)
        }
    }
}

fn remove_target(p: &Path) -> Result<(), BackupError> {
    match std::fs::symlink_metadata(p) {
        Ok(md) if md.is_dir() => std::fs::remove_dir_all(p)?,
        Ok(md) if md.is_file() => std::fs::remove_file(p)?,
        // Anything else (a stray link) is left where it is rather than chased.
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive;
    use crate::testutil;

    #[test]
    fn a_restore_is_refused_unconfirmed_then_stages_and_applies_at_the_next_start() {
        let src = testutil::profile("keep me");
        let dest = tempfile::tempdir().unwrap();
        let created = archive::create(&src.db, src.dir.path(), dest.path(), "0.6.2", 1).unwrap();

        let target = testutil::profile("old data");
        std::fs::write(target.dir.path().join("sessions/only-old.txt"), "goes away").unwrap();
        // The fixture leaves a stale note to prove archives skip it; here the test stages its own.
        std::fs::remove_file(target.dir.path().join(PENDING_FILE)).unwrap();

        // Refused: the wrong phrase, a folder that is not a backup, and nothing has changed yet.
        assert!(matches!(
            prepare(target.dir.path(), &created.archive_dir, "yes"),
            Err(BackupError::WrongConfirm)
        ));
        assert!(matches!(
            prepare(target.dir.path(), &dest.path().join("nope"), CONFIRM_TEXT),
            Err(BackupError::NotABackup)
        ));
        assert!(pending_archive(target.dir.path()).is_none());
        assert_eq!(testutil::db_note(target.dir.path()), "old data");

        // Staged: the phrase is accepted whatever its case; a second stage is refused.
        prepare(
            target.dir.path(),
            &created.archive_dir,
            "Replace Chats and Settings",
        )
        .unwrap();
        assert_eq!(
            pending_archive(target.dir.path()).as_deref(),
            created.archive_dir.to_str()
        );
        assert!(matches!(
            prepare(target.dir.path(), &created.archive_dir, CONFIRM_TEXT),
            Err(BackupError::AlreadyStaged)
        ));
        // Staging alone changed nothing but the note.
        assert_eq!(testutil::db_note(target.dir.path()), "old data");

        // Applied at the next start: the profile now reads from the archive, and the database it
        // replaced sits in the dated safety folder.
        let applied = apply_pending(target.dir.path()).unwrap().unwrap();
        assert_eq!(applied.archive, created.archive_dir.to_string_lossy());
        assert_eq!(applied.entries, 3);
        assert_eq!(testutil::db_note(target.dir.path()), "keep me");
        assert!(target.dir.path().join("sessions/s1/log.jsonl").is_file());
        assert!(!target.dir.path().join("sessions/only-old.txt").exists());
        // The chat logs the restore replaced are kept too, not deleted.
        assert!(applied.safety_dir.join("sessions/only-old.txt").is_file());
        assert!(applied.safety_dir.join(DB_FILE).is_file());
        assert_eq!(testutil::db_note(&applied.safety_dir), "old data");
        // The note is gone; a second start has nothing to do.
        assert!(pending_archive(target.dir.path()).is_none());
        assert!(apply_pending(target.dir.path()).unwrap().is_none());
    }

    #[test]
    fn a_staged_restore_never_touches_credentials_and_a_missing_archive_is_set_aside() {
        let src = testutil::profile("keep me");
        let dest = tempfile::tempdir().unwrap();
        let created = archive::create(&src.db, src.dir.path(), dest.path(), "0.6.2", 1).unwrap();
        let target = testutil::profile("old data");

        // A crafted archive whose manifest tries to name the credential folder, a loose envelope
        // and a path that walks out of the profile.
        let evil = dest.path().join("helm-backup-evil");
        std::fs::create_dir_all(evil.join("credentials")).unwrap();
        std::fs::write(evil.join("credentials/payload.cred"), "not wanted").unwrap();
        std::fs::write(evil.join("evil.cred"), "not wanted").unwrap();
        std::fs::copy(created.archive_dir.join(DB_FILE), evil.join(DB_FILE)).unwrap();
        std::fs::write(
            evil.join(archive::MANIFEST_FILE),
            r#"{"format":1,"created_at":1,"app_version":"x","entries":["credentials","token.cred","sessions/../escape","attachments"]}"#,
        )
        .unwrap();
        let note = serde_json::json!({ "archive_dir": evil.to_string_lossy(), "staged_at": 2 });
        std::fs::write(
            target.dir.path().join(PENDING_FILE),
            serde_json::to_vec(&note).unwrap(),
        )
        .unwrap();

        let applied = apply_pending(target.dir.path()).unwrap().unwrap();
        // Only the one safe-looking entry was applied...
        assert_eq!(applied.entries, 1); // attachments: emptied, since the crafted archive has none
        assert!(!target.dir.path().join("attachments/a.json").exists());
        // ...and every dangerous entry left the profile exactly as it was.
        assert!(target.dir.path().join("sessions/s1/log.jsonl").is_file());
        assert!(target.dir.path().join("credentials/key.cred").is_file());
        assert_eq!(
            std::fs::read_to_string(target.dir.path().join("token.cred")).unwrap(),
            "another envelope"
        );
        assert!(!target.dir.path().join("evil.cred").exists());
        assert!(!target.dir.path().join("escape").exists());

        // A staged archive that went missing: start proceeds and the note is set aside, once.
        std::fs::create_dir_all(target.dir.path()).unwrap();
        let gone = serde_json::json!({ "archive_dir": dest.path().join("since-deleted").to_string_lossy(), "staged_at": 3 });
        std::fs::write(
            target.dir.path().join(PENDING_FILE),
            serde_json::to_vec(&gone).unwrap(),
        )
        .unwrap();
        assert!(apply_pending(target.dir.path()).unwrap().is_none());
        assert!(target.dir.path().join(FAILED_FILE).is_file());
        assert!(!target.dir.path().join(PENDING_FILE).exists());
    }
}
