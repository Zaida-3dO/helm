//! Settings → Hub for chats: the `telegram`, `voice` and `speaker` keys of `app_get_settings` and
//! `app_update_settings`.
//!
//! Each key is the policy file of the person who owns the chat asking: a chat of any person reads
//! and changes its own person's copy, and Undo goes back to the copy the change came from. A call
//! that names another person (`person`) reaches theirs the same way. A
//! chat changes every field itself, with the Undo notice, except two on the Telegram key:
//! `approvals` and `remoteAnswers` decide what an answer from outside the app may approve, so a
//! chat stages them (`app_stage_settings`) and the user approves. The tools decide that before
//! these adapters run (the key's `staged_parts`); nothing here answers "locked".
//!
//! The `voice` key is the voice chat's policy ([`crate::voice::PolicyFile`]): the model and effort
//! a new voice chat opens with, the idle, age and token limits, and what the attention watch wakes
//! it for. `null` puts the model and effort back to the defaults, and a blank one means the
//! app's usual new-chat setting.
//!
//! The `telegram` value is the whole Telegram policy file: the tag limits, the away-alerts switch,
//! whether approval prompts can be answered from Telegram, whether waiting prompts can be
//! answered from the hub's Remote page, and the model and effort a new Telegram chat opens with.
//!
//! An update names only the fields to change; the merged file goes through the same checks as
//! the Settings page (`PolicyFile::check` and the effort set), so a bad value refuses the whole
//! update and stores nothing. `null` resets `model` and `effort` to unset, which leaves new
//! Telegram chats on the app's usual new-chat settings. The file holds no secrets: the bot
//! credentials live in the credential store.

use crate::telegram::{self, PolicyFile};
use crate::voice;
use helm_hosttools::{SettingsExtension, StagedPart};
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub const KEY: &str = "telegram";
const EVENT: &str = "relay_telegram";
/// Model ids are short (a Claude model, or `local/<server>/<model>`); this bounds the stored
/// string without rejecting any id the model picker offers.
const MAX_MODEL_CHARS: usize = 120;
/// The effort set as said to a chat, matching the app's one list of efforts.
const EFFORT_LIST: &str = "low, medium, high, xhigh or max";

pub struct TelegramPolicy {
    dir: PathBuf,
    /// Whose policy: a profile id, or `None` for the home person's.
    profile: Option<String>,
}

/// The two fields a chat stages: an answer sent from outside the app can approve things.
const TELEGRAM_STAGED: &[StagedPart] = &[
    StagedPart::new(
        "approvals",
        "An answer sent from Telegram can approve things for you, so you approve turning that on.",
    ),
    StagedPart::new(
        "remoteAnswers",
        "An answer sent from the hub's Remote page can approve things for you, so you approve turning that on.",
    ),
];

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry. It owns the three
/// channel keys: `telegram`, `voice` and `speaker`.
pub fn settings_extension(profile_dir: &std::path::Path) -> std::sync::Arc<dyn SettingsExtension> {
    std::sync::Arc::new(Both::new(profile_dir, None))
}

/// The channel keys behind one registration: each call goes to the policy that owns the key.
struct Both {
    telegram: TelegramPolicy,
    voice: VoicePolicy,
    speaker: SpeakerPolicy,
}

impl Both {
    /// The three policies of one person (`None`: the home person's).
    fn new(dir: &std::path::Path, profile: Option<String>) -> Self {
        Both {
            telegram: TelegramPolicy {
                dir: dir.to_path_buf(),
                profile: profile.clone(),
            },
            voice: VoicePolicy {
                dir: dir.to_path_buf(),
                profile: profile.clone(),
            },
            speaker: SpeakerPolicy {
                dir: dir.to_path_buf(),
                profile,
            },
        }
    }

    fn owner(&self, key: &str) -> &dyn SettingsExtension {
        match key {
            VOICE_KEY => &self.voice,
            SPEAKER_KEY => &self.speaker,
            _ => &self.telegram,
        }
    }
}

