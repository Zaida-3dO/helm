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
use std::collections::BTreeMap;
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
/// Graphics memory (GB) kept free for speech-to-text when a model loads. 0 keeps none.
pub const MIN_SPEECH_RESERVE_GB: u32 = 0;
pub const MAX_SPEECH_RESERVE_GB: u32 = 24;
pub const DEFAULT_SPEECH_RESERVE_GB: u32 = 2;

/// The most chats that may work on one server at once, as its own cap setting (0 switches the
/// cap off; leaving it unset follows the `localChatCap` preference).
pub const MAX_CHAT_CAP: u32 = 12;

/// Games that unload local models while they run: at most this many, each 1 to 100 characters.
pub const MAX_GAMES: usize = 20;
pub const MAX_GAME_NAME: usize = 100;
/// Minutes after a listed game closes before local models come back.
pub const MIN_GAME_LIFT: u32 = 1;
pub const MAX_GAME_LIFT: u32 = 60;
pub const DEFAULT_GAME_LIFT: u32 = 5;
/// Names the user gave local models: at most this many, each 1 to 40 characters.
pub const MAX_ALIASES: usize = 64;
pub const MAX_ALIAS: usize = 40;
/// The longest a model id may be, as a key of the names.
const MAX_MODEL_ID: usize = 300;
/// How large a chat a model still works well with, as the user states it (tokens). A save cannot
/// measure each model's own window here, so this is the outer bound — the same top the global
/// roll-over size allows.
pub const MIN_COMFORT: u64 = 1_000;
pub const MAX_COMFORT: u64 = 2_000_000;
/// Models that may carry a comfort size or a suggested step-up at all: the two maps each hold at
/// most this many keys.
pub const MAX_FIT: usize = 64;
/// The longest a restart task name and a pause file path may be.
pub const MAX_TASK: usize = 200;
pub const MAX_PAUSE_FILE: usize = 500;

/// The longest a Strata folder path or a preset name may be.
pub const MAX_FOLDER: usize = 500;
pub const MAX_PRESET: usize = 60;
/// The preset Strata is started with when the settings name none.
pub const DEFAULT_PRESET: &str = "Solo";

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
    /// Strata only: Helm may start the server itself when a chat needs it and it is not running.
    #[serde(default = "yes")]
    pub auto_start: bool,
    /// Strata only: the install folder to start from. None is the standard one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Strata only: the preset to launch with. None is [`DEFAULT_PRESET`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// How many of our chats may work on this server at once before their turns queue at their
    /// start. None follows the global default (`localChatCap`); 0 switches the cap off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_cap: Option<u32>,
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
    /// Memory (GB) a model load leaves free for speech-to-text, so dictation and the voice line's
    /// transcription still fit beside a local model.
    pub speech_reserve_gb: u32,
    pub tools: ToolsSetup,
    pub compare_with: CompareModel,
    /// Process names (`game.exe`) that unload local models while they run. Empty: nothing is
    /// watched.
    pub games: Vec<String>,
    /// The names the user gave models, by model id (`local/<server>/<model>`). A model with none
    /// goes by its default name (`names::default_alias`).
    pub aliases: BTreeMap<String, String>,
    /// How large a chat each model still works well with, in tokens, by model id. Unset means 70%
    /// of the model's window — today's rule for calling a local chat large. A value above the
    /// model's real window only delays the banner; it never breaks anything.
    #[serde(default)]
    pub comfort: BTreeMap<String, u64>,
    /// The other local model to suggest when a chat outgrows this one, by model id — set by hand,
    /// never guessed from window sizes or display names. Unset means no suggestion: the banner
    /// keeps Roll over only.
    #[serde(default)]
    pub step_up: BTreeMap<String, String>,
    /// Minutes after the last listed game closes before local models come back.
    pub game_lift_minutes: u32,
    /// A Windows scheduled task that restarts a model server: ended while local models are
    /// unloaded, started again after. A chat only stages it, for the user to approve.
    pub restart_task: Option<String>,
    /// A file written while local models are unloaded, for the user's own restart script to
    /// check. A chat only stages it, for the user to approve.
    pub pause_file: Option<String>,
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
            auto_start: true,
            folder: None,
            preset: None,
            chat_cap: None,
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
            speech_reserve_gb: DEFAULT_SPEECH_RESERVE_GB,
            tools: ToolsSetup::Lean,
            compare_with: CompareModel::Sonnet,
            games: Vec::new(),
            aliases: BTreeMap::new(),
            game_lift_minutes: DEFAULT_GAME_LIFT,
            restart_task: None,
            pause_file: None,
            comfort: BTreeMap::new(),
            step_up: BTreeMap::new(),
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

