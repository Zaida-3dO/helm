//! The people this PC acts for, and where each one's channel files live.
//!
//! Every linked profile has its own folder, `relay/people/<profileId>/`, holding its Telegram
//! tag, policy and notes and its voice tag, policy and heard notes. The hub's frames name the
//! hub person they act for; a frame's person resolves to the profile linked to them, and a
//! frame that names no person (an older hub) is the home profile's.
//!
//! The first builds kept one set of files straight under `relay/`, for the one person a PC
//! served. On the first use of the new layout those files move into the home profile's folder,
//! once; nothing else moves and the current Telegram and voice chats keep going.

use crate::telegram;
use crate::voice;
use crate::voice_heard;
use helm_core::db::Database;
use helm_profiles::store;
use std::path::{Path, PathBuf};

/// The folder of one profile's channel files.
pub fn person_dir(profile_dir: &Path, profile_id: &str) -> PathBuf {
    profile_dir.join("relay").join("people").join(profile_id)
}

/// The files the first build kept straight under `relay/`, by their old names.
const LEGACY: [&str; 7] = [
    "telegram-chat.json",
    "telegram-policy.json",
    "telegram-answered.json",
    "telegram-sent.json",
    "voice-chat.json",
    "voice-policy.json",
    "voice-heard.json",
];

/// Move the single-person files into the home profile's folder, once per run and at most once
/// on disk: each file moves only when its place in the new layout is still free. A file with
/// no home profile to move into stays where it is and is tried again on the next use.
pub fn migrate(profile_dir: &Path, db: &Database) {
    static DONE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    let done = DONE.get_or_init(std::sync::Mutex::default);
    if done
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(profile_dir)
    {
        return;
    }
    let home: Option<String> = db
        .snapshot(|tx| store::home(tx))
        .ok()
        .flatten()
        .map(|p| p.id);
    let Some(home) = home else {
        return;
    };
    done.lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(profile_dir.to_path_buf());
    let to = person_dir(profile_dir, &home);
    let relay = profile_dir.join("relay");
    for name in LEGACY {
        let from = relay.join(name);
        if !from.is_file() {
            continue;
        }
        let dest = to.join(name);
        if dest.exists() {
            continue;
        }
        if let Some(dir) = dest.parent() {
            if helm_core::privfs::create_private_dir_all(dir).is_err() {
                continue;
            }
        }
        match std::fs::rename(&from, &dest) {
            Ok(()) => log::info!("relay: moved the {name} file into the home profile's folder"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => log::warn!("relay: a channel file could not be moved to the home profile"),
        }
    }
    // The attachment folder: the pictures and files of the old single Telegram chat belong to
    // the home profile now. The new folder sits inside the old one (`telegram/<id>`), so the
    // old entries move into it one by one rather than the folder being renamed whole.
    let old_att = profile_dir.join("telegram");
    let new_att = telegram::attachments_dir(profile_dir, &home);
    if old_att.is_dir() && std::fs::create_dir_all(&new_att).is_ok() {
        if let Ok(entries) = std::fs::read_dir(&old_att) {
            for entry in entries.flatten() {
                let from = entry.path();
                // Only the old layout's plain files move; every folder here is another
                // profile's own attachment folder (or the home's new one) and stays.
                if from == new_att || !from.is_file() {
                    continue;
                }
                match std::fs::rename(&from, new_att.join(entry.file_name())) {
                    Ok(()) => {}
                    Err(_) => log::warn!("relay: an attachment could not be moved"),
                }
            }
        }
    }
}

/// The Telegram files of one profile.
pub fn telegram_paths(profile_dir: &Path, profile_id: &str) -> telegram::Paths {
    let dir = person_dir(profile_dir, profile_id);
    telegram::Paths {
        tag: dir.join("telegram-chat.json"),
        policy: dir.join("telegram-policy.json"),
        answered: dir.join("telegram-answered.json"),
        sent: dir.join("telegram-sent.json"),
        attachments: telegram::attachments_dir(profile_dir, profile_id),
    }
}

/// The voice files of one profile.
pub fn voice_paths(profile_dir: &Path, profile_id: &str) -> voice::Paths {
    let dir = person_dir(profile_dir, profile_id);
    voice::Paths {
        tag: dir.join("voice-chat.json"),
        policy: dir.join("voice-policy.json"),
        heard: voice_heard::path_in(&dir),
        folder: voice::folder(profile_dir),
    }
}

/// The profile the channel files for a hub person belong to. `None` (or nobody linked) is the
/// home profile — the one an older hub's unnamed frames are for. `Err` when the person names
/// no profile here: the caller must not use that person's files.
pub fn profile_for(
    db: &Database,
    person: Option<&str>,
) -> Result<helm_profiles::store::Profile, String> {
    match person.map(str::trim).filter(|p| !p.is_empty()) {
        None => db
            .snapshot(|tx| store::home(tx))
            .ok()
            .flatten()
            .ok_or_else(|| "This Helm has no home profile yet.".to_owned()),
        Some(p) => db
            .snapshot(|tx| store::profile_for_person(tx, p))
            .ok()
            .flatten()
            .ok_or_else(|| "This PC has no profile for that person.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;

    fn db_in(dir: &Path) -> helm_core::db::Database {
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        helm_profiles::register_migrations(&mut reg);
        helm_core::db::Database::open(&dir.join("t.db"), &reg).unwrap()
    }

    #[test]
    fn the_single_person_files_move_into_the_home_folder_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(dir.path());
        let prof = dir.path().join("prof");
        let relay = prof.join("relay");
        std::fs::create_dir_all(&relay).unwrap();
        for name in LEGACY {
            std::fs::write(relay.join(name), "x").unwrap();
        }
        let old_att = prof.join("telegram");
        std::fs::create_dir_all(&old_att).unwrap();
        std::fs::write(old_att.join("pic.png"), "p").unwrap();
        // Another profile's attachment folder must stay put on a later start.
        let other = old_att.join("other-profile");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("img.png"), "o").unwrap();
        migrate(&prof, &db);
        migrate(&prof, &db); // second call: nothing left to do, nothing moved twice
        let home = db.snapshot(|tx| store::home(tx)).unwrap().unwrap();
        let to = person_dir(&prof, &home.id);
        for name in LEGACY {
            assert!(to.join(name).is_file(), "{name} did not move");
            assert!(!relay.join(name).exists(), "{name} was left behind");
        }
        let att = telegram::attachments_dir(&prof, &home.id);
        assert!(
            att.join("pic.png").is_file(),
            "the attachments did not move"
        );
        assert!(other.join("img.png").is_file(), "another folder was moved");
        assert!(
            !old_att.join("pic.png").exists(),
            "left behind in the old folder"
        );
    }

    #[test]
    fn a_frames_person_resolves_to_its_profile_or_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(dir.path());
        let second = db
            .write(|tx| -> Result<String, helm_profiles::store::ProfileError> {
                let home = store::home(tx)?.expect("made at open");
                store::update(
                    tx,
                    &home.id,
                    helm_profiles::store::Patch {
                        hub_person_id: Some(Some("7".into())),
                        ..Default::default()
                    },
                )?;
                Ok(store::create(tx, "Second", None, Some("9"), 2)?.id)
            })
            .unwrap();
        let home = db.snapshot(|tx| store::home(tx)).unwrap().unwrap();
        assert_eq!(profile_for(&db, None).unwrap().id, home.id);
        assert_eq!(profile_for(&db, Some("7")).unwrap().id, home.id);
        assert_eq!(profile_for(&db, Some("9")).unwrap().id, second);
        assert!(profile_for(&db, Some("nope")).is_err());
    }
}
