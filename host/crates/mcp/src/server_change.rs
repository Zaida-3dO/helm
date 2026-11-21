//! Changes to the MCP servers that only the user may approve: one Approve card each.
//!
//! Some changes an agent may not make by itself: running the browser broker, changing the Google
//! accounts' program, removing a built-in preset, giving a server one person's own password, or
//! raising what the Google accounts server may do with mail or calendar. The agent asks instead,
//! and Helm files one approval request (allow kind [`KIND`]) whose card lists the change exactly.
//! Nothing is saved until the user clicks Approve; then Helm applies the change itself and the
//! agent hears the outcome as a note.
//!
//! - **Asking.** The admin tools (`app_add_mcp_server`, `app_edit_mcp_server`,
//!   `app_update_mcp_server`, `app_remove_mcp_server`) take `askUser: true`: with it nothing
//!   changes and the card is filed. Two tools always file a card: [`SECRET_TOOL`] (a person's own
//!   value for a secret the server already holds, read from a file) and [`LEVEL_TOOL`] (the Gmail
//!   or Calendar level of a Google accounts server).
//! - **The card shows everything, whole, or is not filed.** Every line fits the card's limits, and
//!   a command or argument with a control or invisible character is refused. Variables and
//!   headers show their names and where their value comes from, never a value. The answer the
//!   agent gets holds the request's id and state only.
//! - **No secret value travels on a card.** A card's target is stored as plain text, so a literal
//!   value is refused when staging. A value comes from a file, read once when applying (and the
//!   file deleted unless the agent asked to keep it), or is typed in Settings.
//! - **The card and the change cannot drift.** Each target holds the revision of the MCP settings
//!   it was checked against. Applying checks again that nothing changed since, runs the same list
//!   change staging ran, and compares the result with the server as the card showed it; any
//!   difference saves nothing.

use crate::admin::{self, Change, Who};
use crate::config::{McpStore, StoreError};
use crate::google::{self, CalendarPermission, GooglePermission};
use crate::types::*;
use helm_consent::store::{self as consent, NewRequest};
use helm_consent::{AllowKind, AllowKinds, Applier, ApplyJob, Canonical};
use helm_core::db::Database;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};
use std::sync::Arc;

/// The allow kind of an MCP server change.
pub const KIND: &str = "mcp_server_change";
/// The tool that files a person's own value for a server's secret.
pub const SECRET_TOOL: &str = "app_set_mcp_secret";
/// The tool that files a change of a Google accounts server's Gmail or Calendar level.
pub const LEVEL_TOOL: &str = "app_set_mcp_level";

/// Longest path a card shows, so its line is whole.
const MAX_PATH: usize = 200;
const MAX_REASON: usize = 1_000;
/// What the consent store needs a reason to say when the agent gave none.
const NO_REASON: &str = "The agent asked for this change to an MCP server.";

const OPS: [&str; 6] = [
    "add",
    "edit",
    "set_enabled",
    "remove",
    "set_secret",
    "set_level",
];

// ---- what a card shows --------------------------------------------------------------------

/// A variable or header as a card shows it: its name and where its value comes from. The digest
/// is never shown; it only tells an edit that swapped one source for another from no change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShownRef {
    name: String,
    from: String,
    digest: String,
}

/// One server as the card shows it. Names only: no value, no variable name a reference points
/// at, no saved secret id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServerShown {
    id: String,
    name: String,
    enabled: bool,
    kind: TransportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    env: Vec<ShownRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    headers: Vec<ShownRef>,
    /// A digest of the browser sign-in settings, when the server has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth: Option<String>,
    allow_private_http: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
}

fn digest_of(v: &impl Serialize) -> String {
    let json = serde_json::to_value(v).expect("serialises");
    let text = helm_core::revisions::canonical_json(&json);
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn shown_refs(m: &BTreeMap<String, ValueRef>) -> Vec<ShownRef> {
    m.iter()
        .map(|(k, r)| ShownRef {
            name: k.clone(),
            from: match r.kind {
                RefKind::Environment => "a variable on this PC",
                RefKind::File => "a file on this PC",
                RefKind::Literal => "a saved value",
                RefKind::ChatPerson => "the chat's person",
            }
            .to_owned(),
            // The reference without its saved id: a saved value kept as it was is no change.
            digest: digest_of(&(
                r.kind,
                &r.value,
                &r.prefix,
                r.profiles.keys().collect::<Vec<_>>(),
            )),
        })
        .collect()
}

fn shown(s: &McpServer) -> ServerShown {
    let t = &s.transport;
    ServerShown {
        id: s.id.clone(),
        name: s.name.clone(),
        enabled: s.enabled,
        kind: t.kind,
        command: t.command.clone(),
        args: t.args.clone(),
        env: shown_refs(&t.env),
        url: t.url.clone(),
        headers: shown_refs(&t.headers),
        oauth: t.oauth.as_ref().map(|o| {
            digest_of(&(
                &o.client_id,
                &o.scope,
                &o.redirect_uri,
                o.client_secret.as_ref().map(|r| r.kind),
            ))
        }),
        allow_private_http: s.allow_private_http,
        preset: s.preset.clone(),
    }
}

/// A person's own value for one secret of a server, as the target holds it. The value is not in
/// it: the file's path is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SecretChange {
    server: String,
    server_name: String,
    /// The variable or header name, as the server stores it.
    name: String,
    /// `env` or `header`.
    slot: String,
    profile_id: String,
    profile_name: String,
    file: String,
    keep_file: bool,
}

