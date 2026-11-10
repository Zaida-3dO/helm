//! The profile table, the chat identity columns and the rules over them.
//!
//! - `profiles`: one row per person who uses this Helm. Exactly one row is the **home** profile:
//!   the person whose Claude sign-in this install runs under. A profile may be linked to a person
//!   known to the hub, and carries an `about` text that tells agents how to address and write for
//!   that person. A home profile nobody has named yet is `needs_name`.
//! - The `chats` table carries `profile_id` (whose chat it is) and `host_device_id` (the device it
//!   runs on); neither is ever empty once the chat is stored. The label filters what the sidebar
//!   shows; it is never a credential, so a chat labelled for anyone still runs here, under this
//!   computer's sign-in.
//! - `chat_shares` (a table the chats crate owns): profiles besides its owner that a chat is shown
//!   to.
//! - `chat_profiles`: the old label table. It is read once, to fill the column on chats made
//!   before the column existed, and never written again.
//!
//! A trigger on `chats` fills both columns for any new row that leaves them empty: a chat nested
//! under another takes its parent's profile, a side chat its source's, any other chat the active
//! profile, and failing that the home profile. The device is this one.

use helm_core::hidden;
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
pub const MAX_ABOUT_CHARS: usize = 2000;
/// The most chats one assignment or share may touch.
pub const MAX_ASSIGN: usize = 500;
/// The name of the home profile made when none exists, until its person gives theirs.
pub const PLACEHOLDER_NAME: &str = "Me";

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

#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub colour: String,
    /// The hub's id for the person this profile is, when linked.
    pub hub_person_id: Option<String>,
    pub is_home: bool,
    /// How to address and write for this person (at most 2000 characters), told to every chat of
    /// the profile.
    pub about: String,
    /// The profile was made as a placeholder and its person has not named it yet.
    pub needs_name: bool,
}

/// A profile with the counts the profile view and the popover show (not archived chats).
#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    #[serde(flatten)]
    pub profile: Profile,
    #[ts(type = "number")]
    pub chats: i64,
    #[ts(type = "number")]
    pub running: i64,
    #[ts(type = "number")]
    pub attention: i64,
}

/// Everything the UI needs to draw the footer chip and the popover.
#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesView {
    pub profiles: Vec<ProfileSummary>,
    /// The home profile, or null before the first one is made.
    pub home_id: Option<String>,
    /// The profile the sidebar shows, or null for "all profiles".
    pub active_id: Option<String>,
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    // The insert trigger below reads the device row, so its table must exist even where the
    // devices crate registers nothing of its own (it is idempotent).
    helm_devices::identity::init(tx)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS profiles (
             id             TEXT PRIMARY KEY,
             name           TEXT NOT NULL,
             colour         TEXT NOT NULL,
             hub_person_id  TEXT,
             is_home        INTEGER NOT NULL DEFAULT 0,
             created_at     INTEGER NOT NULL,
             about          TEXT NOT NULL DEFAULT '',
             needs_name     INTEGER NOT NULL DEFAULT 0
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
         DROP TRIGGER IF EXISTS chat_profile_on_insert;",
    )?;
    // Profiles made before these columns existed.
    for (column, ddl) in [
        (
            "about",
            "ALTER TABLE profiles ADD COLUMN about TEXT NOT NULL DEFAULT ''",
        ),
        (
            "needs_name",
            "ALTER TABLE profiles ADD COLUMN needs_name INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let has: bool = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('profiles') WHERE name = ?1",
            [column],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
        if !has {
            tx.execute(ddl, [])?;
        }
    }
    tx.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS chat_identity_on_insert AFTER INSERT ON chats
         WHEN NEW.profile_id = '' OR NEW.host_device_id = ''
         BEGIN
             UPDATE chats SET
                 profile_id = CASE WHEN NEW.profile_id <> '' THEN NEW.profile_id ELSE COALESCE(
                     (SELECT NULLIF(par.profile_id, '') FROM chats par WHERE par.id = NEW.parent_chat_id),
                     (SELECT NULLIF(src.profile_id, '') FROM chats src WHERE src.id = NEW.side_source_id),
                     (SELECT a.id FROM profiles a
                       WHERE a.id = (SELECT json_extract(pr.value, '$') FROM preferences pr
                                      WHERE pr.key = 'activeProfile' AND json_valid(pr.value))),
                     (SELECT h.id FROM profiles h WHERE h.is_home = 1),
                     '') END,
                 host_device_id = CASE WHEN NEW.host_device_id <> '' THEN NEW.host_device_id ELSE
                     COALESCE((SELECT d.uid FROM device_self d ORDER BY d.created_at, d.rowid LIMIT 1), '')
                 END
             WHERE id = NEW.id;
         END;",
    )
}

