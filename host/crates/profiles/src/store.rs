//! The two profile tables and the rules over them.
//!
//! - `profiles`: one row per person who uses this Helm. Exactly one row is the **home** profile:
//!   the person whose Claude sign-in this install runs under (the first profile made becomes it).
//!   A profile may be linked to a person known to the hub.
//! - `chat_profiles`: the label a chat carries. A chat with no row belongs to the home profile.
//!   The label filters what the sidebar shows; it is never a credential, so a chat labelled for
//!   anyone still runs here, under this computer's sign-in.
//!
//! A trigger on `chats` labels a new chat: a chat nested under another takes its parent's
//! label, any other chat takes the active profile (nothing is written for the home profile or
//! for "all", since an unlabelled chat is the home profile's).

use helm_core::prefs::{self, PrefError, PrefKey};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use std::collections::HashMap;

/// The profile the sidebar shows: a profile id, or [`ALL`].
pub const ACTIVE: PrefKey = PrefKey::classified("activeProfile");
pub const ALL: &str = "all";
pub const MAX_PROFILES: usize = 16;
pub const MAX_NAME_CHARS: usize = 40;
pub const MAX_PERSON_CHARS: usize = 128;
/// The most chats one assignment may move.
pub const MAX_ASSIGN: usize = 500;

/// Colours given to new profiles in turn.
pub const COLOURS: [&str; 8] = [
    "#6d8cff", "#ff8a65", "#4db6ac", "#ba68c8", "#ffd54f", "#81c784", "#f06292", "#90a4ae",
];

#[derive(Debug, PartialEq)]
pub enum ProfileError {
    Bad(String),
    NotFound,
    Db(String),
}

impl ProfileError {
    pub fn message(&self) -> String {
        match self {
            ProfileError::Bad(m) | ProfileError::Db(m) => m.clone(),
            ProfileError::NotFound => "There is no profile with that id.".into(),
        }
    }
}

impl From<rusqlite::Error> for ProfileError {
    fn from(e: rusqlite::Error) -> Self {
        ProfileError::Db(e.to_string())
    }
}

impl From<PrefError> for ProfileError {
    fn from(e: PrefError) -> Self {
        ProfileError::Db(e.to_string())
    }
}

fn bad<T>(m: &str) -> Result<T, ProfileError> {
    Err(ProfileError::Bad(m.into()))
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub colour: String,
    /// The hub's id for the person this profile is, when linked.
    pub hub_person_id: Option<String>,
    pub is_home: bool,
}

/// A profile with the counts the profile view and the popover show (not archived chats).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    #[serde(flatten)]
    pub profile: Profile,
    pub chats: i64,
    pub running: i64,
    pub attention: i64,
}

/// Everything the UI needs to draw the footer chip and the popover.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesView {
    pub profiles: Vec<ProfileSummary>,
    /// The home profile, or null before the first one is made (the first-run prompt shows).
    pub home_id: Option<String>,
    /// The profile the sidebar shows, or null for "all profiles".
    pub active_id: Option<String>,
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS profiles (
             id             TEXT PRIMARY KEY,
             name           TEXT NOT NULL,
             colour         TEXT NOT NULL,
             hub_person_id  TEXT,
             is_home        INTEGER NOT NULL DEFAULT 0,
             created_at     INTEGER NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS profiles_one_home ON profiles(is_home) WHERE is_home = 1;
         CREATE UNIQUE INDEX IF NOT EXISTS profiles_name ON profiles(name COLLATE NOCASE);
         CREATE UNIQUE INDEX IF NOT EXISTS profiles_person
             ON profiles(hub_person_id) WHERE hub_person_id IS NOT NULL;
         CREATE TABLE IF NOT EXISTS chat_profiles (
             chat_id     TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS chat_profiles_profile ON chat_profiles(profile_id);
         CREATE TRIGGER IF NOT EXISTS chat_profile_on_insert AFTER INSERT ON chats
         BEGIN
             INSERT OR IGNORE INTO chat_profiles(chat_id, profile_id)
             SELECT NEW.id, x.pid FROM (
                 SELECT CASE
                     WHEN NEW.parent_chat_id IS NOT NULL THEN
                         (SELECT cp.profile_id FROM chat_profiles cp WHERE cp.chat_id = NEW.parent_chat_id)
                     ELSE
                         (SELECT p.id FROM profiles p
                           WHERE p.is_home = 0
                             AND p.id = (SELECT json_extract(pr.value, '$') FROM preferences pr
                                          WHERE pr.key = 'activeProfile' AND json_valid(pr.value)))
                 END AS pid
             ) x WHERE x.pid IS NOT NULL;
         END;",
    )
}

