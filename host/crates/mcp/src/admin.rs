//! The MCP admin host tools' bodies: list, add, edit, update and
//! remove. Each takes the tool's JSON arguments and returns its JSON result or a public error.
//! They are registered with the host-tool registry by [`crate::tools`] once that seam exists;
//! everything here is plain functions over [`McpStore`] so it is testable without it.
//!
//! Agent rules on top of the config rules: the revision is always required; an agent may set
//! the "Allow unencrypted on my network" opt-in, but only for a plain-http server on a private
//! address or name (see [`crate::unencrypted`] for the notice and Undo that follow), and it
//! cannot carry the opt-in to a new host (changing a server's host clears it).

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
        let m = e.to_string();
        // Settings is where the user ticks the box; an agent is told how to ask for it itself.
        if m.contains(crate::validate::NEEDS_OPT_IN) {
            return AdminError(m.replace(
                crate::validate::NEEDS_OPT_IN,
                "Plain http to another machine needs allowPrivateHttp: true on this server, for a private-network address only (the user is shown a notice with Undo).",
            ));
        }
        AdminError(m)
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

const OPT_IN_NEEDS_PRIVATE_HTTP: &str = "\"allowPrivateHttp\" is for a server whose url is plain http to a private-network address or name. Public hosts need https, and a server that signs in with OAuth needs https too.";

/// True when `url` is plain http to a host the opt-in can cover: a private address or a name.
pub fn is_private_plain_http(url: Option<&str>) -> bool {
    let Some(u) = url.and_then(|u| url::Url::parse(u).ok()) else {
        return false;
    };
    u.scheme() == "http"
        && u.host().is_some_and(|h| {
            matches!(
                crate::validate::class_host(&h),
                crate::validate::HostClass::Private | crate::validate::HostClass::Name
            )
        })
}

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
    #[serde(default)]
    allow_private_http: bool,
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
    if s.allow_private_http
        && (s.transport.kind == TransportKind::Stdio
            || s.transport.oauth.is_some()
            || !is_private_plain_http(s.transport.url.as_deref()))
    {
        return Err(AdminError(OPT_IN_NEEDS_PRIVATE_HTTP.into()));
    }
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
            allow_private_http: s.allow_private_http,
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
    allow_private_http: Option<bool>,
}

fn host_of(url: Option<&str>) -> Option<String> {
    url.and_then(|u| url::Url::parse(u).ok())
        .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
}

