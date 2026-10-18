//! The AFK preference record. Read **leniently** (each field on its
//! own; a bad field falls back to its default) and written **strictly** (unknown keys, wrong types,
//! out-of-range values or control characters refuse the whole save with a specific message).
//!
//! Stored in this crate's own `afk_settings` table rather than the shared
//! `preferences` table, so the crate needs no line in helm-core's key classes. The signing keys
//! are not offered: there is no unsigned-commit wrapper on Windows.

use serde::Serialize;
use serde_json::{Map, Value};

pub const MAX_ANSWER: usize = 4000;
pub const MAX_DIGEST_PROMPT: usize = 16000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AppliesTo {
    All,
    Chat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanApproval {
    Defer,
    Approve,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoEnd {
    OnReturn,
    OnReturnAutoOnly,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSettings {
    pub screen_lock: bool,
    pub lock_grace_seconds: u32,
    pub sleep: bool,
    pub unanswered_minutes: Option<u32>,
    pub idle_minutes: Option<u32>,
    pub end: AutoEnd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AfkSettings {
    pub enabled: bool,
    pub applies_to: AppliesTo,
    pub answer: Option<String>,
    pub plan_approval: PlanApproval,
    pub digest_prompt: Option<String>,
    pub auto: AutoSettings,
}

impl Default for AfkSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            applies_to: AppliesTo::All,
            answer: None,
            plan_approval: PlanApproval::Defer,
            digest_prompt: None,
            auto: AutoSettings {
                screen_lock: true,
                lock_grace_seconds: 0,
                sleep: true,
                unanswered_minutes: Some(30),
                idle_minutes: Some(30),
                end: AutoEnd::OnReturn,
            },
        }
    }
}

fn applies_of(v: &Value) -> Option<AppliesTo> {
    match v.as_str()? {
        "all" => Some(AppliesTo::All),
        "chat" => Some(AppliesTo::Chat),
        _ => None,
    }
}

fn plan_of(v: &Value) -> Option<PlanApproval> {
    match v.as_str()? {
        "defer" => Some(PlanApproval::Defer),
        "approve" => Some(PlanApproval::Approve),
        _ => None,
    }
}

fn end_of(v: &Value) -> Option<AutoEnd> {
    match v.as_str()? {
        "onReturn" => Some(AutoEnd::OnReturn),
        "onReturnAutoOnly" => Some(AutoEnd::OnReturnAutoOnly),
        "manual" => Some(AutoEnd::Manual),
        _ => None,
    }
}

fn int_in(v: &Value, lo: u32, hi: u32) -> Option<u32> {
    let n = v.as_u64()?;
    (n >= lo as u64 && n <= hi as u64).then_some(n as u32)
}

/// Null, or a wait of 5–240 minutes.
fn wait_of(v: &Value) -> Option<Option<u32>> {
    if v.is_null() {
        return Some(None);
    }
    int_in(v, 5, 240).map(Some)
}

/// Text that may span lines: tabs and line breaks are fine, other control characters are not.
fn clean_text(s: &str) -> bool {
    !s.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
}

/// Null or blank resets to the built-in text.
fn text_of(v: &Value, max: usize) -> Option<Option<String>> {
    if v.is_null() {
        return Some(None);
    }
    let s = v.as_str()?;
    if s.chars().count() > max || !clean_text(s) {
        return None;
    }
    Some((!s.trim().is_empty()).then(|| s.to_owned()))
}

/// Lenient read: every field independently, falling back to its default.
pub fn read_lenient(v: &Value) -> AfkSettings {
    let mut s = AfkSettings::default();
    let Some(o) = v.as_object() else { return s };
    if let Some(b) = o.get("enabled").and_then(Value::as_bool) {
        s.enabled = b;
    }
    // An unknown scope reads as `all`.
    if let Some(a) = o.get("appliesTo").and_then(applies_of) {
        s.applies_to = a;
    }
    if let Some(t) = o.get("answer").and_then(|v| text_of(v, MAX_ANSWER)) {
        s.answer = t;
    }
    if let Some(p) = o.get("planApproval").and_then(plan_of) {
        s.plan_approval = p;
    }
    if let Some(t) = o
        .get("digestPrompt")
        .and_then(|v| text_of(v, MAX_DIGEST_PROMPT))
    {
        s.digest_prompt = t;
    }
    if let Some(a) = o.get("auto").and_then(Value::as_object) {
        if let Some(b) = a.get("screenLock").and_then(Value::as_bool) {
            s.auto.screen_lock = b;
        }
        if let Some(n) = a.get("lockGraceSeconds").and_then(|v| int_in(v, 0, 600)) {
            s.auto.lock_grace_seconds = n;
        }
        if let Some(b) = a.get("sleep").and_then(Value::as_bool) {
            s.auto.sleep = b;
        }
        if let Some(w) = a.get("unansweredMinutes").and_then(wait_of) {
            s.auto.unanswered_minutes = w;
        }
        if let Some(w) = a.get("idleMinutes").and_then(wait_of) {
            s.auto.idle_minutes = w;
        }
        if let Some(e) = a.get("end").and_then(end_of) {
            s.auto.end = e;
        }
    }
    s
}

fn only_keys(o: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    match o.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("{what} has no setting called \"{k}\".")),
        None => Ok(()),
    }
}

