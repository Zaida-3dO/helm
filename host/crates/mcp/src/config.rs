//! The stored MCP config.
//!
//! One JSON file, `mcp-servers.json`, in the profile: owner-only, written atomically, read capped
//! at 128 KiB plus one byte. Literal secrets never reach it: every save moves a new literal value
//! into the credential store and keeps only its `secretId`, and forgets secrets no server uses any
//! more. The revision is the SHA-256 of the canonical JSON of the parsed server list, so a
//! pretty-printed or hand-edited file has the revision of its content, not of its bytes.

use crate::types::*;
use crate::validate::{validate_list, Invalid};
use helm_core::credentials::CredentialStore;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const FILE_NAME: &str = "mcp-servers.json";
const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
}

impl From<Invalid> for StoreError {
    fn from(e: Invalid) -> Self {
        StoreError::Invalid(e.0)
    }
}

pub const STALE: &str =
    "The MCP settings changed since they were read. List the servers again, then retry with the new revision.";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileShape {
    version: u32,
    servers: Vec<McpServer>,
}

pub fn revision_of(servers: &[McpServer]) -> String {
    let v = serde_json::to_value(servers).expect("servers serialise");
    helm_core::sha256_hex(helm_core::revisions::canonical_json(&v).as_bytes())
}

/// What a stored literal is bound to in the credential store. A profile's own value is bound to
/// that profile as well, so it cannot be read back as another profile's or as the shared one.
pub(crate) fn identity(server_id: &str, secret_id: &str, profile: Option<&str>) -> String {
    match profile {
        Some(p) => format!("mcp-literal:{server_id}:{secret_id}:{p}"),
        None => format!("mcp-literal:{server_id}:{secret_id}"),
    }
}

type Listener = Box<dyn Fn(&str) + Send + Sync>;
type StrMap = BTreeMap<String, String>;

/// The shared service (one per app). All writes are serialised by its lock.
pub struct McpStore {
    path: PathBuf,
    creds: CredentialStore,
    lock: Mutex<()>,
    listeners: Mutex<Vec<Listener>>,
    /// One lock per server id for refreshing its browser sign-in (see [`crate::oauth_ops`]).
    refresh_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl McpStore {
    /// `profile_dir` holds the config file; `creds` the literal secrets.
    pub fn new(profile_dir: &Path, creds: CredentialStore) -> Self {
        Self {
            path: profile_dir.join(FILE_NAME),
            creds,
            lock: Mutex::new(()),
            listeners: Mutex::new(Vec::new()),
            refresh_locks: Mutex::new(HashMap::new()),
        }
    }

    /// The lock a server's sign-in refreshes and saves take, so two callers never spend the
    /// same refresh token.
    pub(crate) fn refresh_lock(&self, server_id: &str) -> Arc<Mutex<()>> {
        self.refresh_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(server_id.to_owned())
            .or_default()
            .clone()
    }

    /// The credential store this profile uses, for the browser sign-ins [`crate::oauth_ops`]
    /// keeps beside the servers' literal secrets.
    pub fn creds(&self) -> &helm_core::credentials::CredentialStore {
        &self.creds
    }

    /// Called with the new revision after every successful save (the worker re-send hangs here).
    pub fn on_change(&self, f: impl Fn(&str) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Box::new(f));
    }

    fn invalid_file(&self) -> StoreError {
        StoreError::Invalid(format!(
            "Invalid MCP settings in {}. Fix or remove the file.",
            FILE_NAME
        ))
    }

