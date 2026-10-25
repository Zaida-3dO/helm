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
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

fn identity(server_id: &str, secret_id: &str) -> String {
    format!("mcp-literal:{server_id}:{secret_id}")
}

type Listener = Box<dyn Fn(&str) + Send + Sync>;
type StrMap = BTreeMap<String, String>;

/// The shared service (one per app). All writes are serialised by its lock.
pub struct McpStore {
    path: PathBuf,
    creds: CredentialStore,
    lock: Mutex<()>,
    listeners: Mutex<Vec<Listener>>,
}

impl McpStore {
    /// `profile_dir` holds the config file; `creds` the literal secrets.
    pub fn new(profile_dir: &Path, creds: CredentialStore) -> Self {
        Self {
            path: profile_dir.join(FILE_NAME),
            creds,
            lock: Mutex::new(()),
            listeners: Mutex::new(Vec::new()),
        }
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
        // A stored file never holds a literal value.
        if f.servers
            .iter()
            .any(|s| refs(s).any(|r| r.value.is_some() && r.kind == RefKind::Literal))
        {
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

    /// Read, check `expected` against the current revision, apply `f`, validate, seal new
    /// literals, write, forget orphaned secrets, notify. Nothing is written if any step fails.
    pub fn mutate(
        &self,
        expected: Option<&str>,
        f: impl FnOnce(&mut Vec<McpServer>) -> Result<(), StoreError>,
    ) -> Result<McpEditorSnapshot, StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let before = self.load()?;
        if let Some(exp) = expected {
            if exp != revision_of(&before) {
                return Err(StoreError::Conflict(STALE.into()));
            }
        }
        let mut after = before.clone();
        f(&mut after)?;
        validate_list(&after)?;
        // A kept secret id must be one this same server already had.
        let owned: BTreeMap<&str, BTreeSet<String>> = before
            .iter()
            .map(|s| (s.id.as_str(), secret_ids(s)))
            .collect();
        for s in &after {
            for r in refs(s) {
                if let (RefKind::Literal, Some(id)) = (r.kind, &r.secret_id) {
                    if !owned.get(s.id.as_str()).is_some_and(|ids| ids.contains(id)) {
                        return Err(StoreError::Invalid(format!(
                            "{}: a saved secret was referenced that this server does not have. Enter the value again.",
                            s.name
                        )));
                    }
                }
            }
        }
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
                    let id = format!("lit_{}", uuid::Uuid::new_v4().simple());
                    if let Err(e) = self.creds.write(&id, &identity(&sid, &id), v.as_bytes()) {
                        self.forget_all(&minted);
                        return Err(StoreError::Unavailable(format!(
                            "Could not store the secret in Helm's credential store: {e}"
                        )));
                    }
                    minted.push(id.clone());
                    r.secret_id = Some(id);
                }
            }
        }
        Ok(minted)
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
        let servers = self.load()?;
        let revision = revision_of(&servers);
        let resolved = servers
            .iter()
            .filter(|s| s.enabled)
            .map(|s| self.resolve_one(s, env))
            .collect();
        Ok((revision, resolved))
    }

    /// One draft server with its references resolved, for a connection test. A literal secret
    /// that was typed but not saved yet is used as typed; saved ones are read from the credential
    /// store as usual.
    pub fn resolve_draft(&self, s: &McpServer) -> ResolvedServer {
        self.resolve_one(s, &|k| std::env::var(k).ok())
    }

    fn resolve_one(&self, s: &McpServer, env: &dyn Fn(&str) -> Option<String>) -> ResolvedServer {
        let t = &s.transport;
        let mut out = ResolvedServer {
            id: s.id.clone(),
            name: s.name.clone(),
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
        };
        let mut secrets = Vec::new();
        let mut run = || -> Result<(StrMap, StrMap), String> {
            let mut e = BTreeMap::new();
            for (k, r) in &t.env {
                e.insert(
                    k.clone(),
                    self.resolve_ref(
                        &s.id,
                        r,
                        false,
                        &format!("Environment {k}"),
                        env,
                        &mut secrets,
                    )?,
                );
            }
            let mut h = BTreeMap::new();
            for (k, r) in &t.headers {
                h.insert(
                    k.clone(),
                    self.resolve_ref(&s.id, r, true, &format!("Header {k}"), env, &mut secrets)?,
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
        out
    }

    fn resolve_ref(
        &self,
        server_id: &str,
        r: &ValueRef,
        header: bool,
        label: &str,
        env: &dyn Fn(&str) -> Option<String>,
        secrets: &mut Vec<String>,
    ) -> Result<String, String> {
        let raw = match r.kind {
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
            RefKind::Literal => {
                let id = r.secret_id.as_deref().unwrap_or("");
                let bytes = self
                    .creds
                    .read(id, &identity(server_id, id))
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

pub(crate) fn refs(s: &McpServer) -> impl Iterator<Item = &ValueRef> {
    s.transport.env.values().chain(s.transport.headers.values())
}

fn refs_mut(s: &mut McpServer) -> impl Iterator<Item = &mut ValueRef> {
    let t = &mut s.transport;
    t.env.values_mut().chain(t.headers.values_mut())
}

fn secret_ids(s: &McpServer) -> BTreeSet<String> {
    refs(s)
        .filter(|r| r.kind == RefKind::Literal)
        .filter_map(|r| r.secret_id.clone())
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

    fn no_env(_: &str) -> Option<String> {
        None
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
                kind: RefKind::Environment,
                value: Some("HUB_TOKEN".into()),
                secret_id: None,
                prefix: None,
            },
        );
        s.transport.headers.insert(
            "X-File".into(),
            ValueRef {
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
