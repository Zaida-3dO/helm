//! The MCP admin host tools' bodies: list, add, edit, update and
//! remove. Each takes the tool's JSON arguments and returns its JSON result or a public error.
//! They are registered with the host-tool registry by [`crate::tools`] once that seam exists;
//! everything here is plain functions over [`McpStore`] so it is testable without it.
//!
//! Agent rules on top of the config rules: the revision is always required; the
//! "Allow unencrypted on my network" opt-in is user-only, so an agent can neither set it
//! nor carry it to a new host (changing a server's host clears it).

use crate::config::{McpStore, StoreError};
use crate::types::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const LIST: &str = "app_list_mcp_servers";
pub const ADD: &str = "app_add_mcp_server";
pub const EDIT: &str = "app_edit_mcp_server";
pub const UPDATE: &str = "app_update_mcp_server";
pub const REMOVE: &str = "app_remove_mcp_server";

/// A refusal; nothing was changed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AdminError(pub String);

impl From<StoreError> for AdminError {
    fn from(e: StoreError) -> Self {
        AdminError(e.to_string())
    }
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, AdminError> {
    serde_json::from_value(args).map_err(|e| AdminError(format!("Invalid arguments: {e}")))
}

pub fn agent_view(s: &McpServer) -> AgentServerView {
    let t = &s.transport;
    AgentServerView {
        id: s.id.clone(),
        name: s.name.clone(),
        enabled: s.enabled,
        kind: t.kind,
        command: t.command.clone(),
        arg_count: t.args.len(),
        env_names: t.env.keys().cloned().collect(),
        url: t.url.clone(),
        header_names: t.headers.keys().cloned().collect(),
        oauth: t.oauth.is_some(),
        allow_private_http: s.allow_private_http,
    }
}

fn agent_snapshot(revision: String, servers: &[McpServer]) -> Value {
    serde_json::to_value(AgentSnapshot {
        revision,
        servers: servers.iter().map(agent_view).collect(),
    })
    .expect("snapshot serialises")
}

const OPT_IN_USER_ONLY: &str = "\"Allow unencrypted on my network\" is user-only: ask the user to turn it on for this server in Settings → MCP.";

pub fn list(store: &McpStore, args: Value) -> Result<Value, AdminError> {
    if !(args.is_null() || args.as_object().is_some_and(|o| o.is_empty())) {
        return Err(AdminError(format!("{LIST} takes no arguments.")));
    }
    let snap = store.snapshot()?;
    Ok(agent_snapshot(snap.revision, &snap.servers))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddServer {
    id: String,
    name: String,
    enabled: bool,
    transport: Transport,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddArgs {
    revision: String,
    server: AddServer,
}

pub fn add(store: &McpStore, args: Value) -> Result<Value, AdminError> {
    let a: AddArgs = parse(args)?;
    let s = a.server;
    let snap = store.mutate(Some(&a.revision), |list| {
        if list.iter().any(|x| x.id == s.id) {
            return Err(StoreError::Invalid(format!(
                "A server with the id {} already exists.",
                s.id
            )));
        }
        list.push(McpServer {
            id: s.id,
            name: s.name,
            enabled: s.enabled,
            transport: s.transport,
            allow_private_http: false,
            preset: None,
        });
        Ok(())
    })?;
    Ok(agent_snapshot(snap.revision, &snap.servers))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditArgs {
    revision: String,
    server_id: String,
    name: Option<String>,
    #[serde(rename = "type")]
    kind: Option<TransportKind>,
    command: Option<String>,
    args: Option<Vec<String>>,
    url: Option<String>,
    env: Option<BTreeMap<String, ValueRef>>,
    remove_env: Option<Vec<String>>,
    headers: Option<BTreeMap<String, ValueRef>>,
    remove_headers: Option<Vec<String>>,
    oauth: Option<OAuthConfig>,
    #[serde(default)]
    remove_oauth: bool,
}

fn host_of(url: Option<&str>) -> Option<String> {
    url.and_then(|u| url::Url::parse(u).ok())
        .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
}

/// Applies an edit to one server.
fn apply_edit(s: &mut McpServer, e: EditArgs) -> Result<(), StoreError> {
    let bad = |m: &str| Err(StoreError::Invalid(m.to_owned()));
    if let Some(n) = e.name {
        s.name = n;
    }
    if e.oauth.is_some() && e.remove_oauth {
        return bad("Set oauth or removeOauth, not both.");
    }
    for names in [&e.remove_env, &e.remove_headers].into_iter().flatten() {
        if names.len() > 32 || names.iter().any(|n| n.is_empty() || n.len() > 128) {
            return bad("Remove at most 32 names of 1 to 128 characters.");
        }
    }
    let t = &mut s.transport;
    let old_host = host_of(t.url.as_deref());
    if let Some(k) = e.kind {
        let was_stdio = t.kind == TransportKind::Stdio;
        let to_stdio = k == TransportKind::Stdio;
        if was_stdio != to_stdio {
            if to_stdio && e.command.is_none() {
                return bad("Moving a server onto stdio needs a command.");
            }
            if !to_stdio && e.url.is_none() {
                return bad("Moving a server off stdio needs a url.");
            }
            // Drop the old transport's fields.
            if to_stdio {
                t.url = None;
                t.headers.clear();
                t.oauth = None;
            } else {
                t.command = None;
                t.args.clear();
                t.env.clear();
            }
        }
        t.kind = k;
    }
    if let Some(c) = e.command {
        t.command = Some(c);
    }
    if let Some(a) = e.args {
        t.args = a;
    }
    if let Some(u) = e.url {
        t.url = Some(u);
    }
    if let Some(names) = e.remove_env {
        for n in names {
            t.env.remove(&n);
        }
    }
    if let Some(env) = e.env {
        t.env.extend(env);
    }
    if let Some(names) = e.remove_headers {
        for n in names {
            t.headers.retain(|k, _| !k.eq_ignore_ascii_case(&n));
        }
    }
    if let Some(h) = e.headers {
        let mut lower: Vec<String> = h.keys().map(|k| k.to_ascii_lowercase()).collect();
        lower.sort();
        if lower.windows(2).any(|w| w[0] == w[1]) {
            return bad("Two header names in one edit differ only by case.");
        }
        for (k, v) in h {
            t.headers.retain(|old, _| !old.eq_ignore_ascii_case(&k));
            t.headers.insert(k, v);
        }
    }
    if e.remove_oauth {
        t.oauth = None;
    }
    if let Some(o) = e.oauth {
        t.oauth = Some(o);
    }
    // The user's opt-in was for a host; an agent moving the server elsewhere drops it.
    if s.allow_private_http && host_of(s.transport.url.as_deref()) != old_host {
        s.allow_private_http = false;
    }
    if s.transport.kind == TransportKind::Stdio {
        s.allow_private_http = false;
    }
    Ok(())
}

pub fn edit(store: &McpStore, args: Value) -> Result<Value, AdminError> {
    let a: EditArgs = parse(args)?;
    let rev = a.revision.clone();
    let snap = store.mutate(Some(&rev), |list| {
        let s = list
            .iter_mut()
            .find(|s| s.id == a.server_id)
            .ok_or_else(|| StoreError::Invalid(format!("No server has the id {}.", a.server_id)))?;
        apply_edit(s, a)
    })?;
    Ok(agent_snapshot(snap.revision, &snap.servers))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateArgs {
    revision: String,
    server_id: String,
    enabled: bool,
}

pub fn update(store: &McpStore, args: Value) -> Result<Value, AdminError> {
    let a: UpdateArgs = parse(args)?;
    let snap = store.mutate(Some(&a.revision), |list| {
        let s = list
            .iter_mut()
            .find(|s| s.id == a.server_id)
            .ok_or_else(|| StoreError::Invalid(format!("No server has the id {}.", a.server_id)))?;
        s.enabled = a.enabled;
        Ok(())
    })?;
    Ok(agent_snapshot(snap.revision, &snap.servers))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveArgs {
    revision: String,
    server_id: String,
}

pub fn remove(store: &McpStore, args: Value) -> Result<Value, AdminError> {
    let a: RemoveArgs = parse(args)?;
    let snap = store.mutate(Some(&a.revision), |list| {
        let before = list.len();
        list.retain(|s| s.id != a.server_id);
        if list.len() == before {
            return Err(StoreError::Invalid(format!(
                "No server has the id {}.",
                a.server_id
            )));
        }
        Ok(())
    })?;
    Ok(agent_snapshot(snap.revision, &snap.servers))
}

/// Refuses the user-only opt-in in raw add/edit arguments before parsing, with the right words.
pub fn refuse_user_only(args: &Value) -> Result<(), AdminError> {
    let has = |v: &Value| v.get("allowPrivateHttp").is_some();
    if has(args) || args.get("server").is_some_and(has) {
        return Err(AdminError(OPT_IN_USER_ONLY.into()));
    }
    Ok(())
}

/// One admin tool's definition, ready for the host-tool registry.
pub struct AdminToolDef {
    pub name: &'static str,
    pub description: String,
    pub input_schema: Value,
    pub mutating: bool,
    /// Add and edit take up to 32 KiB of arguments; the others 8 KiB.
    pub large_request: bool,
}

const GUIDANCE: &str = " Act only on the user's explicit request, confirm ambiguous matches with app_list_mcp_servers first, and report what changed. Every change needs the current revision from app_list_mcp_servers. Saving never connects or signs in. Prefer environment or file references for credentials; never paste a secret the user did not ask you to store. OAuth sign-in and \"Allow unencrypted on my network\" are user-only, in Settings → MCP. If the reply is lost, the change may have happened: list the servers before retrying.";

fn value_ref_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {"type": "string", "enum": ["environment", "file", "literal"]},
            "value": {"type": "string", "description": "Variable name, absolute file path, or the literal value."},
            "secretId": {"type": "string", "description": "Keep a saved literal unchanged."},
            "prefix": {"type": "string", "maxLength": 64}
        },
        "required": ["kind"],
        "additionalProperties": false
    })
}

fn transport_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": {"type": "string", "enum": ["stdio", "http", "sse"]},
            "command": {"type": "string", "maxLength": 4096},
            "args": {"type": "array", "items": {"type": "string"}, "maxItems": 64},
            "env": {"type": "object", "additionalProperties": value_ref_schema()},
            "url": {"type": "string", "maxLength": 4096},
            "headers": {"type": "object", "additionalProperties": value_ref_schema()},
            "oauth": {"type": "object", "properties": {"clientId": {"type": "string"}, "scope": {"type": "string"}, "redirectUri": {"type": "string"}}, "additionalProperties": false}
        },
        "required": ["type"],
        "additionalProperties": false
    })
}