impl SettingsExtension for Both {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY, VOICE_KEY, SPEAKER_KEY]
    }
    fn describe(&self, key: &str) -> Option<Value> {
        self.owner(key).describe(key)
    }
    fn staged_parts(&self, key: &str) -> &'static [StagedPart] {
        self.owner(key).staged_parts(key)
    }
    fn for_person(
        &self,
        _tx: &Transaction<'_>,
        profile: &str,
    ) -> Option<Arc<dyn SettingsExtension>> {
        Some(Arc::new(Both::new(
            &self.telegram.dir,
            Some(profile.to_owned()),
        )))
    }
    fn for_chat(&self, tx: &Transaction<'_>, chat: &str) -> Option<Arc<dyn SettingsExtension>> {
        let profile = helm_hosttools::settings_ext::chat_profile(tx, chat)?;
        Some(Arc::new(Both::new(&self.telegram.dir, Some(profile))))
    }
    fn read(&self, tx: &Transaction<'_>, key: &str) -> Value {
        self.owner(key).read(tx, key)
    }
    fn apply(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.owner(key).apply(tx, key, value)
    }
    fn check_approved(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.owner(key).check_approved(tx, key, value)
    }
    fn restore(&self, tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), String> {
        self.owner(key).restore(tx, key, value)
    }
    fn changed_event(&self, key: &str) -> AppEvent {
        self.owner(key).changed_event(key)
    }
    fn details(&self, tx: &Transaction<'_>, key: &str) -> Option<Value> {
        self.owner(key).details(tx, key)
    }
    fn after_commit(&self, key: &str) {
        self.owner(key).after_commit(key)
    }
}

fn as_json(f: &PolicyFile) -> Value {
    serde_json::to_value(f).expect("the policy serialises")
}

/// The policy as a value, or a sentence naming what is wrong with it.
fn from_json(v: &Value) -> Result<PolicyFile, String> {
    serde_json::from_value(v.clone()).map_err(|e| {
        format!("Expected maxAgeHours, maxTokens, awayAlerts, approvals, remoteAnswers, and optional model and effort: {e}")
    })
}

/// Merge `patch` into `base`: objects field by field, anything else replaces.
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

/// `model` and `effort` back to unset on top of `current`.
fn reset_changeable(current: &Value) -> Value {
    let mut out = current.clone();
    if let Some(o) = out.as_object_mut() {
        o.remove("model");
        o.remove("effort");
    }
    out
}

/// The two fields a chat may set, checked; a sentence naming the problem, if any.
fn check_changeable(patch: &Value) -> Result<(), String> {
    if let Some(m) = patch.get("model") {
        if !m.is_null() {
            let s = m.as_str().ok_or_else(|| format!("{KEY}.model must be a model id or null (null leaves new Telegram chats on the app's usual new-chat settings)."))?;
            if s.trim().is_empty() {
                return Ok(());
            }
            if s.chars().count() > MAX_MODEL_CHARS || s.chars().any(char::is_control) {
                return Err(format!(
                    "{KEY}.model must be a model id from the model picker (one line, at most {MAX_MODEL_CHARS} characters), or null."
                ));
            }
        }
    }
    if let Some(e) = patch.get("effort") {
        if !e.is_null() {
            let s = e
                .as_str()
                .ok_or_else(|| format!("{KEY}.effort must be one of {EFFORT_LIST} or null."))?;
            if !s.trim().is_empty() && !helm_chats::catalogue::is_effort(s.trim()) {
                return Err(format!(
                    "{KEY}.effort must be one of {EFFORT_LIST} or null."
                ));
            }
        }
    }
    Ok(())
}

fn io_err(e: std::io::Error) -> String {
    format!("The Telegram policy could not be saved: {e}")
}

fn options() -> Value {
    json!({
        "type": "object",
        "about": "Settings > Hub: the Telegram chat's tag limits and switches, and the model and effort a new Telegram chat opens with. Change any field except approvals and remoteAnswers, which decide what an answer from outside the app may approve: those two are staged with app_stage_settings and the user approves them. Send only the fields to change; the rest stay as they are. null resets model and effort to unset.",
        "default": as_json(&PolicyFile::default()),
        "merge": true,
        "fields": {
            "maxAgeHours": {"type": "integer", "min": 1, "max": 720, "about": "How long a message may wait for the chat to be back before it is dropped."},
            "maxTokens": {"type": "integer", "min": 10_000, "max": 5_000_000, "about": "The running token cap for the Telegram chat."},
            "awayAlerts": {"type": "boolean", "about": "Telegram alerts while the user is away."},
            "approvals": {"type": "boolean", "about": "Approval prompts can be answered with Telegram buttons."},
            "remoteAnswers": {"type": "boolean", "about": "Waiting prompts can be answered from the hub's Remote page."},
            "takeTelegram": {"type": "boolean", "nullable": true, "about": "Whether this PC takes its person's Telegram. null: the default, on for the home profile and off for the others; each person can change their own."},
            "model": {"type": "string or null", "rule": "a model id from the model picker (a Claude model, or local/<server>/<model>), at most 120 characters; null or blank: the app's usual new-chat model", "about": "The model a new Telegram chat opens with."},
            "effort": {"type": "enum", "allowed": helm_chats::catalogue::EFFORTS, "nullable": true, "about": "The effort a new Telegram chat opens with; null: the app's usual new-chat effort."},
        },
    })
}