/// An absolute path on this machine (`C:\...` or `C:/...`). A relative folder would mean whatever
/// folder the launcher happens to start in, and a network path is not a local install.
fn full_local_path(f: &str) -> bool {
    let b = f.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
}

/// A game as the list holds it: a bare process name, `.exe` added when it is missing. `None` for
/// a name with a path in it, control characters, or the wrong length.
pub fn game_name(raw: &str) -> Option<String> {
    let g = raw.trim();
    if !plain_text(g, MAX_GAME_NAME) || g.contains(['/', '\\', ':']) {
        return None;
    }
    Some(if g.to_ascii_lowercase().ends_with(".exe") {
        g.to_owned()
    } else {
        format!("{g}.exe")
    })
}

/// The list with bad names and repeats (in any case) left out.
fn game_list<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for g in names.filter_map(game_name) {
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&g)) {
            out.push(g);
        }
    }
    out
}

fn alias_key_ok(k: &str) -> bool {
    k.starts_with("local/") && plain_text(k, MAX_MODEL_ID)
}

fn alias_ok(a: &str) -> bool {
    plain_text(a, MAX_ALIAS)
}

/// A comfort size in its range. The window bound is kept at read and use (a value over the model's
/// window only delays the banner); here it is the outer bound.
fn comfort_ok(n: u64) -> bool {
    (MIN_COMFORT..=MAX_COMFORT).contains(&n)
}

/// A step-up target: a local model id that is not the model asking for the suggestion.
fn step_up_ok(id: &str, target: &str) -> bool {
    alias_key_ok(target) && target != id
}

fn task_ok(t: &str) -> bool {
    plain_text(t, MAX_TASK) && !t.contains('"')
}

