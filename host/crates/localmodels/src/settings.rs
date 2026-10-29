//! The local-model settings record, in this crate's own `local_model_settings` table.
//!
//! It holds the list of servers (name, kind, address, whether it is switched on) and the few
//! numbers and switches around them. Read **leniently**, each field on its own, so a bad value
//! falls back to its default; written **strictly**, so a bad save is refused whole with a message
//! that names the field. Keys are not here: they live in the credential store.
//!
//! **Addresses are loopback only.** A server's address must be `http://` and name this machine
//! (`127.0.0.1`, `localhost` or `[::1]`), because a key is sent to it and a worker is pointed at
//! it. [`check_base_url`] is the one check, used on save, on read and again before any request.

use helm_chats::types::LocalServerKind;
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use ts_rs::TS;

pub const STRATA_URL: &str = "http://127.0.0.1:18731";
pub const OLLAMA_URL: &str = "http://127.0.0.1:11434";

pub const MAX_SERVERS: usize = 8;
pub const MIN_CONTEXT: u32 = 8192;
pub const MAX_CONTEXT: u32 = 262_144;
pub const DEFAULT_CONTEXT: u32 = 65_536;
pub const MIN_KEEP_ALIVE: u32 = 1;
pub const MAX_KEEP_ALIVE: u32 = 240;
pub const DEFAULT_KEEP_ALIVE: u32 = 15;
pub const MIN_STRATA_GB: u32 = 1;
pub const MAX_STRATA_GB: u32 = 256;
pub const DEFAULT_STRATA_GB: u32 = 12;

/// Which tools a local chat is offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ToolsSetup {
    /// Claude Code's own MCP servers are off; Helm's own tools stay. A small model copes better
    /// with a short tool list.
    #[default]
    Lean,
    /// As a Claude chat.
    Full,
}

/// The Claude model local usage is priced against ("what this would have cost on Claude").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum CompareModel {
    Opus,
    #[default]
    Sonnet,
    Haiku,
}

impl CompareModel {
    pub fn key(self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Haiku => "haiku",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "opus" => Some(Self::Opus),
            "sonnet" => Some(Self::Sonnet),
            "haiku" => Some(Self::Haiku),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub name: String,
    pub kind: LocalServerKind,
    pub base_url: String,
    pub enabled: bool,
    /// The model to offer when a Strata server does not list its models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSettings {
    pub servers: Vec<Server>,
    /// Ollama's context window for Helm's variant of a model, in tokens.
    pub ollama_context: u32,
    /// How long Ollama keeps a model loaded after a turn, in minutes.
    pub keep_alive_minutes: u32,
    /// Memory (GB) a Strata model needs, for the check before it loads.
    pub strata_need_gb: u32,
    pub tools: ToolsSetup,
    pub compare_with: CompareModel,
}

pub fn kind_name(k: LocalServerKind) -> &'static str {
    match k {
        LocalServerKind::Strata => "strata",
        LocalServerKind::Ollama => "ollama",
    }
}

pub fn default_url(k: LocalServerKind) -> &'static str {
    match k {
        LocalServerKind::Strata => STRATA_URL,
        LocalServerKind::Ollama => OLLAMA_URL,
    }
}

pub fn default_servers() -> Vec<Server> {
    [LocalServerKind::Strata, LocalServerKind::Ollama]
        .into_iter()
        .map(|kind| Server {
            id: kind_name(kind).to_owned(),
            name: match kind {
                LocalServerKind::Strata => "Strata",
                LocalServerKind::Ollama => "Ollama",
            }
            .to_owned(),
            kind,
            base_url: default_url(kind).to_owned(),
            enabled: true,
            model: None,
        })
        .collect()
}

impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            servers: default_servers(),
            ollama_context: DEFAULT_CONTEXT,
            keep_alive_minutes: DEFAULT_KEEP_ALIVE,
            strata_need_gb: DEFAULT_STRATA_GB,
            tools: ToolsSetup::Lean,
            compare_with: CompareModel::Sonnet,
        }
    }
}

// ---- the address rule -----------------------------------------------------------------------

/// The address in its plain form (`http://127.0.0.1:18731`, no path), or why it is refused.
pub fn check_base_url(raw: &str) -> Result<String, String> {
    let refuse = |why: &str| {
        Err(format!("{why} A local server's address must be http:// followed by 127.0.0.1, localhost or [::1], and a port."))
    };
    let raw = raw.trim();
    let Some(rest) = raw
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("http://"))
        .map(|_| &raw[7..])
    else {
        return refuse("That address does not start with http://.");
    };
    if rest.contains(['@', '?', '#', '\\', ' ']) {
        return refuse("That address has parts a local server's does not.");
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !(path.is_empty() || path == "/") {
        return refuse("That address has a path.");
    }
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let Some((h, after)) = inner.split_once(']') else {
            return refuse("That address is not valid.");
        };
        (format!("[{h}]"), after.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p)),
            None => (authority.to_owned(), None),
        }
    };
    let host = host.to_ascii_lowercase();
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
        return refuse("That address is not on this machine.");
    }
    let port = match port {
        None => 80,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => n,
            _ => return refuse("That port is not valid."),
        },
    };
    Ok(format!("http://{host}:{port}"))
}

