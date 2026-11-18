//! `app_memory_index`: what Helm indexes for the shared memory search, for an agent that has been
//! asked to set it up or look at it.
//!
//! `list` and `status` only read, `switch` turns indexing on or off and picks the embedding
//! server, and `reindex` asks for a pass. Anything that widens what leaves this PC goes through
//! the Allow card the way other settings changes do: `add` and `remove`, turning indexing on, and
//! changing the embedding server. Without a `grant` the ask is recorded and refused with the
//! card's hint, and with the user's approved grant the change is made. Turning indexing off and
//! naming another model on the same server need no card. Any chat, a household member's included,
//! can ask for any of them; a source it adds is filed under the profile it names, or its own.

use crate::model::Kind;
use crate::service::{Service, SettingsChanges, SourceInput};
use crate::store;
use helm_consent::kinds::{AllowKind, AllowKinds, Canonical};
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

/// The Allow kind behind adding and removing a source.
pub const KIND: &str = "memory_source";
const USED_BY: &str = "app_memory_index";

fn refuse(m: impl Into<String>) -> ToolError {
    ToolError::refused(m)
}

fn s<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
}

fn list_of(a: &Value, k: &str) -> Vec<String> {
    let mut v: Vec<String> = a
        .get(k)
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|x| x.as_str().map(|t| t.trim().to_owned()))
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.dedup();
    v
}

fn kind_name(k: &str) -> &'static str {
    match Kind::parse(k) {
        Some(Kind::Folder) => "a folder of notes",
        Some(Kind::ChatLogs) => "a folder of chat logs",
        Some(Kind::HelmChats) => "this PC's Helm chats",
        Some(Kind::ClaudeCode) => "this PC's Claude Code sessions",
        None => "a source",
    }
}

/// The profile a source is filed under: the one asked for, whoever's chat asks. When none is
/// named it is the chat's own profile for a household member's chat, and the PC's own person
/// (empty) otherwise.
fn filed_under(tx: &Transaction<'_>, chat: &str, asked: &str) -> Result<String, String> {
    if !asked.is_empty() || !helm_profiles::store::is_household_member(tx, chat) {
        return Ok(asked.to_owned());
    }
    let own = helm_profiles::store::profile_of_chat(tx, chat)
        .map_err(|e| e.to_string())?
        .ok_or("This chat has no profile to file a source under.")?;
    Ok(own.id)
}

/// The card's target, built from the host's own records. An `add` is bound to everything it would
/// upload and the profile it is filed under; a `remove` to the source as registered.
fn canonical(tx: &Transaction<'_>, chat: &str, target: &Value) -> Result<Canonical, String> {
    let bad = || "That memory change is malformed.".to_owned();
    match s(target, "action").ok_or_else(bad)? {
        "add" => {
            let kind = s(target, "kind").ok_or_else(bad)?;
            let k = Kind::parse(kind).ok_or_else(bad)?;
            let path = if k.has_path() {
                s(target, "path").ok_or_else(bad)?.trim().to_owned()
            } else {
                String::new()
            };
            let scope = match s(target, "scope").unwrap_or("everyone") {
                v @ ("me" | "everyone") => v,
                _ => return Err(bad()),
            };
            let person = filed_under(tx, chat, s(target, "personId").unwrap_or(""))?;
            let who = if person.is_empty() {
                helm_profiles::store::home(tx).map_err(|e| e.to_string())?
            } else {
                helm_profiles::store::get(tx, &person).map_err(|e| e.to_string())?
            }
            .ok_or("That profile does not exist.")?;
            let (include, exclude) = (list_of(target, "include"), list_of(target, "exclude"));
            let what = if path.is_empty() {
                kind_name(kind).to_owned()
            } else {
                format!("{}: {path}", kind_name(kind))
            };
            let mut details = vec![
                format!("Filed under: {}", who.name),
                format!(
                    "Who can find it: {}",
                    if scope == "me" {
                        "only its person"
                    } else {
                        "everyone in the household"
                    }
                ),
            ];
            if !include.is_empty() {
                details.push(format!("Only: {}", include.join(", ")));
            }
            if !exclude.is_empty() {
                details.push(format!("Leave out: {}", exclude.join(", ")));
            }
            Ok(Canonical {
                target: json!({"action": "add", "kind": kind, "path": path, "personId": person,
                               "scope": scope, "include": include, "exclude": exclude}),
                summary: format!("Index {what} in memory search"),
                fixed_form: "Approve adding this memory source.".into(),
                details,
            })
        }
        "remove" => {
            let id = s(target, "id").ok_or_else(bad)?;
            let src = store::source(tx, id)
                .map_err(|e| e.to_string())?
                .ok_or("That source is not registered.")?;
            let what = if src.path.is_empty() {
                kind_name(src.kind.as_str()).to_owned()
            } else {
                format!("{}: {}", kind_name(src.kind.as_str()), src.path)
            };
            Ok(Canonical {
                target: json!({"action": "remove", "id": src.id, "kind": src.kind.as_str(), "path": src.path}),
                summary: format!("Stop indexing {what} and remove it from memory search"),
                fixed_form: "Approve removing this memory source.".into(),
                details: Vec::new(),
            })
        }
        "switch" => {
            let enabled = target.get("enabled").and_then(Value::as_bool);
            let server = s(target, "serverId");
            if enabled != Some(true) && server.is_none() {
                return Err(bad());
            }
            let mut parts = Vec::new();
            let mut details = Vec::new();
            if enabled == Some(true) {
                parts.push("Turn memory indexing on".to_owned());
                details.push(
                    "Notes, chats and sessions in the sources already added are sent to the hub."
                        .to_owned(),
                );
            }
            if let Some(id) = server {
                parts.push(format!("embed memory text on the server {id}"));
                details.push(format!(
                    "Embedding server: {id}. It is sent the text of every passage."
                ));
            }
            let mut summary = parts.join(" and ");
            if let Some(c) = summary.get_mut(..1) {
                c.make_ascii_uppercase();
            }
            Ok(Canonical {
                target: json!({"action": "switch", "enabled": enabled, "serverId": server}),
                summary,
                fixed_form: "Approve this memory indexing change.".into(),
                details,
            })
        }
        _ => Err(bad()),
    }
}

