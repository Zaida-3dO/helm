//! Setup changes a chat stages and the user approves: `app_stage_settings`.
//!
//! Setting up a PC means pairing it with the hub, choosing the update folder and the two
//! Settings → Devices switches. A chat may not change any of these itself. Instead it stages them
//! here, and Helm files one approval request (allow kind `protected_settings`) whose card lists
//! exactly what changes, one line each, built only from what this module checked. When the user
//! clicks Approve, Helm applies the staged changes at that moment ([`SettingsApplier`]), and the
//! chat hears the outcome as a note.
//!
//! - **Pairing names its person and computer up front.** The hub spends a code when it is
//!   exchanged, so Helm cannot ask first who the code is for. The chat says who it should be,
//!   the card shows it, and on Approve Helm saves nothing unless the hub names the same person
//!   and computer.
//! - **The code is read from a file, once, when applying.** Staging only checks the file's
//!   metadata (a plain file of at most 256 bytes). The code never appears in the target, the
//!   card, the tool result, the outcome, a log line or an error; every message about it is fixed
//!   text.
//! - **All or nothing, pairing first.** Every check that changes nothing runs again first; then
//!   pairing; only if that worked do the folder and the switches change, in one transaction.
//! - **Who may stage:** a parent chat of this PC's own person (not a household member's chat).
//!   Staging changes nothing, so it is allowed whatever started the turn; the click is the gate.

use crate::config;
use crate::pair;
use helm_consent::store::{self as consent, NewRequest};
use helm_consent::{AllowKind, AllowKinds, Applier, ApplyJob, Canonical};
use helm_core::db::Database;
use helm_core::prefs::{self, DEVICE_HOUSEHOLD_RUN, DEVICE_RUN_FROM_OTHERS};
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use helm_ipc::{AppEvent, EventSink, Services};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::{Component, Path};
use std::sync::Arc;

/// The allow kind for staged setup changes.
pub const KIND: &str = "protected_settings";
pub const TOOL: &str = "app_stage_settings";
/// The largest code file Helm will read: a code is 8 characters, typed loosely.
const MAX_CODE_FILE: u64 = 256;
/// Longest staged path, so every card line shows it whole.
const MAX_PATH: usize = 200;
const MAX_NAME: usize = 64;
const TITLE: &str = "Change settings on this PC";
const RUN_FROM_OTHERS: &str = "Let other devices use chats on this PC";
const HOUSEHOLD_RUN: &str = "Let people in my household run their chats on this PC";

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change<T> {
    from: T,
    to: T,
}

