//! The household view: one person's Helm asking about another person's Helm.
//!
//! The hub holds no chats. A Helm asks over its own line with a `peer` frame; the hub answers it
//! on the same line with a `peer_result` frame. For `list_chats`, `read_chat` and `send_message`
//! the hub passes the question to the target machine as a `peer_call` frame and hands back that
//! Helm's own answer unchanged:
//!
//! | op | needs the target to share | args passed on |
//! |---|---|---|
//! | `people` | nothing: everyone is listed | (answered by the hub) |
//! | `list_chats` | its chat list | `limit`, `offset` |
//! | `read_chat` | its chat list and transcripts | `chatId`, `beforeMessageId`, `limit` |
//! | `send_message` | transcripts, and "let the household message my chats" | `chatId`, `message` |
//!
//! Every op names a person (`personId`) and may name one of their machines (`machineId`); with no
//! machine, the person's most recently active machine that allows the op is used. The switches come
//! from each line's last `sharing` frame, all off until Helm sends one. A person's own other
//! machines need no switch: the gate applies to other people only. The asker's token needs the
//! `peer` scope. A refusal is always a plain sentence, never silence. Helm checks the switches
//! again on its own side.

use crate::lines::Lines;
use crate::registry::{ClientRow, Registry};
use crate::relay::{CallOutcome, Sharing};
use serde_json::{json, Map, Value};
use std::sync::Mutex;
use std::time::Duration;

/// A peer call waits this long for the other Helm, so one slow machine does not stall a list.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// Who is asking: the machine whose line the `peer` frame arrived on.
#[derive(Debug, Clone)]
pub struct Asker {
    pub person_id: i64,
    pub person: String,
    /// Whether the asker's token carries the `peer` scope.
    pub allowed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    People,
    ListChats,
    ReadChat,
    SendMessage,
}

impl Op {
    pub fn parse(name: &str) -> Option<Op> {
        match name {
            "people" => Some(Op::People),
            "list_chats" => Some(Op::ListChats),
            "read_chat" => Some(Op::ReadChat),
            "send_message" => Some(Op::SendMessage),
            _ => None,
        }
    }

    /// The `op` the target's Helm is asked, and the arguments it is given.
    fn keys(self) -> &'static [&'static str] {
        match self {
            Op::People => &[],
            Op::ListChats => &["limit", "offset"],
            Op::ReadChat => &["chatId", "beforeMessageId", "limit"],
            Op::SendMessage => &["chatId", "message"],
        }
    }

    fn required(self) -> &'static [&'static str] {
        match self {
            Op::ReadChat => &["chatId"],
            Op::SendMessage => &["chatId", "message"],
            _ => &[],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Op::People => "people",
            Op::ListChats => "list_chats",
            Op::ReadChat => "read_chat",
            Op::SendMessage => "send_message",
        }
    }

    /// Whether another person's machine, sharing as `s`, allows this.
    pub fn allowed_by(self, s: Sharing) -> bool {
        match self {
            Op::People => true,
            Op::ListChats => s.chats,
            Op::ReadChat => s.chats && s.transcripts,
            Op::SendMessage => s.transcripts && s.messages,
        }
    }

    /// The sentence for a person whose machine does not allow this.
    fn refusal(self, who: &str) -> String {
        match self {
            Op::People => String::new(),
            Op::ListChats => format!("{who} has not shared their chats with the household."),
            Op::ReadChat => format!("{who} has not shared their transcripts with the household."),
            Op::SendMessage => format!(
                "{who} does not let the household message their chats (it needs transcripts \
                 shared and \"household members can message my chats\" on)."
            ),
        }
    }
}

fn machine_json(m: &ClientRow, lines: &Lines) -> Value {
    let line = lines.get(m.id).filter(|l| l.is_up());
    json!({
        "id": m.id,
        "name": m.machine,
        "online": line.is_some(),
        "lastSeen": m.last_seen_at,
        "sharing": line.map(|l| l.sharing()).unwrap_or_default(),
    })
}

/// Everyone in the household with their machines, online or not.
fn people(registry: &Mutex<Registry>, lines: &Lines, asker: &Asker) -> Result<Value, String> {
    let (people, machines) = {
        let r = registry
            .lock()
            .map_err(|_| "The hub's registry is unavailable.".to_owned())?;
        (
            r.people().map_err(|e| e.to_string())?,
            r.relay_machines().map_err(|e| e.to_string())?,
        )
    };
    let list: Vec<Value> = people
        .iter()
        .map(|p| {
            let machines: Vec<Value> = machines
                .iter()
                .filter(|m| m.person_id == p.id)
                .map(|m| machine_json(m, lines))
                .collect();
            json!({ "id": p.id, "name": p.name, "you": p.id == asker.person_id, "machines": machines })
        })
        .collect();
    Ok(json!({ "people": list }))
}