pub fn register_allow_kinds(k: &mut AllowKinds) {
    k.register(AllowKind {
        id: KIND,
        label: "Change what memory search indexes",
        accepts_words: true,
        canonical,
    });
}

/// The workflow-shaped gate: without a grant, record the ask and refuse with the Allow hint; with
/// one, spend the user's Allow for exactly this change.
fn gate(ctx: &ToolCtx, args: &Value, target: &Value) -> Result<(), ToolError> {
    let db = ctx
        .services
        .get::<helm_core::db::Database>()
        .ok_or_else(|| refuse("Consent is not available."))?;
    let kinds = ctx
        .services
        .get::<AllowKinds>()
        .ok_or_else(|| refuse("Consent is not available."))?;
    let grant = s(args, "grant").is_some();
    let now = helm_core::now_ms();
    let chat = ctx.caller_chat_id.clone();
    let target = target.clone();
    let outcome = db
        .write::<_, rusqlite::Error>(move |tx| {
            let r: Result<(), String> = if grant {
                helm_consent::store::spend_grant(tx, &kinds, &chat, KIND, &target, USED_BY, now)
                    .map(|_| ())
                    .map_err(|r| r.0)
            } else {
                match helm_consent::store::record_ask(tx, &kinds, &chat, KIND, &target, now) {
                    Ok(()) => {
                        let body = "Changing what memory search indexes needs the user's permission: file an Allow card with app_request_allow and ask the user to approve it.";
                        Err(helm_consent::store::with_allow_hint(
                            tx, &kinds, &chat, KIND, &target, body,
                        )
                        .unwrap_or_else(|_| body.to_owned()))
                    }
                    Err(r) => Err(r.0),
                }
            };
            Ok(r)
        })
        .map_err(|e| refuse(e.to_string()))?;
    outcome.map_err(refuse)
}

fn filed_under_chat(ctx: &ToolCtx, asked: &str) -> Result<String, ToolError> {
    let db = ctx
        .services
        .get::<helm_core::db::Database>()
        .ok_or_else(|| refuse("Consent is not available."))?;
    let chat = ctx.caller_chat_id.clone();
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(filed_under(tx, &chat, asked)))
        .map_err(|e| refuse(e.to_string()))?
        .map_err(refuse)
}

fn service(ctx: &ToolCtx) -> Result<Arc<Service>, ToolError> {
    ctx.services
        .get::<Service>()
        .ok_or_else(|| refuse("Memory indexing is not available here."))
}