impl TelegramPolicy {
    /// Check `value`, and store it when `store` is set; answers the policy before and after.
    /// Checking a staged change writes nothing.
    fn put(
        &self,
        tx: &Transaction<'_>,
        value: &Value,
        store: bool,
    ) -> Result<(Value, Value), String> {
        let path = self.policy_path(tx);
        let before = as_json(&telegram::load_file(&path));
        let mut wanted = if value.is_null() {
            reset_changeable(&before)
        } else if value.is_object() {
            check_changeable(value)?;
            let mut m = before.clone();
            merge(&mut m, value);
            m
        } else {
            return Err(format!(
                "{KEY} must be an object with only the fields to change (see options.{KEY}), or null."
            ));
        };
        // Blank model/effort read as unset, so a chat can clear them the way the page's pickers do.
        if let Some(o) = wanted.as_object_mut() {
            for k in ["model", "effort"] {
                let blank = o
                    .get(k)
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.trim().is_empty());
                if blank {
                    o.remove(k);
                }
            }
        }
        let merged = from_json(&wanted)?;
        merged.check()?;
        let after = as_json(&merged);
        if store {
            telegram::save_policy(&path, merged).map_err(io_err)?;
        }
        Ok((before, after))
    }

    /// Settings shows one Telegram policy — the home person's. It lives in the home profile's
    /// folder under `relay/people/`; the file the single-person builds left straight under
    /// `relay/` is still read until it moves there, and a fresh PC writes the new place.
    fn policy_path(&self, tx: &Transaction<'_>) -> PathBuf {
        if let Some(who) = &self.profile {
            return older_for_home(
                tx,
                who,
                crate::people::telegram_paths(&self.dir, who).policy,
                telegram::policy_path(&self.dir),
            );
        }
        let per = helm_profiles::store::home(tx)
            .ok()
            .flatten()
            .map(|h| crate::people::telegram_paths(&self.dir, &h.id).policy)
            .unwrap_or_else(|| telegram::policy_path(&self.dir));
        if per.is_file() {
            return per;
        }
        let legacy = telegram::policy_path(&self.dir);
        if legacy.is_file() {
            legacy
        } else {
            per
        }
    }
}

/// The policy file of the person `who`. For the home person it is the older single-person file
/// while that has not moved into the person's folder yet, so a first change keeps the policy the
/// user already had instead of starting from the defaults beside it.
fn older_for_home(tx: &Transaction<'_>, who: &str, per: PathBuf, legacy: PathBuf) -> PathBuf {
    let is_home = helm_profiles::store::home(tx)
        .ok()
        .flatten()
        .is_some_and(|h| h.id == who);
    if is_home && !per.is_file() && legacy.is_file() {
        legacy
    } else {
        per
    }
}

impl SettingsExtension for TelegramPolicy {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        TELEGRAM_STAGED
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(options)
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        as_json(&telegram::load_file(&self.policy_path(tx)))
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.put(tx, value, true)
    }

    fn check_approved(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.put(tx, value, false)
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let f = from_json(value)?;
        f.check()?;
        telegram::save_policy(&self.policy_path(tx), f).map_err(io_err)
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new(EVENT)
    }
}

// ---- the voice chat's policy ---------------------------------------------------------------

pub const VOICE_KEY: &str = "voice";
const VOICE_EVENT: &str = "relay_voice";

pub struct VoicePolicy {
    dir: PathBuf,
    /// Whose policy: a profile id, or `None` for the home person's.
    profile: Option<String>,
}

fn voice_json(f: &voice::PolicyFile) -> Value {
    serde_json::to_value(f).expect("the policy serialises")
}

/// The voice policy as a value, or a sentence naming what is wrong with it.
fn voice_from_json(v: &Value) -> Result<voice::PolicyFile, String> {
    serde_json::from_value(v.clone()).map_err(|e| {
        format!("Expected model, effort, idleMinutes, maxAgeHours, maxTokens and watch: {e}")
    })
}

/// The two fields a chat may set, checked; a sentence naming the problem, if any.
fn check_voice_changeable(patch: &Value) -> Result<(), String> {
    if let Some(m) = patch.get("model") {
        if !m.is_null() {
            let s = m.as_str().ok_or_else(|| {
                format!("{VOICE_KEY}.model must be a model id or null (null puts back the default; blank uses the app's usual new-chat model).")
            })?;
            if s.chars().count() > MAX_MODEL_CHARS || s.chars().any(char::is_control) {
                return Err(format!(
                    "{VOICE_KEY}.model must be a model id from the model picker (one line, at most {MAX_MODEL_CHARS} characters), or null."
                ));
            }
        }
    }
    if let Some(e) = patch.get("effort") {
        if !e.is_null() {
            let s = e.as_str().ok_or_else(|| {
                format!("{VOICE_KEY}.effort must be one of {EFFORT_LIST} or null.")
            })?;
            if !s.trim().is_empty() && !helm_chats::catalogue::is_effort(s.trim()) {
                return Err(format!(
                    "{VOICE_KEY}.effort must be one of {EFFORT_LIST} or null."
                ));
            }
        }
    }
    Ok(())
}

