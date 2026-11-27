//! The people this PC acts for, and where each one's channel files live.
//!
//! Every linked profile has its own folder, `relay/people/<profileId>/`, holding its Telegram
//! tag, policy and notes and its voice tag, policy and heard notes. The hub's frames name the
//! hub person they act for; a frame's person resolves to the profile linked to them, and a
//! frame that names no person (an older hub) is the home profile's.
//!
//! The first builds kept one set of files straight under `relay/`, for the one person a PC
//! served. On the first use of the new layout those files move into the home profile's folder,
//! once; nothing else moves and the current Telegram and voice chats keep going (the old
//! Telegram chat keeps its own working folder, files and subfolders alike).

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
}

/// What the hub's welcome does to the home profile. It takes the person only when it is not
/// linked to anyone yet, the person is not declined and no other profile here already holds
/// them. A home profile linked to someone else keeps that link (and a warning is logged), so a
/// wrong token or a reassigned machine never mixes two people's rules and memories, and
/// nobody is taken from another profile or declined. `Ok(true)` when the link changed.
pub fn home_link(
    tx: &rusqlite::Transaction<'_>,
    person: &str,
    name: &str,
) -> Result<bool, store::ProfileError> {
    if store::is_declined(tx, person)? {
        return Ok(false);
    }
    let home_person = store::home(tx)?.and_then(|h| h.hub_person_id);
    match home_person {
        Some(h) if h == person => return Ok(false),
        Some(_) => {
            log::warn!("relay: the hub's welcome names a different person than the home profile's");
            return Ok(false);
        }
        None => {}
    }
    if store::profile_for_person(tx, person)?.is_some() {
        return Ok(false);
    }
    store::link_home_to_person(tx, person, name, helm_core::now_ms())?;
    Ok(true)
}

/// The Telegram files of one profile.
pub fn telegram_paths(profile_dir: &Path, profile_id: &str) -> telegram::Paths {
    let dir = person_dir(profile_dir, profile_id);
    telegram::Paths {
        tag: dir.join("telegram-chat.json"),
        policy: dir.join("telegram-policy.json"),
        answered: dir.join("telegram-answered.json"),
        sent: dir.join("telegram-sent.json"),
        folder: telegram::chat_folder(profile_dir, profile_id),
    }
}

/// Whether this chat is a live voice conversation for the profile right now — the phone app's
/// deep lane or its quick lane (each tag file names one chat), or one of the room speaker's
/// talks: open ones, and the ones kept past their close with something still held in them. An
/// ask waiting in such a chat belongs on the phone, not on this PC's screen; both places that
/// route asks read this one answer.
pub fn voice_chat_here(profile_dir: &Path, profile_id: &str, chat: &str) -> bool {
    let vp = voice_paths(profile_dir, profile_id);
    if [vp.tag.as_path(), vp.tag_quick.as_path()]
        .iter()
        .any(|p| voice::load_tag(p).is_some_and(|t| t.chat_id == chat))
    {
        return true;
    }
    let talks = crate::speaker::load_talks(&crate::speaker::paths(profile_dir, profile_id).chats);
    talks.talks.iter().any(|t| t.chat_id == chat) || talks.kept.iter().any(|k| k.chat_id == chat)
}

/// The voice files of one profile.
pub fn voice_paths(profile_dir: &Path, profile_id: &str) -> voice::Paths {
    let dir = person_dir(profile_dir, profile_id);
    voice::Paths {
        tag: dir.join("voice-chat.json"),
        tag_quick: dir.join("voice-chat-quick.json"),
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
        // The old Telegram chat's working folder is left exactly as it is.
        let old_dir = prof.join("telegram");
        std::fs::create_dir_all(old_dir.join("project")).unwrap();
        std::fs::write(old_dir.join("notes.txt"), "p").unwrap();
        std::fs::write(old_dir.join("project").join("a.txt"), "o").unwrap();
        migrate(&prof, &db);
        migrate(&prof, &db); // second call: nothing left to do, nothing moved twice
        let home = db.snapshot(|tx| store::home(tx)).unwrap().unwrap();
        let to = person_dir(&prof, &home.id);
        for name in LEGACY {
            assert!(to.join(name).is_file(), "{name} did not move");
            assert!(!relay.join(name).exists(), "{name} was left behind");
        }
        assert!(
            old_dir.join("notes.txt").is_file(),
            "the old folder's file moved"
        );
        assert!(old_dir.join("project").join("a.txt").is_file());
    }

    #[test]
    fn the_welcome_links_only_an_unlinked_home_and_takes_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(dir.path());
        let run = |p: &str, n: &str| db.write(|tx| home_link(tx, p, n)).unwrap_or(false);
        let home_person = || {
            db.snapshot(|tx| store::home(tx))
                .unwrap()
                .unwrap()
                .hub_person_id
        };
        // Held by another profile: not stolen, home stays unlinked.
        db.write(|tx| store::create(tx, "Other", None, Some("5"), 2).map(|_| ()))
            .unwrap();
        assert!(!run("5", "Pat"));
        assert_eq!(home_person(), None);
        // Empty and over-long ids change nothing.
        assert!(!run("", "Pat"));
        assert!(!run(&"9".repeat(300), "Pat"));
        assert_eq!(home_person(), None);
        // Declined: skipped.
        db.write(|tx| tx.execute("INSERT INTO declined_people(person_id) VALUES ('6')", []))
            .unwrap();
        assert!(!run("6", "Pat"));
        assert_eq!(home_person(), None);
        // Unlinked home links; the same person again, and a different one, change nothing.
        assert!(run("3", "Pat"));
        assert_eq!(home_person().as_deref(), Some("3"));
        assert!(!run("3", "Pat"));
        assert!(!run("4", "Sam"));
        assert_eq!(home_person().as_deref(), Some("3"));
        assert!(!db.snapshot(|tx| store::is_declined(tx, "3")).unwrap());
        assert_eq!(
            db.snapshot(|tx| store::profile_for_person(tx, "5"))
                .unwrap()
                .unwrap()
                .name,
            "Other"
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