const COLS: &str = "id, name, colour, hub_person_id, is_home";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        id: r.get(0)?,
        name: r.get(1)?,
        colour: r.get(2)?,
        hub_person_id: r.get(3)?,
        is_home: r.get(4)?,
    })
}

/// Every profile, the home profile first.
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Profile>> {
    conn.prepare(&format!(
        "SELECT {COLS} FROM profiles ORDER BY is_home DESC, created_at, rowid"
    ))?
    .query_map([], row)?
    .collect()
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Profile>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM profiles WHERE id = ?1"),
        [id],
        row,
    )
    .optional()
}

pub fn home(conn: &Connection) -> rusqlite::Result<Option<Profile>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM profiles WHERE is_home = 1"),
        [],
        row,
    )
    .optional()
}

fn clean_name(raw: &str) -> Result<String, ProfileError> {
    let name = raw.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return bad("A profile's name is 1 to 40 characters.");
    }
    Ok(name.to_owned())
}

fn clean_colour(raw: &str) -> Result<String, ProfileError> {
    let c = raw.trim();
    let ok = c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit());
    if !ok {
        return bad("A colour looks like #6d8cff.");
    }
    Ok(c.to_ascii_lowercase())
}

/// `None` for blank; otherwise the id, checked to be a plain token.
fn clean_person(raw: Option<&str>) -> Result<Option<String>, ProfileError> {
    let Some(p) = raw.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    if p.chars().count() > MAX_PERSON_CHARS
        || p.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return bad("That hub person id is not valid.");
    }
    Ok(Some(p.to_owned()))
}

pub fn create(
    tx: &Transaction<'_>,
    name: &str,
    colour: Option<&str>,
    hub_person_id: Option<&str>,
    now: i64,
) -> Result<Profile, ProfileError> {
    let name = clean_name(name)?;
    let existing = list(tx)?;
    if existing.len() >= MAX_PROFILES {
        return bad("Helm keeps at most 16 profiles.");
    }
    if existing
        .iter()
        .any(|p| p.name.to_lowercase() == name.to_lowercase())
    {
        return bad("A profile with that name already exists.");
    }
    let colour = match colour {
        Some(c) => clean_colour(c)?,
        None => COLOURS[existing.len() % COLOURS.len()].to_owned(),
    };
    let person = clean_person(hub_person_id)?;
    if let Some(p) = &person {
        if existing
            .iter()
            .any(|e| e.hub_person_id.as_deref() == Some(p.as_str()))
        {
            return bad("Another profile is already linked to that person.");
        }
    }
    let profile = Profile {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        colour,
        hub_person_id: person,
        is_home: existing.is_empty(),
    };
    tx.execute(
        "INSERT INTO profiles(id, name, colour, hub_person_id, is_home, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            profile.id,
            profile.name,
            profile.colour,
            profile.hub_person_id,
            profile.is_home,
            now
        ],
    )?;
    Ok(profile)
}

/// What an update changes. `hub_person_id`: `None` leaves it, `Some(None)` unlinks it.
#[derive(Default)]
pub struct Patch {
    pub name: Option<String>,
    pub colour: Option<String>,
    pub hub_person_id: Option<Option<String>>,
}

pub fn update(tx: &Transaction<'_>, id: &str, patch: Patch) -> Result<Profile, ProfileError> {
    let mut p = get(tx, id)?.ok_or(ProfileError::NotFound)?;
    if let Some(n) = patch.name {
        let n = clean_name(&n)?;
        let taken = list(tx)?
            .iter()
            .any(|o| o.id != p.id && o.name.to_lowercase() == n.to_lowercase());
        if taken {
            return bad("A profile with that name already exists.");
        }
        p.name = n;
    }
    if let Some(c) = patch.colour {
        p.colour = clean_colour(&c)?;
    }
    if let Some(link) = patch.hub_person_id {
        let person = clean_person(link.as_deref())?;
        if let Some(pid) = &person {
            let taken = list(tx)?
                .iter()
                .any(|o| o.id != p.id && o.hub_person_id.as_deref() == Some(pid.as_str()));
            if taken {
                return bad("Another profile is already linked to that person.");
            }
        }
        p.hub_person_id = person;
    }
    tx.execute(
        "UPDATE profiles SET name = ?2, colour = ?3, hub_person_id = ?4 WHERE id = ?1",
        params![p.id, p.name, p.colour, p.hub_person_id],
    )?;
    Ok(p)
}

