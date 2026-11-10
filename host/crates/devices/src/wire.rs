//! The device wire: the frames Helms exchange through the hub, and the ops they carry.
//!
//! Every frame is a JSON object with a `type`. The hub's line protocol stays at version 1 and an
//! old peer ignores types it does not know, so nothing here breaks a Helm that predates it.
//!
//! | frame | direction | fields |
//! |---|---|---|
//! | `device` | Helm to hub | `uid, name, run`: sent after `sharing` on every new line and on change |
//! | `devices` | hub to Helm | `list`: every device of the household, who owns it, whether it is up |
//! | `dev` | Helm to hub | `id, to (uid), op, args`: a request to another device |
//! | `dev_call` | hub to Helm | `id (the hub's), from, op, args`: a request from another device |
//! | `dev_result` | both ways | `id, ok, result` or `error {code, message}`; the hub adds `offline`, `timeout`, `scope` and `too_large` |
//! | `dev_push` | both ways | `to`/`from (uid), sub, seq, event`: news for a subscription |
//!
//! Every frame is at most [`MAX_FRAME_BYTES`]; senders trim what they carry to stay under it.

use helm_chats::types::ChatRecord;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// The most one `dev*` frame may hold.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// The most one message row may weigh on the wire before its big parts are cut.
pub const MAX_ROW_BYTES: usize = 256 * 1024;
/// How much of a tool's output (or any long text) stays in a row that was cut.
pub const TRIMMED_TEXT_BYTES: usize = 64 * 1024;
/// The most text one remote send may carry.
pub const MAX_SEND_BYTES: usize = 16 * 1024;
/// The most rows `chat_sub` carries.
pub const SUB_ROWS: i64 = 500;
/// The most rows one `page` carries.
pub const MAX_PAGE_ROWS: usize = 200;

/// Error codes a device answers with.
pub mod code {
    pub const NOT_ALLOWED: &str = "not_allowed";
    pub const REFUSED: &str = "refused";
    pub const BAD_ARGS: &str = "bad_args";
    pub const NOT_FOUND: &str = "not_found";
    pub const UNKNOWN_PERSON: &str = "unknown_person";
    pub const INTERNAL: &str = "internal";
    /// The hub's: the other device is not up, or this Helm has no line.
    pub const OFFLINE: &str = "offline";
    /// The hub's: no answer in 30 seconds.
    pub const TIMEOUT: &str = "timeout";
    /// The hub's: the asking line may not use the household.
    pub const SCOPE: &str = "scope";
    /// The hub's: the frame was over the size limit.
    pub const TOO_LARGE: &str = "too_large";
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

impl WireError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    pub fn bad_args(message: impl Into<String>) -> Self {
        Self::new(code::BAD_ARGS, message)
    }

    pub fn not_found() -> Self {
        Self::new(code::NOT_FOUND, "That chat is not on this device.")
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(code::INTERNAL, message)
    }

    pub fn not_allowed() -> Self {
        Self::new(
            code::NOT_ALLOWED,
            "This PC doesn't let other devices run its chats.",
        )
    }

    /// Anything a watcher may not do from afar.
    pub fn elsewhere() -> Self {
        Self::new(code::REFUSED, "Do that on the PC this chat runs on.")
    }

    pub fn offline() -> Self {
        Self::new(code::OFFLINE, "That device is offline.")
    }
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// What a device says about itself on every line-up and on change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceFrame {
    pub uid: String,
    pub name: String,
    /// Whether other devices may run chats here.
    pub run: bool,
}

/// One device of the household as the hub lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerDevice {
    pub uid: String,
    pub name: String,
    pub person_id: Option<String>,
    pub person: Option<String>,
    pub online: bool,
    /// When the device was last up (ISO 8601), or `None` if it never has been.
    pub last_seen_at: Option<String>,
    pub run: bool,
    /// This is the device the list was sent to.
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// Who a request comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Caller {
    pub uid: String,
    pub name: String,
    pub person_id: Option<String>,
    pub person: Option<String>,
}

/// News for a subscription: `seq` counts from 1 within it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Push {
    /// Helm to hub: the device the news is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Hub to Helm: the device the news is from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub sub: String,
    pub seq: u64,
    pub event: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Device(DeviceFrame),
    Devices {
        list: Vec<PeerDevice>,
    },
    Dev {
        id: String,
        to: String,
        op: String,
        #[serde(default)]
        args: Value,
    },
    DevCall {
        id: String,
        from: Caller,
        op: String,
        #[serde(default)]
        args: Value,
    },
    DevResult {
        id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<WireError>,
    },
    DevPush(Push),
}

impl Frame {
    /// The frame a JSON value holds, or `None` for a type this build does not know.
    pub fn from_value(v: Value) -> Option<Frame> {
        serde_json::from_value(v).ok()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("frames serialise")
    }

    /// The serialised size, for the limit.
    pub fn size(&self) -> usize {
        self.to_value().to_string().len()
    }