/// Strict write: the whole record, every key checked, or a specific refusal.
pub fn parse_strict(v: &Value) -> Result<AfkSettings, String> {
    let o = v.as_object().ok_or("AFK settings must be an object.")?;
    only_keys(
        o,
        &[
            "enabled",
            "appliesTo",
            "answer",
            "planApproval",
            "digestPrompt",
            "auto",
        ],
        "AFK",
    )?;
    let mut s = AfkSettings::default();
    if let Some(v) = o.get("enabled") {
        s.enabled = v.as_bool().ok_or("enabled must be true or false.")?;
    }
    if let Some(v) = o.get("appliesTo") {
        s.applies_to = applies_of(v).ok_or("appliesTo must be \"all\" or \"chat\".")?;
    }
    if s.applies_to == AppliesTo::Chat {
        // Per-chat spans need the worker's AFK answers, which are not
        // built yet. Refused rather than stored, so nothing pretends to be per-chat.
        return Err("One-chat AFK is not available yet; use \"all\".".into());
    }
    if let Some(v) = o.get("answer") {
        s.answer = text_of(v, MAX_ANSWER).ok_or(
            "The automatic answer must be text of at most 4000 characters, without control characters.",
        )?;
    }
    if let Some(v) = o.get("planApproval") {
        s.plan_approval = plan_of(v).ok_or("planApproval must be \"defer\" or \"approve\".")?;
    }
    if let Some(v) = o.get("digestPrompt") {
        s.digest_prompt = text_of(v, MAX_DIGEST_PROMPT).ok_or(
            "The summary prompt must be text of at most 16000 characters, without control characters.",
        )?;
    }
    if let Some(v) = o.get("auto") {
        let a = v.as_object().ok_or("auto must be an object.")?;
        only_keys(
            a,
            &[
                "screenLock",
                "lockGraceSeconds",
                "sleep",
                "unansweredMinutes",
                "idleMinutes",
                "end",
            ],
            "Auto-AFK",
        )?;
        if let Some(v) = a.get("screenLock") {
            s.auto.screen_lock = v.as_bool().ok_or("screenLock must be true or false.")?;
        }
        if let Some(v) = a.get("lockGraceSeconds") {
            s.auto.lock_grace_seconds =
                int_in(v, 0, 600).ok_or("The lock grace must be 0 to 600 seconds.")?;
        }
        if let Some(v) = a.get("sleep") {
            s.auto.sleep = v.as_bool().ok_or("sleep must be true or false.")?;
        }
        if let Some(v) = a.get("unansweredMinutes") {
            s.auto.unanswered_minutes =
                wait_of(v).ok_or("The no-reply wait must be off or 5 to 240 minutes.")?;
        }
        if let Some(v) = a.get("idleMinutes") {
            s.auto.idle_minutes =
                wait_of(v).ok_or("The idle wait must be off or 5 to 240 minutes.")?;
        }
        if let Some(v) = a.get("end") {
            s.auto.end =
                end_of(v).ok_or("end must be \"onReturn\", \"onReturnAutoOnly\" or \"manual\".")?;
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lenient_read_keeps_good_fields_and_defaults_bad_ones() {
        let s = read_lenient(&json!({
            "enabled": true,
            "appliesTo": "sideways",
            "answer": 7,
            "auto": {"idleMinutes": 999, "unansweredMinutes": null, "lockGraceSeconds": 30}
        }));
        assert!(s.enabled);
        assert_eq!(s.applies_to, AppliesTo::All);
        assert_eq!(s.answer, None);
        assert_eq!(
            s.auto.idle_minutes,
            Some(30),
            "out of range falls back to 30"
        );
        assert_eq!(s.auto.unanswered_minutes, None, "null is a valid 'off'");
        assert_eq!(s.auto.lock_grace_seconds, 30);
        assert_eq!(read_lenient(&json!("junk")), AfkSettings::default());
    }

    #[test]
    fn strict_write_refuses_specifically() {
        let e = |v: Value| parse_strict(&v).unwrap_err();
        assert!(e(json!({"enabld": true})).contains("enabld"));
        assert!(e(json!({"auto": {"idleMinutes": 4}})).contains("5 to 240"));
        assert!(e(json!({"auto": {"idleMinutes": 241}})).contains("5 to 240"));
        assert!(e(json!({"auto": {"lockGraceSeconds": 601}})).contains("0 to 600"));
        assert!(e(json!({"answer": "x".repeat(4001)})).contains("4000"));
        assert!(e(json!({"answer": "bell\u{7}"})).contains("control"));
        assert!(e(json!({"appliesTo": "chat"})).contains("not available"));
        assert!(e(json!({"auto": {"bogus": 1}})).contains("bogus"));
    }

    #[test]
    fn strict_write_accepts_edges_and_blank_resets() {
        let s = parse_strict(&json!({
            "enabled": true,
            "answer": "   ",
            "digestPrompt": "line one\nline two",
            "auto": {"idleMinutes": 5, "unansweredMinutes": 240, "lockGraceSeconds": 600, "end": "manual"}
        }))
        .unwrap();
        assert_eq!(s.answer, None, "blank resets to the built-in text");
        assert_eq!(s.digest_prompt.as_deref(), Some("line one\nline two"));
        assert_eq!(s.auto.idle_minutes, Some(5));
        assert_eq!(s.auto.unanswered_minutes, Some(240));
        assert_eq!(s.auto.end, AutoEnd::Manual);
        assert!(parse_strict(&json!({"answer": "x".repeat(4000)})).is_ok());
    }
}