// ---- reading and writing --------------------------------------------------------------------

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_model_settings (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             value  TEXT NOT NULL
         );",
    )
}

pub fn load(tx: &Transaction<'_>) -> rusqlite::Result<LocalSettings> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM local_model_settings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(|v| read_lenient(&v))
        .unwrap_or_default())
}

pub fn store(tx: &Transaction<'_>, s: &LocalSettings) -> rusqlite::Result<()> {
    let v = serde_json::to_string(s).expect("settings serialise");
    tx.execute(
        "INSERT INTO local_model_settings (id, value) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET value = excluded.value",
        [v],
    )?;
    Ok(())
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

fn kind_of(v: &Value) -> Option<LocalServerKind> {
    match v.as_str()? {
        "strata" => Some(LocalServerKind::Strata),
        "ollama" => Some(LocalServerKind::Ollama),
        _ => None,
    }
}

fn plain_text(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

fn int_in(v: &Value, lo: u32, hi: u32) -> Option<u32> {
    let n = v.as_u64()?;
    (n >= u64::from(lo) && n <= u64::from(hi)).then_some(n as u32)
}

/// Every field on its own; a server with a bad address gets its kind's default address.
pub fn read_lenient(v: &Value) -> LocalSettings {
    let mut s = LocalSettings::default();
    let Some(o) = v.as_object() else { return s };
    if let Some(list) = o.get("servers").and_then(Value::as_array) {
        let mut seen: Vec<Server> = Vec::new();
        for e in list.iter().take(MAX_SERVERS) {
            let Some(e) = e.as_object() else { continue };
            let (Some(id), Some(kind)) = (
                e.get("id").and_then(Value::as_str).filter(|i| id_ok(i)),
                e.get("kind").and_then(kind_of),
            ) else {
                continue;
            };
            if seen.iter().any(|x| x.id == id) {
                continue;
            }
            let name = e
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| plain_text(n, 60))
                .unwrap_or(id)
                .to_owned();
            let base_url = e
                .get("baseUrl")
                .and_then(Value::as_str)
                .and_then(|u| check_base_url(u).ok())
                .unwrap_or_else(|| default_url(kind).to_owned());
            seen.push(Server {
                id: id.to_owned(),
                name,
                kind,
                base_url,
                enabled: e.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                model: e
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|m| plain_text(m, 200))
                    .map(str::to_owned),
            });
        }
        s.servers = seen;
    }
    if let Some(n) = o
        .get("ollamaContext")
        .and_then(|v| int_in(v, MIN_CONTEXT, MAX_CONTEXT))
    {
        s.ollama_context = n;
    }
    if let Some(n) = o
        .get("keepAliveMinutes")
        .and_then(|v| int_in(v, MIN_KEEP_ALIVE, MAX_KEEP_ALIVE))
    {
        s.keep_alive_minutes = n;
    }
    if let Some(n) = o
        .get("strataNeedGb")
        .and_then(|v| int_in(v, MIN_STRATA_GB, MAX_STRATA_GB))
    {
        s.strata_need_gb = n;
    }
    if let Some(t) = o
        .get("tools")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
    {
        s.tools = t;
    }
    if let Some(c) = o
        .get("compareWith")
        .and_then(Value::as_str)
        .and_then(CompareModel::parse)
    {
        s.compare_with = c;
    }
    s
}

/// One server as a save names it: an empty or missing `id` makes a new server.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub kind: LocalServerKind,
    pub base_url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub model: Option<String>,
}

fn yes() -> bool {
    true
}

/// The id a new server gets: its name, lower-cased with dashes, made unique.
fn fresh_id(name: &str, taken: &[String]) -> String {
    let mut base: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    base = base.trim_matches('-').chars().take(24).collect();
    if base.is_empty() {
        base = "server".into();
    }
    let mut id = base.clone();
    let mut n = 1;
    while taken.contains(&id) {
        n += 1;
        id = format!("{base}-{n}");
    }
    id
}

/// What a save may change. `servers` replaces the whole list; the rest change one value each.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Changes {
    pub servers: Option<Vec<ServerInput>>,
    pub ollama_context: Option<u32>,
    pub keep_alive_minutes: Option<u32>,
    pub strata_need_gb: Option<u32>,
    pub tools: Option<ToolsSetup>,
    pub compare_with: Option<CompareModel>,
}

