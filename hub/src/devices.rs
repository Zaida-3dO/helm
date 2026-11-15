//! The household's devices: which Helm installs exist, who they belong to, which are up, and the
//! calls and pushes that pass between them.
//!
//! A Helm announces itself with a `device` frame (its own uid, a name, whether other devices
//! may run its chats, and the people it acts for) after every line-up. The hub keeps that on
//! the client's registry row and
//! answers every `peer`-scoped line with a `devices` list, again whenever a device frame arrives or
//! a line comes up or drops. The whole hub is one household: every `peer` line sees every device.
//!
//! Person ids in these frames (`personId`) are the hub's numeric ids written as decimal strings, as
//! the Helm side reads them; the `welcome` frame keeps its number.
//!
//! A `dev` frame is a call for another device. The hub looks the target up by uid, hands the call to
//! its line as a `dev_call` under an id of its own, waits up to [`DEV_TIMEOUT`] and gives the
//! sender the answer as a `dev_result` carrying the sender's own id. The hub never looks inside
//! `op` or `args`. A `dev_push` is handed on the same way and dropped silently when the target is
//! not up. Nothing here depends on a client's address: the hub sits behind a reverse proxy.
//!
//! A `device` frame may also announce the hub people this Helm acts for — one per linked
//! profile, its home person first — and whether it takes each one's Telegram or voice while the
//! line is up. The hub records the people as acts-for rows of source `device`, replacing the
//! machine's previous announced set (rows an admin made by hand stay), and keeps the channel
//! flags on the live line only.
//!
//! A `dev` call may name the person it acts for (`as`, the acting person first). The hub checks
//! every name against what the sender may do — its own person, admin rows and announced rows —
//! and refuses the whole call if any one fails. The `dev_call` it forwards carries the verified
//! people, so the serving Helm reads who is acting from the hub's stamp, never from the sender.
//!
//! | error code | when |
//! |---|---|
//! | `scope` | the sender's token lacks the `peer` scope |
//! | `bad_request` | the sender has not announced its device yet, or `op` is empty or over 64 bytes |
//! | `not_allowed` | the call's `as` names a person the sender does not act for |
//! | `offline` | the target is unknown, revoked or not up |
//! | `timeout` | no `dev_result` within [`DEV_TIMEOUT`], or the target's line dropped mid-call |
//! | `busy` | the sender already has [`MAX_DEV_CALLS`] calls waiting |
//! | `too_large` | the call or its answer is over [`crate::relay::MAX_DEV_FRAME`] |

use crate::auth::Scope;
use crate::lines::Lines;
use crate::registry::{ClientRow, Person};
use crate::relay::{CallOutcome, DeviceClaim, MAX_DEV_FRAME, TOO_LARGE};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Weak;
use std::time::Duration;

/// How long a `dev` call waits for the other device.
pub const DEV_TIMEOUT: Duration = Duration::from_secs(30);

/// How many calls one line may have waiting at once; one more is refused with `busy`, so a
/// device cannot pile up calls (and their arguments) on the hub.
pub const MAX_DEV_CALLS: usize = 8;

/// The `dev_result` for a call over [`MAX_DEV_CALLS`].
pub fn busy(id: &Value) -> Value {
    error_frame(
        id,
        "busy",
        "Too many calls from this device are waiting; try again in a moment.",
    )
}

/// Longest `op` a call may name.
const MAX_OP: usize = 64;

/// A `dev_result` that says why the hub could not pass a call on.
pub fn error_frame(id: &Value, code: &str, message: &str) -> Value {
    json!({
        "type": "dev_result",
        "id": id,
        "ok": false,
        "error": { "code": code, "message": message },
    })
}

fn entry(c: &ClientRow, online: bool, me: Option<&str>, people: &[Person]) -> Value {
    json!({
        "uid": c.device_uid,
        "name": c.device_name,
        "personId": c.person_id.to_string(),
        "person": c.person,
        "people": people
            .iter()
            .map(|p| json!({ "personId": p.id.to_string(), "person": p.name }))
            .collect::<Vec<_>>(),
        "online": online,
        "lastSeenAt": c.last_seen_at,
        "run": c.run,
        "self": c.device_uid.is_some() && c.device_uid.as_deref() == me,
    })
}

