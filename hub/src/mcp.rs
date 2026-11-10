//! The MCP surface: JSON-RPC over streamable HTTP, answered with plain JSON.
//!
//! The hub has no server-initiated messages, so every POST is answered with
//! a single `application/json` body and the GET stream is not offered (the
//! transport allows that). Sessions are not needed either: a session id is
//! issued on `initialize` for clients that expect one, but no state hangs off
//! it.
//!
//! A request from a Helm that carries `X-Helm-Person: <personId>` acts as that person: the memories
//! it reads are that person's (their own and the shared ones) and the ones it writes belong to
//! them. Only a token with the `peer` scope may do this, and only for its own person or a person
//! its machine was allowed to act for (`--acts-for`, see [`act_as`]); it is how a chat of one
//! person, running on another person's computer, keeps to its own person's memories.
//!
//! `memory_write` takes `scope`: `me` (the default: only the person sees it) or `everyone` (the
//! whole household does); `private` and `shared`, and the older `visibility`, mean the same.
//! `memory_search` and `memory_list` take `person`: `me` (the default) and `any` both mean the
//! memories the person may see, their own and the shared ones; a name or id narrows that to what
//! one person wrote. Someone else's private memories are never returned. Results carry `owner`,
//! the name of the person who wrote the memory.

use crate::auth::{Client, Scope};
use crate::registry::{Person, Registry};
use crate::store::{Embedding, MemoryPatch, NewMemory, SearchQuery, Store, StoreError, Visibility};
use serde_json::{json, Value};

pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The request header that names the person a memory call acts as.
pub const ACT_AS_HEADER: &str = "x-helm-person";

pub struct Options {
    /// Offer `memory_update` and `memory_delete`.
    pub allow_mutate: bool,
}

/// Why a request could not act as the person it named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActAsError {
    /// The client's token lacks the `peer` scope.
    NeedsPeer,
    /// No such person.
    UnknownPerson,
    /// Another person this machine was not allowed to act for.
    NotAllowed,
}

/// The client a request acts as: `None` when it names nobody (acts as itself), else a copy of
/// `client` that belongs to the named person (an id or a name). The scopes stay the client's own.
/// A machine acts only for its own person or for people it was allowed to act for, so another
/// person's private memories never reach a machine of someone else by default.
pub fn act_as(
    client: &Client,
    header: Option<&str>,
    registry: &Registry,
) -> Result<Option<Client>, ActAsError> {
    let who = header.map(str::trim).filter(|w| !w.is_empty());
    let Some(who) = who else {
        return Ok(None);
    };
    if !client.has(Scope::Peer) {
        return Err(ActAsError::NeedsPeer);
    }
    let person = registry
        .find_person(who)
        .map_err(|_| ActAsError::UnknownPerson)?;
    if person.id != client.person_id {
        if !registry.may_act_for(client.id, person.id) {
            return Err(ActAsError::NotAllowed);
        }
        eprintln!("mcp: {} acts for {}", client.name, person.name);
    }
    Ok(Some(Client {
        person_id: person.id,
        person: person.name,
        ..client.clone()
    }))
}

/// Answers one JSON-RPC message. `None` for a notification.
pub fn handle(store: &mut Store, client: &Client, opts: &Options, msg: &Value) -> Option<Value> {
    handle_with(store, client, opts, msg, &[])
}

/// [`handle`], knowing who the people are: results then carry `owner` as a name, and `person`
/// can name someone.
pub fn handle_with(
    store: &mut Store,
    client: &Client,
    opts: &Options,
    msg: &Value,
    people: &[Person],
) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        // A JSON-RPC response (a client answering a request) needs no reply; the transport
        // answers it 202. Anything else with an id is garbage and is told so.
        if msg.get("result").is_some() || msg.get("error").is_some() {
            return None;
        }
        return id.map(|id| error(id, -32600, "not a JSON-RPC request"));
    };
    let id = id?; // notifications get no reply
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .filter(|v| PROTOCOL_VERSIONS.contains(v))
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "helm-hub", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Memory for the household. You see your own memories and the ones \
                    others shared. memory_search finds memories by keywords; pass `vector` (with `model`) \
                    for a hybrid keyword + similarity search. Read a hit in full with memory_read. A \
                    new memory is private (scope \"me\") unless written with scope \"everyone\". Look \
                    at other people's memories only when the user asks."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list(client, opts) })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Ok(call_tool(store, client, opts, people, name, &args))
        }
        _ => Err((-32601, format!("method {method:?} not found"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, m)) => error(id, code, &m),
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_result(v: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
        "structuredContent": v,
        "isError": false
    })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": message.into() }], "isError": true })
}