/// A change of the Gmail or Calendar level of a Google accounts server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LevelChange {
    server: String,
    server_name: String,
    /// `gmail` or `calendar`.
    service: String,
    /// The level now: its wire name, or `custom` for one the picker does not offer.
    from: String,
    to: String,
}

/// The canonical target of one staged change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    op: String,
    /// The MCP settings revision the change was checked against.
    revision: String,
    /// The admin tool's arguments, without `askUser`, `reason` and `revision`; null for the
    /// tools that file a card of their own.
    #[serde(default)]
    args: Value,
    before: Option<ServerShown>,
    after: Option<ServerShown>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<SecretChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    level: Option<LevelChange>,
}

/// Text a card may show: no control character and nothing invisible or direction-changing, so
/// what the user reads is what is saved.
fn plain(text: &str, what: &str) -> Result<(), String> {
    if text
        .chars()
        .any(|c| c.is_control() || helm_core::hidden::is_hidden_char(c))
    {
        return Err(format!(
            "{what} holds a control, line-break or invisible character, which a card could not show as it is."
        ));
    }
    Ok(())
}

fn names(list: &[ShownRef]) -> String {
    list.iter()
        .map(|r| format!("{} ({})", r.name, r.from))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The lines that say what a server is.
fn describe(s: &ServerShown) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(p) = &s.preset {
        out.push(format!("Built-in preset: {p}"));
    }
    if s.kind == TransportKind::Stdio {
        out.push(format!("Runs: {}", s.command.as_deref().unwrap_or("")));
        for (i, a) in s.args.iter().enumerate() {
            out.push(format!("Argument {}: {a}", i + 1));
        }
        if !s.env.is_empty() {
            out.push(format!("Variables (names only): {}", names(&s.env)));
        }
    } else {
        out.push(format!("Address: {}", s.url.as_deref().unwrap_or("")));
        if !s.headers.is_empty() {
            out.push(format!("Headers (names only): {}", names(&s.headers)));
        }
        if s.oauth.is_some() {
            out.push("Signs in through the browser (OAuth).".into());
        }
        if s.allow_private_http {
            out.push("Plain http to this address is allowed.".into());
        }
    }
    out
}

/// Which parts of a server an edit changes, in words.
fn changed_fields(b: &ServerShown, a: &ServerShown) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut note = |differs: bool, what: &'static str| {
        if differs {
            out.push(what);
        }
    };
    note(b.name != a.name, "name");
    note(b.kind != a.kind, "type");
    note(b.command != a.command, "program");
    note(b.args != a.args, "arguments");
    note(b.env != a.env, "variables");
    note(b.url != a.url, "address");
    note(b.headers != a.headers, "headers");
    note(b.oauth != a.oauth, "sign-in");
    note(b.allow_private_http != a.allow_private_http, "plain http");
    out
}

fn visible(s: &ServerShown) -> Result<(), String> {
    plain(&s.id, "The server's id")?;
    plain(&s.name, "The server's name")?;
    plain(s.command.as_deref().unwrap_or(""), "The program")?;
    for (i, a) in s.args.iter().enumerate() {
        plain(a, &format!("Argument {}", i + 1))?;
    }
    plain(s.url.as_deref().unwrap_or(""), "The address")?;
    for r in s.env.iter().chain(&s.headers) {
        plain(&r.name, "A variable or header name")?;
    }
    Ok(())
}

const LEVELS_GMAIL: [&str; 3] = ["readonly", "drafts", "send"];
const LEVELS_CALENDAR: [&str; 3] = ["off", "readonly", "edit_events"];

fn level_words(level: &str) -> &str {
    match level {
        "readonly" => "read only",
        "edit_events" => "edit events",
        "custom" => "a custom level",
        other => other,
    }
}

fn service_word(service: &str) -> &str {
    if service == "calendar" {
        "Calendar"
    } else {
        "Gmail"
    }
}