/// Send each `peer` line the current `devices` list, with `self` marked for that line's own device.
pub fn broadcast(lines: &Lines) {
    let (rows, people) = match lines.registry().lock() {
        Ok(r) => (
            r.devices().unwrap_or_default(),
            r.device_people().unwrap_or_default(),
        ),
        Err(_) => return,
    };
    let rows: Vec<ClientRow> = rows
        .into_iter()
        .filter(|c| c.scopes.has(Scope::Peer))
        .collect();
    let up: HashSet<i64> = rows
        .iter()
        .filter(|c| lines.get(c.id).is_some_and(|l| l.is_up()))
        .map(|c| c.id)
        .collect();
    for (client_id, line) in lines.peer_lines() {
        let me = rows
            .iter()
            .find(|c| c.id == client_id)
            .and_then(|c| c.device_uid.as_deref());
        let list: Vec<Value> = rows
            .iter()
            .map(|c| {
                entry(
                    c,
                    up.contains(&c.id),
                    me,
                    people.get(&c.id).map(Vec::as_slice).unwrap_or(&[]),
                )
            })
            .collect();
        line.send(&json!({ "type": "devices", "list": list }));
    }
}

/// A line came up or dropped: stamp the machine's last-seen time and tell everyone.
pub fn on_line_change(lines: &Weak<Lines>, client_id: i64) {
    let Some(lines) = lines.upgrade() else {
        return;
    };
    if !lines.is_peer(client_id) {
        return;
    }
    if let Ok(r) = lines.registry().lock() {
        r.touch(client_id);
    }
    broadcast(&lines);
}

/// A `device` frame: remember the device on the client's row, its announced people as acts-for
/// rows of source `device`, and its channel flags for as long as the line is up; then tell
/// everyone. A frame from a token without the `peer` scope, or one with a bad uid or name,
/// changes nothing.
pub fn on_device(
    lines: &Lines,
    client_id: i64,
    uid: &str,
    name: &str,
    run: bool,
    people: &[DeviceClaim],
) {
    if !lines.is_peer(client_id) {
        return;
    }
    let stored = match lines.registry().lock() {
        Ok(mut r) => r.set_device(client_id, uid, name, run).and_then(|_| {
            let ids: Vec<i64> = people
                .iter()
                .filter_map(|p| p.person_id.trim().parse::<i64>().ok())
                .collect();
            r.set_device_people(client_id, &ids)
        }),
        Err(_) => return,
    };
    match stored {
        Ok(unknown) => {
            if !unknown.is_empty() {
                eprintln!(
                    "relay: a device frame named {} unknown person id(s); skipped",
                    unknown.len()
                );
            }
            // The channel flags live only while the line is up; an unparseable person id
            // cannot be a routing candidate, so it is left out.
            if let Some(line) = lines.get(client_id) {
                let claims: Vec<(i64, bool, bool)> = people
                    .iter()
                    .filter_map(|p| {
                        Some((p.person_id.trim().parse::<i64>().ok()?, p.telegram, p.voice))
                    })
                    .collect();
                line.set_claims(claims);
            }
            broadcast(lines);
        }
        Err(e) => eprintln!("relay: a device frame was refused: {e}"),
    }
}

/// The `person_result` for a `person_ensure` the hub served.
pub fn person_result_ok(id: &Value, person: &Person, created: bool) -> Value {
    json!({
        "type": "person_result",
        "id": id,
        "ok": true,
        "personId": person.id.to_string(),
        "person": person.name,
        "created": created,
    })
}

/// The `person_result` for a `person_ensure` the hub refused.
pub fn person_result_error(id: &Value, message: &str) -> Value {
    json!({ "type": "person_result", "id": id, "ok": false, "error": message })
}

