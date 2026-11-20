//! The launcher line's frames: JSON text, each named by `type`.
//!
//! | dir | type | fields |
//! |---|---|---|
//! | launcher → hub | `hello` | `version` (1), `helmRunning`, `wake` (`{mac, broadcast}` or null) |
//! | hub → launcher | `welcome` | `version`, `machineId`, `machine` |
//! | launcher → hub | `state` | `helmRunning`, `crashLoop`: sent on every change |
//! | hub → launcher | `launch` | `id` (16 to 32 hex digits), `reason` (`telegram`, `voice` or `button`) |
//! | launcher → hub | `launched` | `id`, `ok`, `already` (Helm was running), `error` (a plain sentence, optional) |
//!
//! The launcher sends a WebSocket ping every [`PING_EVERY`]; the hub drops a line that has been
//! silent for [`SILENCE`], and the launcher drops one the hub has been silent on for as long.

use serde_json::{json, Value};
use std::time::Duration;

pub const VERSION: u32 = 1;
pub const PING_EVERY: Duration = Duration::from_secs(30);
pub const SILENCE: Duration = Duration::from_secs(100);

/// What the hub needs to wake this PC: the adapter's MAC (`AA:BB:CC:DD:EE:FF`) and its LAN's
/// directed broadcast address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wake {
    pub mac: String,
    pub broadcast: String,
}

pub fn hello(helm_running: bool, wake: Option<&Wake>) -> Value {
    json!({
        "type": "hello",
        "version": VERSION,
        "helmRunning": helm_running,
        "wake": wake.map(|w| json!({ "mac": w.mac, "broadcast": w.broadcast })),
    })
}

pub fn state(helm_running: bool, crash_loop: bool) -> Value {
    json!({ "type": "state", "helmRunning": helm_running, "crashLoop": crash_loop })
}

pub fn launched(id: &str, ok: bool, already: bool, error: Option<&str>) -> Value {
    let mut v = json!({ "type": "launched", "id": id, "ok": ok, "already": already });
    if let Some(e) = error {
        v["error"] = json!(e);
    }
    v
}

/// A frame from the hub, as far as the launcher acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromHub {
    Welcome {
        /// The hub's number for this PC; 0 when the frame has none.
        machine_id: i64,
        machine: String,
    },
    Launch {
        id: String,
        reason: String,
    },
    /// Anything else, or a frame that does not hold together: ignored.
    Other,
}

pub fn parse(text: &str) -> FromHub {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FromHub::Other;
    };
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    match v.get("type").and_then(Value::as_str) {
        Some("welcome") => FromHub::Welcome {
            machine_id: v.get("machineId").and_then(Value::as_i64).unwrap_or(0),
            machine: s("machine"),
        },
        Some("launch") => {
            let id = s("id");
            let ok = (16..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit());
            if ok {
                FromHub::Launch {
                    id,
                    reason: s("reason"),
                }
            } else {
                FromHub::Other
            }
        }
        _ => FromHub::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_the_line_table() {
        let wake = Wake {
            mac: "AA:BB:CC:DD:EE:FF".into(),
            broadcast: "192.168.1.255".into(),
        };
        let out: Vec<(Value, Value)> = vec![
            (
                hello(true, Some(&wake)),
                json!({"type": "hello", "version": 1, "helmRunning": true,
                       "wake": {"mac": "AA:BB:CC:DD:EE:FF", "broadcast": "192.168.1.255"}}),
            ),
            (
                hello(false, None),
                json!({"type": "hello", "version": 1, "helmRunning": false, "wake": null}),
            ),
            (
                state(false, true),
                json!({"type": "state", "helmRunning": false, "crashLoop": true}),
            ),
            (
                launched("0123456789abcdef", true, true, None),
                json!({"type": "launched", "id": "0123456789abcdef", "ok": true, "already": true}),
            ),
            (
                launched(
                    "0123456789abcdef",
                    false,
                    false,
                    Some("Helm is not installed here."),
                ),
                json!({"type": "launched", "id": "0123456789abcdef", "ok": false, "already": false,
                       "error": "Helm is not installed here."}),
            ),
        ];
        for (got, want) in out {
            assert_eq!(got, want);
        }
        let id = "0123456789abcdef0123456789ABCDEF";
        let rows: Vec<(String, FromHub)> = vec![
            (
                json!({"type": "welcome", "version": 1, "machineId": 7, "machine": "Desk"})
                    .to_string(),
                FromHub::Welcome {
                    machine_id: 7,
                    machine: "Desk".into(),
                },
            ),
            (
                json!({"type": "launch", "id": id, "reason": "telegram"}).to_string(),
                FromHub::Launch {
                    id: id.into(),
                    reason: "telegram".into(),
                },
            ),
            // An id that is too short, too long or not hex is not a launch.
            (
                json!({"type": "launch", "id": "abc", "reason": "voice"}).to_string(),
                FromHub::Other,
            ),
            (
                json!({"type": "launch", "id": format!("{id}0"), "reason": "voice"}).to_string(),
                FromHub::Other,
            ),
            (
                json!({"type": "launch", "id": "0123456789abcdeg", "reason": "voice"}).to_string(),
                FromHub::Other,
            ),
            (json!({"type": "later"}).to_string(), FromHub::Other),
            ("not json".into(), FromHub::Other),
        ];
        for (text, want) in rows {
            assert_eq!(parse(&text), want, "{text}");
        }
    }
}