fn memory_index(ctx: &ToolCtx, a: Value) -> ToolResult {
    let svc = service(ctx)?;
    match s(&a, "action").ok_or_else(|| refuse("action is required."))? {
        "list" => Ok(json!({"sources": svc.view().sources})),
        "status" => {
            let v = svc.view();
            Ok(json!({
                "enabled": v.enabled, "hub": v.hub, "status": v.status, "model": v.model,
                "serverId": v.server_id, "pending": v.pending,
                "sources": v.sources.iter().map(|x| json!({
                    "id": x.id, "kind": x.kind, "docs": x.docs, "pushed": x.pushed, "status": x.status,
                })).collect::<Vec<_>>(),
            }))
        }
        "switch" => {
            let c = SettingsChanges {
                enabled: a.get("enabled").and_then(Value::as_bool),
                server_id: s(&a, "serverId").map(str::to_owned),
                model: s(&a, "model").map(str::to_owned),
                nightly: None,
            };
            if c.enabled.is_none() && c.server_id.is_none() && c.model.is_none() {
                return Err(refuse("Say enabled, serverId or model."));
            }
            // A change that would be refused anyway must not spend the user's Allow.
            c.validate().map_err(refuse)?;
            // Turning indexing on and moving the embedding to another server widen what leaves
            // this PC, so they need the user's Allow; turning it off and naming another model on
            // the same server do not.
            let now = svc.view();
            let turns_on = c.enabled == Some(true) && !now.enabled;
            let moves_server = c.server_id.as_deref().is_some_and(|id| id != now.server_id);
            if turns_on || moves_server {
                let target = json!({
                    "action": "switch",
                    "enabled": turns_on.then_some(true),
                    "serverId": if moves_server { c.server_id.clone() } else { None },
                });
                gate(ctx, &a, &target)?;
            }
            svc.save_settings(c).map_err(refuse)?;
            Ok(json!({"status": svc.view().status}))
        }
        "reindex" => {
            svc.reindex(s(&a, "id").map(str::to_owned), false);
            Ok(json!({"started": true}))
        }
        "add" => {
            let person = filed_under_chat(ctx, s(&a, "personId").unwrap_or(""))?;
            let target = json!({
                "action": "add", "kind": s(&a, "kind"), "path": s(&a, "path").unwrap_or(""),
                "personId": person, "scope": s(&a, "scope").unwrap_or("everyone"),
                "include": list_of(&a, "include"), "exclude": list_of(&a, "exclude"),
            });
            gate(ctx, &a, &target)?;
            let input: SourceInput = serde_json::from_value(json!({
                "kind": s(&a, "kind"), "path": s(&a, "path"), "personId": Some(person.as_str()),
                "scope": s(&a, "scope"), "include": a.get("include"), "exclude": a.get("exclude"),
            }))
            .map_err(|_| refuse("kind is folder, helm_chats, claude_code or chat_logs; scope is me or everyone."))?;
            let id = svc.save_source(input).map_err(refuse)?;
            Ok(json!({"id": id, "started": true}))
        }
        "remove" => {
            let id = s(&a, "id").ok_or_else(|| refuse("id is required."))?;
            gate(ctx, &a, &json!({"action": "remove", "id": id}))?;
            svc.remove_source(id).map_err(refuse)?;
            Ok(json!({"removed": id}))
        }
        _ => Err(refuse(
            "action is list, status, switch, add, remove or reindex.",
        )),
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            "app_memory_index",
            "What Helm indexes for the shared memory search. action: list (the sources), status \
             (is it on, is the hub ready, what is left), switch ({enabled?, serverId?, model?}: \
             turn indexing on or off and pick the Ollama server that embeds), reindex ({id?}), add \
             ({kind: folder|helm_chats|claude_code|chat_logs, path (folder kinds), personId?, \
             scope?: everyone (the default: the household shares one memory)|me (only its person, on purpose), include?, exclude?: glob lists}) and remove ({id}). Add, remove, \
             turning indexing on and moving to another embedding server need the user's Allow: \
             the first call is refused with how to ask, and a call with the approved grant goes \
             through. Turning indexing off needs none.",
            json!({
                "type": "object",
                "required": ["action"],
                "additionalProperties": false,
                "properties": {
                    "action": {"type": "string", "enum": ["list", "status", "switch", "add", "remove", "reindex"]},
                    "id": {"type": "string"},
                    "kind": {"type": "string", "enum": ["folder", "helm_chats", "claude_code", "chat_logs"]},
                    "path": {"type": "string"},
                    "personId": {"type": "string"},
                    "scope": {"type": "string", "enum": ["me", "everyone"]},
                    "include": {"type": "array", "items": {"type": "string"}},
                    "exclude": {"type": "array", "items": {"type": "string"}},
                    "enabled": {"type": "boolean"},
                    "serverId": {"type": "string"},
                    "model": {"type": "string"},
                    "grant": {"type": "string"}
                }
            }),
        ),
        memory_index,
    );
}