fn voice_options() -> Value {
    json!({
        "type": "object",
        "about": "Settings > Hub: the voice chat's model and effort for a new voice chat, its limits, and what the attention watch wakes it for. Read all of it and change any field. Send only the fields to change; the rest stay as they are. null puts model and effort back to the defaults.",
        "default": voice_json(&voice::PolicyFile::default()),
        "merge": true,
        "fields": {
            "model": {"type": "string", "rule": "a model id from the model picker (a Claude model, or local/<server>/<model>), at most 120 characters; blank: the app's usual new-chat model; null: the default", "about": "The model a new voice chat opens with."},
            "effort": {"type": "enum", "allowed": helm_chats::catalogue::EFFORTS, "blankAllowed": true, "nullable": true, "about": "The effort a new voice chat opens with; blank: the app's usual new-chat effort; null: the default."},
            "takeVoice": {"type": "boolean", "nullable": true, "about": "Whether this PC takes its person's voice line. null: the default, on for the home profile and off for the others; each person can change their own."},
            "idleMinutes": {"type": "integer", "min": 15, "max": 1440, "about": "A message arriving this long after the last one starts a new voice chat."},
            "maxAgeHours": {"type": "integer", "min": 1, "max": 720, "about": "A voice chat this old is left behind."},
            "maxTokens": {"type": "integer", "min": 10_000, "max": 5_000_000, "about": "The running token cap for a voice chat."},
            "watch": {"type": "enum", "allowed": ["off", "flags", "flags+done"], "about": "What the attention watch wakes the voice chat for."},
        },
    })
}

impl VoicePolicy {
    /// Check `value`, and store it when `store` is set; answers the policy before and after.
    /// Checking a staged change writes nothing.
    fn put(
        &self,
        tx: &Transaction<'_>,
        value: &Value,
        store: bool,
    ) -> Result<(Value, Value), String> {
        let file = self.voice_path(tx);
        let before = voice_json(&voice::load_file(&file));
        let mut wanted = if value.is_null() {
            reset_changeable(&before)
        } else if value.is_object() {
            check_voice_changeable(value)?;
            let mut m = before.clone();
            merge(&mut m, value);
            m
        } else {
            return Err(format!(
                "{VOICE_KEY} must be an object with only the fields to change (see options.{VOICE_KEY}), or null."
            ));
        };
        // A null model or effort is the default again.
        let defaults = voice_json(&voice::PolicyFile::default());
        if let Some(o) = wanted.as_object_mut() {
            for k in ["model", "effort"] {
                if o.get(k).is_none_or(|v| v.is_null()) {
                    o.insert(k.to_owned(), defaults[k].clone());
                }
            }
        }
        let merged = voice_from_json(&wanted)?;
        merged.check()?;
        let after = voice_json(&merged);
        if store {
            voice::save_policy(&file, &merged).map_err(voice_io_err)?;
        }
        Ok((before, after))
    }

    /// Settings shows one voice policy — the home person's. As with Telegram, it lives in the
    /// home profile's folder under `relay/people/`, with the older single-person file read
    /// until it moves there.
    fn voice_path(&self, tx: &Transaction<'_>) -> PathBuf {
        if let Some(who) = &self.profile {
            return older_for_home(
                tx,
                who,
                crate::people::voice_paths(&self.dir, who).policy,
                voice::policy_path(&self.dir),
            );
        }
        let per = helm_profiles::store::home(tx)
            .ok()
            .flatten()
            .map(|h| crate::people::voice_paths(&self.dir, &h.id).policy)
            .unwrap_or_else(|| voice::policy_path(&self.dir));
        if per.is_file() {
            return per;
        }
        let legacy = voice::policy_path(&self.dir);
        if legacy.is_file() {
            legacy
        } else {
            per
        }
    }
}

impl SettingsExtension for VoicePolicy {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        &[]
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![VOICE_KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == VOICE_KEY).then(voice_options)
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        voice_json(&voice::load_file(&self.voice_path(tx)))
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.put(tx, value, true)
    }

    fn check_approved(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.put(tx, value, false)
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let f = voice_from_json(value)?;
        f.check()?;
        voice::save_policy(&self.voice_path(tx), &f).map_err(voice_io_err)
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new(VOICE_EVENT)
    }
}