/// The card's lines: the title, then one line per fact. Every line is whole; a target whose lines
/// would not fit is refused rather than cut.
fn card_lines(t: &Target) -> Result<Vec<String>, String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(sc) = &t.secret {
        for (v, what) in [
            (&sc.server_name, "The server's name"),
            (&sc.name, "The secret's name"),
            (&sc.profile_name, "The profile's name"),
            (&sc.file, "The file's path"),
        ] {
            plain(v, what)?;
        }
        lines.push(format!(
            "Set {} for {} on {}",
            sc.name, sc.profile_name, sc.server_name
        ));
        lines.push(format!(
            "For {} only; other people keep the shared value",
            sc.profile_name
        ));
        lines.push(format!(
            "Secret: {} ({})",
            sc.name,
            if sc.slot == "header" {
                "a header"
            } else {
                "a variable"
            }
        ));
        lines.push(format!("Read once from the file: {}", sc.file));
        lines.push(if sc.keep_file {
            "The file is kept".into()
        } else {
            "The file is then deleted".into()
        });
    } else if let Some(lv) = &t.level {
        plain(&lv.server_name, "The server's name")?;
        lines.push(format!(
            "Change the {} level on {}",
            service_word(&lv.service),
            lv.server_name
        ));
        lines.push(format!(
            "{}: {} {} → {} (every account on this server)",
            lv.server_name,
            service_word(&lv.service),
            level_words(&lv.from),
            level_words(&lv.to)
        ));
        if lv.service == "gmail" && lv.to == "send" {
            lines.push("Each message still needs its own Allow.".into());
        }
        lines.push(
            "Accounts already signed in must sign in again before a higher level takes effect."
                .into(),
        );
    } else {
        let main = t
            .after
            .as_ref()
            .or(t.before.as_ref())
            .ok_or("The change names no server.")?;
        for s in [&t.before, &t.after].into_iter().flatten() {
            visible(s)?;
        }
        let name = &main.name;
        lines.push(match t.op.as_str() {
            "add" => format!("Add the MCP server {name}"),
            "edit" => format!("Change the MCP server {name}"),
            "set_enabled" if main.enabled => format!("Turn on the MCP server {name}"),
            "set_enabled" => format!("Turn off the MCP server {name}"),
            "remove" => format!("Remove the MCP server {name}"),
            other => return Err(format!("{other} is not a change a card can show.")),
        });
        lines.extend(describe(main));
        if t.op == "edit" {
            if let (Some(b), Some(a)) = (&t.before, &t.after) {
                let c = changed_fields(b, a);
                if c.is_empty() {
                    return Err("That edit changes nothing the card could show.".into());
                }
                lines.push(format!("Changed: {}", c.join(", ")));
            }
        }
        let runs = main.kind == TransportKind::Stdio
            && main.enabled
            && matches!(t.op.as_str(), "add" | "edit" | "set_enabled");
        if runs {
            lines.push(
                "Approving saves this once. The program runs when a chat next uses this server."
                    .into(),
            );
        }
    }
    let fits = lines
        .iter()
        .all(|l| l.chars().count() <= helm_consent::kinds::MAX_SUMMARY_LINE)
        && lines.join("\n").chars().count() <= helm_consent::kinds::MAX_SUMMARY;
    if !fits {
        return Err(format!(
            "This change is too long to show whole on one card (a line holds {} characters, the card {}): use shorter arguments, or ask the user to make it in Settings > MCP.",
            helm_consent::kinds::MAX_SUMMARY_LINE,
            helm_consent::kinds::MAX_SUMMARY
        ));
    }
    Ok(lines)
}

fn canonical(_tx: &Transaction<'_>, _chat: &str, target: &Value) -> Result<Canonical, String> {
    let t: Target = serde_json::from_value(target.clone()).map_err(|_| {
        "Ask for this change with app_mcp_servers (askUser: true, set_secret or set_level)."
            .to_owned()
    })?;
    if !OPS.contains(&t.op.as_str()) {
        return Err("That is not a change an MCP server card can make.".into());
    }
    let lines = card_lines(&t)?;
    Ok(Canonical {
        target: serde_json::to_value(&t).map_err(|e| e.to_string())?,
        summary: lines.join("\n"),
        fixed_form: String::new(),
        details: Vec::new(),
    })
}

fn kind() -> AllowKind {
    AllowKind {
        id: KIND,
        label: "Change an MCP server that needs your approval",
        accepts_words: false,
        canonical,
    }
}

/// This module's allow kind, applied by Helm on Approve.
pub fn register_allow_kinds(k: &mut AllowKinds) {
    k.register_applying(kind(), Arc::new(ServerChangeApplier));
}

// ---- staging ------------------------------------------------------------------------------

fn refuse(m: impl Into<String>) -> ToolError {
    ToolError::refused(format!("[Helm] Nothing was staged: {}", m.into()))
}