    /// The parsed server list; a missing file is an empty list.
    pub fn load(&self) -> Result<Vec<McpServer>, StoreError> {
        let bytes = match helm_core::privfs::read_capped(&self.path, MAX_CONFIG_BYTES as u64 + 1) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(self.invalid_file()),
        };
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(self.invalid_file());
        }
        let f: FileShape = serde_json::from_slice(&bytes).map_err(|_| self.invalid_file())?;
        if f.version != FILE_VERSION || validate_list(&f.servers).is_err() {
            return Err(self.invalid_file());
        }
        // A stored file never holds a literal value, shared or a profile's own.
        if f.servers.iter().any(|s| {
            refs(s).any(|r| {
                (r.value.is_some() && r.kind == RefKind::Literal)
                    || r.profiles.values().any(|p| p.value.is_some())
            })
        }) {
            return Err(self.invalid_file());
        }
        Ok(f.servers)
    }

    pub fn snapshot(&self) -> Result<McpEditorSnapshot, StoreError> {
        let servers = self.load()?;
        Ok(McpEditorSnapshot {
            revision: revision_of(&servers),
            servers,
        })
    }

    /// The steps `mutate` and `preview` share, under the caller's lock: read the list, check
    /// `expected` against its revision, apply `f`, give a new hub memory server its person
    /// header, validate, and refuse a saved secret the server did not already have. Returns the
    /// list before and after; nothing is sealed, written or announced.
    fn checked(
        &self,
        expected: Option<&str>,
        f: impl FnOnce(&mut Vec<McpServer>) -> Result<(), StoreError>,
    ) -> Result<(Vec<McpServer>, Vec<McpServer>), StoreError> {
        let before = self.load()?;
        if let Some(exp) = expected {
            if exp != revision_of(&before) {
                return Err(StoreError::Conflict(STALE.into()));
            }
        }
        let mut after = before.clone();
        f(&mut after)?;
        // A hub memory server made now sends the chat's person from the start.
        for s in after.iter_mut() {
            if !before.iter().any(|b| b.id == s.id) {
                add_person_header(s);
            }
        }
        validate_list(&after)?;
        // A kept secret id must be one this same server already had, and a profile's own value
        // must be one that same profile already had: an id never moves to another profile.
        let owned: BTreeMap<&str, BTreeSet<(Option<String>, String)>> = before
            .iter()
            .map(|s| (s.id.as_str(), secret_slots(s)))
            .collect();
        for s in &after {
            let had = owned.get(s.id.as_str());
            let foreign = secret_slots(s)
                .iter()
                .any(|slot| !had.is_some_and(|h| h.contains(slot)));
            if foreign {
                return Err(StoreError::Invalid(format!(
                    "{}: a saved secret was referenced that this server does not have. Enter the value again.",
                    s.name
                )));
            }
        }
        Ok((before, after))
    }

    /// What [`McpStore::mutate`] would change, without changing anything: the list before and
    /// after `f`, checked exactly as a save checks it. Nothing is sealed, written or announced,
    /// so a literal value in `f`'s result stays in memory only. A caller that shows the result
    /// to someone must show names, never values.
    pub fn preview(
        &self,
        expected: Option<&str>,
        f: impl FnOnce(&mut Vec<McpServer>) -> Result<(), StoreError>,
    ) -> Result<(Vec<McpServer>, Vec<McpServer>), StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.checked(expected, f)
    }

    /// Read, check `expected` against the current revision, apply `f`, validate, seal new
    /// literals, write, forget orphaned secrets, notify. Nothing is written if any step fails.
    pub fn mutate(
        &self,
        expected: Option<&str>,
        f: impl FnOnce(&mut Vec<McpServer>) -> Result<(), StoreError>,
    ) -> Result<McpEditorSnapshot, StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let (before, mut after) = self.checked(expected, f)?;
        let minted = self.seal_literals(&mut after)?;
        let bytes = serde_json::to_vec_pretty(&FileShape {
            version: FILE_VERSION,
            servers: after.clone(),
        })
        .expect("config serialises");
        if bytes.len() > MAX_CONFIG_BYTES {
            self.forget_all(&minted);
            return Err(StoreError::Invalid(
                "The MCP configuration is larger than 128 KiB.".into(),
            ));
        }
        if let Some(dir) = self.path.parent() {
            helm_core::privfs::create_private_dir_all(dir).map_err(|e| {
                StoreError::Unavailable(format!("Could not save MCP settings: {e}"))
            })?;
        }
        if let Err(e) = helm_core::privfs::write_atomic(&self.path, &bytes) {
            self.forget_all(&minted);
            return Err(StoreError::Unavailable(format!(
                "Could not save MCP settings: {e}"
            )));
        }
        let kept: BTreeSet<String> = after.iter().flat_map(secret_ids).collect();
        let orphans: Vec<String> = before
            .iter()
            .flat_map(secret_ids)
            .filter(|id| !kept.contains(id))
            .collect();
        self.forget_all(&orphans);
        // A browser sign-in belongs to one server at one address with one set of sign-in
        // settings: removing the server, or changing either, forgets it.
        for old in &before {
            let same = after.iter().find(|n| n.id == old.id).is_some_and(|n| {
                n.transport.url == old.transport.url && n.transport.oauth == old.transport.oauth
            });
            if !same {
                crate::oauth_ops::forget_sign_in(&self.creds, &old.id);
            }
        }
        // Google accounts belong to the Google accounts server: when the last one is removed,
        // the accounts and their refresh tokens go too — no screen is left to remove them from.
        let google = |list: &[McpServer]| {
            list.iter()
                .any(|s| s.preset.as_deref() == Some(crate::google::PRESET_ID))
        };
        if google(&before) && !google(&after) {
            crate::google::forget_all_accounts(&self.creds);
        }
        let revision = revision_of(&after);
        for l in self
            .listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
        {
            l(&revision);
        }
        Ok(McpEditorSnapshot {
            revision,
            servers: after,
        })
    }

    fn seal_literals(&self, servers: &mut [McpServer]) -> Result<Vec<String>, StoreError> {
        let mut minted = Vec::new();
        for s in servers.iter_mut() {
            let sid = s.id.clone();
            for r in refs_mut(s) {
                if r.kind != RefKind::Literal {
                    continue;
                }
                if let Some(v) = r.value.take() {
                    r.secret_id = Some(self.seal_one(&sid, None, &v, &mut minted)?);
                }
                for (profile, p) in r.profiles.iter_mut() {
                    if let Some(v) = p.value.take() {
                        p.secret_id = Some(self.seal_one(&sid, Some(profile), &v, &mut minted)?);
                    }
                }
            }
        }
        Ok(minted)
    }

    /// Store one literal (shared, or a profile's own) and return its new id; `minted` collects
    /// every id stored so far, so a failure forgets them all.
    fn seal_one(
        &self,
        server_id: &str,
        profile: Option<&str>,
        value: &str,
        minted: &mut Vec<String>,
    ) -> Result<String, StoreError> {
        let id = format!("lit_{}", uuid::Uuid::new_v4().simple());
        if let Err(e) = self
            .creds
            .write(&id, &identity(server_id, &id, profile), value.as_bytes())
        {
            self.forget_all(minted);
            return Err(StoreError::Unavailable(format!(
                "Could not store the secret in Helm's credential store: {e}"
            )));
        }
        minted.push(id.clone());
        Ok(id)
    }

    fn forget_all(&self, ids: &[String]) {
        for id in ids {
            let _ = self.creds.forget(id);
        }
    }

    /// Every enabled server with its references resolved. `env` reads a host
    /// environment variable.
    pub fn resolve_enabled(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<(String, Vec<ResolvedServer>), StoreError> {
        self.resolve_enabled_for(env, None)
    }

    /// [`McpStore::resolve_enabled`] for one chat: a header that names the chat's person is sent
    /// with `person` (the hub's id for the person its profile is linked to), or left out when
    /// there is none.
    pub fn resolve_enabled_for(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
        person: Option<&str>,
    ) -> Result<(String, Vec<ResolvedServer>), StoreError> {
        self.resolve_enabled_where(env, person, false)
    }

    /// As [`McpStore::resolve_enabled_for`], leaving out the servers that send the chat's person
    /// when `without_chat_person` is set (a chat whose profile is linked to no one on the hub).
    pub fn resolve_enabled_where(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
        person: Option<&str>,
        without_chat_person: bool,
    ) -> Result<(String, Vec<ResolvedServer>), StoreError> {
        self.resolve_enabled_scoped(env, person, without_chat_person, false)
    }

    /// As [`McpStore::resolve_enabled_where`], also leaving out the servers that belong to this
    /// PC's own person (the signed-in browser) when `not_home` says the chat belongs to another
    /// profile than the home one.
    pub fn resolve_enabled_scoped(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
        person: Option<&str>,
        without_chat_person: bool,
        not_home: bool,
    ) -> Result<(String, Vec<ResolvedServer>), StoreError> {
        self.resolve_enabled_in(env, person, without_chat_person, not_home, None)
    }

    /// As [`McpStore::resolve_enabled_scoped`] for a chat of `profile`: a secret that has a value
    /// of its own for that profile is read as that value, every other secret as its shared one.
    pub fn resolve_enabled_in(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
        person: Option<&str>,
        without_chat_person: bool,
        not_home: bool,
        profile: Option<&str>,
    ) -> Result<(String, Vec<ResolvedServer>), StoreError> {
        let servers = self.load()?;
        let revision = revision_of(&servers);
        let resolved = servers
            .iter()
            .filter(|s| {
                s.enabled
                    && !(without_chat_person && sends_chat_person(s))
                    && !(not_home && is_home_only(s))
            })
            .map(|s| self.resolve_one(s, env, person, profile, not_home))
            .collect();
        Ok((revision, resolved))
    }

    /// Whether any enabled server sends the chat's person or has a secret of a profile's own, so
    /// its config differs per chat.
    pub fn uses_chat_person(&self) -> bool {
        self.load()
            .map(|l| {
                l.iter().any(|s| {
                    s.enabled
                        && (sends_chat_person(s)
                            || is_home_only(s)
                            || refs(s).any(|r| !r.profiles.is_empty()))
                })
            })
            .unwrap_or(false)
    }

    /// One enabled server by id, resolved for `person` (see [`McpStore::resolve_enabled_for`]).
    pub fn resolve_server(
        &self,
        id: &str,
        person: Option<&str>,
    ) -> Result<Option<ResolvedServer>, StoreError> {
        Ok(self
            .load()?
            .iter()
            .find(|s| s.enabled && s.id == id)
            .map(|s| self.resolve_one(s, &|k| std::env::var(k).ok(), person, None, false)))
    }

    /// One draft server with its references resolved, for a connection test. A literal secret
    /// that was typed but not saved yet is used as typed; saved ones are read from the credential
    /// store as usual. A browser sign-in is attached only when the draft is at the address the
    /// sign-in was made for.
    pub fn resolve_draft(&self, s: &McpServer) -> ResolvedServer {
        self.resolve_one(s, &|k| std::env::var(k).ok(), None, None, false)
    }

    /// The server list as another PC of the same person may see it: every secret value, saved
    /// secret id and credential-file path is blanked to an empty secret slot, while environment
    /// names and the rest of the shape cross as they stand. Nothing blanked can resolve here;
    /// the row waits for the person to enter the secret again.
    pub fn export_copy(&self) -> Result<Vec<McpServer>, StoreError> {
        Ok(self.load()?.iter().map(copy_shape).collect())
    }

    /// Servers copied from another PC of the same person, in the form [`McpStore::export_copy`]
    /// sends: each secret slot blank, each environment name kept. An entry whose id this PC
    /// already has is skipped, never overwritten, and nothing still carrying a secret value, a
    /// saved id or another PC's file path is accepted. A blanked slot saves and reads back as
    /// "enter it again", so the row says it needs its token.
    pub fn import_copy(
        &self,
        servers: &[McpServer],
        expected: Option<&str>,
    ) -> Result<McpEditorSnapshot, StoreError> {
        for s in servers {
            let dirty = refs(s).any(|r| {
                r.secret_id.is_some()
                    || !r.profiles.is_empty()
                    || (r.kind != RefKind::Environment && r.value.is_some())
            });
            if dirty {
                return Err(StoreError::Invalid(format!(
                    "{}: a copied server may not carry a secret value, a saved secret id or a file path from the other PC.",
                    s.name
                )));
            }
        }
        self.mutate(expected, |list| {
            for s in servers {
                if !list.iter().any(|e| e.id == s.id) {
                    list.push(s.clone());
                }
            }
            Ok(())
        })
    }

    /// Give an existing hub memory server the person header, once: chats that belong to another
    /// person then use that person's memories. Returns whether it changed anything. A person who
    /// removes the header afterwards is not given it again.
    pub fn add_person_headers(&self) -> Result<bool, StoreError> {
        let marker = self.path.with_file_name(PERSON_MARKER);
        if marker.exists() || !self.load()?.iter().any(wants_person_header) {
            return Ok(false);
        }
        self.mutate(None, |l| {
            l.iter_mut().for_each(|s| {
                add_person_header(s);
            });
            Ok(())
        })?;
        let _ = helm_core::privfs::write_atomic(&marker, b"1\n");
        Ok(true)
    }

    fn resolve_one(
        &self,
        s: &McpServer,
        env: &dyn Fn(&str) -> Option<String>,
        person: Option<&str>,
        profile: Option<&str>,
        not_home: bool,
    ) -> ResolvedServer {
        let t = &s.transport;
        let mut out = ResolvedServer {
            id: s.id.clone(),
            name: s.name.clone(),
            preset: s.preset.clone(),
            transport: t.kind,
            command: t.command.clone(),
            args: t.args.clone(),
            env: BTreeMap::new(),
            url: t.url.clone(),
            headers: BTreeMap::new(),
            allow_private_http: s.allow_private_http,
            secrets: Vec::new(),
            guidance: crate::presets::guidance_for(s.preset.as_deref()),
            error: None,
            oauth: s.transport.oauth.is_some() && t.kind.is_remote(),
            host_served: false,
        };
        let mut secrets = Vec::new();
        // The variables a preset names that are not secrets (the browser's list of sites, say):
        // ordinary words that must not be blanked out of tool results.
        let plain_env: Vec<String> = crate::presets::presets()
            .into_iter()
            .find(|p| s.preset.as_deref() == Some(p.id.as_str()))
            .map(|p| {
                p.draft_env
                    .into_iter()
                    .filter(|n| !p.secret_env.contains(n))
                    .collect()
            })
            .unwrap_or_default();
        let mut run = || -> Result<(StrMap, StrMap), String> {
            let mut e = BTreeMap::new();
            for (k, r) in &t.env {
                let mut kept = Vec::new();
                let value = self.resolve_ref(
                    &s.id,
                    r,
                    profile,
                    RefSite {
                        header: false,
                        label: &format!("Environment {k}"),
                    },
                    env,
                    &mut kept,
                )?;
                if !plain_env.contains(k) {
                    secrets.extend(kept);
                }
                e.insert(k.clone(), value);
            }
            let mut h = BTreeMap::new();
            for (k, r) in &t.headers {
                if r.kind == RefKind::ChatPerson {
                    // Not a secret: a chat with no person simply sends no such header.
                    if let Some(p) = person {
                        h.insert(
                            k.clone(),
                            format!("{}{p}", r.prefix.as_deref().unwrap_or("")),
                        );
                    }
                    continue;
                }
                h.insert(
                    k.clone(),
                    self.resolve_ref(
                        &s.id,
                        r,
                        profile,
                        RefSite {
                            header: true,
                            label: &format!("Header {k}"),
                        },
                        env,
                        &mut secrets,
                    )?,
                );
            }
            Ok((e, h))
        };
        match run() {
            Ok((e, h)) => {
                out.env = e;
                out.headers = h;
                out.secrets = secrets;
            }
            Err(msg) => out.error = Some(msg),
        }
        if s.preset.as_deref() == Some(crate::google::PRESET_ID) && out.error.is_none() {
            if let Some(profile_dir) = self.path.parent() {
                let scope = crate::google::GoogleScope { profile, not_home };
                crate::google::prepare(&self.creds, profile_dir, scope, &mut out);
            }
        }
        // The built-in browser checks the grants Helm mints when the user approves one of its
        // steps (see crate::owner_step) with a key only Helm and that process hold. It joins the
        // secrets, so the worker scrubs it from everything the server says.
        if s.preset.as_deref() == Some(crate::presets::BROKER_PRESET_ID)
            && t.kind == TransportKind::Stdio
            && out.error.is_none()
        {
            if let Some(key) = crate::owner_step::grant_key() {
                out.env
                    .insert(crate::owner_step::SECRET_ENV.into(), key.to_owned());
                out.secrets.push(key.to_owned());
            }
        }
        // A server that signs in through the browser is only reachable with that sign-in: its
        // header is added here (refreshed first when the token is nearly spent), and the token
        // joins the redaction set like any other resolved secret. Without a sign-in the server
        // reports as not connected, and no chat tries it without a credential.
        if out.oauth && out.error.is_none() {
            match crate::oauth_ops::current_access(self, s, crate::oauth_ops::Want::IfDue) {
                Ok(access) => {
                    out.headers
                        .insert("Authorization".into(), format!("Bearer {access}"));
                    out.secrets.push(access);
                }
                Err(why) => out.error = Some(why),
            }
        }
        out
    }

    fn resolve_ref(
        &self,
        server_id: &str,
        r: &ValueRef,
        profile: Option<&str>,
        site: RefSite<'_>,
        env: &dyn Fn(&str) -> Option<String>,
        secrets: &mut Vec<String>,
    ) -> Result<String, String> {
        let RefSite { header, label } = site;
        // The chat's profile has its own value for this secret: read that one, bound to the
        // profile, and never the shared one or another profile's.
        let own = profile
            .filter(|_| r.kind == RefKind::Literal)
            .and_then(|p| r.profiles.get(p).map(|v| (p, v)));
        let raw = match r.kind {
            RefKind::Literal if own.is_some() => {
                let (p, v) = own.expect("checked");
                match (&v.secret_id, &v.value) {
                    (Some(id), _) => {
                        let bytes = self
                            .creds
                            .read(id, &identity(server_id, id, Some(p)))
                            .map_err(|_| format!("{label}: the saved secret could not be read. Enter it again in Settings → MCP."))?;
                        String::from_utf8(bytes)
                            .map_err(|_| format!("{label}: the saved secret is not text."))?
                    }
                    // Typed in the editor and not saved yet (only a connection test sees this).
                    (None, Some(typed)) => typed.clone(),
                    (None, None) => String::new(),
                }
            }
            RefKind::Environment => {
                let name = r.value.as_deref().unwrap_or("");
                env(name).ok_or_else(|| format!("{label}: the variable {name} is not set."))?
            }
            RefKind::File => read_secret_file(r.value.as_deref().unwrap_or(""))
                .map_err(|why| format!("{label}: {why}"))?,
            RefKind::Literal
                if r.secret_id.as_deref().unwrap_or("").is_empty()
                    && r.value.as_deref().is_some_and(|v| !v.is_empty()) =>
            {
                // Typed in the editor and not saved yet (only a connection test sees this form).
                r.value.clone().unwrap_or_default()
            }
            RefKind::ChatPerson => {
                return Err(format!(
                    "{label}: the chat's person is only sent as a header."
                ))
            }
            RefKind::Literal => {
                let id = r.secret_id.as_deref().unwrap_or("");
                let bytes = self
                    .creds
                    .read(id, &identity(server_id, id, None))
                    .map_err(|_| format!("{label}: the saved secret could not be read. Enter it again in Settings → MCP."))?;
                String::from_utf8(bytes)
                    .map_err(|_| format!("{label}: the saved secret is not text."))?
            }
        };
        if r.kind != RefKind::Literal && (raw.is_empty() || raw.contains(['\r', '\n'])) {
            return Err(format!("{label}: the value is empty or spans lines."));
        }
        if header && raw.chars().any(|c| c.is_control() && c != '\t') {
            return Err(format!("{label}: the value contains control characters."));
        }
        if !raw.is_empty() {
            secrets.push(raw.clone());
        }
        Ok(format!("{}{}", r.prefix.as_deref().unwrap_or(""), raw))
    }
}

