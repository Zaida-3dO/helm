//! Changes a chat stages and the user approves: `app_stage_settings`.
//!
//! A chat may change a low-risk setting itself, but a higher-risk one (pairing this PC with the
//! hub, the update folder, the Devices switches, which shell the terminal runs...) only applies
//! after the user's click. Instead of changing it, the chat stages it here, and Helm files one
//! approval request (allow kind `protected_settings`) whose card lists exactly what changes, one
//! line each, built only from what this module checked. When the user clicks Approve, Helm applies
//! the staged changes at that moment ([`SettingsApplier`]), and the chat hears the outcome as a
//! note. The card never blocks the chat: it carries on while the card waits.
//!
//! - **Any High setting, with its own validators.** Every key the risk table calls High is
//!   admitted. Staging checks the value by running the change in a transaction that is rolled
//!   back (the same writer a chat's own change uses, so the preference's validators, its owner's
//!   rule or the extension's own checks all apply), and the card shows what that run says the
//!   setting would become. Applying runs the same writer for real, records each change for the
//!   user's Undo (never the chat's), and announces it.
//! - **Pairing names its person and computer up front.** The hub spends a code when it is
//!   exchanged, so Helm cannot ask first who the code is for. The chat says who it should be,
//!   the card shows it, and on Approve Helm saves nothing unless the hub names the same person
//!   and computer.
//! - **The code is read from a file, once, when applying.** Staging only checks the file's
//!   metadata (a plain file of at most 256 bytes). The code never appears in the target, the
//!   card, the tool result, the outcome, a log line or an error; every message about it is fixed
//!   text.
//! - **All or nothing, pairing first.** Every check that changes nothing runs again first; then
//!   pairing; only if that worked do the settings change, in one transaction.
//! - **Who may stage:** a parent chat of this PC's own person (not a household member's chat).
//!   Staging changes nothing, so it is allowed whatever started the turn; the click is the gate.

use crate::config;
use crate::pair;
use helm_consent::store::{self as consent, NewRequest};
use helm_consent::{AllowKind, AllowKinds, Applier, ApplyJob, Canonical};
use helm_core::db::Database;
use helm_hosttools::settings_allow::{high_keys, risk, user_refusal, Risk};
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use helm_ipc::{AppEvent, EventSink, Services};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};
use std::sync::Arc;

/// The allow kind for staged changes.
pub const KIND: &str = "protected_settings";
pub const TOOL: &str = "app_stage_settings";
/// The largest code file Helm will read: a code is 8 characters, typed loosely.
const MAX_CODE_FILE: u64 = 256;
/// Longest staged path, so every card line shows it whole.
const MAX_PATH: usize = 200;
const MAX_NAME: usize = 64;
/// The most settings one request may change.
const MAX_SETTINGS: usize = 16;
const TITLE: &str = "Change settings on this PC";
const RUN_FROM_OTHERS: &str = "Let other devices use chats on this PC";
const HOUSEHOLD_RUN: &str = "Let people in my household run their chats on this PC";
const PAIRING: &str = "hubPairing";

/// Staged pairing, as the target holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pairing {
    /// The normalised line address.
    address: String,
    /// The normalised fallback, or empty.
    fallback: String,
    code_file: String,
    person: String,
    machine: String,
    /// "Pat on Laptop" when this PC is paired already, else empty.
    replaces: String,
}

/// One staged setting: what it is now and will be (the values in effect, a default read as its
/// default), and the value to apply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    from: Value,
    to: Value,
    value: Value,
}

/// The canonical target: every staged change with the value it replaces.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Staged {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hub_pairing: Option<Pairing>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    settings: BTreeMap<String, Change>,
}

fn host_port(line_url: &str) -> &str {
    pair::authority(line_url).unwrap_or(line_url)
}

/// The name a setting goes by on a card.
fn label(key: &str) -> &str {
    match key {
        "updateSource" => "Update folder",
        "deviceRunFromOthers" => RUN_FROM_OTHERS,
        "deviceHouseholdRun" => HOUSEHOLD_RUN,
        k => k,
    }
}

