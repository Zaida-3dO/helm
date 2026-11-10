//! The client registry: people, their machines (clients), and pairing codes.
//!
//! It lives in the hub's SQLite file beside the memories, but through its own connection, so a
//! token check never waits for a memory query. The admin commands (`helm-hub person|client|pair`)
//! run in a second process against the same file, so every check reads the database: a revoke
//! takes effect on the next request and, for an open line, within [`crate::server::REVALIDATE_EVERY`].
//!
//! `HUB_TOKENS` entries are mirrored in as *legacy* clients (see [`Registry::sync_legacy`]); the
//! environment stays the only place they are added or removed.

use crate::auth::{bearer, ct_eq, digest, Client, Scopes, Tokens};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

#[derive(Debug)]
pub enum RegistryError {
    Invalid(String),
    /// A pairing code that is wrong, expired or already used (deliberately one answer).
    BadCode,
    NotFound(String),
    Db(rusqlite::Error),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Invalid(m) | RegistryError::NotFound(m) => write!(f, "{m}"),
            RegistryError::BadCode => write!(f, "that code is not valid or has expired"),
            RegistryError::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<rusqlite::Error> for RegistryError {
    fn from(e: rusqlite::Error) -> Self {
        RegistryError::Db(e)
    }
}

pub type Result<T> = std::result::Result<T, RegistryError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Person {
    pub id: i64,
    pub name: String,
}

/// One machine, as the admin commands and the household list see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRow {
    pub id: i64,
    pub person_id: i64,
    pub person: String,
    pub machine: String,
    pub scopes: Scopes,
    /// Added by `HUB_TOKENS`, so only the environment can remove it.
    pub legacy: bool,
    pub created_at: String,
    pub last_seen_at: Option<String>,
    pub revoked_at: Option<String>,
    /// The id this machine's Helm gave itself (see [`Registry::set_device`]); `None` until its
    /// first `device` frame.
    pub device_uid: Option<String>,
    pub device_name: Option<String>,
    /// Whether that Helm lets other devices run its chats, as it last said.
    pub run: bool,
}

/// A machine just paired: its token is shown once and never stored in the clear.
#[derive(Debug, Clone)]
pub struct Paired {
    pub token: String,
    pub client: ClientRow,
}