fn voice_io_err(e: std::io::Error) -> String {
    format!("The voice policy could not be saved: {e}")
}

// ---- the room speaker's policy -------------------------------------------------------------

pub const SPEAKER_KEY: &str = "speaker";
const SPEAKER_EVENT: &str = "relay_speaker";

pub struct SpeakerPolicy {
    dir: PathBuf,
    /// Whose policy: a profile id, or `None` for the home person's.
    profile: Option<String>,
}

fn speaker_json(f: &crate::speaker::PolicyFile) -> Value {
    serde_json::to_value(f).expect("the policy serialises")
}

/// The speaker policy as a value, or a sentence naming what is wrong with it.
fn speaker_from_json(v: &Value) -> Result<crate::speaker::PolicyFile, String> {
    serde_json::from_value(v.clone()).map_err(|e| {
        format!("Expected model, effort, takeSpeaker, previousHours, lean and boardId: {e}")
    })
}

/// The two fields a chat may set, checked; a sentence naming the problem, if any.
fn check_speaker_changeable(patch: &Value) -> Result<(), String> {
    if let Some(m) = patch.get("model") {
        if !m.is_null() {
            let s = m.as_str().ok_or_else(|| {
                format!(
                    "{SPEAKER_KEY}.model must be a model id or null (null puts back the default)."
                )
            })?;
            if s.chars().count() > MAX_MODEL_CHARS || s.chars().any(char::is_control) {
                return Err(format!(
                    "{SPEAKER_KEY}.model must be a model id from the model picker (one line, at most {MAX_MODEL_CHARS} characters), or null."
                ));
            }
        }
    }
    if let Some(e) = patch.get("effort") {
        if !e.is_null() {
            let s = e.as_str().ok_or_else(|| {
                format!("{SPEAKER_KEY}.effort must be one of {EFFORT_LIST} or null.")
            })?;
            if !s.trim().is_empty() && !helm_chats::catalogue::is_effort(s.trim()) {
                return Err(format!(
                    "{SPEAKER_KEY}.effort must be one of {EFFORT_LIST} or null."
                ));
            }
        }
    }
    Ok(())
}

fn speaker_options() -> Value {
    json!({
        "type": "object",
        "about": "Settings > Hub: what a new room-speaker chat opens with, and how the house's speakers are taken. Read all of it and change any field. Send only the fields to change; the rest stay as they are. null puts model and effort back to the defaults.",
        "default": speaker_json(&crate::speaker::PolicyFile::default()),
        "merge": true,
        "fields": {
            "model": {"type": "string", "rule": "a model id from the model picker (a Claude model, or local/<server>/<model>), at most 120 characters; blank: the app's usual new-chat model; null: the default", "about": "The model a new room-speaker chat opens with. A spoken answer has to come back in seconds, so the default is a small local model."},
            "effort": {"type": "enum", "allowed": helm_chats::catalogue::EFFORTS, "blankAllowed": true, "nullable": true, "about": "The effort a new room-speaker chat opens with; blank: the app's usual new-chat effort; null: the default."},
            "takeSpeaker": {"type": "boolean", "nullable": true, "about": "Whether this PC takes its person's room speakers. null: the default, on for the home profile and off for the others."},
            "previousHours": {"type": "integer", "min": 0, "max": 72, "about": "Whether a talk that just ended may be named to the next one on that speaker (always within half an hour of the close); 0 never names it."},
            "lean": {"type": "boolean", "about": "Lean speaker chats: a thin system prompt (a short persona and the speaker prompt) and only the Home Assistant server and the tool that sends a message to the phone. Applies to chats started after the change."},
            "boardId": {"type": "string", "nullable": true, "about": "The board a new speaker chat is shown on; unset leaves it to the app."},
        },
    })
}

impl SpeakerPolicy {
    /// Check `value`, and store it when `store` is set; answers the policy before and after.
    /// Checking a staged change writes nothing.
    fn put(
        &self,
        tx: &Transaction<'_>,
        value: &Value,
        store: bool,
    ) -> Result<(Value, Value), String> {
        let file = self
            .policy_path(tx)
            .ok_or("Helm has no home profile yet.")?;
        let before = speaker_json(&crate::speaker::load_file(&file));
        let mut wanted = if value.is_null() {
            reset_changeable(&before)
        } else if value.is_object() {
            check_speaker_changeable(value)?;
            let mut m = before.clone();
            merge(&mut m, value);
            m
        } else {
            return Err(format!(
                "{SPEAKER_KEY} must be an object with only the fields to change (see options.{SPEAKER_KEY}), or null."
            ));
        };
        // A null model or effort is the default again.
        let defaults = speaker_json(&crate::speaker::PolicyFile::default());
        if let Some(o) = wanted.as_object_mut() {
            for k in ["model", "effort"] {
                if o.get(k).is_none_or(|v| v.is_null()) {
                    o.insert(k.to_owned(), defaults[k].clone());
                }
            }
        }
        let merged = speaker_from_json(&wanted)?;
        merged.check()?;
        let after = speaker_json(&merged);
        if store {
            crate::speaker::save_policy(&file, &merged).map_err(speaker_io_err)?;
        }
        Ok((before, after))
    }