/// A value in a sentence: switches as on and off, text as it is, "default" for a setting at
/// its own behaviour (and "none" for the update folder).
fn show(key: &str, v: &Value) -> String {
    match v {
        Value::Null if key == "updateSource" => "none".into(),
        Value::Null => "default".into(),
        Value::Bool(true) => "on".into(),
        Value::Bool(false) => "off".into(),
        Value::String(s) if s.is_empty() => "\"\"".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The value a setting has: what is stored, else the default its options name.
fn effective(key: &str, stored: Option<&Value>) -> Value {
    match stored.filter(|v| !v.is_null()) {
        Some(v) => v.clone(),
        None => helm_hosttools::settings_options::describe(key)
            .and_then(|d| d.get("default").cloned())
            .unwrap_or(Value::Null),
    }
}

/// The card's lines: the title, then one line per change. Every value is shown whole; a target
/// whose lines would not fit is refused rather than cut.
fn card_lines(s: &Staged) -> Result<Vec<String>, String> {
    let mut lines = vec![TITLE.to_owned()];
    if let Some(p) = &s.hub_pairing {
        let fallback = if p.fallback.is_empty() {
            String::new()
        } else {
            format!(" (fallback {})", host_port(&p.fallback))
        };
        lines.push(format!(
            "Pair with the hub at {}{fallback} as {} on {}",
            host_port(&p.address),
            p.person,
            p.machine
        ));
        lines.push(format!("One-time code read from the file {}", p.code_file));
        lines.push(
            "Helm pairs only if the hub answers with the person and computer named here.".into(),
        );
        if !p.replaces.is_empty() {
            lines.push(format!(
                "This replaces the current pairing (as {}).",
                p.replaces
            ));
        }
    }
    for (key, c) in &s.settings {
        let (from, to) = (show(key, &c.from), show(key, &c.to));
        let one = format!("{}: {from} → {to}", label(key));
        if one.chars().count() <= helm_consent::kinds::MAX_SUMMARY_LINE {
            lines.push(one);
        } else {
            lines.push(format!("{} now: {from}", label(key)));
            lines.push(format!("{} after: {to}", label(key)));
        }
    }
    let fits = lines
        .iter()
        .all(|l| l.chars().count() <= helm_consent::kinds::MAX_SUMMARY_LINE)
        && lines.join("\n").chars().count() <= helm_consent::kinds::MAX_SUMMARY;
    if !fits {
        return Err(
            "These changes are too long to show on one card: stage fewer, or use shorter values."
                .into(),
        );
    }
    Ok(lines)
}

/// The kind's canonical target: the staged changes as this module built them, and their card.
/// Only [`stage`] files these requests (`app_request_allow` refuses applying kinds), so the
/// target here is always host-built; it is still parsed strictly.
fn canonical(_tx: &Transaction<'_>, _chat: &str, target: &Value) -> Result<Canonical, String> {
    let staged: Staged = serde_json::from_value(target.clone())
        .map_err(|_| "Stage these changes with app_stage_settings.".to_owned())?;
    let lines = card_lines(&staged)?;
    Ok(Canonical {
        target: serde_json::to_value(&staged).map_err(|e| e.to_string())?,
        summary: lines.join("\n"),
        fixed_form: String::new(),
        details: Vec::new(),
    })
}

fn kind() -> AllowKind {
    AllowKind {
        id: KIND,
        label: "Change settings that need your approval",
        accepts_words: false,
        canonical,
    }
}

/// This module's allow kind, applied by Helm on Approve.
pub fn register_allow_kinds(k: &mut AllowKinds) {
    k.register_applying(kind(), Arc::new(SettingsApplier));
}

// ---- staging ------------------------------------------------------------------------------

/// A path a chat may name: absolute on a drive or a share (`\\server\share\...`), with no `.` or
/// `..` part, not a device or verbatim path, and short enough to show whole.
pub(crate) fn plain_path(raw: &str, what: &str) -> Result<String, String> {
    let s = raw.trim();
    let bad =
        || format!("{what} must be an absolute path on this PC or a share, with no '..' part.");
    if s.is_empty() || s.chars().count() > MAX_PATH {
        return Err(format!(
            "{what} must be an absolute path of at most {MAX_PATH} characters."
        ));
    }
    let lead: String = s.chars().take(3).collect();
    if matches!(lead.as_str(), r"\\?" | r"\\." | "//?" | "//.") || s.chars().any(char::is_control) {
        return Err(bad());
    }
    let p = Path::new(s);
    if helm_platform::fs::check_plain_absolute(p).is_err()
        || p.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err(bad());
    }
    Ok(s.to_owned())
}

/// Tests pair with a hub on a loopback port; they say so with this marker in their services.
#[cfg(test)]
pub(crate) struct AllowOwnHub;

fn own_hub_allowed(services: &Services) -> bool {
    #[cfg(test)]
    {
        services.get::<AllowOwnHub>().is_some()
    }
    #[cfg(not(test))]
    {
        let _ = services;
        false
    }
}

/// A person's or computer's name as the hub shows it.
fn plain_name(raw: &str, what: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > MAX_NAME || s.chars().any(char::is_control) {
        return Err(format!("{what} must be 1-{MAX_NAME} characters."));
    }
    Ok(s.to_owned())
}

/// The code file at staging: metadata only (it is not opened).
fn code_file_looks_right(path: &str) -> Result<(), String> {
    let m = std::fs::symlink_metadata(path)
        .map_err(|_| "The code file could not be found.".to_owned())?;
    if helm_platform::fs::is_link_like(&m) || !m.is_file() {
        return Err("The code file must be a plain file, not a folder or a link.".into());
    }
    if m.len() > MAX_CODE_FILE {
        return Err("The code file holds more than a pairing code.".into());
    }
    Ok(())
}

/// Whether this chat belongs to someone other than this PC's own person: its profile is linked
/// to a hub person who is not the home profile's.
fn household_member(tx: &rusqlite::Connection, chat_id: &str) -> bool {
    let Ok(Some(p)) = helm_profiles::store::profile_of_chat(tx, chat_id) else {
        return false;
    };
    if p.is_home {
        return false;
    }
    let home = helm_profiles::store::home(tx)
        .ok()
        .flatten()
        .and_then(|h| h.hub_person_id);
    p.hub_person_id.is_some_and(|id| Some(id) != home)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairingArgs {
    address: String,
    #[serde(default)]
    fallback: Option<String>,
    code_file: String,
    person: String,
    machine: String,
}

/// Check the pairing a chat asked for against this PC. Changes nothing.
fn build_pairing(services: &Services, v: &Value) -> Result<Pairing, String> {
    // Never echo the input: it names the code file.
    let a: PairingArgs = serde_json::from_value(v.clone()).map_err(|_| {
        "hubPairing takes address, codeFile, person, machine and optionally fallback.".to_owned()
    })?;
    let address = config::normalize_url(&a.address)?;
    let fallback = match a
        .fallback
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(f) => config::normalize_url(f)?,
        None => String::new(),
    };
    for url in [&address, &fallback] {
        if !url.is_empty() && config::is_own_address(url) && !own_hub_allowed(services) {
            return Err("The hub address is this PC itself (this includes a hub running in WSL or Docker on this PC). Pair with the hub on its own machine's address, not one served from here.".into());
        }
    }
    let code_file = plain_path(&a.code_file, "codeFile")?;
    code_file_looks_right(&code_file)?;
    let current = services
        .get::<helm_core::profile::Profile>()
        .zip(services.get::<helm_platform::Platform>())
        .map(|(p, pl)| config::load(&crate::credentials(&p, &pl)));
    let replaces = match current {
        Some(c) if c.usable() && !c.person_name.is_empty() => {
            format!("{} on {}", c.person_name, c.machine_name)
        }
        Some(c) if c.usable() => "a connection saved by hand".into(),
        _ => String::new(),
    };
    Ok(Pairing {
        address,
        fallback,
        code_file,
        person: plain_name(&a.person, "person")?,
        machine: plain_name(&a.machine, "machine")?,
        replaces,
    })
}

/// Why `key` cannot be staged, or `None` when it can.
fn not_stageable(key: &str) -> Option<String> {
    let shown: String = key.chars().take(64).collect();
    match risk(key) {
        Some(Risk::High) => None,
        Some(Risk::Low) => Some(format!(
            "{shown} is a setting you change yourself with app_update_settings, so it is not staged."
        )),
        Some(Risk::User) => Some(user_refusal(key)),
        None => Some(format!(
            "{shown} is not a setting that can be staged: stage hubPairing or one of {}.",
            high_keys().join(", ")
        )),
    }
}

/// Check what the chat asked for against this PC and build the target. Changes nothing: the
/// settings are run through the writer inside a transaction that is rolled back.
fn build(
    services: &Services,
    events: &dyn EventSink,
    chat_id: &str,
    args: &Value,
) -> Result<Staged, String> {
    let db = services
        .get::<Database>()
        .ok_or("Staging settings is not available here.")?;
    let changes = args
        .get("changes")
        .and_then(Value::as_object)
        .ok_or("changes must be an object.")?;
    if changes.is_empty() {
        return Err("Stage at least one change.".into());
    }
    if changes.len() > MAX_SETTINGS + 1 {
        return Err(format!("Stage at most {MAX_SETTINGS} settings at once."));
    }
    for k in changes.keys().filter(|k| k.as_str() != PAIRING) {
        if let Some(why) = not_stageable(k) {
            return Err(why);
        }
    }
    let member = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(household_member(tx, chat_id)))
        .map_err(|_| "The settings could not be read.".to_owned())?;
    if member {
        return Err(
            "Only chats of this PC's own person can stage its settings, not a household member's."
                .into(),
        );
    }

    let mut staged = Staged::default();
    if let Some(v) = changes.get(PAIRING) {
        staged.hub_pairing = Some(build_pairing(services, v)?);
    }
    let wanted: Vec<(String, Value)> = changes
        .iter()
        .filter(|(k, _)| k.as_str() != PAIRING)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !wanted.is_empty() {
        let would =
            helm_hosttools::settings::apply_approved(services, events, chat_id, &wanted, false)?;
        let mut unchanged: Vec<String> = Vec::new();
        for ((key, value), w) in wanted.iter().zip(would) {
            let from = effective(key, w.before.as_ref());
            let to = effective(key, w.after.as_ref());
            if from == to {
                unchanged.push(format!("{key} is already {}", show(key, &to)));
            }
            staged.settings.insert(
                key.clone(),
                Change {
                    from,
                    to,
                    value: value.clone(),
                },
            );
        }
        if !unchanged.is_empty() {
            return Err(format!(
                "Nothing staged: {}. Leave out what is already set.",
                unchanged.join("; ")
            ));
        }
    }
    card_lines(&staged)?;
    Ok(staged)
}