/// The reason the agent gave, or the standing one. Longer than 1,000 characters is refused.
fn reason_of(args: &Value) -> Result<String, ToolError> {
    match args.get("reason") {
        None | Some(Value::Null) => Ok(NO_REASON.to_owned()),
        Some(Value::String(r)) if r.trim().is_empty() => Ok(NO_REASON.to_owned()),
        Some(Value::String(r)) if r.chars().count() <= MAX_REASON => Ok(r.trim().to_owned()),
        Some(_) => Err(refuse(format!(
            "reason is at most {MAX_REASON} characters of text."
        ))),
    }
}

/// File the request and say it is pending. The answer holds the request's id, its state and a
/// note: never the card's lines.
fn file_request(ctx: &ToolCtx, t: &Target, reason: &str) -> ToolResult {
    let (db, kinds) = match (
        ctx.services.get::<Database>(),
        ctx.services.get::<AllowKinds>(),
    ) {
        (Some(d), Some(k)) if k.applies_on_allow(KIND) => (d, k),
        _ => return Err(refuse("this change cannot be asked for here.")),
    };
    let target = serde_json::to_value(t).map_err(|e| ToolError::refused(e.to_string()))?;
    let r = consent::request(
        &db,
        &kinds,
        NewRequest {
            chat_id: &ctx.caller_chat_id,
            kind: KIND,
            target: &target,
            reason,
            evidence: "",
            grant_minutes: None,
            tool_use_id: ctx.tool_call_id.as_deref(),
        },
        helm_core::now_ms(),
    )
    .map_err(|r| refuse(r.0))?;
    ctx.events
        .emit(AppEvent::new("consent").with("chatId", ctx.caller_chat_id.clone()));
    if let Some(w) = ctx.services.get::<helm_consent::ConsentWake>() {
        w.0.notify_one();
    }
    Ok(json!({
        "requestId": r.id,
        "state": "pending",
        "note": "The user now sees an Approve card in this chat showing exactly this change. Helm makes it only if they approve, and sends you the outcome as a note. Carry on with other work; do not call again or poll.",
    }))
}

fn store_of(ctx: &ToolCtx) -> Result<Arc<McpStore>, ToolError> {
    ctx.services
        .get::<McpStore>()
        .ok_or_else(|| refuse("MCP settings are not available."))
}

/// Whether any literal in `list` still carries a typed value, shared or a person's own, or the
/// OAuth client secret does: a card's target is stored as plain text, so none may reach it.
fn carries_value(list: &[McpServer]) -> bool {
    list.iter().any(|s| {
        crate::config::refs(s).any(|r| {
            (r.kind == RefKind::Literal && r.value.is_some())
                || r.profiles.values().any(|p| p.value.is_some())
        })
    })
}

/// The tool a stored op was made with.
fn tool_of(op: &str) -> Option<&'static str> {
    Some(match op {
        "add" => admin::ADD,
        "edit" => admin::EDIT,
        "set_enabled" => admin::UPDATE,
        "remove" => admin::REMOVE,
        _ => return None,
    })
}

fn op_of(tool: &str) -> Option<&'static str> {
    OPS.iter().copied().find(|op| tool_of(op) == Some(tool))
}

/// File the card for an admin tool called with `askUser: true`: check the change exactly as
/// applying will, and file it only if the card shows all of it. Changes nothing.
pub(crate) fn stage_admin(ctx: &ToolCtx, tool: &str, args: Value) -> ToolResult {
    let mut args = args;
    let reason = reason_of(&args)?;
    if let Some(o) = args.as_object_mut() {
        o.remove("askUser");
        o.remove("reason");
    }
    let store = store_of(ctx)?;
    let op = op_of(tool).ok_or_else(|| refuse("that tool cannot ask for a card."))?;
    let change = Change::parse(tool, args.clone())
        .map_err(|e| refuse(e.0))?
        .ok_or_else(|| refuse("that tool cannot ask for a card."))?;
    let revision = change.revision().to_owned();
    let id = change.server_id().to_owned();
    let (before, after) = store
        .preview(Some(&revision), |list| change.apply_to(list, Who::Approved))
        .map_err(|e| refuse(e.to_string()))?;
    if carries_value(&after) {
        return Err(refuse(
            "a card never carries a secret value. Leave the value out: the user types it in Settings > MCP, or a person's own value is saved with app_set_mcp_secret from a file.",
        ));
    }
    if before == after {
        return Err(refuse("that change would leave the server as it is."));
    }
    if let Some(o) = args.as_object_mut() {
        o.remove("revision");
    }
    let target = Target {
        op: op.to_owned(),
        revision,
        args,
        before: before.iter().find(|s| s.id == id).map(shown),
        after: after.iter().find(|s| s.id == id).map(shown),
        secret: None,
        level: None,
    };
    card_lines(&target).map_err(refuse)?;
    file_request(ctx, &target, &reason)
}