/// Where a reference is used: whether it is an HTTP header value, and how to name it in an error.
#[derive(Clone, Copy)]
struct RefSite<'a> {
    header: bool,
    label: &'a str,
}

fn read_secret_file(path: &str) -> Result<String, String> {
    if !crate::validate::is_local_absolute(path) {
        return Err("the path is not a file on this computer.".into());
    }
    let p = Path::new(path);
    let meta = std::fs::symlink_metadata(p).map_err(|_| "the file cannot be read.".to_string())?;
    if !meta.file_type().is_file() {
        return Err("the path is not a regular file.".into());
    }
    if meta.len() > MAX_LITERAL_BYTES as u64 {
        return Err("the file is larger than 16 KiB.".into());
    }
    let bytes = helm_core::privfs::read_capped(p, MAX_LITERAL_BYTES as u64)
        .map_err(|_| "the file cannot be read.".to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "the file is not text.".to_string())?;
    Ok(text.trim().to_owned())
}

/// One server with its secrets blanked for the copy: an environment name is read on the PC that
/// connects, so it crosses as it stands; a literal value, a saved secret id and a credential
/// file's path are that PC's own, and the slot crosses empty as a secret waiting to be entered.
fn copy_shape(s: &McpServer) -> McpServer {
    let mut c = s.clone();
    for r in refs_mut(&mut c) {
        if r.kind != RefKind::Environment && r.kind != RefKind::ChatPerson {
            r.kind = RefKind::Literal;
            r.value = None;
            r.secret_id = None;
        }
        // A profile's own value belongs to a profile of this PC: none of it crosses.
        r.profiles.clear();
    }
    c
}