const READ_TOOLS: &[&str] = &["memory_search", "memory_read", "memory_list"];
const WRITE_TOOLS: &[&str] = &["memory_write", "memory_update", "memory_delete"];

fn tool_list(client: &Client, opts: &Options) -> Vec<Value> {
    if !client.scopes.can_read_memory() {
        return Vec::new();
    }
    let visibility = json!({
        "type": "string", "enum": ["private", "shared"],
        "description": "private (default): only you see it. shared: everyone in the household sees it."
    });
    let scope = json!({
        "type": "string", "enum": ["me", "everyone", "private", "shared"],
        "description": "me (default): only this person sees it. everyone: the whole household sees it. Use everyone only when the memory holds whoever is asking, or the user says so. private and shared mean the same as me and everyone."
    });
    let person = json!({
        "type": "string",
        "description": "Whose memories: me or any (default: this person's own and the shared ones), or a person's name or id to see only what they wrote. Look at other people's memories only when the user asks."
    });
    let vector = json!({
        "type": "array", "items": { "type": "number" },
        "description": "Embedding computed by the caller (the hub never computes one)."
    });
    let mut tools = vec![
        json!({
            "name": "memory_search",
            "description": "Search memories. Keyword (full-text) search on its own; with `vector`, a hybrid of keyword and cosine similarity over memories embedded with the same dimension (and `model`, if given). Each result names its owner.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string" },
                "k": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 },
                "vector": vector,
                "model": { "type": "string", "description": "Only compare against vectors from this model." },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Every tag must match." },
                "person": person
            }, "required": ["query"] },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "memory_read",
            "description": "Read one memory in full, by numeric id or by name.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": ["integer", "string"], "description": "Numeric id, or the memory's name." }
            }, "required": ["id"] },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "memory_list",
            "description": "List memories newest first, optionally by tag or updated since a date (ISO-8601, UTC). Each result names its owner.",
            "inputSchema": { "type": "object", "properties": {
                "tag": { "type": "string" },
                "since": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 },
                "person": person
            } },
            "annotations": { "readOnlyHint": true }
        }),
    ];
    if client.scopes.can_write_memory() {
        tools.push(json!({
            "name": "memory_write",
            "description": "Store a new memory, for this person only unless `scope` is everyone. Pass `vector` and `model` together to make it findable by similarity.",
            "inputSchema": { "type": "object", "properties": {
                "title": { "type": "string" },
                "body": { "type": "string" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "source": { "type": "string" },
                "name": { "type": "string", "description": "Optional unique key." },
                "scope": scope,
                "visibility": visibility,
                "vector": vector,
                "model": { "type": "string" }
            }, "required": ["title", "body"] }
        }));
        if opts.allow_mutate {
            tools.push(json!({
                "name": "memory_update",
                "description": "Change fields of one of your memories. A new body without a new vector clears the old vector.",
                "inputSchema": { "type": "object", "properties": {
                    "id": { "type": "integer" },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "description": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "source": { "type": "string" },
                    "scope": scope,
                    "visibility": visibility,
                    "vector": vector,
                    "model": { "type": "string" }
                }, "required": ["id"] }
            }));
            tools.push(json!({
                "name": "memory_delete",
                "description": "Delete one of your memories permanently.",
                "inputSchema": { "type": "object", "properties": {
                    "id": { "type": "integer" }
                }, "required": ["id"] },
                "annotations": { "destructiveHint": true }
            }));
        }
    }
    tools
}

