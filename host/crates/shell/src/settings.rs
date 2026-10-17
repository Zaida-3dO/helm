//! Reading and writing the shell's preferences. Every key but `keepAwake` is user-only (only the
//! user, in Settings → Shell); the classes are in `helm_core::prefs::classes`.

use crate::types::{MicChoice, NotificationMode, ShellSettings, TerminalShell, DEFAULT_HOTKEY};
use helm_core::prefs::{self, PrefError, PrefKey};
use rusqlite::{Connection, Transaction};
use serde_json::{json, Value};

pub const GLOBAL_HOTKEY: PrefKey = PrefKey::classified("globalHotkey");
pub const KEEP_AWAKE: PrefKey = PrefKey::classified("keepAwake");
pub const NOTIFICATIONS: PrefKey = PrefKey::classified("notifications");
pub const DICTATION_MIC: PrefKey = PrefKey::classified("dictationMic");
pub const TERMINAL_SHELL: PrefKey = PrefKey::classified("terminalShell");

/// The stored settings; a missing or malformed row reads as its default.
pub fn load(conn: &Connection) -> ShellSettings {
    let d = ShellSettings::default();
    let hotkey = match prefs::read(conn, GLOBAL_HOTKEY.name()) {
        // An empty string is "cleared" (a null value would delete the row, which reads as the
        // default chord).
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => normalise_chord(&s).ok().or(d.global_hotkey.clone()),
        _ => d.global_hotkey.clone(),
    };
    ShellSettings {
        global_hotkey: hotkey,
        keep_awake: prefs::get_or(conn, KEEP_AWAKE, true),
        notifications: prefs::get_or(conn, NOTIFICATIONS, NotificationMode::InApp),
        dictation_mic: prefs::read(conn, DICTATION_MIC.name())
            .and_then(|v| serde_json::from_value::<MicChoice>(v).ok())
            .filter(valid_mic),
        terminal_shell: prefs::get_or(conn, TERMINAL_SHELL, TerminalShell::Pwsh),
    }
}

fn valid_mic(m: &MicChoice) -> bool {
    !m.uid.is_empty() && m.uid.len() <= 512 && m.name.len() <= 256
}

/// Validate and store. Returns the settings as stored (the chord normalised).
pub fn save(tx: &Transaction<'_>, s: &ShellSettings) -> Result<ShellSettings, String> {
    let mut out = s.clone();
    let hotkey = match &s.global_hotkey {
        None => json!(""),
        Some(c) => {
            let n = normalise_chord(c)?;
            out.global_hotkey = Some(n.clone());
            json!(n)
        }
    };
    if let Some(m) = &s.dictation_mic {
        if !valid_mic(m) {
            return Err(
                "the microphone uid must be 1 to 512 bytes and its name at most 256".into(),
            );
        }
    }
    let sql = |e: PrefError| e.to_string();
    prefs::set(tx, GLOBAL_HOTKEY, &hotkey).map_err(sql)?;
    prefs::set(tx, KEEP_AWAKE, &json!(s.keep_awake)).map_err(sql)?;
    prefs::set(
        tx,
        NOTIFICATIONS,
        &serde_json::to_value(s.notifications).unwrap_or(Value::Null),
    )
    .map_err(sql)?;
    let mic = s
        .dictation_mic
        .as_ref()
        .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    prefs::set(tx, DICTATION_MIC, &mic).map_err(sql)?;
    prefs::set(
        tx,
        TERMINAL_SHELL,
        &serde_json::to_value(s.terminal_shell).unwrap_or(Value::Null),
    )
    .map_err(sql)?;
    Ok(out)
}

const MODIFIERS: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];