/// Delete a profile; its chats fall back to the home profile. The home profile stays.
pub fn delete(tx: &Transaction<'_>, id: &str) -> Result<(), ProfileError> {
    let p = get(tx, id)?.ok_or(ProfileError::NotFound)?;
    if p.is_home {
        return bad("The home profile can't be deleted: it is whose sign-in this Helm runs under.");
    }
    tx.execute("DELETE FROM profiles WHERE id = ?1", [id])?;
    if active_raw(tx).as_deref() == Some(id) {
        prefs::set(tx, ACTIVE, &serde_json::Value::Null)?;
    }
    Ok(())
}

fn active_raw(conn: &Connection) -> Option<String> {
    prefs::read(conn, ACTIVE.name()).and_then(|v| v.as_str().map(str::to_owned))
}

/// The profile the sidebar shows; `None` is "all profiles". Before a choice is made, or when the
/// chosen profile is gone, it is the home profile.
pub fn active_id(conn: &Connection) -> Option<String> {
    match active_raw(conn) {
        Some(s) if s == ALL => None,
        Some(id) if matches!(get(conn, &id), Ok(Some(_))) => Some(id),
        _ => home(conn).ok().flatten().map(|p| p.id),
    }
}

/// Choose the profile the sidebar shows (`None`: all profiles).
pub fn set_active(tx: &Transaction<'_>, id: Option<&str>) -> Result<(), ProfileError> {
    let value = match id {
        None => ALL.to_owned(),
        Some(id) => {
            get(tx, id)?.ok_or(ProfileError::NotFound)?;
            id.to_owned()
        }
    };
    prefs::set(tx, ACTIVE, &serde_json::Value::String(value))?;
    Ok(())
}

