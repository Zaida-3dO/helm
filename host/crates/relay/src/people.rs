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
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let home: Option<String> = db
        .snapshot(|tx| store::home(tx).ok().flatten())
        .ok()
        .flatten()
        .map(|p| p.id);
    let Some(home) = home else {
        return;
    };
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
    // the home profile now. It is renamed whole, and only when the new one is not taken.
    let old_att = profile_dir.join("telegram");
    let new_att = telegram::attachments_dir(profile_dir, &home);
    if old_att.is_dir() && !new_att.exists() {
        if let Some(parent) = new_att.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::rename(&old_att, &new_att);
    }
    DONE.store(true, std::sync::atomic::Ordering::SeqCst);
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
            .snapshot(|tx| store::home(tx).ok().flatten())
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