/// The server a chat named, by id or by display name.
fn find_server<'a>(list: &'a [McpServer], what: &str) -> Result<&'a McpServer, String> {
    if let Some(s) = list.iter().find(|s| s.id == what) {
        return Ok(s);
    }
    let by_name: Vec<&McpServer> = list
        .iter()
        .filter(|s| s.name.eq_ignore_ascii_case(what.trim()))
        .collect();
    match by_name.as_slice() {
        [one] => Ok(one),
        [] => {
            Err("No server has that id or name. List the servers with app_list_mcp_servers.".into())
        }
        many => Err(format!(
            "More than one server has that name: use its id ({}).",
            many.iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn text_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| refuse(format!("{key} is required.")))
}

// ---- a person's own value for a secret ------------------------------------------------------

/// The profile a chat named, by id or by name (any case), from this PC's profiles.
fn find_profile(db: &Database, what: &str) -> Result<(String, String), String> {
    let all: Vec<(String, String)> = db
        .snapshot(|tx| {
            let mut q = tx.prepare("SELECT id, COALESCE(name, '') FROM profiles")?;
            let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|_| "Profiles are not available here.".to_owned())?;
    if let Some(p) = all.iter().find(|(id, _)| id == what) {
        return Ok(p.clone());
    }
    let named: Vec<&(String, String)> = all
        .iter()
        .filter(|(_, n)| n.eq_ignore_ascii_case(what.trim()))
        .collect();
    match named.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err("No profile on this PC has that name or id.".into()),
        many => Err(format!(
            "More than one profile has that name: use its id ({}).",
            many.iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Where a secret lives on a server: its place and the stored name. A header's name matches in
/// any case, as the servers' own header rules do.
fn locate<'a>(
    s: &'a McpServer,
    name: &str,
) -> Result<(&'static str, String, &'a ValueRef), String> {
    let t = &s.transport;
    let env = t.env.get_key_value(name);
    let header = t.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name));
    match (env, header) {
        (Some(_), Some(_)) => Err(format!(
            "{name} names both a variable and a header of that server: ask the user to set it in Settings > MCP."
        )),
        (Some((k, r)), None) => Ok(("env", k.clone(), r)),
        (None, Some((k, r))) => Ok(("header", k.clone(), r)),
        (None, None) => Err(format!(
            "That server has no variable or header called {name}: add {name} first."
        )),
    }
}

fn slot_mut<'a>(s: &'a mut McpServer, slot: &str, name: &str) -> Option<&'a mut ValueRef> {
    if slot == "header" {
        s.transport.headers.get_mut(name)
    } else {
        s.transport.env.get_mut(name)
    }
}

/// Give one profile its own value for the secret `sc` names.
fn set_profile_value(
    list: &mut [McpServer],
    sc: &SecretChange,
    value: &str,
) -> Result<(), StoreError> {
    let s = list
        .iter_mut()
        .find(|s| s.id == sc.server)
        .ok_or_else(|| StoreError::Invalid("That server no longer exists.".into()))?;
    let r = slot_mut(s, &sc.slot, &sc.name)
        .ok_or_else(|| StoreError::Invalid(format!("That server no longer has {}.", sc.name)))?;
    if r.kind != RefKind::Literal {
        return Err(StoreError::Invalid(
            "A per-person value needs a typed secret.".into(),
        ));
    }
    r.profiles.insert(
        sc.profile_id.clone(),
        ProfileSecret {
            value: Some(value.to_owned()),
            secret_id: None,
        },
    );
    Ok(())
}

/// A path a chat may name: absolute on this PC, no `.` or `..` part, short enough to show whole.
fn plain_path(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    let bad = || "valueFile must be an absolute path on this PC, with no '..' part.".to_owned();
    if s.is_empty() || s.chars().count() > MAX_PATH {
        return Err(format!(
            "valueFile must be an absolute path of at most {MAX_PATH} characters."
        ));
    }
    plain(s, "valueFile")?;
    let p = Path::new(s);
    if !crate::validate::is_local_absolute(s)
        || helm_platform::fs::check_plain_absolute(p).is_err()
        || p.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err(bad());
    }
    Ok(s.to_owned())
}

/// The value file as metadata shows it (it is not opened): a plain file, not a link, small.
fn value_file_looks_right(path: &str) -> Result<(), String> {
    let m = std::fs::symlink_metadata(path)
        .map_err(|_| "The value file could not be found.".to_owned())?;
    if helm_platform::fs::is_link_like(&m) || !m.is_file() {
        return Err("The value file must be a plain file, not a folder or a link.".into());
    }
    if m.len() > MAX_LITERAL_BYTES as u64 {
        return Err("The value file is larger than 16 KiB.".into());
    }
    Ok(())
}