pub fn tool_defs() -> Vec<AdminToolDef> {
    let rev = json!({"type": "string", "minLength": 64, "maxLength": 64});
    let sid = json!({"type": "string", "minLength": 1, "maxLength": 24});
    vec![
        AdminToolDef {
            name: LIST,
            description: "List Helm's configured MCP servers, disabled ones included, and the revision that every change needs. Values of arguments, env and headers are never shown.".into(),
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            mutating: false,
            large_request: false,
        },
        AdminToolDef {
            name: ADD,
            description: format!("Add one MCP server to Helm. stdio servers run a real .exe; remote servers use https (plain http only on this computer, or on the private network after the user's opt-in).{GUIDANCE}"),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "revision": rev,
                    "server": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "pattern": "^[a-z0-9_]{1,24}$"},
                            "name": {"type": "string", "maxLength": 120},
                            "enabled": {"type": "boolean"},
                            "transport": transport_schema()
                        },
                        "required": ["id", "name", "enabled", "transport"],
                        "additionalProperties": false
                    }
                },
                "required": ["revision", "server"],
                "additionalProperties": false
            }),
            mutating: true,
            large_request: true,
        },
        AdminToolDef {
            name: EDIT,
            description: format!("Edit one MCP server in place (its id and enabled state stay). Moving onto stdio needs a command, off it a url; the old transport's fields are dropped. Changing only the url keeps the saved headers, so they go to the new host. Header names are case-insensitive.{GUIDANCE}"),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "revision": rev,
                    "serverId": sid,
                    "name": {"type": "string", "maxLength": 120},
                    "type": {"type": "string", "enum": ["stdio", "http", "sse"]},
                    "command": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}, "maxItems": 64},
                    "url": {"type": "string"},
                    "env": {"type": "object", "additionalProperties": value_ref_schema()},
                    "removeEnv": {"type": "array", "items": {"type": "string"}, "maxItems": 32},
                    "headers": {"type": "object", "additionalProperties": value_ref_schema()},
                    "removeHeaders": {"type": "array", "items": {"type": "string"}, "maxItems": 32},
                    "oauth": {"type": "object"},
                    "removeOauth": {"type": "boolean"}
                },
                "required": ["revision", "serverId"],
                "additionalProperties": false
            }),
            mutating: true,
            large_request: true,
        },
        AdminToolDef {
            name: UPDATE,
            description: format!("Enable or disable one MCP server.{GUIDANCE}"),
            input_schema: json!({
                "type": "object",
                "properties": {"revision": rev, "serverId": sid, "enabled": {"type": "boolean"}},
                "required": ["revision", "serverId", "enabled"],
                "additionalProperties": false
            }),
            mutating: true,
            large_request: false,
        },
        AdminToolDef {
            name: REMOVE,
            description: format!("Remove one MCP server. A saved OAuth sign-in is kept, so re-adding the same server reuses it.{GUIDANCE}"),
            input_schema: json!({
                "type": "object",
                "properties": {"revision": rev, "serverId": sid},
                "required": ["revision", "serverId"],
                "additionalProperties": false
            }),
            mutating: true,
            large_request: false,
        },
    ]
}