/// The canonical target: every staged change with the value it replaces.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Staged {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hub_pairing: Option<Pairing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    update_source: Option<Change<Option<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device_run_from_others: Option<Change<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device_household_run: Option<Change<bool>>,
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn host_port(line_url: &str) -> &str {
    pair::authority(line_url).unwrap_or(line_url)
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
    if let Some(c) = &s.update_source {
        let from = c.from.as_deref().unwrap_or("none");
        let to = c.to.as_deref().unwrap_or("none");
        let one = format!("Update folder: {from} → {to}");
        if one.chars().count() <= helm_consent::kinds::MAX_SUMMARY_LINE {
            lines.push(one);
        } else {
            lines.push(format!("Update folder now: {from}"));
            lines.push(format!("Update folder after: {to}"));
        }
    }
    if let Some(c) = &s.device_run_from_others {
        lines.push(format!(
            "{RUN_FROM_OTHERS}: {} → {}",
            on_off(c.from),
            on_off(c.to)
        ));
    }
    if let Some(c) = &s.device_household_run {
        lines.push(format!(
            "{HOUSEHOLD_RUN}: {} → {}",
            on_off(c.from),
            on_off(c.to)
        ));
    }
    let fits = lines
        .iter()
        .all(|l| l.chars().count() <= helm_consent::kinds::MAX_SUMMARY_LINE)
        && lines.join("\n").chars().count() <= helm_consent::kinds::MAX_SUMMARY;
    if !fits {
        return Err("These changes are too long to show on one card: use shorter paths.".into());
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
fn plain_path(raw: &str, what: &str) -> Result<String, String> {
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

fn switch(tx: &rusqlite::Connection, key: &str) -> bool {
    prefs::read(tx, key)
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
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

/// Check what the chat asked for against this PC and build the target. Changes nothing.
fn build(services: &Services, chat_id: &str, args: &Value) -> Result<Staged, String> {
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
    if let Some(k) = changes.keys().find(|k| {
        !matches!(
            k.as_str(),
            "hubPairing" | "updateSource" | "deviceRunFromOthers" | "deviceHouseholdRun"
        )
    }) {
        return Err(format!(
            "{} cannot be staged: only hubPairing, updateSource, deviceRunFromOthers and deviceHouseholdRun can.",
            k.chars().take(64).collect::<String>()
        ));
    }
    let (member, source, run, household) = db
        .snapshot(|tx| {
            Ok::<_, rusqlite::Error>((
                household_member(tx, chat_id),
                helm_updates::current_source(tx),
                switch(tx, DEVICE_RUN_FROM_OTHERS.name()),
                switch(tx, DEVICE_HOUSEHOLD_RUN.name()),
            ))
        })
        .map_err(|_| "The settings could not be read.".to_owned())?;
    if member {
        return Err(
            "Only chats of this PC's own person can stage its settings, not a household member's."
                .into(),
        );
    }

    let mut staged = Staged::default();
    let mut unchanged: Vec<String> = Vec::new();
    if let Some(v) = changes.get("hubPairing") {
        // Never echo the input: it names the code file.
        let a: PairingArgs = serde_json::from_value(v.clone()).map_err(|_| {
            "hubPairing takes address, codeFile, person, machine and optionally fallback."
                .to_owned()
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
        staged.hub_pairing = Some(Pairing {
            address,
            fallback,
            code_file,
            person: plain_name(&a.person, "person")?,
            machine: plain_name(&a.machine, "machine")?,
            replaces,
        });
    }
    if let Some(v) = changes.get("updateSource") {
        let to = match v {
            Value::Null => None,
            Value::String(s) => helm_updates::checked_source(Some(s)).map_err(|_| {
                "updateSource must be an existing folder (an absolute path, not a link), or null for none."
                    .to_owned()
            })?,
            _ => return Err("updateSource must be a folder path or null.".into()),
        };
        if let Some(t) = &to {
            plain_path(t, "updateSource")?;
        }
        if to == source {
            unchanged.push(format!(
                "updateSource is already {}",
                to.as_deref().unwrap_or("none")
            ));
        }
        staged.update_source = Some(Change { from: source, to });
    }
    for (key, now, slot) in [
        (
            "deviceRunFromOthers",
            run,
            &mut staged.device_run_from_others,
        ),
        (
            "deviceHouseholdRun",
            household,
            &mut staged.device_household_run,
        ),
    ] {
        if let Some(v) = changes.get(key) {
            let to = v
                .as_bool()
                .ok_or_else(|| format!("{key} must be true or false."))?;
            if to == now {
                unchanged.push(format!("{key} is already {}", on_off(now)));
            }
            *slot = Some(Change { from: now, to });
        }
    }
    if !unchanged.is_empty() {
        return Err(format!(
            "Nothing staged: {}. Leave out what is already set.",
            unchanged.join("; ")
        ));
    }
    card_lines(&staged)?;
    Ok(staged)
}

fn stage(ctx: &ToolCtx, args: Value) -> ToolResult {
    let refuse = |m: String| ToolError::refused(format!("[Helm] Nothing was staged: {m}"));
    let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
    let staged = build(&ctx.services, &ctx.caller_chat_id, &args).map_err(refuse)?;
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
        "note": "The user now sees an Approve card in this chat listing exactly these changes. Helm applies them when the user approves and sends you the outcome as a note; do not call again or poll.",
    }))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            TOOL,
            "Stage setup changes that only the user may approve, for Helm to apply when they click Approve: hubPairing {address, fallback?, codeFile, person, machine} pairs this PC with the hub using the one-time code in codeFile (Helm reads it only when applying, and pairs only if the hub names that person and machine), updateSource sets the update folder (a folder path, or null for none), and deviceRunFromOthers and deviceHouseholdRun set the two Settings > Devices switches. Answers at once with pending; the user sees one Approve card listing every change, and the outcome reaches you as a note from Helm. Use it only when the user asked for these changes.",
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
                            },
                            "updateSource": {"type": ["string", "null"], "maxLength": 260},
                            "deviceRunFromOthers": {"type": "boolean"},
                            "deviceHouseholdRun": {"type": "boolean"}
                        },
                        "minProperties": 1,
                        "additionalProperties": false
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
            Ok(s) => apply_staged(job.services, job.events, &s),
            Err(_) => "Nothing changed: the staged changes could not be read.".into(),
        }
    }
}

