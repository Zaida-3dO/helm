//! The gatekeeper record, per person. Read **leniently** (each field on its own; a bad field
//! falls back to its default) and written **strictly** (unknown keys, wrong types, out-of-range
//! values or control characters refuse the whole save with a specific message).
//!
//! Stored in this crate's own `gate_settings` table rather than the shared `preferences` table,
//! so the crate needs no line in helm-core's key classes. Each person's row is keyed by their
//! profile id; the empty key is the fallback for a chat with no person and for a machine where
//! no person is set.

use serde::Serialize;
use serde_json::{Map, Value};

pub const MAX_POLICY: usize = 4_000;
pub const MAX_MODEL: usize = 120;

/// The built-in policy: the same principle in generic words, naming no one. A person's own
/// copy, naming their live services, lives in their row and never in the repo.
pub const DEFAULT_POLICY: &str = "Only ask the user when it truly needs them. Where a reasonable \
default exists and staying easy to undo, take it and carry on. Always hand the choice to the \
user when it cannot be undone, costs money, sends anything to other people, touches credentials, \
or touches anything live or in production. Prefer the choice that keeps the work moving and \
stays easy to reverse.";

/// How often the gate comes to the user. The bar is the confidence the gate needs before it
/// decides: at "more" it asks often, at "less" it almost never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AskFrequency {
    Less,
    Balanced,
    More,
}

impl AskFrequency {
    pub fn bar(self) -> f64 {
        match self {
            Self::Less => 0.6,
            Self::Balanced => 0.75,
            Self::More => 0.9,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Less => "less",
            Self::Balanced => "balanced",
            Self::More => "more",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "less" => Some(Self::Less),
            "balanced" => Some(Self::Balanced),
            "more" => Some(Self::More),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateSettings {
    pub enabled: bool,
    #[serde(rename = "askFrequency")]
    pub ask: AskFrequency,
    /// The person's own policy words; the built-in [`DEFAULT_POLICY`] when none.
    pub policy: Option<String>,
    /// The model the gate judges with. This build only ever asks Claude through the ordinary
    /// helper path, so this is a Claude model name.
    pub model: String,
}

impl Default for GateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            ask: AskFrequency::Balanced,
            policy: None,
            model: "sonnet".into(),
        }
    }
}

impl GateSettings {
    pub fn bar(&self) -> f64 {
        self.ask.bar()
    }

    pub fn policy_text(&self) -> &str {
        self.policy.as_deref().unwrap_or(DEFAULT_POLICY)
    }
}

/// Text without control characters (line breaks and tabs are fine).
fn clean_text(s: &str) -> bool {
    !s.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
}

/// A policy of at most [`MAX_POLICY`] characters; blank resets to the built-in words.
fn policy_of(v: &Value) -> Option<Option<String>> {
    if v.is_null() {
        return Some(None);
    }
    let s = v.as_str()?;
    if s.chars().count() > MAX_POLICY || !clean_text(s) {
        return None;
    }
    Some((!s.trim().is_empty()).then(|| s.to_owned()))
}

/// A one-line model name.
fn model_of(v: &Value) -> Option<String> {
    let s = v.as_str()?.trim();
    if s.is_empty() || s.chars().count() > MAX_MODEL || s.contains(|c: char| c.is_control()) {
        return None;
    }
    Some(s.to_owned())
}

/// Lenient read: every field independently, falling back to its default.
pub fn read_lenient(v: &Value) -> GateSettings {
    let mut s = GateSettings::default();
    let Some(o) = v.as_object() else { return s };
    if let Some(b) = o.get("enabled").and_then(Value::as_bool) {
        s.enabled = b;
    }
    if let Some(a) = o
        .get("askFrequency")
        .and_then(|v| AskFrequency::parse(v.as_str()?))
    {
        s.ask = a;
    }
    if let Some(p) = o.get("policy").and_then(|v| policy_of(v)) {
        s.policy = p;
    }
    if let Some(m) = o.get("model").and_then(model_of) {
        s.model = m;
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
pub fn parse_strict(v: &Value) -> Result<GateSettings, String> {
    let o = v
        .as_object()
        .ok_or("Gatekeeper settings must be an object.")?;
    only_keys(
        o,
        &["enabled", "askFrequency", "policy", "model"],
        "Gatekeeper",
    )?;
    let mut s = GateSettings::default();
    if let Some(v) = o.get("enabled") {
        s.enabled = v.as_bool().ok_or("enabled must be true or false.")?;
    }
    if let Some(v) = o.get("askFrequency") {
        s.ask = AskFrequency::parse(v.as_str().unwrap_or_default())
            .ok_or("askFrequency must be \"less\", \"balanced\" or \"more\".")?;
    }
    if let Some(v) = o.get("policy") {
        s.policy = policy_of(v).ok_or_else(|| {
            format!("The policy must be text of at most {MAX_POLICY} characters, without control characters.")
        })?;
    }
    if let Some(v) = o.get("model") {
        s.model = model_of(v).ok_or_else(|| {
            format!("The model must be one line of at most {MAX_MODEL} characters.")
        })?;
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_records_read_and_write() {
        let s = read_lenient(&json!({
            "enabled": false,
            "askFrequency": "more",
            "policy": 7,
            "model": " strata/other "
        }));
        assert!(!s.enabled);
        assert_eq!(s.ask, AskFrequency::More);
        assert_eq!(s.policy, None, "a bad field falls back");
        assert_eq!(s.model, "strata/other");
        assert_eq!(read_lenient(&json!("junk")), GateSettings::default());
        assert_eq!(
            (
                AskFrequency::Less.bar(),
                AskFrequency::Balanced.bar(),
                AskFrequency::More.bar()
            ),
            (0.6, 0.75, 0.9)
        );
        assert!(parse_strict(&json!({
            "enabled": true,
            "askFrequency": "less",
            "policy": "line one\nline two",
            "model": "opus"
        }))
        .is_ok());
        assert!(parse_strict(&json!({"askFrequency": "sometimes"})).is_err());
        assert!(parse_strict(&json!({"policy": "x".repeat(MAX_POLICY + 1)})).is_err());
        assert!(parse_strict(&json!({"policy": "bell\u{7}"})).is_err());
        assert!(parse_strict(&json!({"model": ""})).is_err());
        assert!(parse_strict(&json!({"model": "a\nb"})).is_err());
        assert!(parse_strict(&json!({"bogy": 1})).is_err());
        let s = parse_strict(&json!({"policy": "  "})).unwrap();
        assert_eq!(s.policy, None, "blank resets to the built-in words");
        assert_eq!(GateSettings::default().policy_text(), DEFAULT_POLICY);
    }
}