fn call_tool(
    store: &mut Store,
    client: &Client,
    opts: &Options,
    people: &[Person],
    name: &str,
    args: &Value,
) -> Value {
    // A name the hub does not have is unknown whatever the token may do.
    if !READ_TOOLS.contains(&name) && !WRITE_TOOLS.contains(&name) {
        return tool_error(format!("unknown tool {name:?}"));
    }
    let writes = !READ_TOOLS.contains(&name);
    if !client.scopes.can_read_memory() {
        return tool_error(format!("{name} needs a token with the memory scope"));
    }
    if writes && !client.scopes.can_write_memory() {
        return tool_error(format!("{name} needs a read-write token"));
    }
    let r = match name {
        "memory_search" => search(store, client, people, args),
        "memory_read" => read(store, client, people, args),
        "memory_list" => list(store, client, people, args),
        "memory_write" => write(store, client, args),
        "memory_update" | "memory_delete" if !opts.allow_mutate => {
            return tool_error(format!("{name} is turned off on this hub"));
        }
        "memory_update" => update(store, client, args),
        "memory_delete" => delete(store, client, args),
        _ => return tool_error(format!("unknown tool {name:?}")),
    };
    match r {
        Ok(v) => tool_result(v),
        Err(e) => tool_error(e),
    }
}

type ToolResult = Result<Value, String>;

fn str_arg(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn int_arg(args: &Value, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{key} must be an integer")),
    }
}

fn tags_arg(args: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|t| {
                t.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{key} must be strings"))
            })
            .collect::<Result<_, _>>()
            .map(Some),
        Some(_) => Err(format!("{key} must be an array of strings")),
    }
}

fn vector_arg(args: &Value) -> Result<Option<Vec<f32>>, String> {
    match args.get("vector") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| "vector must be numbers".to_owned())
            })
            .collect::<Result<_, _>>()
            .map(Some),
        Some(_) => Err("vector must be an array of numbers".into()),
    }
}

fn embedding_arg(args: &Value) -> Result<Option<Embedding>, String> {
    let model = str_arg(args, "model")?;
    match (vector_arg(args)?, model) {
        (None, _) => Ok(None),
        (Some(_), None) => Err("a vector needs `model`, the id of the model that made it".into()),
        (Some(values), Some(model)) => Ok(Some(Embedding { model, values })),
    }
}

fn store_err(e: StoreError) -> String {
    e.to_string()
}

/// Who may see a memory: `scope` (`me` or `everyone`, or `private` and `shared`) or the older
/// `visibility` (`private` or `shared`). Naming both is fine when they agree.
fn visibility_arg(args: &Value) -> Result<Option<Visibility>, String> {
    let by_scope = match str_arg(args, "scope")? {
        None => None,
        Some(s) => Some(match s.trim() {
            "me" => Visibility::Private,
            "everyone" => Visibility::Shared,
            other => {
                Visibility::parse(other).ok_or_else(|| "scope must be me or everyone".to_owned())?
            }
        }),
    };
    let by_visibility = match str_arg(args, "visibility")? {
        None => None,
        Some(v) => Some(
            Visibility::parse(&v)
                .ok_or_else(|| "visibility must be private or shared".to_owned())?,
        ),
    };
    match (by_scope, by_visibility) {
        (Some(a), Some(b)) if a != b => Err("scope and visibility disagree".into()),
        (a, b) => Ok(a.or(b)),
    }
}

/// `owner` as a name: the person's, or null when the hub does not know them.
fn with_owner(mut v: Value, owner: Option<i64>, people: &[Person]) -> Value {
    let name = owner
        .and_then(|id| people.iter().find(|p| p.id == id))
        .map_or(Value::Null, |p| Value::String(p.name.clone()));
    v["owner"] = name;
    v
}

/// The `person` argument: `None` for me and any (the person's own memories and the shared ones,
/// as the store already limits them), else the one person to narrow to, by id or name.
fn person_arg(args: &Value, people: &[Person]) -> Result<Option<i64>, String> {
    let Some(who) = str_arg(args, "person")? else {
        return Ok(None);
    };
    let who = who.trim();
    if who.is_empty() || who.eq_ignore_ascii_case("me") || who.eq_ignore_ascii_case("any") {
        return Ok(None);
    }
    let by_id = who
        .parse::<i64>()
        .ok()
        .and_then(|id| people.iter().find(|p| p.id == id));
    by_id
        .or_else(|| people.iter().find(|p| p.name.eq_ignore_ascii_case(who)))
        .map(|p| Some(p.id))
        .ok_or_else(|| format!("no person called {who:?}"))
}

/// A memory the caller may change: their own. Someone else's shared memory answers that only its
/// owner can change it; someone else's private one is "not found", as for a read.
fn owned_by(store: &Store, client: &Client, id: i64) -> Result<(), String> {
    let m = store.get(id).map_err(store_err)?;
    if m.owner_person == Some(client.person_id) {
        Ok(())
    } else if m.visibility == Visibility::Shared.as_str() {
        Err("only the person who wrote this memory can change it".into())
    } else {
        Err(store_err(StoreError::NotFound))
    }
}