fn pause_file_ok(f: &str) -> bool {
    plain_text(f, MAX_PAUSE_FILE) && full_local_path(f)
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
                auto_start: e.get("autoStart").and_then(Value::as_bool).unwrap_or(true),
                folder: e
                    .get("folder")
                    .and_then(Value::as_str)
                    .filter(|f| plain_text(f, MAX_FOLDER))
                    .map(str::to_owned),
                preset: e
                    .get("preset")
                    .and_then(Value::as_str)
                    .filter(|p| plain_text(p, MAX_PRESET))
                    .map(str::to_owned),
                chat_cap: e.get("chatCap").and_then(|v| int_in(v, 0, MAX_CHAT_CAP)),
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
    if let Some(n) = o
        .get("speechReserveGb")
        .and_then(|v| int_in(v, MIN_SPEECH_RESERVE_GB, MAX_SPEECH_RESERVE_GB))
    {
        s.speech_reserve_gb = n;
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
    if let Some(list) = o.get("games").and_then(Value::as_array) {
        s.games = game_list(list.iter().filter_map(Value::as_str));
        s.games.truncate(MAX_GAMES);
    }
    if let Some(map) = o.get("aliases").and_then(Value::as_object) {
        s.aliases = map
            .iter()
            .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?.trim())))
            .filter(|(k, v)| alias_key_ok(k) && alias_ok(v))
            .take(MAX_ALIASES)
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
    }
    if let Some(map) = o.get("comfort").and_then(Value::as_object) {
        s.comfort = map
            .iter()
            .filter_map(|(k, v)| {
                let n = v.as_u64()?;
                (alias_key_ok(k) && comfort_ok(n)).then(|| (k.to_owned(), n))
            })
            .take(MAX_FIT)
            .collect();
    }
    if let Some(map) = o.get("stepUp").and_then(Value::as_object) {
        s.step_up = map
            .iter()
            .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?.trim())))
            .filter(|(k, v)| alias_key_ok(k) && step_up_ok(k, v))
            .take(MAX_FIT)
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
    }
    if let Some(n) = o
        .get("gameLiftMinutes")
        .and_then(|v| int_in(v, MIN_GAME_LIFT, MAX_GAME_LIFT))
    {
        s.game_lift_minutes = n;
    }
    s.restart_task = o
        .get("restartTask")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| task_ok(t))
        .map(str::to_owned);
    s.pause_file = o
        .get("pauseFile")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|f| pause_file_ok(f))
        .map(str::to_owned);
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
    #[serde(default = "yes")]
    pub auto_start: bool,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub chat_cap: Option<u32>,
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
    pub speech_reserve_gb: Option<u32>,
    pub tools: Option<ToolsSetup>,
    pub compare_with: Option<CompareModel>,
    /// Replaces the whole list.
    pub games: Option<Vec<String>>,
    /// Model id to name: sets those names and leaves the others. Empty text clears a name.
    pub aliases: Option<BTreeMap<String, String>>,
    /// Model id to the chat size it works well up to, in tokens: sets those and leaves the
    /// others. 0 clears one.
    pub comfort: Option<BTreeMap<String, u64>>,
    /// Model id to the other model suggested when a chat outgrows it: sets those and leaves the
    /// others. Empty text clears one.
    pub step_up: Option<BTreeMap<String, String>>,
    pub game_lift_minutes: Option<u32>,
    /// Empty text clears it.
    pub restart_task: Option<String>,
    /// Empty text clears it.
    pub pause_file: Option<String>,
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
            let folder = match e.folder.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(f) if plain_text(f, MAX_FOLDER) && full_local_path(f) => Some(f.to_owned()),
                Some(_) => return Err(format!(
                    "Server {n}: the Strata folder must be a full path on this machine, like C:\\Strata\\terminal\\current."
                )),
            };
            let preset = match e.preset.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(p) if plain_text(p, MAX_PRESET) => {
                    // A name starting with "-" would reach the launcher as an option, not a preset.
                    if p.starts_with('-') {
                        return Err(format!(
                            "Server {n}: the preset name cannot start with \"-\"."
                        ));
                    }
                    Some(p.to_owned())
                }
                Some(_) => return Err(format!("Server {n}: the preset name is not valid.")),
            };
            let chat_cap = match e.chat_cap {
                None => None,
                Some(n) if n <= MAX_CHAT_CAP => Some(n),
                Some(_) => {
                    return Err(format!(
                        "Server {n}: the working-chat cap must be 0 to {MAX_CHAT_CAP} \
                         (0 switches it off; leave it unset to follow the default)."
                    ))
                }
            };
            out.push(Server {
                id,
                name: e.name.trim().to_owned(),
                kind: e.kind,
                base_url,
                enabled: e.enabled,
                model,
                auto_start: e.auto_start,
                folder,
                preset,
                chat_cap,
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
    if let Some(n) = changes.speech_reserve_gb {
        if !(MIN_SPEECH_RESERVE_GB..=MAX_SPEECH_RESERVE_GB).contains(&n) {
            return Err(range_err(
                "The memory kept for speech (GB)",
                MIN_SPEECH_RESERVE_GB,
                MAX_SPEECH_RESERVE_GB,
            ));
        }
        s.speech_reserve_gb = n;
    }
    if let Some(t) = changes.tools {
        s.tools = t;
    }
    if let Some(c) = changes.compare_with {
        s.compare_with = c;
    }
    if let Some(list) = &changes.games {
        let named: Vec<&str> = list
            .iter()
            .map(|g| g.trim())
            .filter(|g| !g.is_empty())
            .collect();
        if let Some(bad) = named.iter().find(|g| game_name(g).is_none()) {
            return Err(format!(
                "\"{}\" is not a program name. Name the game's program, like game.exe, without a folder.",
                bad.chars().take(40).collect::<String>()
            ));
        }
        let games = game_list(named.into_iter());
        if games.len() > MAX_GAMES {
            return Err(format!("At most {MAX_GAMES} games can be listed."));
        }
        s.games = games;
    }
    if let Some(map) = &changes.aliases {
        for (id, name) in map {
            if !alias_key_ok(id) {
                return Err("That is not a local model.".to_owned());
            }
            match name.trim() {
                "" => {
                    s.aliases.remove(id);
                }
                n if alias_ok(n) => {
                    s.aliases.insert(id.clone(), n.to_owned());
                }
                _ => {
                    return Err(format!(
                        "A model's name must be one line of text, up to {MAX_ALIAS} characters."
                    ))
                }
            }
        }
        if s.aliases.len() > MAX_ALIASES {
            return Err(format!(
                "At most {MAX_ALIASES} models can have a name of your own."
            ));
        }
    }
    if let Some(map) = &changes.comfort {
        for (id, n) in map {
            if !alias_key_ok(id) {
                return Err("That is not a local model.".to_owned());
            }
            if *n == 0 {
                s.comfort.remove(id);
                continue;
            }
            if !comfort_ok(*n) {
                return Err(format!(
                    "The size a model works well up to must be a whole number from {MIN_COMFORT} to {MAX_COMFORT} (0 clears it)."
                ));
            }
            s.comfort.insert(id.clone(), *n);
        }
        if s.comfort.len() > MAX_FIT {
            return Err(format!(
                "At most {MAX_FIT} models can have a size they work well up to."
            ));
        }
    }
    if let Some(map) = &changes.step_up {
        for (id, target) in map {
            if !alias_key_ok(id) {
                return Err("That is not a local model.".to_owned());
            }
            match target.trim() {
                "" => {
                    s.step_up.remove(id);
                }
                t if step_up_ok(id, t) => {
                    s.step_up.insert(id.clone(), t.to_owned());
                }
                t if !alias_key_ok(t) => {
                    return Err(format!(
                        "A suggested model must be a local model id (local/<server>/<model>), not \"{t}\"."
                    ));
                }
                _ => {
                    return Err("A model cannot be suggested in place of itself.".to_owned());
                }
            }
        }
        if s.step_up.len() > MAX_FIT {
            return Err(format!(
                "At most {MAX_FIT} models can suggest a step-up model."
            ));
        }
    }
    if let Some(n) = changes.game_lift_minutes {
        if !(MIN_GAME_LIFT..=MAX_GAME_LIFT).contains(&n) {
            return Err(range_err(
                "The wait after a game closes (minutes)",
                MIN_GAME_LIFT,
                MAX_GAME_LIFT,
            ));
        }
        s.game_lift_minutes = n;
    }
    if let Some(t) = &changes.restart_task {
        s.restart_task = match t.trim() {
            "" => None,
            t if task_ok(t) => Some(t.to_owned()),
            _ => {
                return Err(format!(
                    "The restart task must be the task's name, up to {MAX_TASK} characters on one line, without quotes."
                ))
            }
        };
    }
    if let Some(f) = &changes.pause_file {
        s.pause_file = match f.trim() {
            "" => None,
            f if pause_file_ok(f) => Some(f.to_owned()),
            _ => return Err(
                "The pause file must be a full path on this machine, like C:\\Models\\paused.flag."
                    .to_owned(),
            ),
        };
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
                {"id": "strata", "name": "Strata", "kind": "strata", "baseUrl": "http://10.0.0.5:1", "enabled": false,
                 "autoStart": false, "folder": "C:/Strata/terminal/current", "preset": "Fast", "chatCap": 1},
                {"id": "ollama", "name": "Ollama", "kind": "ollama", "baseUrl": "http://127.0.0.1:1", "folder": "x".repeat(600), "preset": "ok", "chatCap": 99},
                {"id": "Bad Id", "name": "x", "kind": "ollama", "baseUrl": "http://127.0.0.1:1"},
                {"id": "strata", "name": "twice", "kind": "strata", "baseUrl": "http://127.0.0.1:1"},
            ],
            "ollamaContext": 100, "keepAliveMinutes": 30, "tools": "full", "compareWith": "haiku"
        }));
        assert_eq!(s.servers.len(), 2);
        assert_eq!(
            (
                s.servers[0].auto_start,
                s.servers[0].folder.as_deref(),
                s.servers[0].preset.as_deref(),
                s.servers[0].chat_cap
            ),
            (
                false,
                Some("C:/Strata/terminal/current"),
                Some("Fast"),
                Some(1),
            ),
            "the start-when-needed fields and the server's own cap read back"
        );
        assert_eq!(
            (
                s.servers[1].auto_start,
                s.servers[1].folder.as_deref(),
                s.servers[1].preset.as_deref(),
                s.servers[1].chat_cap
            ),
            (true, None, Some("ok"), None),
            "missing autoStart means yes; an over-long folder and a cap out of range drop"
        );
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
        assert_eq!(
            (
                read_lenient(&json!({"speechReserveGb": 0})).speech_reserve_gb,
                read_lenient(&json!({"speechReserveGb": 6})).speech_reserve_gb,
                read_lenient(&json!({"speechReserveGb": 99})).speech_reserve_gb,
            ),
            (0, 6, DEFAULT_SPEECH_RESERVE_GB),
            "the speech reserve reads back, and a value out of range is the default"
        );
        // The graphics-card settings: bad names and paths drop, repeats fold, the list is capped.
        let many: Vec<String> = (0..25).map(|i| format!("game{i}.exe")).collect();
        let g = read_lenient(&json!({
            "games": ["somegame", "SomeGame.EXE", "C:\\Games\\x.exe", "", "other.exe"],
            "gameLiftMinutes": 10, "restartTask": " Model Restart ", "pauseFile": "C:/models/paused.flag"
        }));
        assert_eq!(
            (
                g.games.as_slice(),
                g.game_lift_minutes,
                g.restart_task.as_deref(),
                g.pause_file.as_deref()
            ),
            (
                ["somegame.exe".to_owned(), "other.exe".to_owned()].as_slice(),
                10,
                Some("Model Restart"),
                Some("C:/models/paused.flag")
            )
        );
        let g = read_lenient(&json!({
            "games": many, "gameLiftMinutes": 0, "restartTask": "a\"b", "pauseFile": "paused.flag"
        }));
        assert_eq!(
            (
                g.games.len(),
                g.game_lift_minutes,
                g.restart_task,
                g.pause_file
            ),
            (MAX_GAMES, DEFAULT_GAME_LIFT, None, None),
            "out of range or unsafe values fall back"
        );
        assert_eq!(read_lenient(&json!("junk")), LocalSettings::default());
        assert_eq!(
            read_lenient(&json!({"servers": []})).servers.len(),
            0,
            "an empty list is the user's choice"
        );

        // Per-model fit: a bad key, an out-of-range size or a self-suggestion drops on its own;
        // good ones trim in.
        let fit = read_lenient(&json!({
            "comfort": {"local/ollama/qwen": 20_000, "ollama/qwen": 30_000, "local/ollama/a": 999, "local/ollama/b": 50_000},
            "stepUp": {"local/ollama/a": " local/ollama/big ", "local/ollama/b": "local/ollama/b", "local/ollama/c": "flash-next"}
        }));
        assert_eq!(
            (
                fit.comfort.get("local/ollama/qwen").copied(),
                fit.comfort.len(),
                fit.step_up.get("local/ollama/a").cloned(),
                fit.step_up.len()
            ),
            (Some(20_000), 2, Some("local/ollama/big".to_owned()), 1),
            "comfort sizes read, a self-suggestion and a foreign target drop"
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
            auto_start: true,
            folder: None,
            preset: None,
            chat_cap: None,
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
                auto_start: x.auto_start,
                folder: x.folder.clone(),
                preset: x.preset.clone(),
                chat_cap: x.chat_cap,
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

        // The start-when-needed fields save and clear; a blank or over-long folder is refused.
        let mut tuned = servers.clone();
        tuned[0].auto_start = false;
        tuned[0].folder = Some(" D:/Strata/current ".into());
        tuned[0].preset = Some("Fast".into());
        let next = apply_changes(
            &next,
            &Changes {
                servers: Some(tuned.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (
                next.servers[0].auto_start,
                next.servers[0].folder.as_deref(),
                next.servers[0].preset.as_deref()
            ),
            (false, Some("D:/Strata/current"), Some("Fast")),
            "trimmed on the way in"
        );
        tuned[0].auto_start = true;
        tuned[0].folder = Some("".into());
        tuned[0].preset = Some("  ".into());
        let next = apply_changes(
            &next,
            &Changes {
                servers: Some(tuned.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (
                next.servers[0].auto_start,
                next.servers[0].folder.as_deref(),
                next.servers[0].preset.as_deref()
            ),
            (true, None, None),
            "blank means the standard folder and preset"
        );
        // A folder must be a full path on this machine; a preset must not read as an option.
        for bad in [
            "x".repeat(MAX_FOLDER + 1),
            "Strata/terminal/current".to_string(),
            "\\\\server\\share\\Strata".to_string(),
        ] {
            tuned[0].folder = Some(bad.clone());
            assert!(
                apply_changes(
                    &next,
                    &Changes {
                        servers: Some(tuned.clone()),
                        ..Default::default()
                    }
                )
                .unwrap_err()
                .contains("Strata folder"),
                "{bad}"
            );
        }
        tuned[0].folder = Some("D:/Strata/current".into());
        tuned[0].preset = Some("-danger".into());
        assert!(apply_changes(
            &next,
            &Changes {
                servers: Some(tuned.clone()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("cannot start"));
        tuned[0].preset = Some("Fast".into());
        assert!(apply_changes(
            &next,
            &Changes {
                servers: Some(tuned.clone()),
                ..Default::default()
            }
        )
        .is_ok());

        // A server's own working-chat cap saves, and a value out of range is refused whole.
        tuned[0].chat_cap = Some(0);
        tuned[1].chat_cap = Some(5);
        let capped = apply_changes(
            &next,
            &Changes {
                servers: Some(tuned.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (capped.servers[0].chat_cap, capped.servers[1].chat_cap),
            (Some(0), Some(5)),
            "0 switches the cap off; 5 caps the server at five chats"
        );
        tuned[0].chat_cap = Some(MAX_CHAT_CAP + 1);
        assert!(apply_changes(
            &next,
            &Changes {
                servers: Some(tuned.clone()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("working-chat cap"));

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
            (
                Changes {
                    speech_reserve_gb: Some(25),
                    ..Default::default()
                },
                "kept for speech",
            ),
            (
                Changes {
                    restart_task: Some("Model \"Restart\"".into()),
                    ..Default::default()
                },
                "restart task",
            ),
            (
                Changes {
                    restart_task: Some("x".repeat(MAX_TASK + 1)),
                    ..Default::default()
                },
                "restart task",
            ),
            (
                Changes {
                    pause_file: Some("models\\paused.flag".into()),
                    ..Default::default()
                },
                "full path",
            ),
            (
                Changes {
                    games: Some((0..21).map(|i| format!("game{i}.exe")).collect()),
                    ..Default::default()
                },
                "At most 20",
            ),
            (
                Changes {
                    games: Some(vec!["C:\\Games\\somegame.exe".into()]),
                    ..Default::default()
                },
                "without a folder",
            ),
            (
                Changes {
                    game_lift_minutes: Some(0),
                    ..Default::default()
                },
                "after a game closes",
            ),
            (
                Changes {
                    game_lift_minutes: Some(61),
                    ..Default::default()
                },
                "after a game closes",
            ),
            (
                Changes {
                    comfort: Some(BTreeMap::from([("local/ollama/qwen".to_owned(), 999u64)])),
                    ..Default::default()
                },
                "works well up to must be a whole number",
            ),
            (
                Changes {
                    comfort: Some(BTreeMap::from([("qwen".to_owned(), 20_000u64)])),
                    ..Default::default()
                },
                "not a local model",
            ),
            (
                Changes {
                    step_up: Some(BTreeMap::from([(
                        "local/ollama/qwen".to_owned(),
                        "local/ollama/qwen".to_owned(),
                    )])),
                    ..Default::default()
                },
                "in place of itself",
            ),
            (
                Changes {
                    step_up: Some(BTreeMap::from([(
                        "local/ollama/qwen".to_owned(),
                        "flash-next".to_owned(),
                    )])),
                    ..Default::default()
                },
                "must be a local model id",
            ),
        ] {
            let e = apply_changes(&cur, &changes).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
        // A good save: `.exe` is added, repeats in any case and blank lines go, empty text clears.
        let set = apply_changes(
            &cur,
            &Changes {
                games: Some(vec![
                    " somegame ".into(),
                    "SOMEGAME.exe".into(),
                    "".into(),
                    "other.exe".into(),
                ]),
                game_lift_minutes: Some(60),
                restart_task: Some(" Model Restart ".into()),
                pause_file: Some("D:\\models\\paused.flag".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(set.games, ["somegame.exe", "other.exe"]);
        assert_eq!(
            (
                set.game_lift_minutes,
                set.restart_task.as_deref(),
                set.pause_file.as_deref()
            ),
            (60, Some("Model Restart"), Some("D:\\models\\paused.flag"))
        );
        let cleared = apply_changes(
            &set,
            &Changes {
                games: Some(Vec::new()),
                restart_task: Some(" ".into()),
                pause_file: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (
                cleared.games.len(),
                cleared.restart_task,
                cleared.pause_file
            ),
            (0, None, None)
        );
        // A per-model fit saves like a name does: it sets those keys and leaves the others,
        // 0 clears a size and empty text clears a suggestion.
        let fitted = apply_changes(
            &cur,
            &Changes {
                comfort: Some(BTreeMap::from([(
                    "local/strata/qwen".to_owned(),
                    16_000u64,
                )])),
                step_up: Some(BTreeMap::from([(
                    "local/strata/qwen".to_owned(),
                    "local/strata/big".to_owned(),
                )])),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (
                fitted.comfort.get("local/strata/qwen").copied(),
                fitted.step_up.get("local/strata/qwen").cloned()
            ),
            (Some(16_000), Some("local/strata/big".to_owned()))
        );
        let fit_off = apply_changes(
            &fitted,
            &Changes {
                comfort: Some(BTreeMap::from([("local/strata/qwen".to_owned(), 0u64)])),
                step_up: Some(BTreeMap::from([(
                    "local/strata/qwen".to_owned(),
                    " ".to_owned(),
                )])),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            fitted.comfort.len() == 1 && fit_off.comfort.is_empty() && fit_off.step_up.is_empty(),
            "0 and empty text clear"
        );
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
        s.comfort.insert("local/ollama/qwen".to_owned(), 20_000);
        s.step_up.insert(
            "local/ollama/qwen".to_owned(),
            "local/strata/big".to_owned(),
        );
        db.write(|tx| store(tx, &s)).unwrap();
        assert_eq!(db.snapshot(load).unwrap(), s);
    }
}