/// Label chats with a profile. The home profile is the absence of a label, so labelling a chat
/// with it removes the row. Unknown chats refuse the whole assignment.
pub fn assign(
    tx: &Transaction<'_>,
    chat_ids: &[String],
    profile_id: &str,
) -> Result<usize, ProfileError> {
    if chat_ids.len() > MAX_ASSIGN {
        return bad("Move at most 500 chats at a time.");
    }
    let p = get(tx, profile_id)?.ok_or(ProfileError::NotFound)?;
    for id in chat_ids {
        let known = tx
            .query_row("SELECT 1 FROM chats WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some();
        if !known {
            return bad("There is no chat with that id.");
        }
        if p.is_home {
            tx.execute("DELETE FROM chat_profiles WHERE chat_id = ?1", [id])?;
        } else {
            tx.execute(
                "INSERT INTO chat_profiles(chat_id, profile_id) VALUES (?1, ?2)
                 ON CONFLICT(chat_id) DO UPDATE SET profile_id = excluded.profile_id",
                params![id, p.id],
            )?;
        }
    }
    Ok(chat_ids.len())
}

/// The ids of the chats a profile shows: those labelled with it, and for the home profile every
/// chat with no label. The sidebar's filter.
pub fn chat_ids_for(conn: &Connection, profile_id: &str) -> Result<Vec<String>, ProfileError> {
    let p = get(conn, profile_id)?.ok_or(ProfileError::NotFound)?;
    let sql = if p.is_home {
        "SELECT c.id FROM chats c LEFT JOIN chat_profiles cp ON cp.chat_id = c.id
         WHERE cp.profile_id IS NULL OR cp.profile_id = ?1"
    } else {
        "SELECT chat_id FROM chat_profiles WHERE profile_id = ?1"
    };
    let ids = conn
        .prepare(sql)?
        .query_map([profile_id], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

/// The profile a chat belongs to: its label, else the home profile.
pub fn profile_of_chat(conn: &Connection, chat_id: &str) -> rusqlite::Result<Option<Profile>> {
    let labelled = conn
        .query_row(
            &format!(
                "SELECT {COLS} FROM profiles
                 WHERE id = (SELECT profile_id FROM chat_profiles WHERE chat_id = ?1)"
            ),
            [chat_id],
            row,
        )
        .optional()?;
    match labelled {
        Some(p) => Ok(Some(p)),
        None => home(conn),
    }
}

/// Link the home profile to the hub person this machine was paired as, making the home profile
/// from the person's name when there is none yet. A different profile holding the same link
/// loses it first.
pub fn link_home_to_person(
    tx: &Transaction<'_>,
    person_id: &str,
    person_name: &str,
    now: i64,
) -> Result<Profile, ProfileError> {
    let person = clean_person(Some(person_id))?
        .ok_or_else(|| ProfileError::Bad("That hub person id is not valid.".into()))?;
    match home(tx)? {
        Some(h) => {
            tx.execute(
                "UPDATE profiles SET hub_person_id = NULL WHERE hub_person_id = ?1 AND id <> ?2",
                params![person, h.id],
            )?;
            update(
                tx,
                &h.id,
                Patch {
                    hub_person_id: Some(Some(person)),
                    ..Default::default()
                },
            )
        }
        None => {
            // A bad display name must not stop pairing: fall back to a plain one.
            let name = clean_name(person_name).unwrap_or_else(|_| "Me".to_owned());
            create(tx, &name, None, Some(&person), now)
        }
    }
}

fn counts(conn: &Connection) -> HashMap<String, (i64, i64, i64)> {
    let mut m: HashMap<String, (i64, i64, i64)> = HashMap::new();
    let home = "(SELECT id FROM profiles WHERE is_home = 1)";
    let sql = format!(
        "SELECT COALESCE(cp.profile_id, {home}), COUNT(*),
                COALESCE(SUM(c.status IN ('running', 'starting', 'stopping')), 0)
         FROM chats c LEFT JOIN chat_profiles cp ON cp.chat_id = c.id
         WHERE c.archived = 0 AND c.side_source_id IS NULL GROUP BY 1"
    );
    if let Ok(mut st) = conn.prepare(&sql) {
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        });
        for (id, n, running) in rows.into_iter().flatten().flatten() {
            if let Some(id) = id {
                let e = m.entry(id).or_default();
                e.0 = n;
                e.1 = running;
            }
        }
    }
    // Chats an agent has flagged for the user's attention (a table the chats crate owns, so a
    // database without it simply counts none).
    let sql = format!(
        "SELECT COALESCE(cp.profile_id, {home}), COUNT(*)
         FROM chats c JOIN chat_flags f ON f.chat_id = c.id
         LEFT JOIN chat_profiles cp ON cp.chat_id = c.id
         WHERE c.archived = 0 GROUP BY 1"
    );
    if let Ok(mut st) = conn.prepare(&sql) {
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        });
        for (id, n) in rows.into_iter().flatten().flatten() {
            if let Some(id) = id {
                m.entry(id).or_default().2 = n;
            }
        }
    }
    m
}