/// Read the value once. Every error is fixed text: none echoes the file's contents.
fn read_value(path: &str) -> Result<String, String> {
    value_file_looks_right(path)?;
    let unreadable = || "The value file could not be read.".to_owned();
    let file =
        helm_platform::fs::open_regular_no_follow(Path::new(path)).map_err(|_| unreadable())?;
    let mut bytes = Vec::with_capacity(MAX_LITERAL_BYTES + 1);
    file.take(MAX_LITERAL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() > MAX_LITERAL_BYTES {
        return Err("The value file is larger than 16 KiB.".into());
    }
    let mut text =
        String::from_utf8(bytes).map_err(|_| "The value file is not text.".to_owned())?;
    if text.contains('\0') {
        return Err("The value file is not text.".into());
    }
    // One trailing line end: editors add it, and no value wants it.
    if text.ends_with("\r\n") {
        text.truncate(text.len() - 2);
    } else if text.ends_with(['\n', '\r']) {
        text.truncate(text.len() - 1);
    }
    if text.is_empty() {
        return Err("The value file is empty.".into());
    }
    Ok(text)
}

fn stage_secret(ctx: &ToolCtx, args: Value) -> ToolResult {
    let reason = reason_of(&args)?;
    let store = store_of(ctx)?;
    let db = ctx
        .services
        .get::<Database>()
        .ok_or_else(|| refuse("this change cannot be asked for here."))?;
    let keep_file = match args.get("keepFile") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(refuse("keepFile is true or false.")),
    };
    let snapshot = store.snapshot().map_err(|e| refuse(e.to_string()))?;
    let server = find_server(&snapshot.servers, text_arg(&args, "server")?).map_err(refuse)?;
    let (profile_id, profile_name) =
        find_profile(&db, text_arg(&args, "profile")?).map_err(refuse)?;
    let (slot, name, r) = locate(server, text_arg(&args, "secret")?).map_err(refuse)?;
    if r.kind != RefKind::Literal {
        return Err(refuse(
            "a per-person value needs a typed secret: this one is read from a variable or file, so ask the user to change it in Settings > MCP.",
        ));
    }
    let file = plain_path(text_arg(&args, "valueFile")?).map_err(refuse)?;
    value_file_looks_right(&file).map_err(refuse)?;
    let sc = SecretChange {
        server: server.id.clone(),
        server_name: server.name.clone(),
        name,
        slot: slot.to_owned(),
        profile_id,
        profile_name,
        file,
        keep_file,
    };
    // The same list change applying runs, with a stand-in value, so the profile's entry is
    // checked as a save checks it.
    let (before, after) = store
        .preview(Some(&snapshot.revision), |l| set_profile_value(l, &sc, "x"))
        .map_err(|e| refuse(e.to_string()))?;
    let t = Target {
        op: "set_secret".into(),
        revision: snapshot.revision,
        args: Value::Null,
        before: before.iter().find(|s| s.id == sc.server).map(shown),
        after: after.iter().find(|s| s.id == sc.server).map(shown),
        secret: Some(sc),
        level: None,
    };
    card_lines(&t).map_err(refuse)?;
    file_request(ctx, &t, &reason)
}

// ---- the Gmail and Calendar level ------------------------------------------------------------

fn gmail_name(p: Option<GooglePermission>) -> &'static str {
    match p {
        Some(GooglePermission::Readonly) => "readonly",
        Some(GooglePermission::Drafts) => "drafts",
        Some(GooglePermission::Send) => "send",
        None => "custom",
    }
}

fn calendar_name(p: Option<CalendarPermission>) -> &'static str {
    match p {
        Some(CalendarPermission::Off) => "off",
        Some(CalendarPermission::Readonly) => "readonly",
        Some(CalendarPermission::EditEvents) => "edit_events",
        None => "custom",
    }
}

/// The level a Google accounts server holds now, as its wire name.
fn level_now(s: &McpServer, service: &str) -> &'static str {
    if service == "calendar" {
        calendar_name(google::calendar_shown(&s.transport.args))
    } else {
        gmail_name(google::permission_of(&s.transport.args))
    }
}

/// Set the level `lv` names, when the server still holds the level the card showed.
fn set_level_in(list: &mut [McpServer], lv: &LevelChange) -> Result<(), StoreError> {
    let s = list
        .iter_mut()
        .find(|s| s.id == lv.server)
        .ok_or_else(|| StoreError::Invalid("That server no longer exists.".into()))?;
    if s.preset.as_deref() != Some(google::PRESET_ID) {
        return Err(StoreError::Invalid(
            "Only the Google accounts server has a Gmail and Calendar level.".into(),
        ));
    }
    if level_now(s, &lv.service) != lv.from {
        return Err(StoreError::Invalid(format!(
            "The {} level is not {} any more.",
            service_word(&lv.service),
            level_words(&lv.from)
        )));
    }
    let bad = || StoreError::Invalid("That is not a level this server offers.".into());
    if lv.service == "calendar" {
        let to: CalendarPermission = serde_json::from_value(json!(lv.to)).map_err(|_| bad())?;
        s.transport.args = google::with_calendar(&s.transport.args, to);
    } else {
        let to: GooglePermission = serde_json::from_value(json!(lv.to)).map_err(|_| bad())?;
        s.transport.args = google::with_permission(&s.transport.args, to);
    }
    Ok(())
}