    /// Settings shows one speaker policy — the home person's, whose file lives in that profile's
    /// folder under `relay/people/`.
    fn policy_path(&self, tx: &Transaction<'_>) -> Option<PathBuf> {
        if let Some(who) = &self.profile {
            return Some(crate::speaker::paths(&self.dir, who).policy);
        }
        helm_profiles::store::home(tx)
            .ok()
            .flatten()
            .map(|h| crate::speaker::paths(&self.dir, &h.id).policy)
    }
}

impl SettingsExtension for SpeakerPolicy {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        &[]
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![SPEAKER_KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == SPEAKER_KEY).then(speaker_options)
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        match self.policy_path(tx) {
            Some(p) => speaker_json(&crate::speaker::load_file(&p)),
            None => speaker_json(&crate::speaker::PolicyFile::default()),
        }
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.put(tx, value, true)
    }

    fn check_approved(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.put(tx, value, false)
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let f = speaker_from_json(value)?;
        f.check()?;
        let file = self
            .policy_path(tx)
            .ok_or("Helm has no home profile yet.")?;
        crate::speaker::save_policy(&file, &f).map_err(speaker_io_err)
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new(SPEAKER_EVENT)
    }
}

fn speaker_io_err(e: std::io::Error) -> String {
    format!("The speaker policy could not be saved: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};
    use serde_json::json;

    struct Env {
        db: Database,
        ext: TelegramPolicy,
        // Declared last so it outlives the open database when the test ends.
        dir: tempfile::TempDir,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &MigrationRegistry::new()).unwrap();
        let ext = TelegramPolicy {
            dir: dir.path().join("prof"),
            profile: None,
        };
        Env { db, ext, dir }
    }

    impl Env {
        fn read(&self) -> Value {
            self.db
                .snapshot(|tx| Ok::<_, rusqlite::Error>(self.ext.read(tx, KEY)))
                .unwrap()
        }
        fn apply(&self, v: Value) -> Result<(Value, Value), String> {
            self.db
                .write(|tx| Ok::<_, rusqlite::Error>(self.ext.apply(tx, KEY, &v)))
                .expect("the policy file save is not a sql error")
        }
        fn restore(&self, v: Value) -> Result<(), String> {
            self.db
                .write(|tx| Ok::<_, rusqlite::Error>(self.ext.restore(tx, KEY, &v)))
                .expect("the policy file save is not a sql error")
        }
    }

    #[test]
    fn a_chat_sets_the_telegram_policy_and_undo_puts_it_back() {
        let e = env();
        assert_eq!(e.ext.keys(), vec![KEY]);
        assert!(e.ext.describe(KEY).is_some());
        assert!(e.ext.describe("theme").is_none());
        assert_eq!(e.ext.changed_event(KEY).kind, EVENT);
        // The said list is the app's own, phrased with an "or".
        let efforts = helm_chats::catalogue::EFFORTS;
        assert_eq!(
            EFFORT_LIST,
            format!(
                "{} or {}",
                efforts[..efforts.len() - 1].join(", "),
                efforts[efforts.len() - 1]
            )
        );

        // Unset to start; the whole policy reads, with its defaults.
        let off = e.read();
        assert_eq!(off["maxAgeHours"], json!(24));
        assert!(off.get("model").is_none());
        assert!(off.get("effort").is_none());

        // A change names only the fields to set.
        let (before, after) = e
            .apply(json!({"model": "local/strata/qwen-small", "effort": "low"}))
            .unwrap();
        assert!(before.get("model").is_none());
        assert_eq!(after["model"], json!("local/strata/qwen-small"));
        assert_eq!(after["effort"], json!("low"));
        // What the file itself holds, as the page would read it back.
        let saved = telegram::load_file(&telegram::policy_path(&e.dir.path().join("prof")));
        assert_eq!(
            saved.new_chat(),
            (Some("local/strata/qwen-small"), Some("low"))
        );
        // Undo (and the page) still see the user's fields untouched.
        assert_eq!(e.read()["maxAgeHours"], json!(24));
        assert_eq!(e.read()["approvals"], json!(false));
        assert_eq!(e.read()["remoteAnswers"], json!(false));

        // A later field change merges with the earlier one; the effort set is the app's.
        let (_, after) = e.apply(json!({"effort": "xhigh"})).unwrap();
        assert_eq!(after["model"], json!("local/strata/qwen-small"));
        assert_eq!(after["effort"], json!("xhigh"));
        for bad in ["ultra", "ULTRA", "HIGH"] {
            let err = e.apply(json!({"effort": bad})).unwrap_err();
            assert!(
                err.contains("low, medium, high, xhigh or max"),
                "{bad}: {err}"
            );
        }
        // (Blank effort is a clear, checked below; it is not a refusal.)

        // Model: a string of the picker's shape, or null; not a number, not absurdly long.
        assert!(e
            .apply(json!({"model": 3}))
            .unwrap_err()
            .contains("model id"));
        let long = "x".repeat(121);
        assert!(e
            .apply(json!({"model": long}))
            .unwrap_err()
            .contains("at most 120"));
        assert!(e
            .apply(json!({"model": "ha\nku"}))
            .unwrap_err()
            .contains("at most 120"));

        // The adapter holds nothing back: the two switches that decide what an outside answer may
        // approve are declared as staged parts, which the tools stage before this ever runs.
        assert_eq!(
            e.ext
                .staged_parts(KEY)
                .iter()
                .map(|p| p.path)
                .collect::<Vec<_>>(),
            ["approvals", "remoteAnswers"]
        );
        // Every other field is a chat's own: limits and the alerts switch change like the rest.
        let (_, after) = e
            .apply(json!({"maxAgeHours": 48, "awayAlerts": false, "takeTelegram": true}))
            .unwrap();
        assert_eq!(
            (after["maxAgeHours"].clone(), after["awayAlerts"].clone()),
            (json!(48), json!(false))
        );
        assert_eq!(
            after["takeTelegram"],
            json!(true),
            "each person decides their own"
        );
        // Applying the staged switches (the path an approved change takes) stores them.
        let (_, after) = e
            .apply(json!({"approvals": true, "remoteAnswers": true}))
            .unwrap();
        assert_eq!(after["approvals"], json!(true));
        assert!(
            telegram::load_file(&telegram::policy_path(&e.dir.path().join("prof"))).remote_answers
        );
        e.apply(json!({"approvals": false, "remoteAnswers": false, "maxAgeHours": 24, "awayAlerts": true}))
            .unwrap();

        // The merged file goes through the page's own range checks all the same.
        assert!(e
            .apply(json!({"maxAgeHours": 1000, "model": "opus"}))
            .unwrap_err()
            .contains("age limit"));

        // A field outside the policy refuses the whole update.
        assert!(e
            .apply(json!({"token": "abc"}))
            .unwrap_err()
            .contains("maxAgeHours"));
        assert!(e.apply(json!("opus")).unwrap_err().contains("object"));

        // The page's pickers save through the same file: a UI save still round-trips.
        telegram::save_policy(
            &telegram::policy_path(&e.dir.path().join("prof")),
            PolicyFile {
                max_age_hours: 72,
                max_tokens: 50_000,
                away_alerts: false,
                approvals: true,
                remote_answers: false,
                take_telegram: None,
                model: Some("sonnet".into()),
                effort: Some("medium".into()),
            },
        )
        .unwrap();
        let now = e.read();
        assert_eq!(now["maxAgeHours"], json!(72));
        assert_eq!(now["model"], json!("sonnet"));
        // A change that repeats the other fields at their current values still sets the effort.
        let (_, after) = e
            .apply(json!({
                "maxAgeHours": 72, "maxTokens": 50_000, "awayAlerts": false, "approvals": true,
                "model": "sonnet", "effort": "max",
            }))
            .unwrap();
        assert_eq!(after["effort"], json!("max"));

        // Undo: the before-value from an earlier apply goes straight back.
        let (before, after) = e.apply(json!({"model": null})).unwrap();
        assert!(after.get("model").is_none());
        e.restore(before).unwrap();
        assert_eq!(e.read()["model"], json!("sonnet"));
        e.restore(after).unwrap();
        assert!(e.read().get("model").is_none());

        // null resets the model and effort and leaves the limits and switches as they are.
        e.apply(json!({"model": "opus", "effort": "high", "awayAlerts": false}))
            .unwrap();
        e.apply(json!(null)).unwrap();
        let back = e.read();
        assert!(back.get("model").is_none());
        assert!(back.get("effort").is_none());
        assert_eq!(back["awayAlerts"], json!(false));
        // Blank string clears, the way the picker's "App default" option saves.
        e.apply(json!({"model": "opus"})).unwrap();
        e.apply(json!({"model": ""})).unwrap();
        assert!(e.read().get("model").is_none());
        // Everything lived under the temp folder's profile path; nothing touched the real one.
        assert!(telegram::policy_path(&e.dir.path().join("prof")).is_file());
    }

    #[test]
    fn a_chat_sets_the_voice_chats_policy_and_undo_puts_it_back() {
        let e = env();
        let voice_ext = VoicePolicy {
            dir: e.dir.path().join("prof"),
            profile: None,
        };
        let apply = |v: Value| {
            e.db.write(|tx| Ok::<_, rusqlite::Error>(voice_ext.apply(tx, VOICE_KEY, &v)))
                .expect("the policy file save is not a sql error")
        };
        let read = || {
            e.db.snapshot(|tx| Ok::<_, rusqlite::Error>(voice_ext.read(tx, VOICE_KEY)))
                .unwrap()
        };
        // One registration owns both keys, and each goes to its own policy.
        let both = settings_extension(&e.dir.path().join("prof"));
        assert_eq!(both.keys(), vec![KEY, VOICE_KEY, SPEAKER_KEY]);
        assert!(both.describe(VOICE_KEY).is_some() && both.describe(KEY).is_some());
        assert!(both.describe(SPEAKER_KEY).is_some());
        assert!(both.describe("theme").is_none());
        assert_eq!(both.changed_event(VOICE_KEY).kind, VOICE_EVENT);
        assert_eq!(both.changed_event(KEY).kind, EVENT);

        // The defaults read in full.
        let start = read();
        assert_eq!(
            (
                start["model"].as_str(),
                start["effort"].as_str(),
                start["idleMinutes"].as_i64()
            ),
            (Some("claude-sonnet-5-5"), Some("low"), Some(240))
        );
        assert_eq!(start["watch"], json!("flags"));

        // A chat sets the two fields, and the file holds them.
        let (before, after) = apply(json!({"model": "opus", "effort": "high"})).unwrap();
        assert_eq!(before["model"], json!("claude-sonnet-5-5"));
        assert_eq!(
            (after["model"].as_str(), after["effort"].as_str()),
            (Some("opus"), Some("high"))
        );
        assert_eq!(
            voice::load_file(&voice::policy_path(&e.dir.path().join("prof")))
                .new_chat(crate::voice::Lane::Deep),
            (Some("opus"), Some("high"))
        );
        assert_eq!(read()["idleMinutes"], json!(240));

        // Refusals: a bad effort, a bad model, a bad limit, a stranger, a bare value.
        for bad in ["ultra", "HIGH"] {
            let err = apply(json!({"effort": bad})).unwrap_err();
            assert!(
                err.contains("low, medium, high, xhigh or max"),
                "{bad}: {err}"
            );
        }
        assert!(apply(json!({"model": 3})).unwrap_err().contains("model id"));
        assert!(apply(json!({"model": "ha\nku"}))
            .unwrap_err()
            .contains("at most 120"));
        // The limits and the watch are a chat's own to change.
        apply(json!({"idleMinutes": 30, "watch": "off", "maxTokens": 20_000})).unwrap();
        assert_eq!(read()["watch"], json!("off"));
        assert!(both.staged_parts(VOICE_KEY).is_empty());
        assert!(apply(json!({"idleMinutes": 5}))
            .unwrap_err()
            .contains("idle time"));
        assert!(apply(json!({"token": "abc"}))
            .unwrap_err()
            .contains("idleMinutes"));
        assert!(apply(json!("opus")).unwrap_err().contains("object"));
        // Put the limits back, with the model, in one change.
        assert!(apply(json!({"idleMinutes": 240, "watch": "flags", "model": "sonnet"})).is_ok());
        assert_eq!(read()["model"], json!("sonnet"));

        // Blank means the app's usual setting; null puts the defaults back; undo restores.
        apply(json!({"model": "", "effort": ""})).unwrap();
        assert_eq!(
            voice::load_file(&voice::policy_path(&e.dir.path().join("prof")))
                .new_chat(crate::voice::Lane::Deep),
            (None, None)
        );
        let (before, after) = apply(json!(null)).unwrap();
        assert_eq!(after["model"], json!("claude-sonnet-5-5"));
        assert_eq!(after["effort"], json!("low"));
        voice_ext_restore(&e, &voice_ext, &before);
        assert_eq!(read()["model"], json!(""));
        voice_ext_restore(&e, &voice_ext, &after);
        assert_eq!(read()["model"], json!("claude-sonnet-5-5"));
    }

    fn voice_ext_restore(e: &Env, ext: &VoicePolicy, v: &Value) {
        e.db.write(|tx| Ok::<_, rusqlite::Error>(ext.restore(tx, VOICE_KEY, v)))
            .expect("the policy file save is not a sql error")
            .unwrap();
    }
}
