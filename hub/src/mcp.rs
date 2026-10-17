//! The MCP surface: JSON-RPC over streamable HTTP, answered with plain JSON.
//!
//! The hub has no server-initiated messages, so every POST is answered with
//! a single `application/json` body and the GET stream is not offered (the
//! transport allows that). Sessions are not needed either: a session id is
//! issued on `initialize` for clients that expect one, but no state hangs off
//! it.

use crate::auth::{Client, Scope};
use crate::store::{Embedding, MemoryPatch, NewMemory, SearchQuery, Store, StoreError};
use serde_json::{json, Value};

pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct Options {
    /// Offer `memory_update` and `memory_delete`.
    pub allow_mutate: bool,
}

/// Answers one JSON-RPC message. `None` for a notification.
pub fn handle(store: &mut Store, client: &Client, opts: &Options, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        // A response or garbage: nothing to answer unless it carried an id.
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
                "instructions": "Shared memory. memory_search finds memories by keywords; pass `vector` \
                    (with `model`) for a hybrid keyword + similarity search. Read a hit in full with memory_read."
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
            Ok(call_tool(store, client, opts, name, &args))
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

fn tool_list(client: &Client, opts: &Options) -> Vec<Value> {
    let vector = json!({
        "type": "array", "items": { "type": "number" },
        "description": "Embedding computed by the caller (the hub never computes one)."
    });
    let mut tools = vec![
        json!({
            "name": "memory_search",
            "description": "Search memories. Keyword (full-text) search on its own; with `vector`, a hybrid of keyword and cosine similarity over memories embedded with the same dimension (and `model`, if given).",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string" },
                "k": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 },
                "vector": vector,
                "model": { "type": "string", "description": "Only compare against vectors from this model." },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Every tag must match." }
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
            "description": "List memories newest first, optionally by tag or updated since a date (ISO-8601, UTC).",
            "inputSchema": { "type": "object", "properties": {
                "tag": { "type": "string" },
                "since": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 }
            } },
            "annotations": { "readOnlyHint": true }
        }),
    ];
    if client.scope == Scope::ReadWrite {
        tools.push(json!({
            "name": "memory_write",
            "description": "Store a new memory. Pass `vector` and `model` together to make it findable by similarity.",
            "inputSchema": { "type": "object", "properties": {
                "title": { "type": "string" },
                "body": { "type": "string" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "source": { "type": "string" },
                "name": { "type": "string", "description": "Optional unique key." },
                "vector": vector,
                "model": { "type": "string" }
            }, "required": ["title", "body"] }
        }));
        if opts.allow_mutate {
            tools.push(json!({
                "name": "memory_update",
                "description": "Change fields of a memory. A new body without a new vector clears the old vector.",
                "inputSchema": { "type": "object", "properties": {
                    "id": { "type": "integer" },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "description": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "source": { "type": "string" },
                    "vector": vector,
                    "model": { "type": "string" }
                }, "required": ["id"] }
            }));
            tools.push(json!({
                "name": "memory_delete",
                "description": "Delete a memory permanently.",
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
    name: &str,
    args: &Value,
) -> Value {
    let writes = !READ_TOOLS.contains(&name);
    if writes && client.scope == Scope::ReadOnly {
        return tool_error(format!("{name} needs a read-write token"));
    }
    let r = match name {
        "memory_search" => search(store, args),
        "memory_read" => read(store, args),
        "memory_list" => list(store, args),
        "memory_write" => write(store, args),
        "memory_update" | "memory_delete" if !opts.allow_mutate => {
            return tool_error(format!("{name} is turned off on this hub"));
        }
        "memory_update" => update(store, args),
        "memory_delete" => delete(store, args),
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

fn search(store: &mut Store, args: &Value) -> ToolResult {
    let q = SearchQuery {
        query: str_arg(args, "query")?.unwrap_or_default(),
        k: int_arg(args, "k")?.unwrap_or(10).max(1) as usize,
        vector: vector_arg(args)?,
        model: str_arg(args, "model")?,
        tags: tags_arg(args, "tags")?.unwrap_or_default(),
    };
    let mode = if q.vector.is_some() {
        "hybrid"
    } else {
        "keyword"
    };
    let hits = store.search(&q).map_err(store_err)?;
    Ok(json!({ "mode": mode, "count": hits.len(), "results": hits }))
}

fn read(store: &mut Store, args: &Value) -> ToolResult {
    let m = match args.get("id") {
        Some(Value::Number(n)) => store.get(n.as_i64().ok_or("id must be an integer")?),
        Some(Value::String(s)) => match s.parse::<i64>() {
            Ok(n) => store.get(n),
            Err(_) => store.get_by_name(s),
        },
        _ => return Err("id is required".into()),
    };
    m.map(|m| json!(m)).map_err(store_err)
}

fn list(store: &mut Store, args: &Value) -> ToolResult {
    let tag = str_arg(args, "tag")?;
    let since = str_arg(args, "since")?;
    let limit = int_arg(args, "limit")?.unwrap_or(50).max(1) as usize;
    let rows = store
        .list(tag.as_deref(), since.as_deref(), limit)
        .map_err(store_err)?;
    Ok(json!({ "count": rows.len(), "results": rows }))
}

fn write(store: &mut Store, args: &Value) -> ToolResult {
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
    };
    let id = store.insert(m).map_err(store_err)?;
    Ok(json!({ "id": id, "has_vector": store.has_vector(id).map_err(store_err)? }))
}

fn update(store: &mut Store, args: &Value) -> ToolResult {
    let id = int_arg(args, "id")?.ok_or("id is required")?;
    let patch = MemoryPatch {
        title: str_arg(args, "title")?,
        body: str_arg(args, "body")?,
        description: str_arg(args, "description")?,
        tags: tags_arg(args, "tags")?,
        source: str_arg(args, "source")?,
        embedding: embedding_arg(args)?,
    };
    let m = store.update(id, patch).map_err(store_err)?;
    Ok(json!({ "id": m.id, "updated_at": m.updated_at, "has_vector": m.dim.is_some() }))
}

fn delete(store: &mut Store, args: &Value) -> ToolResult {
    let id = int_arg(args, "id")?.ok_or("id is required")?;
    store.delete(id).map_err(store_err)?;
    Ok(json!({ "id": id, "deleted": true }))
}
