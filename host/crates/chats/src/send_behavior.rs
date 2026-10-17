//! Send behaviour (settings-schema `sendBehavior`).
//!
//! Helm's defaults are [`HOME_DEFAULTS`]: asap, a 30 s wait limit,
//! long commands redirected, per-tool Stop shown when supported. Everything that is measured
//! "against the defaults" (what gets stored, and whether a chat counts as opted in) is measured
//! against these, so a profile that never touched the setting behaves the default way.
//!
//! Storage: the `sendBehavior` preference holds only the keys that differ from the defaults. The
//! generic `preference` op validates it with [`helm_core::prefs::validate`] (unknown keys and bad
//! values are refused); a stored value that cannot be read resolves to the defaults.

use crate::types::{LongCommands, SendBehavior, SendMode};
use serde_json::{Map, Value};

/// Helm's default send behaviour. The one place the defaults are written down.
pub const HOME_DEFAULTS: SendBehavior = SendBehavior {
    mode: SendMode::Asap,
    show_choices: true,
    stop_sends_waiting: false,
    wait_limit_seconds: Some(30),
    tool_stop: true,
    long_commands: LongCommands::Redirect,
};

/// The full setting from the stored diff. Anything unreadable (not an object, a key of the wrong
/// shape) reads as the defaults for that key; a stored value that is not an object reads as all
/// defaults.
pub fn resolve(stored: Option<&Value>) -> SendBehavior {
    let mut out = HOME_DEFAULTS;
    let Some(Value::Object(m)) = stored else {
        return out;
    };
    if let Some(v) = m
        .get("mode")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
    {
        out.mode = v;
    }
    if let Some(v) = m.get("showChoices").and_then(Value::as_bool) {
        out.show_choices = v;
    }
    if let Some(v) = m.get("stopSendsWaiting").and_then(Value::as_bool) {
        out.stop_sends_waiting = v;
    }
    match m.get("waitLimitSeconds") {
        Some(Value::Null) => out.wait_limit_seconds = None,
        Some(v) => {
            if let Some(n) = v.as_u64().filter(|n| (5..=3600).contains(n)) {
                out.wait_limit_seconds = Some(n as u32);
            }
        }
        None => {}
    }
    if let Some(v) = m.get("toolStop").and_then(Value::as_bool) {
        out.tool_stop = v;
    }
    if let Some(v) = m
        .get("longCommands")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
    {
        out.long_commands = v;
    }
    out
}

/// Only the keys that differ from [`HOME_DEFAULTS`], as the preference stores them.
pub fn diff(sb: &SendBehavior) -> Value {
    let full = serde_json::to_value(sb).expect("send behaviour serialises");
    let defaults = serde_json::to_value(HOME_DEFAULTS).expect("defaults serialise");
    let (Value::Object(full), Value::Object(defaults)) = (full, defaults) else {
        unreachable!("structs serialise to objects")
    };
    let out: Map<String, Value> = full
        .into_iter()
        .filter(|(k, v)| defaults.get(k) != Some(v))
        .collect();
    Value::Object(out)
}

/// A chat counts as opted in when any field differs from the defaults. The
/// worker receives `{longCommands, optIn}` at launch.
pub fn opt_in(sb: &SendBehavior) -> bool {
    *sb != HOME_DEFAULTS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nothing_stored_is_the_home_defaults() {
        let sb = resolve(None);
        assert_eq!(sb.mode, SendMode::Asap);
        assert_eq!(sb.wait_limit_seconds, Some(30));
        assert_eq!(sb.long_commands, LongCommands::Redirect);
        assert!(sb.tool_stop);
        assert!(sb.show_choices);
        assert!(!sb.stop_sends_waiting);
        assert!(!opt_in(&sb));
        assert_eq!(diff(&sb), json!({}));
    }

    #[test]
    fn a_stored_diff_overrides_only_its_keys() {
        let sb = resolve(Some(&json!({"mode": "queue", "waitLimitSeconds": null})));
        assert_eq!(sb.mode, SendMode::Queue);
        assert_eq!(sb.wait_limit_seconds, None);
        assert_eq!(sb.long_commands, LongCommands::Redirect);
        assert!(opt_in(&sb));
        assert_eq!(
            diff(&sb),
            json!({"mode": "queue", "waitLimitSeconds": null})
        );
    }

    #[test]
    fn a_stored_diff_passes_the_preference_validator_and_round_trips() {
        let sb = SendBehavior {
            mode: SendMode::Queue,
            show_choices: false,
            stop_sends_waiting: true,
            wait_limit_seconds: None,
            tool_stop: false,
            long_commands: LongCommands::Off,
        };
        helm_core::prefs::validate(helm_core::prefs::SEND_BEHAVIOR, &diff(&sb)).unwrap();
        assert_eq!(resolve(Some(&diff(&sb))), sb);
    }

    #[test]
    fn unreadable_values_read_as_defaults() {
        assert_eq!(resolve(Some(&json!("nonsense"))), HOME_DEFAULTS);
        let sb = resolve(Some(
            &json!({"mode": "sideways", "waitLimitSeconds": 2, "toolStop": 1}),
        ));
        assert_eq!(sb, HOME_DEFAULTS);
    }
}