/// How long a pairing code works.
pub const PAIR_CODE_MINUTES: i64 = 15;
pub const MAX_NAME: usize = 64;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS people (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS clients (
    id           INTEGER PRIMARY KEY,
    person_id    INTEGER NOT NULL REFERENCES people(id),
    machine_name TEXT NOT NULL,
    scopes       TEXT NOT NULL,
    token_sha256 BLOB NOT NULL UNIQUE,
    legacy_name  TEXT UNIQUE,
    created_at   TEXT NOT NULL,
    last_seen_at TEXT,
    revoked_at   TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS clients_machine
    ON clients(person_id, machine_name COLLATE NOCASE) WHERE revoked_at IS NULL;
CREATE TABLE IF NOT EXISTS pairings (
    code_sha256  BLOB PRIMARY KEY,
    person_id    INTEGER NOT NULL REFERENCES people(id),
    machine_name TEXT NOT NULL,
    scopes       TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    expires_ms   INTEGER NOT NULL,
    used_at      TEXT
);
"#;

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
const CLIENT_COLS: &str = "c.id, c.person_id, p.name, c.machine_name, c.scopes, \
    c.legacy_name IS NOT NULL, c.created_at, c.last_seen_at, c.revoked_at, \
    c.device_uid, c.device_name, c.run";

/// Longest device name a Helm may announce.
pub const MAX_DEVICE_NAME: usize = 40;
/// A device uid is what the Helm made for itself: letters, digits and dashes, 8 to 64 of them.
pub const MAX_DEVICE_UID: usize = 64;

/// Pairing codes avoid look-alike characters (no 0, 1, I or O).
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub struct Registry {
    conn: Connection,
}

impl Registry {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// An in-memory registry holding the clients of a `HUB_TOKENS` string, for tests and tools.
    pub fn from_legacy_spec(spec: &str, person: &str) -> std::result::Result<Self, String> {
        let tokens = Tokens::parse(spec)?;
        let mut r = Self::open_in_memory().map_err(|e| e.to_string())?;
        r.sync_legacy(&tokens, person).map_err(|e| e.to_string())?;
        Ok(r)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.execute_batch(SCHEMA)?;
        // A registry made before devices existed gains their columns.
        let has_device = conn
            .prepare("SELECT 1 FROM pragma_table_info('clients') WHERE name = 'device_uid'")?
            .exists([])?;
        if !has_device {
            conn.execute_batch(
                "ALTER TABLE clients ADD COLUMN device_uid TEXT;
                 ALTER TABLE clients ADD COLUMN device_name TEXT;
                 ALTER TABLE clients ADD COLUMN run INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        Ok(Registry { conn })
    }

    // ---- people ----

    /// The person called `name`, made if there is none. Names are unique ignoring case.
    pub fn ensure_person(&mut self, name: &str) -> Result<Person> {
        let name = clean_name(name, "a person's name")?;
        if let Some(p) = self.person_by_name(&name)? {
            return Ok(p);
        }
        self.conn.execute(
            &format!("INSERT INTO people (name, created_at) VALUES (?1, {NOW})"),
            [&name],
        )?;
        Ok(Person {
            id: self.conn.last_insert_rowid(),
            name,
        })
    }

    pub fn person_by_name(&self, name: &str) -> Result<Option<Person>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name FROM people WHERE name = ?1",
                [name.trim()],
                person_from_row,
            )
            .optional()?)
    }

    pub fn person_by_id(&self, id: i64) -> Result<Option<Person>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name FROM people WHERE id = ?1",
                [id],
                person_from_row,
            )
            .optional()?)
    }

    /// A person by name or by numeric id.
    pub fn find_person(&self, who: &str) -> Result<Person> {
        let found = match who.trim().parse::<i64>() {
            Ok(id) => self.person_by_id(id)?,
            Err(_) => None,
        };
        match found {
            Some(p) => Ok(p),
            None => self
                .person_by_name(who)?
                .ok_or_else(|| RegistryError::NotFound(format!("no person called {who:?}"))),
        }
    }

    pub fn people(&self) -> Result<Vec<Person>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM people ORDER BY id")?;
        let rows = stmt.query_map([], person_from_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- clients ----

    /// Register a machine for `person` (made if new) and return its token, shown once.
    pub fn add_client(&mut self, person: &str, machine: &str, scopes: &Scopes) -> Result<Paired> {
        let person = self.ensure_person(person)?;
        let machine = clean_name(machine, "a machine name")?;
        let token = new_token();
        self.insert_client(&person, &machine, scopes, &token, None)
    }

    fn insert_client(
        &mut self,
        person: &Person,
        machine: &str,
        scopes: &Scopes,
        token: &str,
        legacy_name: Option<&str>,
    ) -> Result<Paired> {
        if self.active_machine(person.id, machine)?.is_some() {
            return Err(RegistryError::Invalid(format!(
                "{} already has a machine called {machine:?}; revoke it first or pick another name",
                person.name
            )));
        }
        self.conn.execute(
            &format!(
                "INSERT INTO clients (person_id, machine_name, scopes, token_sha256, legacy_name,
                 created_at) VALUES (?1, ?2, ?3, ?4, ?5, {NOW})"
            ),
            params![
                person.id,
                machine,
                scopes.to_csv(),
                digest(token).to_vec(),
                legacy_name
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(Paired {
            token: token.to_owned(),
            client: self
                .client_by_id(id)?
                .ok_or_else(|| RegistryError::NotFound("the new client vanished".into()))?,
        })
    }

    fn active_machine(&self, person_id: i64, machine: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM clients WHERE person_id = ?1 AND machine_name = ?2 COLLATE NOCASE
                 AND revoked_at IS NULL",
                params![person_id, machine],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn client_by_id(&self, id: i64) -> Result<Option<ClientRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {CLIENT_COLS} FROM clients c JOIN people p ON p.id = c.person_id
                     WHERE c.id = ?1"
                ),
                [id],
                client_from_row,
            )
            .optional()?)
    }

    /// Every client, revoked ones included, oldest first.
    pub fn clients(&self) -> Result<Vec<ClientRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLIENT_COLS} FROM clients c JOIN people p ON p.id = c.person_id ORDER BY c.id"
        ))?;
        let rows = stmt.query_map([], client_from_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The active machines of every person that hold a relay line of their own.
    pub fn relay_machines(&self) -> Result<Vec<ClientRow>> {
        Ok(self
            .clients()?
            .into_iter()
            .filter(|c| c.revoked_at.is_none() && c.scopes.has(crate::auth::Scope::Relay))
            .collect())
    }

    /// The active client picked out by a machine name or a numeric id; `person` narrows a name
    /// that two people share.
    pub fn find_client(&self, selector: &str, person: Option<&str>) -> Result<ClientRow> {
        let selector = selector.trim();
        let person = match person {
            Some(p) => Some(self.find_person(p)?),
            None => None,
        };
        let active: Vec<ClientRow> = self
            .clients()?
            .into_iter()
            .filter(|c| c.revoked_at.is_none())
            .filter(|c| person.as_ref().is_none_or(|p| p.id == c.person_id))
            .collect();
        if let Ok(id) = selector.parse::<i64>() {
            if let Some(c) = active.iter().find(|c| c.id == id) {
                return Ok(c.clone());
            }
        }
        let mut named = active
            .into_iter()
            .filter(|c| c.machine.eq_ignore_ascii_case(selector));
        match (named.next(), named.next()) {
            (Some(c), None) => Ok(c),
            (Some(_), Some(_)) => Err(RegistryError::Invalid(format!(
                "more than one machine is called {selector:?}; add --person or use its id"
            ))),
            _ => Err(RegistryError::NotFound(format!(
                "no active machine {selector:?}"
            ))),
        }
    }

    /// Revoke a machine: its token stops working at once.
    pub fn revoke(&mut self, selector: &str, person: Option<&str>) -> Result<ClientRow> {
        let c = self.find_client(selector, person)?;
        if c.legacy {
            return Err(RegistryError::Invalid(format!(
                "{:?} comes from HUB_TOKENS; remove it there and restart the hub",
                c.machine
            )));
        }
        self.conn.execute(
            &format!("UPDATE clients SET revoked_at = {NOW} WHERE id = ?1 AND revoked_at IS NULL"),
            [c.id],
        )?;
        self.client_by_id(c.id)?
            .ok_or_else(|| RegistryError::NotFound("the client vanished".into()))
    }

    /// Record the device a machine's Helm announced: its own id and name, and whether other
    /// devices may run its chats. A uid already held by another person's machine is refused; the
    /// same person's older machine gives it up (a Helm paired again keeps its device id).
    pub fn set_device(
        &mut self,
        client_id: i64,
        uid: &str,
        name: &str,
        run: bool,
    ) -> Result<ClientRow> {
        if !valid_device_uid(uid) {
            return Err(RegistryError::Invalid(
                "a device id is 8 to 64 letters, digits or dashes".into(),
            ));
        }
        let name = clean_name(name, "a device name")?;
        if name.chars().count() > MAX_DEVICE_NAME {
            return Err(RegistryError::Invalid(format!(
                "a device name is at most {MAX_DEVICE_NAME} characters"
            )));
        }
        let me = self
            .client_by_id(client_id)?
            .ok_or_else(|| RegistryError::NotFound("the client vanished".into()))?;
        let other: Option<i64> = self
            .conn
            .query_row(
                "SELECT person_id FROM clients
                 WHERE device_uid = ?1 AND id <> ?2 AND revoked_at IS NULL AND person_id <> ?3",
                params![uid, client_id, me.person_id],
                |r| r.get(0),
            )
            .optional()?;
        if other.is_some() {
            return Err(RegistryError::Invalid(
                "that device id belongs to another person's machine".into(),
            ));
        }
        self.conn.execute(
            "UPDATE clients SET device_uid = NULL, device_name = NULL, run = 0
             WHERE device_uid = ?1 AND id <> ?2",
            params![uid, client_id],
        )?;
        self.conn.execute(
            "UPDATE clients SET device_uid = ?2, device_name = ?3, run = ?4 WHERE id = ?1",
            params![client_id, uid, name, run],
        )?;
        self.client_by_id(client_id)?
            .ok_or_else(|| RegistryError::NotFound("the client vanished".into()))
    }

    /// The active machines that announced a device, oldest first.
    pub fn devices(&self) -> Result<Vec<ClientRow>> {
        Ok(self
            .clients()?
            .into_iter()
            .filter(|c| c.revoked_at.is_none() && c.device_uid.is_some())
            .collect())
    }

    /// The active machine that announced the device `uid`.
    pub fn client_by_device(&self, uid: &str) -> Result<Option<ClientRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {CLIENT_COLS} FROM clients c JOIN people p ON p.id = c.person_id
                     WHERE c.device_uid = ?1 AND c.revoked_at IS NULL"
                ),
                [uid],
                client_from_row,
            )
            .optional()?)
    }

    /// Whether the client may still be used: it exists and is not revoked.
    pub fn is_active(&self, id: i64) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM clients WHERE id = ?1 AND revoked_at IS NULL",
                [id],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    /// Remember when a machine was last connected (or dropped), for "offline since".
    pub fn touch(&self, id: i64) {
        let _ = self.conn.execute(
            &format!("UPDATE clients SET last_seen_at = {NOW} WHERE id = ?1"),
            [id],
        );
    }

    /// The client a presented `Authorization` header belongs to. Every active digest is visited
    /// so the time taken does not depend on which one matched.
    pub fn authenticate(&self, header: Option<&str>) -> Option<Client> {
        let presented = digest(bearer(header?)?);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.id, c.legacy_name, c.machine_name, c.scopes, c.token_sha256,
                        p.id, p.name
                 FROM clients c JOIN people p ON p.id = c.person_id WHERE c.revoked_at IS NULL",
            )
            .ok()?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .ok()?;
        let mut found = None;
        for (id, legacy, machine, scopes, d, person_id, person) in rows.flatten() {
            if ct_eq(&d, &presented) {
                found = Some(Client {
                    id,
                    name: legacy.unwrap_or_else(|| machine.clone()),
                    person_id,
                    person,
                    machine,
                    scopes: Scopes::parse_list(&scopes).unwrap_or_default(),
                });
            }
        }
        found
    }

    /// Mirror `HUB_TOKENS` into the registry as clients of `person`: new entries are added,
    /// changed ones updated, and entries no longer listed are revoked. The environment is the
    /// authority for these.
    pub fn sync_legacy(&mut self, tokens: &Tokens, person: &str) -> Result<()> {
        let person = self.ensure_person(person)?;
        let listed: Vec<&str> = tokens.entries().iter().map(|e| e.name.as_str()).collect();
        let stale: Vec<i64> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, legacy_name FROM clients
                 WHERE legacy_name IS NOT NULL AND revoked_at IS NULL",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            let rows: Vec<(i64, String)> = rows.collect::<std::result::Result<_, _>>()?;
            rows.into_iter()
                .filter(|(_, n)| !listed.contains(&n.as_str()))
                .map(|(id, _)| id)
                .collect()
        };
        for id in stale {
            self.conn.execute(
                &format!("UPDATE clients SET revoked_at = {NOW} WHERE id = ?1"),
                [id],
            )?;
        }
        for e in tokens.entries() {
            let existing: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM clients WHERE legacy_name = ?1",
                    [&e.name],
                    |r| r.get(0),
                )
                .optional()?;
            match existing {
                Some(id) => {
                    self.conn.execute(
                        "UPDATE clients SET person_id = ?2, machine_name = ?3, scopes = ?4,
                         token_sha256 = ?5, revoked_at = NULL WHERE id = ?1",
                        params![id, person.id, e.name, e.scopes.to_csv(), e.digest.to_vec()],
                    )?;
                }
                None => {
                    self.conn.execute(
                        &format!(
                            "INSERT INTO clients (person_id, machine_name, scopes, token_sha256,
                             legacy_name, created_at) VALUES (?1, ?2, ?3, ?4, ?5, {NOW})"
                        ),
                        params![
                            person.id,
                            e.name,
                            e.scopes.to_csv(),
                            e.digest.to_vec(),
                            e.name
                        ],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Whether any client could authenticate.
    pub fn has_active_clients(&self) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM clients WHERE revoked_at IS NULL LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    // ---- pairing ----

    /// A one-time code for a machine that does not exist yet. Returns the code (shown once) and
    /// when it expires, in ms since the epoch.
    pub fn create_pairing(
        &mut self,
        person: &str,
        machine: &str,
        scopes: &Scopes,
        now_ms: i64,
    ) -> Result<(String, i64)> {
        let person = self.ensure_person(person)?;
        let machine = clean_name(machine, "a machine name")?;
        if self.active_machine(person.id, &machine)?.is_some() {
            return Err(RegistryError::Invalid(format!(
                "{} already has a machine called {machine:?}; revoke it first or pick another name",
                person.name
            )));
        }
        // Codes that can no longer be used are dropped as new ones are made.
        self.conn.execute(
            "DELETE FROM pairings WHERE used_at IS NOT NULL OR expires_ms < ?1",
            [now_ms - 86_400_000],
        )?;
        let code = new_code();
        let expires = now_ms + PAIR_CODE_MINUTES * 60_000;
        self.conn.execute(
            &format!(
                "INSERT INTO pairings (code_sha256, person_id, machine_name, scopes, created_at,
                 expires_ms) VALUES (?1, ?2, ?3, ?4, {NOW}, ?5)"
            ),
            params![
                digest(&code).to_vec(),
                person.id,
                machine,
                scopes.to_csv(),
                expires
            ],
        )?;
        Ok((code, expires))
    }

    /// Spend a pairing code. Single use: the second try, an expired code and a wrong code all
    /// answer the same way, so nothing tells a guesser which codes exist.
    pub fn redeem_pairing(&mut self, code: &str, now_ms: i64) -> Result<Paired> {
        let bad = || RegistryError::BadCode;
        let code = normalize_code(code).ok_or_else(bad)?;
        let hash = digest(&code).to_vec();
        let claimed = self.conn.execute(
            &format!(
                "UPDATE pairings SET used_at = {NOW}
                 WHERE code_sha256 = ?1 AND used_at IS NULL AND expires_ms >= ?2"
            ),
            params![hash, now_ms],
        )?;
        if claimed != 1 {
            return Err(bad());
        }
        let (person_id, machine, scopes): (i64, String, String) = self.conn.query_row(
            "SELECT person_id, machine_name, scopes FROM pairings WHERE code_sha256 = ?1",
            [&hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let person = self
            .person_by_id(person_id)?
            .ok_or_else(|| RegistryError::NotFound("the person is gone".into()))?;
        let scopes = Scopes::parse_list(&scopes).map_err(RegistryError::Invalid)?;
        let token = new_token();
        self.insert_client(&person, &machine, &scopes, &token, None)
    }
}

fn person_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: r.get(0)?,
        name: r.get(1)?,
    })
}