/// Dispatch by tool name (for the registry seam and tests).
pub fn call(store: &McpStore, name: &str, args: Value) -> Result<Value, AdminError> {
    match name {
        LIST => list(store, args),
        ADD => {
            refuse_user_only(&args)?;
            add(store, args)
        }
        EDIT => {
            refuse_user_only(&args)?;
            edit(store, args)
        }
        UPDATE => update(store, args),
        REMOVE => remove(store, args),
        _ => Err(AdminError(format!("Unknown MCP admin tool {name}."))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::{hub, store};

    fn rev(st: &McpStore) -> String {
        st.snapshot().unwrap().revision
    }

    fn add_stdio(st: &McpStore, id: &str) -> Value {
        call(
            st,
            ADD,
            json!({"revision": rev(st), "server": {"id": id, "name": "Fake", "enabled": true,
                "transport": {"type": "stdio", "command": "node.exe", "args": ["--secret-arg"],
                    "env": {"TOKEN": {"kind": "literal", "value": "hunter2"}}}}}),
        )
        .unwrap()
    }

    #[test]
    fn list_never_shows_values() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        add_stdio(&st, "fake");
        let out = call(&st, LIST, json!({})).unwrap().to_string();
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("--secret-arg"));
        assert!(out.contains("\"envNames\":[\"TOKEN\"]"));
        assert!(out.contains("\"argCount\":1"));
    }

    #[test]
    fn revision_is_required_and_checked() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let e = call(&st, UPDATE, json!({"serverId": "x", "enabled": true})).unwrap_err();
        assert!(e.0.contains("revision"), "{e}");
        add_stdio(&st, "fake");
        let stale = "0".repeat(64);
        let e = call(
            &st,
            UPDATE,
            json!({"revision": stale, "serverId": "fake", "enabled": false}),
        )
        .unwrap_err();
        assert!(e.0.contains("changed since"), "{e}");
        assert!(st.load().unwrap()[0].enabled);
    }

    #[test]
    fn agents_cannot_set_the_unencrypted_opt_in() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let e = call(
            &st,
            ADD,
            json!({"revision": rev(&st), "server": {"id": "h", "name": "H", "enabled": true, "allowPrivateHttp": true,
                "transport": {"type": "http", "url": "http://10.20.30.5/mcp"}}}),
        )
        .unwrap_err();
        assert!(e.0.contains("user-only"), "{e}");
        // Without the opt-in, private http is refused with the Settings hint.
        let e = call(
            &st,
            ADD,
            json!({"revision": rev(&st), "server": {"id": "h", "name": "H", "enabled": true,
                "transport": {"type": "http", "url": "http://10.20.30.5/mcp"}}}),
        )
        .unwrap_err();
        assert!(e.0.contains("Settings"), "{e}");
    }

    #[test]
    fn moving_an_opted_in_server_to_another_host_clears_the_opt_in() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        st.mutate(None, |l| {
            let mut s = hub("t");
            s.transport.url = Some("http://10.20.30.5:1/mcp".into());
            s.allow_private_http = true;
            l.push(s);
            Ok(())
        })
        .unwrap();
        // Same host, new path: kept.
        call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "hub", "url": "http://10.20.30.5:1/other"}),
        )
        .unwrap();
        assert!(st.load().unwrap()[0].allow_private_http);
        // Another private host: the opt-in is dropped, so the save is refused.
        let e = call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "hub", "url": "http://10.20.30.6:1/mcp"}),
        )
        .unwrap_err();
        assert!(e.0.contains("Allow unencrypted"), "{e}");
        assert_eq!(
            st.load().unwrap()[0].transport.url.as_deref(),
            Some("http://10.20.30.5:1/other")
        );
    }

    #[test]
    fn edit_semantics() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        add_stdio(&st, "fake");
        // Off stdio without a url is refused.
        assert!(call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "fake", "type": "http"})
        )
        .is_err());
        call(&st, EDIT, json!({"revision": rev(&st), "serverId": "fake", "type": "http", "url": "https://example.com/mcp",
            "headers": {"X-Key": {"kind": "environment", "value": "KEY"}}})).unwrap();
        let s = &st.load().unwrap()[0];
        assert!(
            s.transport.command.is_none()
                && s.transport.env.is_empty()
                && s.transport.args.is_empty()
        );
        // http -> sse keeps url and headers.
        call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "fake", "type": "sse"}),
        )
        .unwrap();
        let s = &st.load().unwrap()[0];
        assert_eq!(s.transport.kind, TransportKind::Sse);
        assert!(s.transport.headers.contains_key("X-Key"));
        // Case variants: replace, refuse duplicates in one edit, remove all variants.
        call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "fake",
            "headers": {"x-key": {"kind": "environment", "value": "KEY2"}}}),
        )
        .unwrap();
        assert_eq!(
            st.load().unwrap()[0]
                .transport
                .headers
                .keys()
                .collect::<Vec<_>>(),
            ["x-key"]
        );
        assert!(call(&st, EDIT, json!({"revision": rev(&st), "serverId": "fake",
            "headers": {"A": {"kind": "environment", "value": "K"}, "a": {"kind": "environment", "value": "K"}}})).is_err());
        call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "fake", "removeHeaders": ["X-KEY"]}),
        )
        .unwrap();
        assert!(st.load().unwrap()[0].transport.headers.is_empty());
        // oauth and removeOauth together are refused.
        assert!(call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "fake", "oauth": {}, "removeOauth": true})
        )
        .is_err());
    }

    #[test]
    fn add_refuses_duplicates_and_remove_and_update_work() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        add_stdio(&st, "fake");
        assert!(call(
            &st,
            ADD,
            json!({"revision": rev(&st), "server": {"id": "fake", "name": "F", "enabled": true,
            "transport": {"type": "stdio", "command": "x.exe"}}})
        )
        .is_err());
        call(
            &st,
            UPDATE,
            json!({"revision": rev(&st), "serverId": "fake", "enabled": false}),
        )
        .unwrap();
        assert!(!st.load().unwrap()[0].enabled);
        call(
            &st,
            REMOVE,
            json!({"revision": rev(&st), "serverId": "fake"}),
        )
        .unwrap();
        assert!(st.load().unwrap().is_empty());
        assert!(call(
            &st,
            REMOVE,
            json!({"revision": rev(&st), "serverId": "fake"})
        )
        .is_err());
    }

    #[test]
    fn tool_definitions_fit_claude_codes_limit() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 5);
        for d in defs {
            assert!(d.name.starts_with("app_"));
            assert!(
                d.description.chars().count() <= 2048,
                "{} is too long",
                d.name
            );
            assert_eq!(d.input_schema["type"], "object");
        }
    }
}