fn range_err(what: &str, lo: u32, hi: u32) -> String {
    format!("{what} must be a whole number from {lo} to {hi}.")
}

/// `current` with `changes` applied, or the first problem. Nothing is stored here.
pub fn apply_changes(current: &LocalSettings, changes: &Changes) -> Result<LocalSettings, String> {
    let mut s = current.clone();
    if let Some(list) = &changes.servers {
        if list.len() > MAX_SERVERS {
            return Err(format!("At most {MAX_SERVERS} servers can be set up."));
        }
        let mut out: Vec<Server> = Vec::new();
        let mut taken: Vec<String> = list
            .iter()
            .filter_map(|e| e.id.clone().filter(|i| !i.is_empty()))
            .collect();
        for (i, e) in list.iter().enumerate() {
            let n = i + 1;
            if !plain_text(&e.name, 60) {
                return Err(format!(
                    "Server {n}: the name must be 1 to 60 characters, on one line."
                ));
            }
            let base_url = check_base_url(&e.base_url).map_err(|m| format!("Server {n}: {m}"))?;
            let id = match e.id.as_deref().filter(|i| !i.is_empty()) {
                Some(id) => {
                    if !id_ok(id) {
                        return Err(format!(
                            "Server {n}: the id may hold a-z, 0-9 and -, up to 32 characters."
                        ));
                    }
                    id.to_owned()
                }
                None => {
                    let id = fresh_id(&e.name, &taken);
                    taken.push(id.clone());
                    id
                }
            };
            if out.iter().any(|x| x.id == id) {
                return Err(format!("Server {n}: the id {id} is used twice."));
            }
            // The kind of a server that already exists does not change: chats are tied to it.
            if let Some(old) = current.servers.iter().find(|x| x.id == id) {
                if old.kind != e.kind {
                    return Err(format!(
                        "Server {n}: the kind of {id} can't change. Add a new server instead."
                    ));
                }
            }
            let model = match e.model.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(m) if plain_text(m, 200) => Some(m.to_owned()),
                Some(_) => return Err(format!("Server {n}: the model name is not valid.")),
            };
            out.push(Server {
                id,
                name: e.name.trim().to_owned(),
                kind: e.kind,
                base_url,
                enabled: e.enabled,
                model,
            });
        }
        s.servers = out;
    }
    if let Some(n) = changes.ollama_context {
        if !(MIN_CONTEXT..=MAX_CONTEXT).contains(&n) {
            return Err(range_err(
                "The Ollama context size",
                MIN_CONTEXT,
                MAX_CONTEXT,
            ));
        }
        s.ollama_context = n;
    }
    if let Some(n) = changes.keep_alive_minutes {
        if !(MIN_KEEP_ALIVE..=MAX_KEEP_ALIVE).contains(&n) {
            return Err(range_err(
                "The keep-alive time (minutes)",
                MIN_KEEP_ALIVE,
                MAX_KEEP_ALIVE,
            ));
        }
        s.keep_alive_minutes = n;
    }
    if let Some(n) = changes.strata_need_gb {
        if !(MIN_STRATA_GB..=MAX_STRATA_GB).contains(&n) {
            return Err(range_err(
                "The Strata memory figure (GB)",
                MIN_STRATA_GB,
                MAX_STRATA_GB,
            ));
        }
        s.strata_need_gb = n;
    }
    if let Some(t) = changes.tools {
        s.tools = t;
    }
    if let Some(c) = changes.compare_with {
        s.compare_with = c;
    }
    Ok(s)
}