fn client_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ClientRow> {
    Ok(ClientRow {
        id: r.get(0)?,
        person_id: r.get(1)?,
        person: r.get(2)?,
        machine: r.get(3)?,
        scopes: Scopes::parse_list(&r.get::<_, String>(4)?).unwrap_or_default(),
        legacy: r.get(5)?,
        created_at: r.get(6)?,
        last_seen_at: r.get(7)?,
        revoked_at: r.get(8)?,
        device_uid: r.get(9)?,
        device_name: r.get(10)?,
        run: r.get(11)?,
    })
}

/// A device uid as a Helm makes it: 8 to [`MAX_DEVICE_UID`] letters, digits or dashes.
pub fn valid_device_uid(uid: &str) -> bool {
    (8..=MAX_DEVICE_UID).contains(&uid.len())
        && uid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A person or machine name: trimmed, 1 to [`MAX_NAME`] characters, no control characters.
fn clean_name(name: &str, what: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(RegistryError::Invalid(format!("{what} must not be empty")));
    }
    if name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(RegistryError::Invalid(format!(
            "{what} must be at most {MAX_NAME} characters, without control characters"
        )));
    }
    Ok(name.to_owned())
}

/// 64 random hex characters after a recognisable prefix.
fn new_token() -> String {
    format!(
        "hub_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Eight characters from [`CODE_ALPHABET`]. The alphabet has 32 symbols, so a random byte maps
/// onto it without bias.
fn new_code() -> String {
    let (a, b) = (
        uuid::Uuid::new_v4().into_bytes(),
        uuid::Uuid::new_v4().into_bytes(),
    );
    a[..4]
        .iter()
        .chain(&b[..4])
        .map(|byte| CODE_ALPHABET[(*byte % 32) as usize] as char)
        .collect()
}

/// A code as typed (any case, with a dash or spaces) in the form it is stored: eight symbols
/// from the alphabet, or `None`.
pub fn normalize_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == 8 && code.bytes().all(|b| CODE_ALPHABET.contains(&b))).then_some(code)
}