fn search(store: &mut Store, client: &Client, people: &[Person], args: &Value) -> ToolResult {
    let q = SearchQuery {
        query: str_arg(args, "query")?.unwrap_or_default(),
        k: int_arg(args, "k")?.unwrap_or(10).max(1) as usize,
        vector: vector_arg(args)?,
        model: str_arg(args, "model")?,
        tags: tags_arg(args, "tags")?.unwrap_or_default(),
        viewer: Some(client.person_id),
        owner: person_arg(args, people)?,
    };
    let mode = if q.vector.is_some() {
        "hybrid"
    } else {
        "keyword"
    };
    let hits = store.search(&q).map_err(store_err)?;
    let results: Vec<Value> = hits
        .iter()
        .map(|h| with_owner(json!(h), h.summary.owner_person, people))
        .collect();
    Ok(json!({ "mode": mode, "count": results.len(), "results": results }))
}

fn read(store: &mut Store, client: &Client, people: &[Person], args: &Value) -> ToolResult {
    let me = client.person_id;
    let m = match args.get("id") {
        Some(Value::Number(n)) => store.get_visible(n.as_i64().ok_or("id must be an integer")?, me),
        Some(Value::String(s)) => match s.parse::<i64>() {
            Ok(n) => store.get_visible(n, me),
            Err(_) => store.get_by_name_visible(s, me),
        },
        _ => return Err("id is required".into()),
    };
    m.map(|m| with_owner(json!(m), m.owner_person, people))
        .map_err(store_err)
}

fn list(store: &mut Store, client: &Client, people: &[Person], args: &Value) -> ToolResult {
    let tag = str_arg(args, "tag")?;
    let since = str_arg(args, "since")?;
    let limit = int_arg(args, "limit")?.unwrap_or(50).max(1) as usize;
    let rows = store
        .list_by(
            Some(client.person_id),
            person_arg(args, people)?,
            tag.as_deref(),
            since.as_deref(),
            limit,
        )
        .map_err(store_err)?;
    let results: Vec<Value> = rows
        .iter()
        .map(|r| with_owner(json!(r), r.owner_person, people))
        .collect();
    Ok(json!({ "count": results.len(), "results": results }))
}

fn write(store: &mut Store, client: &Client, args: &Value) -> ToolResult {
    let m = NewMemory {
        name: str_arg(args, "name")?,
        title: str_arg(args, "title")?.ok_or("title is required")?,
        body: str_arg(args, "body")?.ok_or("body is required")?,
        description: None,
        kind: None,
        tags: tags_arg(args, "tags")?.unwrap_or_default(),
        source: str_arg(args, "source")?,
        links: crate::import::wiki_links(args.get("body").and_then(Value::as_str).unwrap_or("")),
        embedding: embedding_arg(args)?,
        // The owner is the caller's person, never something the caller can claim.
        owner: Some(client.person_id),
        visibility: visibility_arg(args)?.unwrap_or_default(),
    };
    let id = store.insert(m).map_err(store_err)?;
    Ok(json!({ "id": id, "has_vector": store.has_vector(id).map_err(store_err)? }))
}

fn update(store: &mut Store, client: &Client, args: &Value) -> ToolResult {
    let id = int_arg(args, "id")?.ok_or("id is required")?;
    owned_by(store, client, id)?;
    let patch = MemoryPatch {
        title: str_arg(args, "title")?,
        body: str_arg(args, "body")?,
        description: str_arg(args, "description")?,
        tags: tags_arg(args, "tags")?,
        source: str_arg(args, "source")?,
        embedding: embedding_arg(args)?,
        visibility: visibility_arg(args)?,
    };
    let m = store.update(id, patch).map_err(store_err)?;
    Ok(json!({ "id": m.id, "updated_at": m.updated_at, "has_vector": m.dim.is_some() }))
}

fn delete(store: &mut Store, client: &Client, args: &Value) -> ToolResult {
    let id = int_arg(args, "id")?.ok_or("id is required")?;
    owned_by(store, client, id)?;
    store.delete(id).map_err(store_err)?;
    Ok(json!({ "id": id, "deleted": true }))
}