/// The settings as the agents' `app_get_settings` shows them: every value, plus the servers'
/// enabled flags by id. Never an address or a key.
pub fn enabled_map(s: &LocalSettings) -> Map<String, Value> {
    s.servers
        .iter()
        .map(|x| (x.id.clone(), json!(x.enabled)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_rule_keeps_servers_on_this_machine() {
        for (raw, want) in [
            ("http://127.0.0.1:9101", Ok("http://127.0.0.1:9101")),
            ("http://127.0.0.1:9101/", Ok("http://127.0.0.1:9101")),
            ("  HTTP://LocalHost:9102  ", Ok("http://localhost:9102")),
            ("http://[::1]:8080", Ok("http://[::1]:8080")),
            ("http://127.0.0.1", Ok("http://127.0.0.1:80")),
            ("https://127.0.0.1:1", Err("http://")),
            ("http://192.168.1.5:9102", Err("not on this machine")),
            ("http://localhost.evil.test:1", Err("not on this machine")),
            ("http://127.0.0.1:1@evil.test:2", Err("parts")),
            ("http://user@127.0.0.1:1", Err("parts")),
            ("http://127.0.0.1:1/v1", Err("path")),
            ("http://127.0.0.1:1?x=1", Err("parts")),
            ("http://127.0.0.1:0", Err("port")),
            ("http://127.0.0.1:99999", Err("port")),
            ("http://127.0.0.1:abc", Err("port")),
            ("http://0.0.0.0:1", Err("not on this machine")),
            ("", Err("http://")),
        ] {
            match (check_base_url(raw), want) {
                (Ok(got), Ok(w)) => assert_eq!(got, w, "{raw}"),
                (Err(e), Err(w)) => assert!(e.contains(w), "{raw}: {e}"),
                (got, want) => panic!("{raw}: {got:?} vs {want:?}"),
            }
        }
    }

    #[test]
    fn lenient_reads_and_strict_saves() {
        // A stored record with a bad field keeps the good ones; a foreign address is replaced.
        let s = read_lenient(&json!({
            "servers": [
                {"id": "strata", "name": "Strata", "kind": "strata", "baseUrl": "http://10.0.0.5:1", "enabled": false},
                {"id": "Bad Id", "name": "x", "kind": "ollama", "baseUrl": "http://127.0.0.1:1"},
                {"id": "strata", "name": "twice", "kind": "strata", "baseUrl": "http://127.0.0.1:1"},
            ],
            "ollamaContext": 100, "keepAliveMinutes": 30, "tools": "full", "compareWith": "haiku"
        }));
        assert_eq!(s.servers.len(), 1);
        assert_eq!(
            s.servers[0].base_url, STRATA_URL,
            "a foreign address falls back"
        );
        assert!(!s.servers[0].enabled);
        assert_eq!(
            (
                s.ollama_context,
                s.keep_alive_minutes,
                s.tools,
                s.compare_with
            ),
            (DEFAULT_CONTEXT, 30, ToolsSetup::Full, CompareModel::Haiku)
        );
        assert_eq!(read_lenient(&json!("junk")), LocalSettings::default());
        assert_eq!(
            read_lenient(&json!({"servers": []})).servers.len(),
            0,
            "an empty list is the user's choice"
        );

        // Strict saves: a new server gets an id from its name, and nothing is stored on a refusal.
        let cur = LocalSettings::default();
        let input = |name: &str, kind: LocalServerKind, url: &str| ServerInput {
            id: None,
            name: name.into(),
            kind,
            base_url: url.into(),
            enabled: true,
            model: None,
        };
        let mut servers: Vec<ServerInput> = cur
            .servers
            .iter()
            .map(|x| ServerInput {
                id: Some(x.id.clone()),
                name: x.name.clone(),
                kind: x.kind,
                base_url: x.base_url.clone(),
                enabled: x.enabled,
                model: None,
            })
            .collect();
        servers.push(input(
            "My Ollama",
            LocalServerKind::Ollama,
            "http://localhost:11500",
        ));
        let next = apply_changes(
            &cur,
            &Changes {
                servers: Some(servers.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(next.servers[2].id, "my-ollama");
        assert_eq!(next.servers[2].base_url, "http://localhost:11500");
        for (changes, why) in [
            (
                Changes {
                    ollama_context: Some(100),
                    ..Default::default()
                },
                "8192",
            ),
            (
                Changes {
                    keep_alive_minutes: Some(0),
                    ..Default::default()
                },
                "keep-alive",
            ),
            (
                Changes {
                    strata_need_gb: Some(999),
                    ..Default::default()
                },
                "Strata",
            ),
        ] {
            let e = apply_changes(&cur, &changes).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
        let mut bad = servers.clone();
        bad[2].base_url = "http://example.com:1".into();
        assert!(apply_changes(
            &cur,
            &Changes {
                servers: Some(bad),
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("Server 3"));
        let mut swapped = servers.clone();
        swapped[0].kind = LocalServerKind::Ollama;
        assert!(apply_changes(
            &cur,
            &Changes {
                servers: Some(swapped),
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("can't change"));
        let mut twice = servers;
        twice[2].id = Some("strata".into());
        assert!(apply_changes(
            &cur,
            &Changes {
                servers: Some(twice),
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("twice"));
    }

    #[test]
    fn the_record_round_trips_through_its_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = helm_core::db::MigrationRegistry::new();
        reg.initializer("local-models", init);
        let db = helm_core::db::Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        assert_eq!(db.snapshot(load).unwrap(), LocalSettings::default());
        let mut s = LocalSettings {
            ollama_context: 32_768,
            ..Default::default()
        };
        s.servers[1].enabled = false;
        db.write(|tx| store(tx, &s)).unwrap();
        assert_eq!(db.snapshot(load).unwrap(), s);
    }
}