/// Give every chat its two identity columns and make sure a home profile exists. Idempotent:
/// registered both as a one-time step (the first start after the columns appear) and as a recovery
/// step (every start), so a row left empty by any other path is filled the same way.
///
/// - the home profile is made first (`Me`, waiting for its person's name) when there is none;
/// - a chat that had a label in the old table takes it, any other chat takes the home profile;
/// - every chat without a device takes this one.
pub fn fill_identity(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let (device, _) = helm_devices::identity::ensure(tx)?;
    let home = ensure_home(tx, helm_core::now_ms()).map_err(|e| match e {
        ProfileError::Db(m) | ProfileError::Bad(m) => rusqlite::Error::InvalidParameterName(m),
        ProfileError::NotFound => rusqlite::Error::QueryReturnedNoRows,
    })?;
    tx.execute(
        "UPDATE chats SET profile_id = COALESCE(
             (SELECT cp.profile_id FROM chat_profiles cp
               WHERE cp.chat_id = chats.id AND cp.profile_id IN (SELECT id FROM profiles)),
             ?1)
         WHERE profile_id = ''",
        [&home.id],
    )?;
    tx.execute(
        "UPDATE chats SET host_device_id = ?1 WHERE host_device_id = ''",
        [&device],
    )?;
    Ok(())
}

const COLS: &str = "id, name, colour, hub_person_id, is_home, about, needs_name";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        id: r.get(0)?,
        name: r.get(1)?,
        colour: r.get(2)?,
        hub_person_id: r.get(3)?,
        is_home: r.get(4)?,
        about: r.get(5)?,
        needs_name: r.get(6)?,
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

/// The home profile, made first when this install has no profile at all: `Me`, marked as waiting
/// for its person's name. If profiles exist but none is home, the oldest becomes home.
pub fn ensure_home(tx: &Transaction<'_>, now: i64) -> Result<Profile, ProfileError> {
    if let Some(h) = home(tx)? {
        return Ok(h);
    }
    let oldest: Option<String> = tx
        .query_row(
            "SELECT id FROM profiles ORDER BY created_at, rowid LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = oldest {
        tx.execute("UPDATE profiles SET is_home = 1 WHERE id = ?1", [&id])?;
    } else {
        tx.execute(
            "INSERT INTO profiles(id, name, colour, hub_person_id, is_home, created_at, needs_name)
             VALUES (?1, ?2, ?3, NULL, 1, ?4, 1)",
            params![
                uuid::Uuid::new_v4().to_string(),
                PLACEHOLDER_NAME,
                COLOURS[0],
                now
            ],
        )?;
    }
    Ok(home(tx)?.expect("a home profile was just made"))
}