fn stage(ctx: &ToolCtx, args: Value) -> ToolResult {
    let refuse = |m: String| ToolError::refused(format!("[Helm] Nothing was staged: {m}"));
    let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
    let staged = build(
        &ctx.services,
        ctx.events.as_ref(),
        &ctx.caller_chat_id,
        &args,
    )
    .map_err(refuse)?;
    let (db, kinds) = match (
        ctx.services.get::<Database>(),
        ctx.services.get::<AllowKinds>(),
    ) {
        (Some(d), Some(k)) if k.applies_on_allow(KIND) => (d, k),
        _ => return Err(refuse("staging settings is not available here.".into())),
    };
    let target = serde_json::to_value(&staged).map_err(|e| ToolError::refused(e.to_string()))?;
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
        "changes": r.summary.lines().skip(1).collect::<Vec<_>>(),
        "note": "The user now sees an Approve card in this chat listing exactly these changes. Carry on with other work: Helm applies them whenever the user approves and sends you the outcome as a note. Do not call again or poll, and wait only for work that truly needs these settings.",
    }))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            TOOL,
            "Stage changes that only the user may approve, for Helm to apply when they click Approve. changes holds any settings that app_get_settings marks needsApproval (setting name: value, validated like app_update_settings; null restores a default) and optionally hubPairing {address, fallback?, codeFile, person, machine}, which pairs this PC with the hub using the one-time code in codeFile (Helm reads it only when applying, and pairs only if the hub names that person and machine). Answers at once with pending; the user sees one Approve card listing every change, and the outcome reaches you as a note from Helm. Carry on with other work meanwhile. Use it only when the user asked for these changes.",
            json!({
                "type": "object",
                "properties": {
                    "changes": {
                        "type": "object",
                        "properties": {
                            "hubPairing": {
                                "type": "object",
                                "properties": {
                                    "address": {"type": "string", "maxLength": 200},
                                    "fallback": {"type": "string", "maxLength": 200},
                                    "codeFile": {"type": "string", "maxLength": 260},
                                    "person": {"type": "string", "maxLength": 64},
                                    "machine": {"type": "string", "maxLength": 64}
                                },
                                "required": ["address", "codeFile", "person", "machine"],
                                "additionalProperties": false
                            }
                        },
                        "minProperties": 1,
                        "maxProperties": 17
                    },
                    "reason": {"type": "string", "minLength": 1, "maxLength": 1000}
                },
                "required": ["changes", "reason"],
                "additionalProperties": false
            }),
        )
        .large(),
        stage,
    );
}