pub(crate) fn refs(s: &McpServer) -> impl Iterator<Item = &ValueRef> {
    let t = &s.transport;
    t.env
        .values()
        .chain(t.headers.values())
        .chain(t.oauth.iter().filter_map(|o| o.client_secret.as_ref()))
}

fn refs_mut(s: &mut McpServer) -> impl Iterator<Item = &mut ValueRef> {
    let t = &mut s.transport;
    t.env
        .values_mut()
        .chain(t.headers.values_mut())
        .chain(t.oauth.iter_mut().filter_map(|o| o.client_secret.as_mut()))
}

/// The header that tells the hub whose memories a chat's calls are for.
pub const PERSON_HEADER: &str = "X-Helm-Person";
/// Written beside the config once existing hub servers have been given [`PERSON_HEADER`].
const PERSON_MARKER: &str = ".mcp-person-header";

/// A server that reaches the home person's own things — the signed-in browser — and so goes to
/// the chats of the home profile only, never to a household person's or a guest's.
fn is_home_only(s: &McpServer) -> bool {
    s.preset.as_deref() == Some(crate::presets::BROKER_PRESET_ID)
}

fn sends_chat_person(s: &McpServer) -> bool {
    s.transport
        .headers
        .values()
        .any(|r| r.kind == RefKind::ChatPerson)
}