fn clean_name(raw: &str) -> Result<String, ProfileError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return bad("A profile's name is 1 to 40 characters.");
    }
    if hidden::has_hidden(name) {
        return bad("A profile's name can't hold invisible or direction-changing characters.");
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

/// The about text: at most 2000 characters, line breaks and tabs allowed, no other control or
/// invisible, direction-changing characters.
fn clean_about(raw: &str) -> Result<String, ProfileError> {
    let about = raw.trim();
    if about.chars().count() > MAX_ABOUT_CHARS {
        return bad("A profile's about text is at most 2000 characters.");
    }
    let odd = about
        .chars()
        .any(|c| hidden::is_hidden_char(c) || (c.is_control() && !matches!(c, '\n' | '\r' | '\t')));
    if odd {
        return bad(
            "A profile's about text can't hold invisible or direction-changing characters.",
        );
    }
    Ok(about.to_owned())
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
        about: String::new(),
        needs_name: false,
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

/// What an update changes. `hub_person_id`: `None` leaves it, `Some(None)` unlinks it. Giving a
/// name settles a placeholder profile (`needs_name` clears).
#[derive(Default)]
pub struct Patch {
    pub name: Option<String>,
    pub colour: Option<String>,
    pub hub_person_id: Option<Option<String>>,
    pub about: Option<String>,
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
        p.needs_name = false;
    }
    if let Some(c) = patch.colour {
        p.colour = clean_colour(&c)?;
    }
    if let Some(a) = patch.about {
        p.about = clean_about(&a)?;
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
        "UPDATE profiles SET name = ?2, colour = ?3, hub_person_id = ?4, about = ?5, needs_name = ?6
         WHERE id = ?1",
        params![p.id, p.name, p.colour, p.hub_person_id, p.about, p.needs_name],
    )?;
    Ok(p)
}

/// Delete a profile; its chats move to the home profile. The home profile stays.
pub fn delete(tx: &Transaction<'_>, id: &str) -> Result<(), ProfileError> {
    let p = get(tx, id)?.ok_or(ProfileError::NotFound)?;
    if p.is_home {
        return bad("The home profile can't be deleted: it is whose sign-in this Helm runs under.");
    }
    let home = ensure_home(tx, helm_core::now_ms())?;
    tx.execute(
        "UPDATE chats SET profile_id = ?2 WHERE profile_id = ?1",
        params![id, home.id],
    )?;
    tx.execute("DELETE FROM chat_shares WHERE profile_id = ?1", [id])?;
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

/// Check that every chat id names a chat, and that there are not too many of them.
fn check_chats(tx: &Transaction<'_>, chat_ids: &[String]) -> Result<(), ProfileError> {
    if chat_ids.len() > MAX_ASSIGN {
        return bad("Move at most 500 chats at a time.");
    }
    for id in chat_ids {
        let known = tx
            .query_row("SELECT 1 FROM chats WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some();
        if !known {
            return bad("There is no chat with that id.");
        }
    }
    Ok(())
}

/// Make chats belong to a profile. A chat that was shared with that profile stops being shared
/// (it is now its own). Unknown chats refuse the whole assignment.
pub fn assign(
    tx: &Transaction<'_>,
    chat_ids: &[String],
    profile_id: &str,
) -> Result<usize, ProfileError> {
    let p = get(tx, profile_id)?.ok_or(ProfileError::NotFound)?;
    check_chats(tx, chat_ids)?;
    let now = helm_core::now_ms();
    for id in chat_ids {
        // A changed chat moves its `updated_at`, so other devices catching up hear of it.
        tx.execute(
            "UPDATE chats SET profile_id = ?2, updated_at = MAX(updated_at, ?3)
             WHERE id = ?1 AND profile_id <> ?2",
            params![id, p.id, now],
        )?;
        tx.execute(
            "DELETE FROM chat_shares WHERE chat_id = ?1 AND profile_id = ?2",
            params![id, p.id],
        )?;
    }
    Ok(chat_ids.len())
}

/// Show chats to a profile besides their owner (`shared`), or stop showing them. A chat is never
/// shared with its own profile. Unknown chats refuse the whole change.
pub fn share(
    tx: &Transaction<'_>,
    chat_ids: &[String],
    profile_id: &str,
    shared: bool,
) -> Result<usize, ProfileError> {
    let p = get(tx, profile_id)?.ok_or(ProfileError::NotFound)?;
    check_chats(tx, chat_ids)?;
    let now = helm_core::now_ms();
    for id in chat_ids {
        let n = if shared {
            tx.execute(
                "INSERT OR IGNORE INTO chat_shares(chat_id, profile_id)
                 SELECT ?1, ?2 WHERE NOT EXISTS (SELECT 1 FROM chats WHERE id = ?1 AND profile_id = ?2)",
                params![id, p.id],
            )?
        } else {
            tx.execute(
                "DELETE FROM chat_shares WHERE chat_id = ?1 AND profile_id = ?2",
                params![id, p.id],
            )?
        };
        // A changed chat moves its `updated_at`, so other devices catching up hear of it.
        if n > 0 {
            tx.execute(
                "UPDATE chats SET updated_at = MAX(updated_at, ?2) WHERE id = ?1",
                params![id, now],
            )?;
        }
    }
    Ok(chat_ids.len())
}

/// The ids of the chats a profile shows: those it owns, those shared with it, and for the home
/// profile any chat still without an owner. The sidebar's filter.
pub fn chat_ids_for(conn: &Connection, profile_id: &str) -> Result<Vec<String>, ProfileError> {
    let p = get(conn, profile_id)?.ok_or(ProfileError::NotFound)?;
    let ids = conn
        .prepare(
            "SELECT id FROM chats WHERE profile_id = ?1 OR (?2 AND profile_id = '')
             UNION
             SELECT chat_id FROM chat_shares WHERE profile_id = ?1",
        )?
        .query_map(params![profile_id, p.is_home], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

/// The profile a chat belongs to: its column, else the home profile.
pub fn profile_of_chat(conn: &Connection, chat_id: &str) -> rusqlite::Result<Option<Profile>> {
    let owned = conn
        .query_row(
            &format!(
                "SELECT {COLS} FROM profiles
                 WHERE id = (SELECT NULLIF(profile_id, '') FROM chats WHERE id = ?1)"
            ),
            [chat_id],
            row,
        )
        .optional()?;
    match owned {
        Some(p) => Ok(Some(p)),
        None => home(conn),
    }
}

/// The id of the profile a chat belongs to, for settings and tools that act for a chat's owner.
pub fn chat_owner_profile(conn: &Connection, chat_id: &str) -> Option<String> {
    profile_of_chat(conn, chat_id).ok().flatten().map(|p| p.id)
}

/// Link the home profile to the hub person this machine was paired as. A home profile that is
/// still the unnamed placeholder takes the person's name (when it is a usable name no other
/// profile has). A different profile holding the same link loses it first.
pub fn link_home_to_person(
    tx: &Transaction<'_>,
    person_id: &str,
    person_name: &str,
    now: i64,
) -> Result<Profile, ProfileError> {
    let person = clean_person(Some(person_id))?
        .ok_or_else(|| ProfileError::Bad("That hub person id is not valid.".into()))?;
    let h = ensure_home(tx, now)?;
    tx.execute(
        "UPDATE profiles SET hub_person_id = NULL WHERE hub_person_id = ?1 AND id <> ?2",
        params![person, h.id],
    )?;
    // A bad or taken display name must not stop pairing: the placeholder keeps its name.
    let name = if h.needs_name {
        clean_name(person_name).ok().filter(|n| {
            !list(tx)
                .unwrap_or_default()
                .iter()
                .any(|o| o.id != h.id && o.name.to_lowercase() == n.to_lowercase())
        })
    } else {
        None
    };
    update(
        tx,
        &h.id,
        Patch {
            name,
            hub_person_id: Some(Some(person)),
            ..Default::default()
        },
    )
}

/// Make sure each person the hub names has a profile here. A person already linked is left as
/// they are; an unlinked profile of the same name is linked to them; otherwise a profile is made
/// (with their colour when it is a usable one). Nothing is ever deleted or renamed. `true` when
/// anything changed.
pub fn sync_people(
    tx: &Transaction<'_>,
    people: &[helm_devices::wire::PersonInfo],
    now: i64,
) -> Result<bool, ProfileError> {
    let mut changed = false;
    for p in people {
        let id = p.person_id.trim();
        if id.is_empty() || p.name.trim().is_empty() {
            continue;
        }
        let profiles = list(tx)?;
        if profiles
            .iter()
            .any(|e| e.hub_person_id.as_deref() == Some(id))
        {
            continue;
        }
        let name = p.name.trim().to_lowercase();
        let same_name = profiles
            .iter()
            .find(|e| e.hub_person_id.is_none() && e.name.to_lowercase() == name);
        let done = match same_name {
            Some(e) => update(
                tx,
                &e.id,
                Patch {
                    hub_person_id: Some(Some(id.to_owned())),
                    ..Default::default()
                },
            )
            .map(|_| ()),
            None => {
                let colour = Some(p.colour.as_str()).filter(|c| clean_colour(c).is_ok());
                create(tx, &p.name, colour, Some(id), now).map(|_| ())
            }
        };
        match done {
            Ok(()) => changed = true,
            // A name that can't be used, or the limit on profiles: that person has no profile
            // here, and their chats simply stay out of the lists.
            Err(e) => log::info!(
                "profiles: no profile made for a hub person: {}",
                e.message()
            ),
        }
    }
    Ok(changed)
}

fn counts(conn: &Connection) -> HashMap<String, (i64, i64, i64)> {
    let mut m: HashMap<String, (i64, i64, i64)> = HashMap::new();
    let home = "(SELECT id FROM profiles WHERE is_home = 1)";
    let sql = format!(
        "SELECT COALESCE(NULLIF(c.profile_id, ''), {home}), COUNT(*),
                COALESCE(SUM(c.status IN ('running', 'starting', 'stopping')), 0)
         FROM chats c
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
        "SELECT COALESCE(NULLIF(c.profile_id, ''), {home}), COUNT(*)
         FROM chats c JOIN chat_flags f ON f.chat_id = c.id
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

    fn registry() -> MigrationRegistry {
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        reg
    }

    fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &registry()).unwrap();
        (dir, db)
    }

    /// A chat as another path than the chats crate's own insert makes it: no profile, no device.
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

    fn column(tx: &Transaction<'_>, id: &str, col: &str) -> String {
        tx.query_row(
            &format!("SELECT {col} FROM chats WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn profiles_owners_shares_and_the_active_choice_hold_together() {
        let (_d, db) = temp_db();
        let (home, other, device) = db
            .write(|tx| -> Result<_, ProfileError> {
                // A fresh install starts with a placeholder home profile waiting for a name.
                let home = home(tx)?.expect("made at open");
                assert!(home.is_home && home.needs_name && home.name == PLACEHOLDER_NAME);
                assert_eq!(home.colour, COLOURS[0]);
                // Naming it (any case change counts) settles it; names are unique regardless of case.
                let home = update(
                    tx,
                    &home.id,
                    Patch {
                        name: Some("  First ".into()),
                        ..Default::default()
                    },
                )?;
                assert!(!home.needs_name && home.name == "First");
                assert!(create(tx, "first", None, None, 2).is_err());
                assert!(create(tx, "", None, None, 2).is_err());
                assert!(create(tx, "Sam\u{202e}", None, None, 2).is_err());
                assert!(create(tx, "Bad colour", Some("red"), None, 2).is_err());
                let other = create(tx, "Second", Some("#AABBCC"), Some("person-2"), 2)?;
                assert!(!other.is_home);
                assert_eq!(other.colour, "#aabbcc");
                // A hub person is linked to one profile only.
                assert!(create(tx, "Third", None, Some("person-2"), 3).is_err());
                // About text: trimmed, multi-line, capped, and screened.
                let about = update(
                    tx,
                    &other.id,
                    Patch {
                        about: Some(" Likes short answers.\nCalls me Sam. ".into()),
                        ..Default::default()
                    },
                )?;
                assert_eq!(about.about, "Likes short answers.\nCalls me Sam.");
                for bad in ["a\u{202e}b".to_owned(), "x".repeat(MAX_ABOUT_CHARS + 1)] {
                    let patch = Patch {
                        about: Some(bad),
                        ..Default::default()
                    };
                    assert!(update(tx, &other.id, patch).is_err());
                }
                let device = helm_devices::self_uid(tx).expect("the device is made at open");
                Ok((home, other, device))
            })
            .unwrap();

        db.write(|tx| -> Result<_, ProfileError> {
            // A chat made with no identity gets the home profile and this device.
            chat(tx, "c1", None, false);
            chat(tx, "c2", None, false);
            chat(tx, "c3", None, true);
            assert_eq!(column(tx, "c1", "profile_id"), home.id);
            assert_eq!(column(tx, "c1", "host_device_id"), device);
            // Before any choice the home profile is active and shows every chat.
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
            assert_eq!(chat_owner_profile(tx, "c2"), Some(home.id.clone()));
            assert_eq!(chat_owner_profile(tx, "c1"), Some(other.id.clone()));

            // Sharing shows a chat to another profile without moving it; never to its own owner.
            assert_eq!(share(tx, &["c2".into()], &other.id, true)?, 1);
            share(tx, &["c1".into()], &other.id, true)?;
            assert_eq!(chat_ids_for(tx, &other.id)?.len(), 2);
            assert_eq!(chat_ids_for(tx, &home.id)?.len(), 2);
            assert!(share(tx, &["nope".into()], &other.id, true).is_err());
            share(tx, &["c2".into()], &other.id, false)?;
            assert_eq!(chat_ids_for(tx, &other.id)?, ["c1"]);
            // Handing a shared chat to the profile it is shared with ends the share.
            share(tx, &["c3".into()], &other.id, true)?;
            assign(tx, &["c3".into()], &other.id)?;
            let shares: i64 = tx
                .query_row("SELECT COUNT(*) FROM chat_shares", [], |r| r.get(0))
                .unwrap();
            assert_eq!(shares, 0);
            assign(tx, &["c3".into()], &home.id)?;

            // A new chat takes the active profile; a nested chat takes its parent's; "all" and
            // the home profile give the home profile.
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
            let first = serde_json::to_value(&v).unwrap()["profiles"][0].clone();
            assert!(first["isHome"].as_bool().unwrap());
            assert_eq!(first["needsName"], json!(false));

            // Deleting a profile sends its chats (and drops its shares) home and resets the
            // choice; home stays.
            share(tx, &["c2".into()], &other.id, true)?;
            set_active(tx, Some(&other.id))?;
            assert!(delete(tx, &home.id).is_err());
            delete(tx, &other.id)?;
            assert_eq!(
                profile_of_chat(tx, "c1")?.map(|p| p.id),
                Some(home.id.clone())
            );
            assert_eq!(column(tx, "c7", "profile_id"), home.id);
            assert_eq!(active_id(tx).as_deref(), Some(home.id.as_str()));
            assert_eq!(delete(tx, &other.id), Err(ProfileError::NotFound));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn older_databases_get_their_chats_an_owner_and_a_device() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sqlite3");
        // A database as the previous version left it: labelled chats in the old table, and no
        // owner or device on any chat.
        {
            let db = Database::open(&path, &registry()).unwrap();
            db.write(|tx| -> Result<_, ProfileError> {
                tx.execute_batch(
                    "DROP TRIGGER chat_identity_on_insert;
                     DELETE FROM profiles;
                     DELETE FROM device_self;
                     DELETE FROM counters WHERE key = 'migration:profiles-chat-identity';",
                )?;
                let h = create(tx, "Old home", None, None, 1)?;
                let o = create(tx, "Old other", None, None, 2)?;
                assert!(h.is_home);
                for c in ["a", "b", "c"] {
                    chat(tx, c, None, false);
                }
                tx.execute_batch(&format!(
                    "UPDATE chats SET profile_id = '', host_device_id = '';
                     INSERT INTO chat_profiles(chat_id, profile_id) VALUES ('b', '{}');",
                    o.id
                ))?;
                Ok(())
            })
            .unwrap();
        }
        // Reopening migrates: labels become the column, the rest go home, the device is made.
        let db = Database::open(&path, &registry()).unwrap();
        db.write(|tx| -> Result<_, ProfileError> {
            let h = home(tx)?.unwrap();
            let o = list(tx)?.into_iter().find(|p| !p.is_home).unwrap();
            assert_eq!(h.name, "Old home");
            assert_eq!(column(tx, "a", "profile_id"), h.id);
            assert_eq!(column(tx, "b", "profile_id"), o.id);
            assert_eq!(
                column(tx, "c", "profile_id"),
                h.id,
                "an unlabelled chat goes home"
            );
            let device = helm_devices::self_uid(tx).unwrap();
            for c in ["a", "b", "c"] {
                assert_eq!(column(tx, c, "host_device_id"), device);
            }
            // The start-up check refills a row any other path left empty.
            tx.execute(
                "UPDATE chats SET profile_id = '', host_device_id = '' WHERE id = 'a'",
                [],
            )?;
            fill_identity(tx)?;
            assert_eq!(column(tx, "a", "profile_id"), h.id);
            assert_eq!(column(tx, "a", "host_device_id"), device);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn pairing_names_the_placeholder_home_or_links_the_named_one() {
        let (_d, db) = temp_db();
        db.write(|tx| -> Result<_, ProfileError> {
            // The placeholder home takes the person's name and settles.
            let h = link_home_to_person(tx, "person-1", "First", 1)?;
            assert!(h.is_home && h.name == "First" && !h.needs_name);
            assert_eq!(h.hub_person_id.as_deref(), Some("person-1"));
            // A second profile holding the same link loses it to the home profile; a named home
            // keeps its name.
            let o = create(tx, "Other", None, Some("person-2"), 2)?;
            let h = link_home_to_person(tx, "person-2", "Someone Else", 3)?;
            assert_eq!(h.name, "First");
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
        // A taken or unusable display name leaves the placeholder as it is.
        let (_d2, db2) = temp_db();
        db2.write(|tx| -> Result<_, ProfileError> {
            create(tx, "Taken", None, None, 1)?;
            let h = link_home_to_person(tx, "person-1", "taken", 2)?;
            assert!(h.needs_name && h.name == PLACEHOLDER_NAME);
            assert_eq!(h.hub_person_id.as_deref(), Some("person-1"));
            Ok(())
        })
        .unwrap();
    }
}