// ---- applying -----------------------------------------------------------------------------

/// Read the code once: re-check the file, read at most one byte past the limit, and normalise.
/// Every error is fixed text: none echoes the file's contents.
fn read_code(path: &str) -> Result<String, String> {
    code_file_looks_right(path)?;
    let unreadable = || "The code file could not be read.".to_owned();
    let file =
        helm_platform::fs::open_regular_no_follow(Path::new(path)).map_err(|_| unreadable())?;
    let mut bytes = Vec::with_capacity(MAX_CODE_FILE as usize + 1);
    file.take(MAX_CODE_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() as u64 > MAX_CODE_FILE {
        return Err("The code file holds more than a pairing code.".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "The code file is not text.".to_owned())?;
    pair::normalize_code(&text).map_err(|_| "The code file does not hold a pairing code.".into())
}

/// Applies an approved `protected_settings` request.
pub struct SettingsApplier;

impl Applier for SettingsApplier {
    fn staged_with(&self) -> &'static str {
        TOOL
    }

    fn apply(&self, job: &ApplyJob<'_>) -> String {
        match serde_json::from_value::<Staged>(job.target.clone()) {
            Ok(s) => apply_staged(job.services, job.events, job.chat_id, &s),
            Err(_) => "Nothing changed: the staged changes could not be read.".into(),
        }
    }
}

fn apply_staged(
    services: &Arc<Services>,
    events: &Arc<dyn EventSink>,
    chat_id: &str,
    s: &Staged,
) -> String {
    let wanted: Vec<(String, Value)> = s
        .settings
        .iter()
        .map(|(k, c)| (k.clone(), c.value.clone()))
        .collect();
    // (a) Every check that changes nothing, again.
    if !wanted.is_empty() {
        if let Err(why) = helm_hosttools::settings::apply_approved(
            services,
            events.as_ref(),
            chat_id,
            &wanted,
            false,
        ) {
            return format!("Nothing changed: {why}");
        }
    }
    let code = match &s.hub_pairing {
        Some(p) => {
            if config::normalize_url(&p.address).is_err()
                || (!p.fallback.is_empty() && config::normalize_url(&p.fallback).is_err())
            {
                return "Nothing changed: the hub address is not valid.".into();
            }
            match read_code(&p.code_file) {
                Ok(c) => Some(c),
                Err(m) => return format!("Nothing changed: {m}"),
            }
        }
        None => None,
    };

    // (b) Pairing first; on any failure nothing else changes.
    let mut done: Vec<String> = Vec::new();
    if let (Some(p), Some(code)) = (&s.hub_pairing, code) {
        let paired = pair::pair_with(
            services,
            events,
            &p.address,
            &p.fallback,
            &code,
            Some((&p.person, &p.machine)),
        );
        drop(code);
        match paired {
            Ok(h) => {
                let mut line = format!("Paired as {} on {}", h.person_name, h.machine_name);
                line.push_str(if h.memory_server {
                    "; the shared-memory server is set up"
                } else {
                    "; the shared-memory server was not set up (add it in Settings > Hub)"
                });
                line.push_str(if h.profile_id.is_some() {
                    "; the home profile is linked."
                } else {
                    "; the home profile was not linked."
                });
                done.push(line);
            }
            Err(e) => {
                let spent = match e.code.as_str() {
                    "refused" | "mismatch" => " The code is used up: ask for a new one.",
                    _ => "",
                };
                return format!("Nothing changed: {}{spent}", e.message);
            }
        }
    }

    // (c) The settings together. A value that no longer matches the card's "from" is named in
    // the outcome, so the agent knows the card was read against something else.
    let mut drift: Vec<String> = Vec::new();
    if !wanted.is_empty() {
        match helm_hosttools::settings::apply_approved(
            services,
            events.as_ref(),
            chat_id,
            &wanted,
            true,
        ) {
            Ok(applied) => {
                for a in applied {
                    let Some(c) = s.settings.get(&a.key) else {
                        continue;
                    };
                    let now = effective(&a.key, a.before.as_ref());
                    if now != c.from {
                        drift.push(format!(
                            "{} was {}, not {} as the card said.",
                            label(&a.key),
                            show(&a.key, &now),
                            show(&a.key, &c.from)
                        ));
                    }
                    done.push(format!(
                        "{} is now {}.",
                        label(&a.key),
                        show(&a.key, &effective(&a.key, a.after.as_ref()))
                    ));
                }
            }
            Err(why) => {
                log::warn!("staged settings: saving failed: {why}");
                let lead = if done.is_empty() {
                    "Nothing changed".to_owned()
                } else {
                    done.join(" ")
                };
                let names: Vec<&str> = s.settings.keys().map(|k| label(k)).collect();
                return format!(
                    "{lead}; these did not change: {}. {why} Stage them again.",
                    names.join(", ")
                );
            }
        }
    }
    let mut out = format!("Applied. {}", done.join(" "));
    for d in drift {
        out.push(' ');
        out.push_str(&d);
    }
    if s.hub_pairing.is_some() {
        out.push_str(" The code file can be deleted.");
    }
    out
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
