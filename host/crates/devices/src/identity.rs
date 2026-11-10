//! The `device_self` table: one row holding this install's device id and name.
//!
//! The id is 128 random bits, hex, made once and never changed. The name starts as the computer's
//! name in title case (`DESKTOP-AB12` reads `Desktop-Ab12`) and the person may rename it.

use helm_core::hidden;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

pub const MAX_NAME_CHARS: usize = 40;
/// The name used when the computer has none that survives screening.
pub const FALLBACK_NAME: &str = "This PC";

#[derive(Debug, PartialEq)]
pub enum DeviceError {
    Bad(String),
    Db(String),
}

impl DeviceError {
    pub fn message(&self) -> String {
        match self {
            DeviceError::Bad(m) | DeviceError::Db(m) => m.clone(),
        }
    }
}

impl From<rusqlite::Error> for DeviceError {
    fn from(e: rusqlite::Error) -> Self {
        DeviceError::Db(e.to_string())
    }
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS device_self (
             uid         TEXT PRIMARY KEY,
             name        TEXT NOT NULL,
             created_at  INTEGER NOT NULL
         );",
    )
}

/// Make the one row if it is missing. Registered as a start-up step, so the row exists before
/// anything asks for the id.
pub fn ensure_self(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    ensure(tx).map(|_| ())
}

/// This device's `(uid, name)`, made first if there is none yet.
pub fn ensure(tx: &Transaction<'_>) -> rusqlite::Result<(String, String)> {
    if let Some(row) = read(tx)? {
        return Ok(row);
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| rusqlite::Error::InvalidParameterName(format!("no random bytes: {e}")))?;
    let uid = hex::encode(bytes);
    let name = default_name();
    tx.execute(
        "INSERT INTO device_self(uid, name, created_at) VALUES (?1, ?2, ?3)",
        params![uid, name, helm_core::now_ms()],
    )?;
    Ok((uid, name))
}

fn read(conn: &Connection) -> rusqlite::Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT uid, name FROM device_self ORDER BY created_at, rowid LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// This device's id; `None` before the row is made (or without the table).
pub fn self_uid(conn: &Connection) -> Option<String> {
    read(conn).ok().flatten().map(|r| r.0)
}

/// This device's name; `None` before the row is made (or without the table).
pub fn self_name(conn: &Connection) -> Option<String> {
    read(conn).ok().flatten().map(|r| r.1)
}

/// Check a name a person typed: 1 to 40 characters, none invisible or controlling.
pub fn clean_name(raw: &str) -> Result<String, DeviceError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS || hidden::has_hidden(name) {
        return Err(DeviceError::Bad(
            "A device's name is 1 to 40 characters.".into(),
        ));
    }
    Ok(name.to_owned())
}

/// Rename this device and answer with its `(uid, name)`.
pub fn rename(tx: &Transaction<'_>, raw: &str) -> Result<(String, String), DeviceError> {
    let name = clean_name(raw)?;
    let (uid, _) = ensure(tx)?;
    tx.execute(
        "UPDATE device_self SET name = ?2 WHERE uid = ?1",
        params![uid, name],
    )?;
    Ok((uid, name))
}

/// The computer's own name, title-cased and screened.
fn default_name() -> String {
    let raw = if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else {
        std::env::var("HOSTNAME")
            .ok()
            .filter(|h| !h.trim().is_empty())
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
    };
    name_from_host(raw.as_deref().unwrap_or(""))
}

/// Title-case each word (words end at a space, hyphen, underscore or dot), drop invisible
/// characters and cap the length; fall back to [`FALLBACK_NAME`] when nothing is left.
pub fn name_from_host(raw: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for c in raw.trim().chars() {
        if c.is_control() || hidden::is_hidden_char(c) {
            continue;
        }
        if matches!(c, ' ' | '-' | '_' | '.') {
            start = true;
            out.push(c);
        } else if start {
            out.extend(c.to_uppercase());
            start = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    let out: String = out.trim().chars().take(MAX_NAME_CHARS).collect();
    if out.is_empty() {
        FALLBACK_NAME.to_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    fn ids(db: &Database) -> (Option<String>, Option<String>) {
        db.snapshot(|tx| Ok::<_, rusqlite::Error>((self_uid(tx), self_name(tx))))
            .unwrap()
    }

    #[test]
    fn the_device_is_made_once_and_names_are_cased_and_screened() {
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sqlite3");
        let db = Database::open(&path, &reg).unwrap();
        let (uid, name) = ids(&db);
        let uid = uid.expect("made at open");
        assert_eq!(uid.len(), 32);
        assert!(uid.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!name.unwrap().is_empty());

        // Renaming keeps the id; a bad name changes nothing; reopening keeps both.
        let (same, renamed) = db.write(|tx| rename(tx, "  Desk  ")).unwrap();
        assert_eq!((same.as_str(), renamed.as_str()), (uid.as_str(), "Desk"));
        let long = "x".repeat(41);
        for bad in ["", "   ", "a\u{202e}b", "tab\there", long.as_str()] {
            assert!(db.write(|tx| rename(tx, bad)).is_err(), "{bad:?}");
        }
        drop(db);
        let db = Database::open(&path, &reg).unwrap();
        assert_eq!(ids(&db), (Some(uid), Some("Desk".to_owned())));

        // The computer's name, title-cased, screened, capped, with a fallback.
        for (raw, want) in [
            ("LAPTOP", "Laptop"),
            ("desktop-AB12", "Desktop-Ab12"),
            ("my_pc name", "My_Pc Name"),
            ("PC\u{200b}1\u{202e}", "Pc1"),
            ("  ", FALLBACK_NAME),
            ("", FALLBACK_NAME),
        ] {
            assert_eq!(name_from_host(raw), want, "{raw:?}");
        }
        assert_eq!(
            name_from_host(&"A".repeat(60)).chars().count(),
            MAX_NAME_CHARS
        );
    }
}