    pub fn result(id: &str, r: Result<Value, WireError>) -> Frame {
        match r {
            Ok(v) => Frame::DevResult {
                id: id.to_owned(),
                ok: true,
                result: Some(v),
                error: None,
            },
            Err(e) => Frame::DevResult {
                id: id.to_owned(),
                ok: false,
                result: None,
                error: Some(e),
            },
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Ops
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatsSubArgs {
    /// The cursor of the last list this watcher holds; 0 for all of it.
    #[serde(default)]
    pub since: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSubArgs {
    pub chat_id: String,
    /// The newest row revision the watcher holds for the chat; 0 for none.
    #[serde(default)]
    pub since: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatIdArgs {
    pub chat_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageArgs {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_message_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendMode {
    Steer,
    Queue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendArgs {
    pub chat_id: String,
    /// A fresh UUID per send: a repeat is answered without sending twice.
    pub client_id: String,
    pub text: String,
    pub mode: SendMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnswerArgs {
    pub chat_id: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
    /// The words on the button or choice the person pressed; they must be the ones the request
    /// shows.
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateArgs {
    pub person_id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetModelArgs {
    pub chat_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameArgs {
    pub chat_id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveArgs {
    pub chat_id: String,
    pub archived: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShareArgs {
    pub chat_id: String,
    pub person_id: String,
    pub shared: bool,
}

/// An op a device carries for another, parsed.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Hello,
    ChatsSub(ChatsSubArgs),
    ChatSub(ChatSubArgs),
    ChatUnsub(ChatIdArgs),
    ChatsUnsub,
    Page(PageArgs),
    Send(SendArgs),
    Stop(ChatIdArgs),
    Answer(AnswerArgs),
    Create(CreateArgs),
    SetModel(SetModelArgs),
    Rename(RenameArgs),
    Archive(ArchiveArgs),
    Share(ShareArgs),
}

fn args<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, WireError> {
    // A missing argument object reads as an empty one.
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| WireError::bad_args(e.to_string()))
}

impl Op {
    /// Parse `op` and its arguments. An op this build does not carry is refused with the same
    /// sentence as anything a watcher may not do.
    pub fn parse(op: &str, a: Value) -> Result<Op, WireError> {
        Ok(match op {
            "hello" => {
                args::<NoArgs>(a)?;
                Op::Hello
            }
            "chats_sub" => Op::ChatsSub(args(a)?),
            "chat_sub" => Op::ChatSub(args(a)?),
            "chat_unsub" => Op::ChatUnsub(args(a)?),
            "chats_unsub" => {
                args::<NoArgs>(a)?;
                Op::ChatsUnsub
            }
            "page" => Op::Page(args(a)?),
            "send" => Op::Send(args(a)?),
            "stop" => Op::Stop(args(a)?),
            "answer" => Op::Answer(args(a)?),
            "create" => Op::Create(args(a)?),
            "set_model" => Op::SetModel(args(a)?),
            "rename" => Op::Rename(args(a)?),
            "archive" => Op::Archive(args(a)?),
            "share" => Op::Share(args(a)?),
            _ => return Err(WireError::elsewhere()),
        })
    }

    /// Whether the op changes something on the PC that runs the chat. These need the PC's
    /// switch "Let other devices use chats on this PC"; reads need only the pairing.
    pub fn is_run(&self) -> bool {
        matches!(
            self,
            Op::Send(_)
                | Op::Stop(_)
                | Op::Answer(_)
                | Op::Create(_)
                | Op::SetModel(_)
                | Op::Rename(_)
                | Op::Archive(_)
                | Op::Share(_)
        )
    }
}

// ---------------------------------------------------------------------------------------------
// What the ops answer with
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub uid: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonInfo {
    pub person_id: String,
    pub name: String,
    pub colour: String,
    pub about: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub colour: String,
    pub kind: String,
    pub path: String,
}

/// The answer to `hello`: who this device is, who uses it, where chats can start and which
/// models they can run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub device: DeviceInfo,
    pub people: Vec<PersonInfo>,
    pub projects: Vec<ProjectInfo>,
    pub catalogue: Value,
}

/// A chat as another device sees it: the chat record with the profile replaced by the person it
/// belongs to (the hub's id, or `null` when that profile is not linked to one), `sharedWith` as
/// person ids, and a few fields that spare the watcher a project lookup. The draft is the
/// owner's own and is not sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoteChat(pub Map<String, Value>);

impl RemoteChat {
    pub fn from_record(
        record: &ChatRecord,
        person_id: Option<String>,
        shared_with: Vec<String>,
        project_name: &str,
        project_colour: &str,
        needs_attention: bool,
    ) -> RemoteChat {
        let mut m = match serde_json::to_value(record) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        m.remove("profileId");
        m.insert("draft".into(), json!(""));
        m.insert("draftRevision".into(), json!(0));
        if let Some(Value::String(e)) = m.get("error") {
            if e.len() > 500 {
                let cut: String = e.chars().take(500).collect();
                m.insert("error".into(), json!(cut));
            }
        }
        m.insert("personId".into(), json!(person_id));
        m.insert("sharedWith".into(), json!(shared_with));
        m.insert("projectName".into(), json!(project_name));
        m.insert("projectColour".into(), json!(project_colour));
        m.insert("needsAttention".into(), json!(needs_attention));
        RemoteChat(m)
    }

    pub fn id(&self) -> &str {
        self.0.get("id").and_then(Value::as_str).unwrap_or_default()
    }

    pub fn revision(&self) -> i64 {
        self.0.get("revision").and_then(Value::as_i64).unwrap_or(0)
    }

    pub fn to_value(&self) -> Value {
        Value::Object(self.0.clone())
    }
}

/// Seconds-resolution ISO 8601 UTC text for a Unix time in milliseconds.
pub fn iso_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Days since 1970-01-01 to a civil date.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