pub fn view(conn: &Connection) -> rusqlite::Result<ProfilesView> {
    let counts = counts(conn);
    let profiles: Vec<ProfileSummary> = list(conn)?
        .into_iter()
        .map(|profile| {
            let (chats, running, attention) = counts.get(&profile.id).copied().unwrap_or_default();
            ProfileSummary {
                profile,
                chats,
                running,
                attention,
            }
        })
        .collect();
    Ok(ProfilesView {
        home_id: profiles
            .iter()
            .find(|p| p.profile.is_home)
            .map(|p| p.profile.id.clone()),
        active_id: active_id(conn),
        profiles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};
    use serde_json::json;

    fn temp_db() -> (tempfile::TempDir, Database) {
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        (dir, db)
    }

    fn chat(tx: &Transaction<'_>, id: &str, parent: Option<&str>, archived: bool) {
        tx.execute(
            "INSERT OR IGNORE INTO projects(id, kind, name, created_at)
             VALUES ('p1', 'folder', 'p', 1)",
            [],
        )
        .ok();
        tx.execute(
            "INSERT INTO chats(id, project_id, parent_chat_id, archived, created_at, updated_at)
             VALUES (?1, 'p1', ?2, ?3, 1, 1)",
            params![id, parent, archived],
        )
        .unwrap();
    }

    #[test]
    fn profiles_labels_filters_and_the_active_choice_hold_together() {
        let (_d, db) = temp_db();
        let (home, other) = db
            .write(|tx| -> Result<_, ProfileError> {
                // The first profile is the home profile; names are unique regardless of case.
                let home = create(tx, "  First ", None, None, 1)?;
                assert!(home.is_home);
                assert_eq!(home.colour, COLOURS[0]);
                assert!(create(tx, "first", None, None, 2).is_err());
                assert!(create(tx, "", None, None, 2).is_err());
                assert!(create(tx, "Bad colour", Some("red"), None, 2).is_err());
                let other = create(tx, "Second", Some("#AABBCC"), Some("person-2"), 2)?;
                assert!(!other.is_home);
                assert_eq!(other.colour, "#aabbcc");
                // A hub person is linked to one profile only.
                assert!(create(tx, "Third", None, Some("person-2"), 3).is_err());
                Ok((home, other))
            })
            .unwrap();

        db.write(|tx| -> Result<_, ProfileError> {
            chat(tx, "c1", None, false);
            chat(tx, "c2", None, false);
            chat(tx, "c3", None, true);
            // Before any choice the home profile is active, and an unlabelled chat is its own.
            assert_eq!(active_id(tx).as_deref(), Some(home.id.as_str()));
            assert_eq!(chat_ids_for(tx, &home.id)?.len(), 3);
            assert!(chat_ids_for(tx, &other.id)?.is_empty());
            assert_eq!(assign(tx, &["c1".into()], &other.id)?, 1);
            assert!(assign(tx, &["nope".into()], &other.id).is_err());
            assert_eq!(chat_ids_for(tx, &other.id)?, ["c1"]);
            assert_eq!(chat_ids_for(tx, &home.id)?.len(), 2);
            assert_eq!(
                profile_of_chat(tx, "c1")?.map(|p| p.id),
                Some(other.id.clone())
            );
            assert_eq!(
                profile_of_chat(tx, "c2")?.map(|p| p.id),
                Some(home.id.clone())
            );

            // A new chat takes the active profile; a nested chat takes its parent's; "all" and
            // the home profile label nothing.
            set_active(tx, Some(&other.id))?;
            chat(tx, "c4", None, false);
            chat(tx, "c5", Some("c2"), false);
            set_active(tx, None)?;
            chat(tx, "c6", None, false);
            chat(tx, "c7", Some("c1"), false);
            set_active(tx, Some(&home.id))?;
            chat(tx, "c8", None, false);
            let who = |id: &str| profile_of_chat(tx, id).unwrap().unwrap().id;
            assert_eq!(who("c4"), other.id);
            assert_eq!(who("c5"), home.id, "its parent is the home profile's");
            assert_eq!(who("c6"), home.id);
            assert_eq!(who("c7"), other.id);
            assert_eq!(who("c8"), home.id);

            // Counts leave archived chats out; a profile view reads the same list.
            let v = view(tx)?;
            assert_eq!(v.home_id.as_deref(), Some(home.id.as_str()));
            let n = |id: &str| {
                v.profiles
                    .iter()
                    .find(|p| p.profile.id == id)
                    .unwrap()
                    .chats
            };
            assert_eq!((n(&home.id), n(&other.id)), (4, 3));
            assert!(serde_json::to_value(&v).unwrap()["profiles"][0]["isHome"]
                .as_bool()
                .unwrap());

            // Deleting a profile sends its chats home and resets the choice; home stays.
            set_active(tx, Some(&other.id))?;
            assert!(delete(tx, &home.id).is_err());
            delete(tx, &other.id)?;
            assert_eq!(
                profile_of_chat(tx, "c1")?.map(|p| p.id),
                Some(home.id.clone())
            );
            assert_eq!(active_id(tx).as_deref(), Some(home.id.as_str()));
            assert_eq!(delete(tx, &other.id), Err(ProfileError::NotFound));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn pairing_links_the_home_profile_or_makes_it() {
        let (_d, db) = temp_db();
        db.write(|tx| -> Result<_, ProfileError> {
            // No profile yet: the person's name makes the home profile.
            let h = link_home_to_person(tx, "person-1", "First", 1)?;
            assert!(h.is_home && h.name == "First");
            assert_eq!(h.hub_person_id.as_deref(), Some("person-1"));
            // A second profile holding the same link loses it to the home profile.
            let o = create(tx, "Other", None, Some("person-2"), 2)?;
            link_home_to_person(tx, "person-2", "First", 3)?;
            assert_eq!(get(tx, &o.id)?.unwrap().hub_person_id, None);
            assert_eq!(
                home(tx)?.unwrap().hub_person_id.as_deref(),
                Some("person-2")
            );
            // The stored choice is plain JSON a trigger can read.
            set_active(tx, Some(&o.id))?;
            assert_eq!(prefs::read(tx, "activeProfile"), Some(json!(o.id)));
            Ok(())
        })
        .unwrap();
    }
}