/// Carry a `dev` call to its target and return the `dev_result` for the sender. The people the
/// call named (`persons`, the acting one first) are each checked against what the sender may
/// do; one that fails refuses the whole call. With none named the sender acts as its own
/// person, as it always did.
// The call's own fields (id, to, op, args, size) stay separate; `persons` is the frame's `as`
// list, verified here and stamped onto the dev_call.
#[allow(clippy::too_many_arguments)]
pub async fn route(
    lines: &Lines,
    client_id: i64,
    id: Value,
    to: &str,
    op: &str,
    args: Value,
    size: usize,
    persons: &[String],
) -> Value {
    if !lines.is_peer(client_id) {
        return error_frame(
            &id,
            "scope",
            "This machine's token cannot use the household.",
        );
    }
    if op.is_empty() || op.len() > MAX_OP {
        return error_frame(
            &id,
            "bad_request",
            "The call needs an op of 1 to 64 characters.",
        );
    }
    if size > MAX_DEV_FRAME {
        return too_large(&id);
    }
    let (from, target) = {
        let Ok(r) = lines.registry().lock() else {
            return error_frame(&id, "offline", "The hub's registry is unavailable.");
        };
        let sender = r.client_by_id(client_id).ok().flatten();
        let Some(sender) = sender.filter(|s| s.device_uid.is_some()) else {
            return error_frame(
                &id,
                "bad_request",
                "This Helm has not announced its device yet.",
            );
        };
        let not_allowed =
            || error_frame(&id, "not_allowed", "This PC does not act for that person.");
        let mut verified: Vec<i64> = Vec::new();
        if persons.is_empty() {
            verified.push(sender.person_id);
        } else {
            for p in persons {
                let Ok(pid) = p.trim().parse::<i64>() else {
                    return not_allowed();
                };
                if !r.may_act_for(client_id, pid) {
                    return not_allowed();
                }
                if !verified.contains(&pid) {
                    verified.push(pid);
                }
            }
        }
        let name_of = |pid: i64| r.person_by_id(pid).ok().flatten().map(|p| p.name);
        let from = json!({
            "uid": sender.device_uid,
            "name": sender.device_name,
            "personId": verified[0].to_string(),
            "person": name_of(verified[0]),
            "people": verified
                .iter()
                .map(|pid| json!({ "personId": pid.to_string(), "person": name_of(*pid) }))
                .collect::<Vec<_>>(),
            "machinePersonId": sender.person_id.to_string(),
        });
        let target = r
            .client_by_device(to)
            .ok()
            .flatten()
            .filter(|t| t.scopes.has(Scope::Peer));
        (from, target)
    };
    let line = target.and_then(|t| lines.get(t.id)).filter(|l| l.is_up());
    let Some(line) = line else {
        return error_frame(&id, "offline", "That device isn't connected to the hub.");
    };
    match line.dev_call(&from, op, &args, DEV_TIMEOUT).await {
        CallOutcome::Ok(frame) if frame.get("ok") == Some(&Value::Bool(true)) => json!({
            "type": "dev_result",
            "id": id,
            "ok": true,
            "result": frame.get("result").cloned().unwrap_or(Value::Null),
        }),
        CallOutcome::Ok(frame) => {
            let error = match frame.get("error") {
                Some(e) if e.is_object() => e.clone(),
                Some(Value::String(s)) => json!({ "code": "error", "message": s }),
                _ => json!({ "code": "error", "message": "The device refused the call." }),
            };
            json!({ "type": "dev_result", "id": id, "ok": false, "error": error })
        }
        CallOutcome::Err { text, .. } if text == TOO_LARGE => too_large(&id),
        CallOutcome::Err { text, .. } => error_frame(&id, "error", &text),
        CallOutcome::Offline => {
            error_frame(&id, "offline", "That device isn't connected to the hub.")
        }
        CallOutcome::Unknown(_) => error_frame(
            &id,
            "timeout",
            "The device didn't answer in time; the call may or may not have run.",
        ),
    }
}

fn too_large(id: &Value) -> Value {
    error_frame(id, "too_large", "That is too large for the hub to pass on.")
}

/// A `dev_push`: hand it to the target stamped with the sender's device uid, or drop it.
pub fn push(lines: &Lines, client_id: i64, to: &str, sub: Value, seq: Value, event: Value) {
    if !lines.is_peer(client_id) {
        return;
    }
    let (from, target) = {
        let Ok(r) = lines.registry().lock() else {
            return;
        };
        let from = r
            .client_by_id(client_id)
            .ok()
            .flatten()
            .and_then(|s| s.device_uid);
        let target = r
            .client_by_device(to)
            .ok()
            .flatten()
            .filter(|t| t.scopes.has(Scope::Peer));
        (from, target)
    };
    let (Some(from), Some(target)) = (from, target) else {
        return;
    };
    if let Some(line) = lines.get(target.id).filter(|l| l.is_up()) {
        line.send(
            &json!({ "type": "dev_push", "from": from, "sub": sub, "seq": seq, "event": event }),
        );
    }
}