/// A code shown to a person: `ABCD-EFGH`.
pub fn display_code(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    #[test]
    fn pairing_is_single_use_timed_and_uniform_in_its_refusals() {
        let mut r = Registry::open_in_memory().unwrap();
        let scopes = Scopes::parse_list(crate::auth::DEFAULT_PAIRED_SCOPES).unwrap();
        let (code, expires) = r.create_pairing("Sam", "laptop", &scopes, NOW).unwrap();
        assert_eq!(expires, NOW + 15 * 60_000);
        assert_eq!(code.len(), 8);
        assert_eq!(
            normalize_code(&display_code(&code).to_lowercase()),
            Some(code.clone())
        );

        // A wrong code and an expired one are refused alike; neither spends the real one.
        let wrong = r.redeem_pairing("AAAA-AAAA", NOW).unwrap_err().to_string();
        let late = r
            .redeem_pairing(&code, expires + 1)
            .unwrap_err()
            .to_string();
        assert_eq!(wrong, late);
        assert!(r.redeem_pairing("short", NOW).is_err());

        let paired = r.redeem_pairing(&display_code(&code), NOW + 1000).unwrap();
        assert_eq!(paired.client.person, "Sam");
        assert_eq!(paired.client.machine, "laptop");
        assert!(paired.token.starts_with("hub_") && paired.token.len() == 68);
        // The second use fails with the same words.
        let again = r.redeem_pairing(&code, NOW + 2000).unwrap_err().to_string();
        assert_eq!(again, wrong);

        // The token authenticates; the stored form is a digest only.
        let c = r
            .authenticate(Some(&format!("Bearer {}", paired.token)))
            .unwrap();
        assert_eq!((c.person.as_str(), c.machine.as_str()), ("Sam", "laptop"));
        assert!(c.scopes.has(crate::auth::Scope::Relay));
        // A machine name is taken while it is active, free once it is revoked.
        assert!(r.create_pairing("sam", "Laptop", &scopes, NOW).is_err());
        r.revoke("laptop", None).unwrap();
        assert!(r
            .authenticate(Some(&format!("Bearer {}", paired.token)))
            .is_none());
        assert!(!r.is_active(c.id));
        assert!(r.create_pairing("sam", "Laptop", &scopes, NOW).is_ok());
    }

    #[test]
    fn legacy_tokens_mirror_the_environment() {
        let a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut r =
            Registry::from_legacy_spec(&format!("helm:{a}, remote:{b}:ro+app"), "owner").unwrap();
        let c = r.authenticate(Some(&format!("Bearer {b}"))).unwrap();
        assert_eq!((c.name.as_str(), c.person.as_str()), ("remote", "owner"));
        assert!(c.scopes.can_read_memory() && !c.scopes.can_write_memory());
        // They cannot be revoked from the command line...
        assert!(r.revoke("remote", None).is_err());
        // ...and drop out when the environment drops them.
        let only_helm = Tokens::parse(&format!("helm:{a}")).unwrap();
        r.sync_legacy(&only_helm, "owner").unwrap();
        assert!(r.authenticate(Some(&format!("Bearer {b}"))).is_none());
        assert!(r.authenticate(Some(&format!("Bearer {a}"))).is_some());
        // The same names bring the same ids back.
        let id = r.authenticate(Some(&format!("Bearer {a}"))).unwrap().id;
        r.sync_legacy(&only_helm, "owner").unwrap();
        assert_eq!(r.authenticate(Some(&format!("Bearer {a}"))).unwrap().id, id);
    }
}