/// "ctrl + alt + h" → "Ctrl+Alt+H". A chord needs at least one modifier (F1-F24 may stand alone),
/// one key (A-Z, 0-9, F1-F24, Space, or a backquote), and is at most 64 bytes.
pub fn normalise_chord(chord: &str) -> Result<String, String> {
    let bad = |why: &str| Err(format!("hotkey {chord:?}: {why}"));
    if chord.len() > 64 {
        return bad("is too long");
    }
    let parts: Vec<&str> = chord
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let Some((key, mods)) = parts.split_last() else {
        return bad("is empty");
    };
    let mut have = [false; 4];
    for m in mods {
        let i = match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmdorctrl" => 0,
            "alt" | "option" => 1,
            "shift" => 2,
            "super" | "win" | "meta" => 3,
            _ => return bad("has an unknown modifier"),
        };
        have[i] = true;
    }
    let k = key.to_ascii_uppercase();
    let fkey = k.len() >= 2
        && k.starts_with('F')
        && k[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n));
    let key = if (k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric())) || fkey {
        k
    } else if k == "SPACE" {
        "Space".to_owned()
    } else if k == "`" || k == "BACKQUOTE" {
        "`".to_owned()
    } else {
        return bad("has an unsupported key");
    };
    if !have.iter().any(|h| *h) && !fkey {
        return bad("needs Ctrl, Alt, Shift or Super");
    }
    let mut out: Vec<String> = MODIFIERS
        .iter()
        .zip(have)
        .filter(|(_, h)| *h)
        .map(|(m, _)| (*m).to_owned())
        .collect();
    out.push(key);
    Ok(out.join("+"))
}

/// The default chord, for callers that need it as text.
pub fn default_hotkey() -> &'static str {
    DEFAULT_HOTKEY
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    fn temp_db(reg: &MigrationRegistry) -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), reg).unwrap();
        (dir, db)
    }

    #[test]
    fn chords_normalise_and_bad_ones_are_refused() {
        assert_eq!(normalise_chord("ctrl + alt + h").unwrap(), "Ctrl+Alt+H");
        assert_eq!(normalise_chord("Shift+Ctrl+1").unwrap(), "Ctrl+Shift+1");
        assert_eq!(normalise_chord("F9").unwrap(), "F9");
        assert_eq!(normalise_chord("Win+space").unwrap(), "Super+Space");
        for bad in ["H", "Ctrl+", "Hyper+H", "Ctrl+Enter", "", "Ctrl+F25"] {
            assert!(normalise_chord(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn defaults_when_nothing_is_stored() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let s = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(load(tx)))
            .unwrap();
        assert_eq!(s, ShellSettings::default());
        assert_eq!(s.global_hotkey.as_deref(), Some("Ctrl+Alt+H"));
        assert!(s.keep_awake);
    }

    #[test]
    fn round_trip_including_a_cleared_hotkey_and_a_mic() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let want = ShellSettings {
            global_hotkey: None,
            keep_awake: false,
            notifications: NotificationMode::Native,
            dictation_mic: Some(MicChoice {
                uid: "{0.0.1}.{abc}".into(),
                name: "USB mic".into(),
            }),
            terminal_shell: TerminalShell::Gitbash,
        };
        db.write(|tx| save(tx, &want).map_err(|_| rusqlite::Error::InvalidQuery))
            .unwrap();
        let got = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(load(tx)))
            .unwrap();
        assert_eq!(got, want);
        // Back to the default mic deletes the row.
        let back = ShellSettings {
            dictation_mic: None,
            ..want
        };
        db.write(|tx| save(tx, &back).map_err(|_| rusqlite::Error::InvalidQuery))
            .unwrap();
        let got = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(load(tx)))
            .unwrap();
        assert_eq!(got.dictation_mic, None);
    }

    #[test]
    fn save_normalises_the_chord_and_refuses_a_bad_one() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        let s = ShellSettings {
            global_hotkey: Some("alt+ctrl+j".into()),
            ..Default::default()
        };
        let stored = db
            .write(|tx| save(tx, &s).map_err(|_| rusqlite::Error::InvalidQuery))
            .unwrap();
        assert_eq!(stored.global_hotkey.as_deref(), Some("Ctrl+Alt+J"));
        let bad = ShellSettings {
            global_hotkey: Some("J".into()),
            ..Default::default()
        };
        assert!(db
            .write(|tx| Ok::<_, rusqlite::Error>(save(tx, &bad)))
            .unwrap()
            .is_err());
    }
}