/// Whether moving from `from` to `to` asks Google for a scope the accounts were not granted.
fn scope_grows(lv: &LevelChange) -> bool {
    let scopes = |level: &str| -> Option<Vec<&'static str>> {
        if lv.service == "calendar" {
            serde_json::from_value::<CalendarPermission>(json!(level))
                .ok()
                .map(CalendarPermission::scopes)
        } else {
            serde_json::from_value::<GooglePermission>(json!(level))
                .ok()
                .map(GooglePermission::scopes)
        }
    };
    match (scopes(&lv.from), scopes(&lv.to)) {
        (Some(from), Some(to)) => to.iter().any(|s| !from.contains(s)),
        // A level this picker does not know: say so rather than guess it grew or not.
        _ => true,
    }
}

fn stage_level(ctx: &ToolCtx, args: Value) -> ToolResult {
    let reason = reason_of(&args)?;
    let store = store_of(ctx)?;
    let snapshot = store.snapshot().map_err(|e| refuse(e.to_string()))?;
    let server = find_server(&snapshot.servers, text_arg(&args, "server")?).map_err(refuse)?;
    if server.preset.as_deref() != Some(google::PRESET_ID) {
        return Err(refuse(
            "only a Google accounts server has a Gmail and Calendar level.",
        ));
    }
    let service = text_arg(&args, "service")?;
    let to = text_arg(&args, "level")?;
    let offered = match service {
        "gmail" => &LEVELS_GMAIL,
        "calendar" => &LEVELS_CALENDAR,
        _ => return Err(refuse("service is gmail or calendar.")),
    };
    if !offered.contains(&to) {
        return Err(refuse(format!(
            "the {} level is one of {}.",
            service_word(service),
            offered.join(", ")
        )));
    }
    let lv = LevelChange {
        server: server.id.clone(),
        server_name: server.name.clone(),
        service: service.to_owned(),
        from: level_now(server, service).to_owned(),
        to: to.to_owned(),
    };
    if lv.from == lv.to {
        return Err(refuse(format!(
            "the {} level is already {}.",
            service_word(service),
            level_words(to)
        )));
    }
    let (before, after) = store
        .preview(Some(&snapshot.revision), |l| set_level_in(l, &lv))
        .map_err(|e| refuse(e.to_string()))?;
    let t = Target {
        op: "set_level".into(),
        revision: snapshot.revision,
        args: Value::Null,
        before: before.iter().find(|s| s.id == lv.server).map(shown),
        after: after.iter().find(|s| s.id == lv.server).map(shown),
        secret: None,
        level: Some(lv),
    };
    card_lines(&t).map_err(refuse)?;
    file_request(ctx, &t, &reason)
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            SECRET_TOOL,
            "Ask the user to approve a person's own value for a secret an MCP server already holds (a variable or header saved as a typed secret). The value is read once, from valueFile, when the user approves; the file is then deleted unless keepFile is true. The card shows names and the file's path, never the value; other people keep the shared value. Answers pending; the outcome reaches you as a note.",
            json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string", "description": "The server's id or display name."},
                    "profile": {"type": "string", "description": "The profile's name or id on this PC."},
                    "secret": {"type": "string", "description": "The variable or header name."},
                    "valueFile": {"type": "string", "maxLength": 200, "description": "Absolute path of a plain file holding the value."},
                    "keepFile": {"type": "boolean", "description": "Keep the file after the value is saved (default: delete it)."},
                    "reason": {"type": "string", "minLength": 1, "maxLength": 1000}
                },
                "required": ["server", "profile", "secret", "valueFile"],
                "additionalProperties": false
            }),
        ),
        stage_secret,
    );
    r.blocking(
        ToolSpec::write(
            LEVEL_TOOL,
            "Ask the user to approve a new Gmail or Calendar level for the Google accounts server. The level belongs to the whole server, every account on it, and takes effect once the accounts sign in again. Answers pending; the outcome reaches you as a note.",
            json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string", "description": "The Google accounts server's id or name."},
                    "service": {"type": "string", "enum": ["gmail", "calendar"]},
                    "level": {"type": "string", "enum": ["off", "readonly", "drafts", "send", "edit_events"], "description": "Gmail: readonly, drafts or send. Calendar: off, readonly or edit_events."},
                    "reason": {"type": "string", "minLength": 1, "maxLength": 1000}
                },
                "required": ["server", "service", "level"],
                "additionalProperties": false
            }),
        ),
        stage_level,
    );
}

// ---- applying -----------------------------------------------------------------------------

/// Applies an approved `mcp_server_change` request.
pub struct ServerChangeApplier;