/// Applies an edit to one server.
fn apply_edit(s: &mut McpServer, e: EditArgs) -> Result<(), StoreError> {
    let bad = |m: &str| Err(StoreError::Invalid(m.to_owned()));
    // Which hub person a chat acts as is Helm's to set, from the chat's profile.
    let names_person = |k: &String| k.eq_ignore_ascii_case(crate::config::PERSON_HEADER);
    if e.headers
        .as_ref()
        .is_some_and(|h| h.keys().any(names_person))
        || e.remove_headers
            .as_ref()
            .is_some_and(|h| h.iter().any(names_person))
    {
        return bad("Helm sets the person header itself; change it in Settings > MCP.");
    }
    // The Google accounts' program, its arguments (which hold the mail permission level) and its
    // sign-in settings are the user's to change.
    if s.preset.as_deref() == Some(crate::google::PRESET_ID) {
        let sign_in = |k: &String| {
            let u = k.to_ascii_uppercase();
            ["WORKSPACE_MCP_", "GOOGLE_OAUTH_", "OAUTHLIB_"]
                .iter()
                .any(|p| u.starts_with(p))
        };
        if e.command.is_some()
            || e.args.is_some()
            || e.kind.is_some()
            || e.env.as_ref().is_some_and(|m| m.keys().any(sign_in))
            || e.remove_env.as_ref().is_some_and(|l| l.iter().any(sign_in))
        {
            return bad("The Google accounts' program, arguments and sign-in settings are changed in Settings > MCP.");
        }
    }
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
    let was_stdio = s.transport.kind == TransportKind::Stdio;
    let t = &mut s.transport;
    let old_host = host_of(t.url.as_deref());
    if let Some(k) = e.kind {
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
    // An explicit choice, made after the host rule above so a call can move the server and
    // opt in for the new address together.
    match e.allow_private_http {
        Some(true) => {
            if was_stdio {
                return bad("Move the server off stdio first, then turn on allowPrivateHttp in a separate call.");
            }
            if s.transport.oauth.is_some() || !is_private_plain_http(s.transport.url.as_deref()) {
                return bad(OPT_IN_NEEDS_PRIVATE_HTTP);
            }
            s.allow_private_http = true;
        }
        Some(false) => s.allow_private_http = false,
        None => {}
    }
    Ok(())
}

pub fn edit(store: &McpStore, args: Value) -> Result<Value, AdminError> {
    let a: EditArgs = parse(args)?;
    let rev = a.revision.clone();
    let mut note = None;
    let snap = store.mutate(Some(&rev), |list| {
        let s = list
            .iter_mut()
            .find(|s| s.id == a.server_id)
            .ok_or_else(|| StoreError::Invalid(format!("No server has the id {}.", a.server_id)))?;
        let old_host = host_of(s.transport.url.as_deref());
        apply_edit(s, a)?;
        // Saved headers stay with a server whose address changed; here they now travel unencrypted.
        if s.allow_private_http
            && !s.transport.headers.is_empty()
            && host_of(s.transport.url.as_deref()) != old_host
        {
            note = Some(
                "This server's saved headers will now be sent in plain text over http to the new address. Remove them with removeHeaders if they are secrets.",
            );
        }
        Ok(())
    })?;
    let mut out = agent_snapshot(snap.revision, &snap.servers);
    if let (Some(n), Some(o)) = (note, out.as_object_mut()) {
        o.insert("note".into(), json!(n));
    }
    Ok(out)
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

/// One admin tool's definition, ready for the host-tool registry.
pub struct AdminToolDef {
    pub name: &'static str,
    pub description: String,
    pub input_schema: Value,
    pub mutating: bool,
    /// Add and edit take up to 32 KiB of arguments; the others 8 KiB.
    pub large_request: bool,
}

const GUIDANCE: &str = " Act only on the user's explicit request, confirm ambiguous matches with app_list_mcp_servers first, and report what changed. Every change needs the current revision from app_list_mcp_servers. Saving never connects or signs in. Prefer environment or file references for credentials; never paste a secret the user did not ask you to store. OAuth sign-in is user-only, in Settings → MCP. If the reply is lost, the change may have happened: list the servers before retrying.";

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
            description: format!("Add one MCP server to Helm. stdio servers run a real .exe; remote servers use https; plain http is for this computer, or for the private network (10.x, 172.16-31.x, 192.168.x, Tailscale 100.64-127.x, fc00::/7) when the server has allowPrivateHttp true. Set allowPrivateHttp only when the user asked for a server on their own network: the user sees a notice with Undo.{GUIDANCE}"),
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
                            "transport": transport_schema(),
                            "allowPrivateHttp": {"type": "boolean", "description": "Allow unencrypted http to a private-network address. The user is told and can Undo."}
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
            description: format!("Edit one MCP server in place (its id and enabled state stay). Moving onto stdio needs a command, off it a url; the old transport's fields are dropped. Changing only the url keeps the saved headers, so they go to the new host. Header names are case-insensitive. allowPrivateHttp turns \"Allow unencrypted on my network\" on or off for a plain-http server on a private address (the user sees a notice with Undo); moving the server to another host turns it off unless the same call sets it again.{GUIDANCE}"),
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
                    "removeOauth": {"type": "boolean"},
                    "allowPrivateHttp": {"type": "boolean"}
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
        ADD => add(store, args),
        EDIT => edit(store, args),
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
    fn agents_set_the_unencrypted_opt_in_only_for_private_plain_http() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let add_http = |st: &McpStore, id: &str, url: &str, opt: bool| {
            call(
                st,
                ADD,
                json!({"revision": rev(st), "server": {"id": id, "name": "H", "enabled": true, "allowPrivateHttp": opt,
                    "transport": {"type": "http", "url": url}}}),
            )
        };
        // Without the opt-in, private http is refused, and the agent is told how to ask for it.
        let e = add_http(&st, "h", "http://10.20.30.5/mcp", false).unwrap_err();
        assert!(
            e.0.contains("allowPrivateHttp") && !e.0.contains("Settings"),
            "{e}"
        );
        // The private ranges take it; public hosts, https and loopback do not need or take it.
        for ok in [
            "http://10.20.30.5/mcp",
            "http://172.20.0.1/mcp",
            "http://192.168.1.9:8080/mcp",
            "http://100.101.102.103/mcp",
            "http://[fd00::1]/mcp",
            "http://nas.local/mcp",
        ] {
            let id = format!("s{}", st.load().unwrap().len());
            add_http(&st, &id, ok, true).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "http://8.8.8.8/mcp",
            "https://10.20.30.5/mcp",
            "http://127.0.0.1/mcp",
        ] {
            let e = add_http(&st, "x", bad, true).unwrap_err();
            assert!(e.0.contains("allowPrivateHttp"), "{bad}: {e}");
        }
        assert!(st.load().unwrap().iter().all(|s| s.allow_private_http));
        // An edit turns it on and off.
        call(
            &st,
            ADD,
            json!({"revision": rev(&st), "server": {"id": "web", "name": "W", "enabled": true,
            "transport": {"type": "http", "url": "https://example.com/mcp"}}}),
        )
        .unwrap();
        let e = call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "web", "allowPrivateHttp": true}),
        )
        .unwrap_err();
        assert!(e.0.contains("allowPrivateHttp"), "{e}");
        call(&st, EDIT, json!({"revision": rev(&st), "serverId": "web", "url": "http://10.0.0.2/mcp", "allowPrivateHttp": true})).unwrap();
        assert!(
            st.load()
                .unwrap()
                .iter()
                .find(|s| s.id == "web")
                .unwrap()
                .allow_private_http
        );
        call(&st, EDIT, json!({"revision": rev(&st), "serverId": "web", "url": "https://example.com/mcp", "allowPrivateHttp": false})).unwrap();
        assert!(
            !st.load()
                .unwrap()
                .iter()
                .find(|s| s.id == "web")
                .unwrap()
                .allow_private_http
        );
        // A stdio server cannot be moved onto http and opted in in one call.
        add_stdio(&st, "fake");
        let e = call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "fake", "type": "http",
            "url": "http://10.0.0.3/mcp", "allowPrivateHttp": true}),
        )
        .unwrap_err();
        assert!(e.0.contains("separate call"), "{e}");
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
        assert!(e.0.contains("allowPrivateHttp"), "{e}");
        assert_eq!(
            st.load().unwrap()[0].transport.url.as_deref(),
            Some("http://10.20.30.5:1/other")
        );
        // Moving it and opting in together, with headers saved, warns they go out in plain text.
        let moved = call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "hub", "url": "http://10.20.30.6:1/mcp",
            "allowPrivateHttp": true, "headers": {"X-Key": {"kind": "environment", "value": "KEY"}}}),
        )
        .unwrap();
        assert!(moved["note"].as_str().unwrap().contains("plain text"));
        let same = call(
            &st,
            EDIT,
            json!({"revision": rev(&st), "serverId": "hub", "url": "http://10.20.30.6:1/again"}),
        )
        .unwrap();
        assert!(same.get("note").is_none(), "no move, no warning");
    }

    #[test]
    fn edit_semantics() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        add_stdio(&st, "fake");
        // The person header is Helm's, and the Google preset's program, arguments and sign-in
        // settings are the user's: an agent's edit of them is refused, other edits pass.
        call(
            &st,
            ADD,
            json!({"revision": rev(&st), "server": {"id": "google", "name": "Google", "enabled": true,
                "transport": {"type": "stdio", "command": "uvx.exe", "args": ["workspace-mcp"]}}}),
        )
        .unwrap();
        st.mutate(None, |l| {
            if let Some(g) = l.iter_mut().find(|s| s.id == "google") {
                g.preset = Some(crate::google::PRESET_ID.into());
            }
            Ok(())
        })
        .unwrap();
        for (id, edit, ok) in [
            (
                "fake",
                json!({"headers": {"x-helm-person": {"kind": "literal", "value": "7"}}}),
                false,
            ),
            ("fake", json!({"removeHeaders": ["X-Helm-Person"]}), false),
            (
                "google",
                json!({"args": ["workspace-mcp", "--permissions", "gmail:send"]}),
                false,
            ),
            ("google", json!({"command": "evil.exe"}), false),
            (
                "google",
                json!({"env": {"WORKSPACE_MCP_PORT": {"kind": "literal", "value": "1"}}}),
                false,
            ),
            (
                "google",
                json!({"removeEnv": ["oauthlib_insecure_transport"]}),
                false,
            ),
            ("google", json!({"name": "Mail"}), true),
            (
                "google",
                json!({"env": {"TZ": {"kind": "literal", "value": "UTC"}}}),
                true,
            ),
        ] {
            let mut args = json!({"revision": rev(&st), "serverId": id});
            args.as_object_mut()
                .unwrap()
                .extend(edit.as_object().unwrap().clone());
            assert_eq!(call(&st, EDIT, args).is_ok(), ok, "{id} {edit}");
        }
        st.mutate(None, |l| {
            l.retain(|s| s.id != "google");
            Ok(())
        })
        .unwrap();
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