/// A hub memory server (the preset's, or one that took its id) over the network that does not
/// send the person header yet.
fn wants_person_header(s: &McpServer) -> bool {
    let hub = s.preset.as_deref() == Some("helm_hub") || s.id == "helm_hub";
    hub && s.transport.kind.is_remote()
        && !s
            .transport
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case(PERSON_HEADER))
}

fn add_person_header(s: &mut McpServer) -> bool {
    if !wants_person_header(s) {
        return false;
    }
    s.transport.headers.insert(
        PERSON_HEADER.to_owned(),
        ValueRef {
            profiles: Default::default(),
            kind: RefKind::ChatPerson,
            value: None,
            secret_id: None,
            prefix: None,
        },
    );
    true
}

/// Every saved secret id of a server, shared and per profile.
fn secret_ids(s: &McpServer) -> BTreeSet<String> {
    secret_slots(s).into_iter().map(|(_, id)| id).collect()
}

/// Each saved secret id with the profile it belongs to (`None` for the shared value).
fn secret_slots(s: &McpServer) -> BTreeSet<(Option<String>, String)> {
    refs(s)
        .filter(|r| r.kind == RefKind::Literal)
        .flat_map(|r| {
            let shared = r.secret_id.clone().map(|id| (None, id));
            let own = r
                .profiles
                .iter()
                .filter_map(|(p, v)| v.secret_id.clone().map(|id| (Some(p.clone()), id)));
            shared.into_iter().chain(own)
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use helm_platform::keychain::MemoryKeychain;
    use std::sync::Arc;

    pub fn store(dir: &Path) -> McpStore {
        let creds = CredentialStore::new(
            &dir.join("credentials"),
            "testhash",
            Arc::new(MemoryKeychain::default()),
        );
        McpStore::new(dir, creds)
    }

    pub fn hub(token: &str) -> McpServer {
        let mut headers = BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            ValueRef {
                profiles: Default::default(),
                kind: RefKind::Literal,
                value: Some(token.into()),
                secret_id: None,
                prefix: Some("Bearer ".into()),
            },
        );
        McpServer {
            id: "hub".into(),
            name: "Hub".into(),
            enabled: true,
            transport: Transport {
                kind: TransportKind::Http,
                command: None,
                args: vec![],
                env: BTreeMap::new(),
                url: Some("https://example.com/mcp".into()),
                headers,
                oauth: None,
            },
            allow_private_http: false,
            preset: None,
        }
    }

    /// A saved built-in browser server: stdio, broker preset, the broker tool entry named.
    pub fn browser_server(id: &str) -> McpServer {
        McpServer {
            id: id.into(),
            name: "My browser".into(),
            enabled: true,
            transport: Transport {
                kind: TransportKind::Stdio,
                command: Some("node.exe".into()),
                args: vec!["bb/dist/src/bin/broker-tool.js".into()],
                env: BTreeMap::new(),
                url: None,
                headers: BTreeMap::new(),
                oauth: None,
            },
            allow_private_http: false,
            preset: Some(crate::presets::BROKER_PRESET_ID.into()),
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn the_resolved_server_carries_its_preset() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        st.mutate(None, |l| {
            let mut b = browser_server("bb");
            for (k, v) in [
                ("BROKER_ATTACHED_BROWSERS", "chrome"),
                ("BROKER_ATTACHED_TOKEN", "tok-secret-1234"),
            ] {
                b.transport.env.insert(
                    k.into(),
                    ValueRef {
                        profiles: Default::default(),
                        kind: RefKind::Literal,
                        value: Some(v.into()),
                        secret_id: None,
                        prefix: None,
                    },
                );
            }
            l.push(b);
            Ok(())
        })
        .unwrap();
        let (_, r) = st.resolve_enabled(&no_env).unwrap();
        assert_eq!(
            r[0].preset.as_deref(),
            Some(crate::presets::BROKER_PRESET_ID),
            "the worker recognises the built-in browser server by its preset"
        );
        // The grant key reaches the browser server only, and as a secret the worker scrubs.
        let key = crate::owner_step::grant_key().unwrap();
        assert_eq!(
            r[0].secrets,
            ["tok-secret-1234", key],
            "the token and the grant key are blanked out of results; the plain browser name is an ordinary word"
        );
        assert_eq!(r[0].env["BROKER_ATTACHED_BROWSERS"], "chrome");
        assert_eq!(
            r[0].env
                .get(crate::owner_step::SECRET_ENV)
                .map(String::as_str),
            Some(key)
        );
        assert!(!st
            .resolve_draft(&hub("t"))
            .env
            .contains_key(crate::owner_step::SECRET_ENV));
        assert_eq!(st.resolve_draft(&hub("t")).preset, None);
    }

    #[test]
    fn literal_never_reaches_the_file_and_resolves() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let snap = st
            .mutate(None, |l| {
                l.push(hub("s3cret-token-value"));
                Ok(())
            })
            .unwrap();
        let text = std::fs::read_to_string(d.path().join(FILE_NAME)).unwrap();
        assert!(!text.contains("s3cret-token-value"));
        let h = &snap.servers[0].transport.headers["Authorization"];
        assert!(h.value.is_none() && h.secret_id.is_some());
        let (_, resolved) = st.resolve_enabled(&no_env).unwrap();
        assert_eq!(
            resolved[0].headers["Authorization"],
            "Bearer s3cret-token-value"
        );
        assert_eq!(resolved[0].secrets, vec!["s3cret-token-value".to_string()]);
        assert!(resolved[0].error.is_none());
    }

    /// A secret can have a value of its own per profile: the chat's profile reads its own value,
    /// a profile with none reads the shared one, no value shows up in the file, the redaction set
    /// or another profile's config, and an id cannot be moved to another profile or the shared slot.
    #[test]
    fn a_profile_can_have_its_own_value_for_a_secret() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let plain = st
            .mutate(None, |l| {
                l.push(hub("shared-tok"));
                Ok(())
            })
            .unwrap();
        let own = |v: &str| ProfileSecret {
            value: Some(v.into()),
            secret_id: None,
        };
        let snap = st
            .mutate(Some(&plain.revision), |l| {
                let r = l[0].transport.headers.get_mut("Authorization").unwrap();
                r.profiles.insert("pa".into(), own("pa-tok"));
                r.profiles.insert("pb".into(), own("pb-tok"));
                Ok(())
            })
            .unwrap();
        assert_ne!(
            snap.revision, plain.revision,
            "a profile's value changes the revision"
        );
        let text = std::fs::read_to_string(d.path().join(FILE_NAME)).unwrap();
        assert!(!text.contains("pa-tok") && !text.contains("pb-tok"));
        assert!(st.uses_chat_person(), "the config now differs per chat");

        let for_profile = |p: Option<&str>| {
            let (_, r) = st
                .resolve_enabled_in(&no_env, None, false, false, p)
                .unwrap();
            (r[0].headers["Authorization"].clone(), r[0].secrets.clone())
        };
        assert_eq!(
            for_profile(Some("pa")),
            ("Bearer pa-tok".into(), vec!["pa-tok".into()])
        );
        assert_eq!(
            for_profile(Some("pb")),
            ("Bearer pb-tok".into(), vec!["pb-tok".into()])
        );
        let shared = (
            "Bearer shared-tok".to_string(),
            vec!["shared-tok".to_string()],
        );
        assert_eq!(
            for_profile(Some("pc")),
            shared,
            "a profile with none falls back"
        );
        assert_eq!(for_profile(None), shared);

        // Saving the list back unchanged keeps every value (the ids go round, the values do not).
        let again = st.mutate(Some(&snap.revision), |_| Ok(())).unwrap();
        assert_eq!(again.revision, snap.revision);
        assert_eq!(for_profile(Some("pa")).0, "Bearer pa-tok");

        // An id never moves to another profile or to the shared slot.
        let ids = |l: &[McpServer]| {
            let r = &l[0].transport.headers["Authorization"];
            (
                r.secret_id.clone().unwrap(),
                r.profiles["pa"].secret_id.clone().unwrap(),
            )
        };
        let (shared_id, pa_id) = ids(&again.servers);
        let moved = |f: fn(&mut ValueRef, &str, &str)| {
            st.mutate(None, |l| {
                f(
                    l[0].transport.headers.get_mut("Authorization").unwrap(),
                    &shared_id,
                    &pa_id,
                );
                Ok(())
            })
        };
        assert!(
            moved(|r, _, pa| r.profiles.get_mut("pb").unwrap().secret_id = Some(pa.into()))
                .is_err()
        );
        assert!(moved(
            |r, shared, _| r.profiles.get_mut("pb").unwrap().secret_id = Some(shared.into())
        )
        .is_err());
        assert!(moved(|r, _, pa| r.secret_id = Some(pa.into())).is_err());
        assert_eq!(
            for_profile(Some("pb")).0,
            "Bearer pb-tok",
            "refused saves change nothing"
        );

        // The shared value, a plain profile id and one value per profile are all that is accepted.
        assert!(moved(|r, _, _| {
            r.profiles.insert(
                "bad id".into(),
                ProfileSecret {
                    value: Some("x".into()),
                    secret_id: None,
                },
            );
        })
        .is_err());
        assert!(moved(|r, _, _| {
            r.profiles.insert("pc".into(), ProfileSecret::default());
        })
        .is_err());

        // Another PC's copy carries none of it.
        let copy = st.export_copy().unwrap();
        assert!(copy[0].transport.headers["Authorization"]
            .profiles
            .is_empty());
        let mut dirty = copy[0].clone();
        dirty.id = "other".into();
        dirty
            .transport
            .headers
            .get_mut("Authorization")
            .unwrap()
            .profiles
            .insert(
                "pa".into(),
                ProfileSecret {
                    value: None,
                    secret_id: Some(pa_id.clone()),
                },
            );
        assert!(st.import_copy(&[dirty], None).is_err());

        // Dropping a profile's value forgets its secret; the others stay.
        let after = st
            .mutate(None, |l| {
                l[0].transport
                    .headers
                    .get_mut("Authorization")
                    .unwrap()
                    .profiles
                    .remove("pa");
                Ok(())
            })
            .unwrap();
        assert!(st
            .creds()
            .read(&pa_id, &identity("hub", &pa_id, Some("pa")))
            .is_err());
        assert_eq!(for_profile(Some("pa")), shared);
        assert_eq!(for_profile(Some("pb")).0, "Bearer pb-tok");
        assert_ne!(after.revision, snap.revision);
    }

    #[test]
    fn stale_revision_is_refused_and_nothing_changes() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let r0 = st.snapshot().unwrap().revision;
        st.mutate(Some(&r0), |l| {
            l.push(hub("a"));
            Ok(())
        })
        .unwrap();
        let err = st
            .mutate(Some(&r0), |l| {
                l.clear();
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
        assert_eq!(st.load().unwrap().len(), 1);
    }

    #[test]
    fn revision_is_of_content_not_bytes() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let snap = st
            .mutate(None, |l| {
                l.push(hub("a"));
                Ok(())
            })
            .unwrap();
        let p = d.path().join(FILE_NAME);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_vec(&v).unwrap()).unwrap(); // compact rewrite
        assert_eq!(st.snapshot().unwrap().revision, snap.revision);
        assert_eq!(snap.revision.len(), 64);
    }

    #[test]
    fn replacing_a_literal_forgets_the_old_secret() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let s1 = st
            .mutate(None, |l| {
                l.push(hub("one"));
                Ok(())
            })
            .unwrap();
        let old = s1.servers[0].transport.headers["Authorization"]
            .secret_id
            .clone()
            .unwrap();
        st.mutate(None, |l| {
            l[0] = hub("two");
            Ok(())
        })
        .unwrap();
        assert!(!d
            .path()
            .join("credentials")
            .join(format!("{old}.cred"))
            .exists());
        let (_, r) = st.resolve_enabled(&no_env).unwrap();
        assert_eq!(r[0].headers["Authorization"], "Bearer two");
    }

    #[test]
    fn a_secret_id_from_another_server_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let s1 = st
            .mutate(None, |l| {
                l.push(hub("one"));
                Ok(())
            })
            .unwrap();
        let stolen = s1.servers[0].transport.headers["Authorization"].clone();
        let err = st
            .mutate(None, |l| {
                let mut other = hub("x");
                other.id = "other".into();
                other
                    .transport
                    .headers
                    .insert("Authorization".into(), stolen);
                l.push(other);
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)));
    }

    #[test]
    fn invalid_file_is_one_error_naming_the_file() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        std::fs::write(d.path().join(FILE_NAME), b"{not json").unwrap();
        let e = st.load().unwrap_err();
        assert!(e.to_string().contains(FILE_NAME));
        // A literal value in the file is also refused.
        let f = serde_json::json!({"version":1,"servers":[hub("leak")]});
        std::fs::write(d.path().join(FILE_NAME), f.to_string()).unwrap();
        assert!(st.load().is_err());
    }

    #[test]
    fn environment_and_file_references_resolve_at_connect_time() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let tokfile = d.path().join("tok.txt");
        std::fs::write(&tokfile, "  from-file \n").unwrap();
        let mut s = hub("x");
        s.transport.headers.clear();
        s.transport.headers.insert(
            "X-Env".into(),
            ValueRef {
                profiles: Default::default(),
                kind: RefKind::Environment,
                value: Some("HUB_TOKEN".into()),
                secret_id: None,
                prefix: None,
            },
        );
        s.transport.headers.insert(
            "X-File".into(),
            ValueRef {
                profiles: Default::default(),
                kind: RefKind::File,
                value: Some(tokfile.to_string_lossy().into_owned()),
                secret_id: None,
                prefix: None,
            },
        );
        st.mutate(None, |l| {
            l.push(s);
            Ok(())
        })
        .unwrap();
        let env = |n: &str| (n == "HUB_TOKEN").then(|| "from-env".to_string());
        let (_, r) = st.resolve_enabled(&env).unwrap();
        assert_eq!(r[0].headers["X-Env"], "from-env");
        assert_eq!(r[0].headers["X-File"], "from-file");
        let (_, r) = st.resolve_enabled(&no_env).unwrap();
        assert!(r[0].error.as_deref().unwrap().contains("HUB_TOKEN"));
        assert!(r[0].headers.is_empty());
    }

    #[test]
    fn the_hub_server_sends_the_chats_person_when_it_has_one() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let memory = |id: &str, preset: Option<&str>| {
            let mut s = hub("tok-abc");
            s.id = id.into();
            s.preset = preset.map(Into::into);
            s
        };
        // A hub server made now has the header; another server does not.
        st.mutate(None, |l| {
            l.push(memory("helm_hub", Some("helm_hub")));
            l.push(memory("plain", None));
            Ok(())
        })
        .unwrap();
        let made = st.load().unwrap();
        assert!(sends_chat_person(&made[0]) && !sends_chat_person(&made[1]));
        // One saved before the header existed is given it once, and only that one.
        st.mutate(None, |l| {
            l[0].transport.headers.remove(PERSON_HEADER);
            Ok(())
        })
        .unwrap();
        assert!(!st.uses_chat_person());
        assert!(st.add_person_headers().unwrap());
        let after = st.load().unwrap();
        assert!(sends_chat_person(&after[0]) && !sends_chat_person(&after[1]));
        assert!(st.uses_chat_person());
        assert!(!st.add_person_headers().unwrap(), "only once");
        // A person who takes the header off keeps it off; a hub server made later has it.
        st.mutate(None, |l| {
            l[0].transport.headers.remove(PERSON_HEADER);
            l.push(memory("memories", Some("helm_hub")));
            Ok(())
        })
        .unwrap();
        assert!(!st.add_person_headers().unwrap());
        let now = st.load().unwrap();
        assert!(!sends_chat_person(&now[0]) && sends_chat_person(&now[2]));

        // Resolved per chat: the person's id when there is one, no header when there is not, and
        // nothing about it is a secret.
        let (_, with) = st.resolve_enabled_for(&no_env, Some("7")).unwrap();
        let (_, without) = st.resolve_enabled_for(&no_env, None).unwrap();
        assert_eq!(with[2].headers[PERSON_HEADER], "7");
        assert!(!without[2].headers.contains_key(PERSON_HEADER));
        assert_eq!(with[2].secrets, ["tok-abc"]);
        let one = st.resolve_server("memories", Some("9")).unwrap().unwrap();
        assert_eq!(one.headers[PERSON_HEADER], "9");
        assert!(st.resolve_server("nope", None).unwrap().is_none());
    }

    /// Seed one server of each secret kind: a literal header, an env header, a credential-file
    /// env, a literal env, and a browser sign-in client with its client secret on its own server
    /// (a sign-in server carries no Authorization header of its own).
    fn seed_each_secret_kind(st: &McpStore, tokfile: &Path) {
        let mut s = hub("s3cret-header-token");
        s.id = "mix".into();
        s.name = "Mix".into();
        s.transport.headers.insert(
            "X-Env".into(),
            ValueRef {
                profiles: Default::default(),
                kind: RefKind::Environment,
                value: Some("MIX_TOKEN".into()),
                secret_id: None,
                prefix: None,
            },
        );
        let mut g = stdio_server();
        g.id = "signed".into();
        g.name = "Signed".into();
        g.transport.kind = TransportKind::Http;
        g.transport.command = None;
        g.transport.args = vec![];
        g.transport.url = Some("https://signed.example/mcp".into());
        g.transport.oauth = Some(OAuthConfig {
            client_id: Some("client-id-1".into()),
            client_secret: Some(ValueRef {
                profiles: Default::default(),
                kind: RefKind::Literal,
                value: Some("s3cret-client-secret".into()),
                secret_id: None,
                prefix: None,
            }),
            ..Default::default()
        });
        st.mutate(None, |l| {
            l.push(s);
            l.push(g);
            let mut p = stdio_server();
            p.transport.env.insert(
                "MIX_FILE".into(),
                ValueRef {
                    profiles: Default::default(),
                    kind: RefKind::File,
                    value: Some(tokfile.to_string_lossy().into_owned()),
                    secret_id: None,
                    prefix: None,
                },
            );
            p.transport.env.insert(
                "MIX_LIT".into(),
                ValueRef {
                    profiles: Default::default(),
                    kind: RefKind::Literal,
                    value: Some("s3cret-env-secret".into()),
                    secret_id: None,
                    prefix: None,
                },
            );
            p.transport.env.insert(
                "MIX_KEEP".into(),
                ValueRef {
                    profiles: Default::default(),
                    kind: RefKind::Environment,
                    value: Some("KEEP_ME".into()),
                    secret_id: None,
                    prefix: None,
                },
            );
            l.push(p);
            Ok(())
        })
        .unwrap();
    }

    fn stdio_server() -> McpServer {
        McpServer {
            id: "prog".into(),
            name: "Program".into(),
            enabled: true,
            transport: Transport {
                kind: TransportKind::Stdio,
                command: Some("node.exe".into()),
                args: vec!["server.js".into()],
                env: BTreeMap::new(),
                url: None,
                headers: BTreeMap::new(),
                oauth: None,
            },
            allow_private_http: false,
            preset: None,
        }
    }

    #[test]
    fn a_copy_blanks_every_secret_and_keeps_the_shape() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let tokfile = d.path().join("tok.txt");
        std::fs::write(&tokfile, "from-file\n").unwrap();
        seed_each_secret_kind(&st, &tokfile);
        let copy = st.export_copy().unwrap();
        let wire = serde_json::to_string(&copy).unwrap();
        let tokpath = tokfile.to_string_lossy().into_owned();
        for gone in [
            "s3cret-header-token",
            "s3cret-client-secret",
            "s3cret-env-secret",
            "lit_",
            tokpath.as_str(),
        ] {
            assert!(!wire.contains(gone), "the copy must not carry {gone}");
        }
        // Environment names, the command, the preset shape and prefixes cross as they stand.
        assert!(
            wire.contains("MIX_TOKEN") && wire.contains("KEEP_ME") && wire.contains("client-id-1")
        );
        let mix = copy.iter().find(|s| s.id == "mix").unwrap();
        assert_eq!(
            mix.transport.headers["Authorization"],
            ValueRef {
                profiles: Default::default(),
                kind: RefKind::Literal,
                value: None,
                secret_id: None,
                prefix: Some("Bearer ".into()),
            },
            "the token slot crosses empty, with its prefix"
        );
        let signed = copy.iter().find(|s| s.id == "signed").unwrap();
        assert_eq!(
            signed.transport.oauth.as_ref().unwrap().client_secret,
            Some(ValueRef {
                profiles: Default::default(),
                kind: RefKind::Literal,
                value: None,
                secret_id: None,
                prefix: None,
            })
        );
        let prog = copy.iter().find(|s| s.id == "prog").unwrap();
        assert_eq!(
            prog.transport.env["MIX_KEEP"].value.as_deref(),
            Some("KEEP_ME")
        );
        // A credential file's path is blanked too: the slot arrives as a secret to enter.
        assert_eq!(prog.transport.env["MIX_FILE"].kind, RefKind::Literal);
        assert_eq!(prog.transport.env["MIX_FILE"].value, None);
    }

    #[test]
    fn an_imported_copy_lands_needs_its_token_and_never_overwrites() {
        let src = tempfile::tempdir().unwrap();
        let sstore = store(src.path());
        let tokfile = src.path().join("tok.txt");
        std::fs::write(&tokfile, "from-file\n").unwrap();
        seed_each_secret_kind(&sstore, &tokfile);
        let copy = sstore.export_copy().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let dstore = store(dst.path());
        let snap = dstore.import_copy(&copy, None).unwrap();
        assert_eq!(snap.servers.len(), 3);
        // A second copy of the same list skips the ids this PC already has: no overwrite, no
        // duplicate, and the revision does not move without a change.
        let again = dstore.import_copy(&copy, Some(&snap.revision)).unwrap();
        assert_eq!(again.servers.len(), 3);
        assert_eq!(again.servers, snap.servers);
        // It saved clean: the file carries no value; and it reads as a secret that must be
        // entered again — the row's "Needs its token", in the host's own words.
        let text = std::fs::read_to_string(dst.path().join(FILE_NAME)).unwrap();
        let tokpath = tokfile.to_string_lossy().into_owned();
        for gone in ["s3cret", "lit_", tokpath.as_str()] {
            assert!(!text.contains(gone), "the saved copy must not carry {gone}");
        }
        // Any blank slot answers with the host's own "enter it again" words when a chat tries
        // the server: the row says it needs its token.
        let env = |_: &str| None;
        let (_, resolved) = dstore.resolve_enabled(&env).unwrap();
        for id in ["mix", "prog"] {
            let r = resolved.iter().find(|r| r.id == id).unwrap();
            let err = r.error.as_deref().unwrap_or_default();
            assert!(err.contains("Enter it again"), "{id}: {err}");
        }
        assert!(
            resolved
                .iter()
                .find(|r| r.id == "prog")
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains("MIX_FILE"),
            "the words name the slot that waits"
        );
        // An entry still carrying a secret of the other PC is refused.
        let mut dirty = copy.clone();
        dirty[0]
            .transport
            .headers
            .values_mut()
            .next()
            .unwrap()
            .value = Some("leak".into());
        let err = dstore.import_copy(&dirty, None).unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)));
        assert_eq!(dstore.load().unwrap().len(), 3);
    }

    #[test]
    fn disabled_servers_are_not_resolved_and_listeners_hear_saves() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let heard = Arc::new(Mutex::new(Vec::<String>::new()));
        let h = heard.clone();
        st.on_change(move |r| h.lock().unwrap().push(r.to_owned()));
        let snap = st
            .mutate(None, |l| {
                let mut s = hub("x");
                s.enabled = false;
                l.push(s);
                Ok(())
            })
            .unwrap();
        assert_eq!(*heard.lock().unwrap(), vec![snap.revision]);
        assert!(st.resolve_enabled(&no_env).unwrap().1.is_empty());
    }
}