impl Applier for ServerChangeApplier {
    fn staged_with(&self) -> &'static str {
        "app_mcp_servers with askUser: true"
    }

    fn apply(&self, job: &ApplyJob<'_>) -> String {
        let Ok(t) = serde_json::from_value::<Target>(job.target.clone()) else {
            return "Not applied: the staged change could not be read. Ask again.".into();
        };
        let Some(store) = job.services.get::<McpStore>() else {
            return "Not applied: MCP settings are not available.".into();
        };
        if let Some(sc) = &t.secret {
            return apply_secret(&store, &t, sc);
        }
        if let Some(lv) = &t.level {
            return apply_level(&store, &t, lv);
        }
        apply_admin(&store, &t)
    }
}

/// The outcome for a store refusal: a changed list is the common one and has its own words.
fn not_applied(e: &StoreError) -> String {
    match e {
        StoreError::Conflict(_) => {
            "Not applied: MCP settings changed since this was asked. Ask again.".into()
        }
        other => format!("Not applied: {other}"),
    }
}

/// Run `f` as the card showed it: preview it, compare the server it leaves with the card's
/// `after`, and only then save it. The revision pins both runs to the same list.
fn apply_checked(
    store: &McpStore,
    t: &Target,
    id: &str,
    f: impl Fn(&mut Vec<McpServer>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let (_, after) = store.preview(Some(&t.revision), &f)?;
    if after.iter().find(|s| s.id == id).map(shown) != t.after {
        return Err(StoreError::Invalid(
            "what would be saved is not what the card showed.".into(),
        ));
    }
    store.mutate(Some(&t.revision), f).map(|_| ())
}

fn apply_admin(store: &McpStore, t: &Target) -> String {
    let Some(tool) = tool_of(&t.op) else {
        return "Not applied: the staged change could not be read. Ask again.".into();
    };
    let mut args = t.args.clone();
    if let Some(o) = args.as_object_mut() {
        o.insert("revision".into(), json!(t.revision));
    }
    let parse = || Change::parse(tool, args.clone());
    let Ok(Some(first)) = parse() else {
        return "Not applied: the staged change could not be read. Ask again.".into();
    };
    let id = first.server_id().to_owned();
    drop(first);
    let name = t
        .after
        .as_ref()
        .or(t.before.as_ref())
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let done = apply_checked(store, t, &id, |list| match parse() {
        Ok(Some(c)) => c.apply_to(list, Who::Approved),
        _ => Err(StoreError::Invalid(
            "the staged change could not be read.".into(),
        )),
    });
    match done {
        Ok(()) => match t.op.as_str() {
            "add" => format!("Applied. Added the MCP server {name}."),
            "remove" => format!("Applied. Removed the MCP server {name}."),
            "set_enabled" if t.after.as_ref().is_some_and(|s| s.enabled) => {
                format!("Applied. Turned on the MCP server {name}.")
            }
            "set_enabled" => format!("Applied. Turned off the MCP server {name}."),
            _ => format!("Applied. Changed the MCP server {name}."),
        },
        Err(e) => not_applied(&e),
    }
}

fn apply_level(store: &McpStore, t: &Target, lv: &LevelChange) -> String {
    match apply_checked(store, t, &lv.server, |list| set_level_in(list, lv)) {
        Ok(()) => {
            let mut out = format!(
                "Applied. {}'s {} level is now {} (every account on this server).",
                lv.server_name,
                service_word(&lv.service),
                level_words(&lv.to)
            );
            out.push_str(" A higher level takes effect after the accounts sign in again.");
            if scope_grows(lv) {
                out.push_str(" This asks Google for more than the accounts were given: ask the user to sign in again.");
            }
            out
        }
        Err(e) => not_applied(&e),
    }
}

fn apply_secret(store: &McpStore, t: &Target, sc: &SecretChange) -> String {
    // Cheap checks first, so a changed list or a swapped-out file never gets as far as a read.
    match store.snapshot() {
        Ok(s) if s.revision == t.revision => {}
        Ok(_) => return not_applied(&StoreError::Conflict(String::new())),
        Err(e) => return not_applied(&e),
    }
    let value = match read_value(&sc.file) {
        Ok(v) => v,
        Err(m) => return format!("Not applied: {m}"),
    };
    let saved = apply_checked(store, t, &sc.server, |list| {
        set_profile_value(list, sc, &value)
    });
    drop(value);
    if let Err(e) = saved {
        // The file stays: nothing was stored.
        return not_applied(&e);
    }
    let head = format!("Applied. {} is saved for {}", sc.name, sc.profile_name);
    if sc.keep_file {
        return format!("{head}; the file was kept.");
    }
    match std::fs::remove_file(&sc.file) {
        Ok(()) => format!("{head}; the file was deleted."),
        Err(_) => format!("{head}, but the file could not be deleted: delete it yourself."),
    }
}

#[cfg(test)]
#[path = "server_change_tests.rs"]
mod tests;