/// An id sent as a number or as a string of digits.
fn id_of(args: &Value, key: &str) -> Option<i64> {
    match args.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Answer one question: the result to send back, or the sentence that says why not.
pub async fn answer(
    registry: &Mutex<Registry>,
    lines: &Lines,
    asker: &Asker,
    op: &str,
    args: &Value,
) -> Result<Value, String> {
    if !asker.allowed {
        return Err("This machine's token cannot use the household.".into());
    }
    let op = Op::parse(op).ok_or_else(|| format!("{op:?} is not a household question."))?;
    if op == Op::People {
        return people(registry, lines, asker);
    }
    let person_id = id_of(args, "personId").ok_or_else(|| "personId is required.".to_owned())?;
    let machine_id = id_of(args, "machineId");
    let (person, machines) = {
        let r = registry
            .lock()
            .map_err(|_| "The hub's registry is unavailable.".to_owned())?;
        let person = r
            .person_by_id(person_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "There is no such person.".to_owned())?;
        let machines: Vec<ClientRow> = r
            .relay_machines()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|m| m.person_id == person.id)
            .collect();
        (person, machines)
    };
    let own = person.id == asker.person_id;
    // The machines asked about: the one named, or all of the person's.
    let candidates: Vec<&ClientRow> = match machine_id {
        Some(id) => vec![machines
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| format!("{} has no such computer.", person.name))?],
        None => machines.iter().collect(),
    };
    let online: Vec<_> = candidates
        .iter()
        .filter_map(|m| lines.get(m.id).filter(|l| l.is_up()).map(|l| (*m, l)))
        .collect();
    if online.is_empty() {
        return Err(match candidates.as_slice() {
            [m] => format!("{} is offline.", m.machine),
            _ => format!("{} has no computer online.", person.name),
        });
    }
    let allowed: Vec<_> = online
        .into_iter()
        .filter(|(_, l)| own || op.allowed_by(l.sharing()))
        .collect();
    // The most recently active machine that allows it.
    let Some((_, line)) = allowed.into_iter().max_by_key(|(_, l)| l.activity()) else {
        return Err(op.refusal(&person.name));
    };

    let mut passed = Map::new();
    for key in op.keys() {
        if let Some(v) = args.get(key).filter(|v| !v.is_null()) {
            passed.insert((*key).to_owned(), v.clone());
        }
    }
    if let Some(missing) = op.required().iter().find(|k| !passed.contains_key(**k)) {
        return Err(format!("{missing} is required."));
    }
    match line
        .peer_call(
            &asker.person,
            op.name(),
            &Value::Object(passed),
            PEER_TIMEOUT,
        )
        .await
    {
        CallOutcome::Ok(v) => Ok(v),
        CallOutcome::Err { text, .. } => Err(text),
        CallOutcome::Offline => Err(format!("{} went offline.", person.name)),
        CallOutcome::Unknown(text) => Err(text),
    }
}

/// The `peer_result` frame for a question's answer.
pub fn result_frame(id: &Value, answer: Result<Value, String>) -> Value {
    match answer {
        Ok(result) => json!({ "type": "peer_result", "id": id, "ok": true, "result": result }),
        Err(error) => json!({ "type": "peer_result", "id": id, "ok": false, "error": error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_need_the_switches_they_name() {
        let s = |chats, transcripts, messages| Sharing {
            chats,
            transcripts,
            messages,
        };
        // (sharing, list, read, send)
        for (sharing, list, read, send) in [
            (s(false, false, false), false, false, false),
            (s(true, false, false), true, false, false),
            (s(true, true, false), true, true, false),
            (s(true, true, true), true, true, true),
            // Messaging alone is nothing without transcripts.
            (s(false, false, true), false, false, false),
        ] {
            assert_eq!(Op::ListChats.allowed_by(sharing), list, "{sharing:?}");
            assert_eq!(Op::ReadChat.allowed_by(sharing), read, "{sharing:?}");
            assert_eq!(Op::SendMessage.allowed_by(sharing), send, "{sharing:?}");
        }
        assert_eq!(Op::parse("read_chat"), Some(Op::ReadChat));
        assert_eq!(Op::parse("app_stop_chat"), None);
        assert_eq!(id_of(&json!({"a": "7", "b": 8, "c": []}), "a"), Some(7));
        assert_eq!(id_of(&json!({"b": 8}), "b"), Some(8));
        assert_eq!(id_of(&json!({"c": []}), "c"), None);
    }
}