fn apply_staged(services: &Arc<Services>, events: &Arc<dyn EventSink>, s: &Staged) -> String {
    // (a) Every check that changes nothing, again.
    let source = match &s.update_source {
        Some(c) => match helm_updates::checked_source(c.to.as_deref()) {
            Ok(v) => Some(v),
            Err(_) => {
                return "Nothing changed: the update folder is no longer an existing folder.".into()
            }
        },
        None => None,
    };
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
    let Some(db) = services.get::<Database>() else {
        return "Nothing changed: the settings are not available.".into();
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

    // (c) The folder and the switches together. A value that no longer matches the card's "from"
    // is named in the outcome, so the agent knows the card was read against something else.
    let mut drift: Vec<String> = Vec::new();
    let _ = db.snapshot(|tx| {
        if let Some(c) = &s.update_source {
            let now = helm_updates::current_source(tx);
            if now != c.from {
                drift.push(format!(
                    "The update folder was {}, not {} as the card said.",
                    now.as_deref().unwrap_or("none"),
                    c.from.as_deref().unwrap_or("none")
                ));
            }
        }
        for (label, key, c) in [
            (
                RUN_FROM_OTHERS,
                DEVICE_RUN_FROM_OTHERS.name(),
                &s.device_run_from_others,
            ),
            (
                HOUSEHOLD_RUN,
                DEVICE_HOUSEHOLD_RUN.name(),
                &s.device_household_run,
            ),
        ] {
            if let Some(c) = c {
                let now = switch(tx, key);
                if now != c.from {
                    drift.push(format!(
                        "{label} was {}, not {} as the card said.",
                        on_off(now),
                        on_off(c.from)
                    ));
                }
            }
        }
        Ok::<_, rusqlite::Error>(())
    });
    let mut changed = Map::new();
    if let Some(v) = &source {
        changed.insert(
            helm_updates::UPDATE_SOURCE.name().into(),
            v.clone().map_or(Value::Null, Value::String),
        );
    }
    if let Some(c) = &s.device_run_from_others {
        changed.insert(DEVICE_RUN_FROM_OTHERS.name().into(), Value::Bool(c.to));
    }
    if let Some(c) = &s.device_household_run {
        changed.insert(DEVICE_HOUSEHOLD_RUN.name().into(), Value::Bool(c.to));
    }
    let names = |m: &Map<String, Value>| -> Vec<&'static str> {
        m.keys()
            .map(|k| match k.as_str() {
                "updateSource" => "the update folder",
                "deviceRunFromOthers" => RUN_FROM_OTHERS,
                _ => HOUSEHOLD_RUN,
            })
            .collect()
    };
    if !changed.is_empty() {
        let saved = db.write(|tx| {
            for (key, value) in &changed {
                let k = match key.as_str() {
                    "updateSource" => helm_updates::UPDATE_SOURCE,
                    "deviceRunFromOthers" => DEVICE_RUN_FROM_OTHERS,
                    _ => DEVICE_HOUSEHOLD_RUN,
                };
                prefs::set(tx, k, value)?;
            }
            Ok::<_, prefs::PrefError>(())
        });
        if let Err(e) = saved {
            log::warn!("staged settings: saving failed: {e}");
            let lead = if done.is_empty() {
                "Nothing changed".to_owned()
            } else {
                done.join(" ")
            };
            return format!(
                "{lead}; these did not change: {}. Stage them again.",
                names(&changed).join(", ")
            );
        }
        if source.is_some() {
            if let Some(svc) = services.get::<helm_updates::UpdateService>() {
                svc.check();
            }
            done.push(format!(
                "The update folder is now {}.",
                source.clone().flatten().as_deref().unwrap_or("none")
            ));
        }
        for (label, c) in [
            (RUN_FROM_OTHERS, &s.device_run_from_others),
            (HOUSEHOLD_RUN, &s.device_household_run),
        ] {
            if let Some(c) = c {
                done.push(format!("{label} is now {}.", on_off(c.to)));
            }
        }
        events.emit(AppEvent::new("preferences").with("changed", Value::Object(changed)));
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
