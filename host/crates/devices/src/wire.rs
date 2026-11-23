//! The device wire: the frames Helms exchange through the hub, and the ops they carry.
//!
//! Every frame is a JSON object with a `type`. The hub's line protocol stays at version 1 and an
//! old peer ignores types it does not know, so nothing here breaks a Helm that predates it.
//!
//! | frame | direction | fields |
//! |---|---|---|
//! | `device` | Helm to hub | `uid, name, run, people?`: sent after `sharing` on every new line and on change; `people` lists every hub person this PC acts for, home first |
//! | `devices` | hub to Helm | `list`: every device of the household, who owns it, who it acts for, whether it is up |
//! | `dev` | Helm to hub | `id, to (uid), op, args, as?`: a request to another device; `as` names the acting person first, then anyone whose chats a read may cover |
//! | `dev_call` | hub to Helm | `id (the hub's), from, op, args`: a request from another device; `from` carries the hub's verified acting person and read set |
//! | `dev_result` | both ways | `id, ok, result` or `error {code, message}`; the hub adds `offline`, `timeout`, `scope` and `too_large` |
//! | `dev_push` | both ways | `to`/`from (uid), sub, seq, event`: news for a subscription |
//!
//! Every frame is at most [`MAX_FRAME_BYTES`]; senders trim what they carry to stay under it.
//! A file bigger than a frame crosses as an ordered run of `attach_put` calls (at most
//! [`MAX_CHUNK_BYTES`] raw bytes each), and the `send` that follows names the upload ids. The
//! other way, a big picture, a project file or a tool's whole output comes back as a run of
//! `blob_read` calls, each from the offset the last one ended at, at most [`MAX_BLOB_BYTES`] in all.

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

    /// The hub's: the sender asked to act for a person it never announced.
    pub fn not_acting() -> Self {
        Self::new(code::NOT_ALLOWED, "This PC does not act for that person.")
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

/// One hub person a PC acts for, with the channels it takes for them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DevicePerson {
    pub person_id: String,
    /// Whether this PC takes that person's Telegram.
    pub telegram: bool,
    /// Whether this PC takes that person's voice.
    pub voice: bool,
}

/// A hub person as the hub names them: the id and the stored name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonRef {
    pub person_id: String,
    pub person: String,
}

/// What a device says about itself on every line-up and on change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFrame {
    pub uid: String,
    pub name: String,
    /// Whether other devices may run chats here.
    pub run: bool,
    /// Every hub person this PC acts for, the home profile's first. Absent (an older Helm)
    /// means "no claims; the paired person only".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<DevicePerson>,
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
    /// Every hub person this device announced acting for, as the hub last heard it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<PersonRef>,
    /// This is the device the list was sent to.
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// Who a request comes from, as the hub stamps it: the acting person in `personId`/`person`,
/// every verified person a read may cover in `people`, and the sender machine's own person in
/// `machine_person_id` (for display only). An older hub sends only the machine's person.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Caller {
    pub uid: String,
    pub name: String,
    pub person_id: Option<String>,
    pub person: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<PersonRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_person_id: Option<String>,
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
        /// The people this call acts for: the acting person first, then anyone whose chats a
        /// read may cover. Empty (an older Helm) means "the paired person, as always".
        #[serde(rename = "as", default, skip_serializing_if = "Vec::is_empty")]
        act_as: Vec<String>,
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

/// A pinned page on the PC that holds it, by the id of its pin there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageIdArgs {
    pub page_id: String,
}

/// One piece of a pinned page's bundle: its document (no `path`) or one file beside it, from
/// `offset`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PagesReadArgs {
    pub page_id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub offset: u64,
}

/// One picture of a message, by the id its row carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentGetArgs {
    pub chat_id: String,
    pub attachment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageArgs {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_message_id: Option<i64>,
    /// Continue toward newer rows from this one: the way back after a jump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_message_id: Option<i64>,
    /// Centre the page on this row: a jump into an older session of the stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub around_message_id: Option<i64>,
    /// Read the whole roll-over stack as one transcript: walks run on across session
    /// boundaries and the answer carries where they meet.
    #[serde(default)]
    pub stack: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Find across a chat's whole roll-over stack, read on the PC that runs the chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StackFindArgs {
    pub chat_id: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_activity: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
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
    /// The files carried with the message. Their bytes came over just before, in `attach_put`
    /// calls under these ids, and wait on this PC for the message that names them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
}

/// One file's place in a send: the id its bytes came under.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRef {
    pub upload_id: String,
}

/// One chunk of a file being carried to a chat on another PC. The first chunk (sequence 0)
/// also names the file. Every frame is at most [`MAX_FRAME_BYTES`], so a large file comes in
/// chunks; the serving PC joins them and keeps the whole bytes under the upload id until a
/// send names it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachPutArgs {
    pub chat_id: String,
    /// A fresh UUID per file, chosen by the sending side.
    pub upload_id: String,
    /// `image` or `file`, named with the first chunk.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime: String,
    /// The chunk's place in the file, from 0; one out of turn is refused.
    #[serde(default)]
    pub seq: u32,
    /// Marks the last chunk. With it the file is complete.
    #[serde(default)]
    pub eof: bool,
    /// Base64 of this chunk's bytes, at most [`MAX_CHUNK_BYTES`] raw.
    #[serde(default)]
    pub data: String,
}

/// Raw bytes a chunk carries. Its base64 plus the rest of the frame stays under
/// [`MAX_FRAME_BYTES`].
pub const MAX_CHUNK_BYTES: usize = 640 * 1024;
/// The most bytes one upload may reach; the chats of the PC that receives answer with their
/// own sentence past this.
pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
/// The most files one message sent from another device may carry.
pub const MAX_SEND_ATTACHMENTS: usize = 20;

/// Stop a chat's turn. With `noResend`, what the agent had not read yet is not sent again, as
/// the stop button's "without resending" does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopArgs {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_resend: bool,
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
    /// A question of several steps: go to that step instead of answering ("Go back a step").
    /// It approves nothing; `value` then only keeps what was typed as a draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goto: Option<u32>,
    /// The words on the button or choice the person pressed; they must be the ones the request
    /// shows.
    pub label: String,
}

/// One gallery an agent showed in a chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GalleryArgs {
    pub chat_id: String,
    pub gallery_id: String,
}

/// The thumbnail of one picture of a gallery or of an option board (`galleryId` is then the
/// board's id). The full picture crosses with `blob_read`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShownImageArgs {
    pub chat_id: String,
    pub gallery_id: String,
    pub index: i64,
}

/// One option board an agent offered in a chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OptionBoardArgs {
    pub chat_id: String,
    pub board_id: String,
}

/// The person's choice on an option board, checked there as a local choice is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OptionBoardAnswerArgs {
    pub chat_id: String,
    pub board_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

/// What `blob_read` reads, exactly one of: a picture sent with a message (`attachment`), a
/// picture of a gallery or option board (`gallery` and `index`), a file of the chat's project
/// folder (`file`, an absolute path there), or a tool's whole output (`output`, the row id).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlobRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gallery: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<i64>,
}

impl BlobRef {
    /// Whether it names exactly one thing, with an index for a gallery and only for one.
    pub fn is_valid(&self) -> bool {
        let named = [
            self.attachment.is_some(),
            self.gallery.is_some(),
            self.file.is_some(),
            self.output.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        named == 1 && self.gallery.is_some() == self.index.is_some()
    }

    /// A file or a tool's output is read from this PC's disk, so it needs the run switch as the
    /// panel's reads do; the pictures are part of the chat and need only the pairing.
    pub fn reads_disk(&self) -> bool {
        self.file.is_some() || self.output.is_some()
    }
}

/// One chunk of something big of a chat, from `offset`: at most [`MAX_CHUNK_BYTES`] raw bytes.
/// Answers `{offset, total, mime, name, data}` (`data` base64); the whole is at most
/// [`MAX_BLOB_BYTES`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlobReadArgs {
    pub chat_id: String,
    #[serde(rename = "ref")]
    pub blob: BlobRef,
    #[serde(default)]
    pub offset: u64,
}

/// The most one `blob_read` transfer may carry in all, on both sides.
pub const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsentDecideArgs {
    pub chat_id: String,
    pub request_id: String,
    /// The button the person pressed: `allow` or `deny`.
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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
    /// Name the chat before its first message; left out, its title comes on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Start it muted, so nothing it does is announced. Left out is the PC's usual new chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkArgs {
    pub chat_id: String,
    /// A fresh UUID per fork: a repeat is answered without forking twice.
    pub fork_id: String,
    /// Fork at that assistant reply ("fork from here"); absent forks the whole chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    /// With `excerpt`, the copy is a side chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollOverArgs {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MuteArgs {
    pub chat_id: String,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrganizeArgs {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_end: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_project_id: Option<String>,
    /// Absent changes nothing; `null` moves the chat to top level; an id nests under it.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_chat_id: Option<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveProfileArgs {
    pub chat_id: String,
    /// One of the profiles this PC listed in its hello.
    pub profile_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetPermissionArgs {
    pub chat_id: String,
    /// The composer's permission pick, as `chat_set_permission_mode` takes it.
    pub mode: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerbositySetArgs {
    pub chat_id: String,
    /// `low`, `medium` or `high`; absent follows this PC's new-chat default again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuickOpenArgs {
    pub chat_id: String,
    pub query: String,
    /// Drop the cached listing first, as the panel's Go-to-file refresh does.
    #[serde(default)]
    pub reload: bool,
}

/// A side-panel read that names one path: a file, a folder listing, an archive listing or a
/// path written in a reply. The serving PC keeps it inside the chat's project folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PanelPathArgs {
    pub chat_id: String,
    pub path: String,
}

/// Change stamps for the files a mirrored chat's viewer has open.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PanelStatArgs {
    pub chat_id: String,
    pub paths: Vec<String>,
}

/// One file's diff on the PC that runs the chat; `side` is `head`, `staged` or `unstaged`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PanelDiffArgs {
    pub chat_id: String,
    pub path: String,
    pub side: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Stage or unstage one file of the chat's repository, on the PC that runs the chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PanelStageArgs {
    pub chat_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolStopArgs {
    pub chat_id: String,
    pub tool_call_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmissionArgs {
    pub chat_id: String,
    pub submission_id: String,
    /// `withdraw` or `recover`.
    pub action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointArgs {
    pub chat_id: String,
    pub message_id: i64,
}

/// Open a sent message for editing, read where the chat runs: the answer is the message's text
/// and the revision a later `resend` must carry back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditOpenArgs {
    pub chat_id: String,
    pub message_id: i64,
}

/// Stage an edit of a sent message and send the new text, where the chat runs. It answers what
/// the chat's own resend answers with; the transcript rewinds there when the worker closes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResendArgs {
    pub chat_id: String,
    pub message_id: i64,
    pub revision: i64,
    pub submission_id: String,
    pub text: String,
}

/// Promote a side chat into an ordinary chat, or discard it for good, where the chat runs.
/// `action` is checked on arrival; discarding one that has sent anything carries `confirmed`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SideResolveArgs {
    pub chat_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatIdsArgs {
    pub chat_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnoozeArgs {
    pub chat_id: String,
    /// Until (Unix ms); absent or null unsnoozes — the same rule as the local op.
    #[serde(default)]
    pub until: Option<i64>,
}

/// One read of the task board of the PC that runs the chat. `params` is the read the Tasks
/// view asks for (action, column, project, cursor, requestId…) without the `chatId`; the
/// serving PC runs it against its own saved board connection, unchanged. Reads only — the
/// Tasks view never writes to the board.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardReadArgs {
    pub chat_id: String,
    #[serde(flatten)]
    pub params: Value,
}

/// Stop one board read still waiting on the PC that runs the chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardCancelArgs {
    pub chat_id: String,
    pub request_id: String,
}

/// The PC that runs a chat's defaults for new chats, as far as the model picker cares: the
/// model, effort and reply length a new chat there starts with. A read answers with these;
/// `defaults_set` accepts only these three, so a pick from afar cannot rewrite the other
/// PC's whole runtime page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefaultsArgs {
    /// The chat the picker was open on: the defaults are the ones on the PC that runs it.
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_verbosity: Option<String>,
}

/// A new project on the PC that serves, made for the person acting. It is a board when that PC
/// has a task board connected and a folder project otherwise, as "New project" is there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectCreateArgs {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// One of the folder projects this PC listed in its hello: where the board's chats start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_project_id: Option<String>,
}

/// A change to one of the serving PC's projects from the sidebar: pin it, or point a board at
/// one of that PC's folder projects. Both are optional; naming neither changes nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSetArgs {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_project_id: Option<String>,
}

/// Forward a port of this PC to the asking one, for a chat of the asking person here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardOpenArgs {
    pub chat_id: String,
    pub port: u16,
}

/// End a forward the asking PC holds from this one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardCloseArgs {
    pub forward_id: String,
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
    /// The roll-over stack of a chat, read on the PC that runs it. A read.
    ChatStack(ChatIdArgs),
    /// Find across a stack's sessions, read there. A read.
    StackFind(StackFindArgs),
    AttachPut(AttachPutArgs),
    Send(SendArgs),
    Stop(StopArgs),
    Answer(AnswerArgs),
    ConsentDecide(ConsentDecideArgs),
    /// Where this install's Allow requests for one chat stand, read only. A tool row's chip
    /// asks for it on a chat that runs here, so a copy can say Allowed or Refused too.
    ConsentStates(ChatIdArgs),
    Create(CreateArgs),
    SetModel(SetModelArgs),
    Rename(RenameArgs),
    Archive(ArchiveArgs),
    Share(ShareArgs),
    Fork(ForkArgs),
    Compact(ChatIdArgs),
    RollOver(RollOverArgs),
    /// What the chat has used, read from the PC that runs it. A read: no switch needed.
    Cost(ChatIdArgs),
    ExportMarkdown(ChatIdArgs),
    Organize(OrganizeArgs),
    MarkUnread(ChatIdArgs),
    Mute(MuteArgs),
    MoveProfile(MoveProfileArgs),
    /// The composer's permission pick, applied on the PC that runs the chat.
    SetPermission(SetPermissionArgs),
    /// The chat's reply-length level, as the model picker shows it. A read.
    VerbosityGet(ChatIdArgs),
    VerbositySet(VerbositySetArgs),
    /// Whether the side panel's context eye is on for the chat there. A read.
    ContextOn(ChatIdArgs),
    /// The chat's project files matching a mention query, read on the PC that runs it. Runs with
    /// the run switch, since it reads this PC's disk.
    QuickOpen(QuickOpenArgs),
    /// The chat's project folder path, as the panel shows it. A read.
    ChatFolder(ChatIdArgs),
    /// A picture sent with a message, read where it is stored: a thumbnail or the full image,
    /// base64 with its type. A read, since the message itself can be read without the switch.
    AttachmentGet(AttachmentGetArgs),
    /// The pages pinned on this PC that the asking person may open. Reads this PC's disk, so
    /// the pages ops run with the switch like the project folder's reads.
    PagesList,
    /// What a pinned page needs to run elsewhere: its name and the files its text names.
    PagesFiles(PageIdArgs),
    /// One chunk of the page's document or of a file beside it.
    PagesRead(PagesReadArgs),
    /// One file or folder listing from the chat's project folder, read on the PC that runs it.
    /// A read; the serving PC keeps the path inside the folder.
    FileView(PanelPathArgs),
    FileStat(PanelStatArgs),
    /// The chat's repository changes, as the panel's Changes view shows them. A read.
    Changes(ChatIdArgs),
    Diff(PanelDiffArgs),
    /// Stage or unstage one file of the chat's repository, on the PC that runs it.
    Stage(PanelStageArgs),
    Unstage(PanelStageArgs),
    /// A ZIP's listing inside the chat's project folder. A read.
    ArchiveList(PanelPathArgs),
    /// A path written in a reply, resolved against the chat's project folder. A read.
    PathResolve(PanelPathArgs),
    /// Whether a task board is saved on the PC that runs the chat. A read.
    BoardConnection(ChatIdArgs),
    /// One read of that PC's task board, for the Tasks view. A read.
    BoardRead(BoardReadArgs),
    /// Stop one board read that view no longer waits for. A read.
    BoardCancel(BoardCancelArgs),
    /// Start loading the chat's model on its own PC (the first keystroke of a local chat).
    Warm(ChatIdArgs),
    ToolStop(ToolStopArgs),
    Submission(SubmissionArgs),
    /// What going back to a message means there, for the confirmation dialog. A read.
    Checkpoint(CheckpointArgs),
    /// A sent message's text, for the edit window. A read: the transcript is mirrored anyway.
    EditOpen(EditOpenArgs),
    /// Replace a sent message and everything after it, on the PC that runs the chat.
    Resend(ResendArgs),
    /// Delete a chat here for good. The chat's owner only; answered behind their confirm.
    Delete(ChatIdArgs),
    /// Promote a side chat here, or discard it for good. The chat's owner only, like a delete.
    SideResolve(SideResolveArgs),
    /// The commands and skills of a chat's worker here, for the `/` menu of its window. A read.
    Skills(ChatIdArgs),
    /// Suggested names for the Rename dialog of a chat here, made by this install's own title
    /// helper from the chat's own words. A read: it writes nothing.
    TitleSuggest(ChatIdArgs),
    /// Needs-you and idle-check flag rows for named chats. A read.
    Attention(ChatIdsArgs),
    /// Board-item links for named chats, as the PC that runs them lists them. A read.
    ItemLinks(ChatIdsArgs),
    Snooze(SnoozeArgs),
    Address(ChatIdArgs),
    /// The PC's new-chat model/effort/reply-length, three fields only. A read.
    DefaultsGet(ChatIdArgs),
    DefaultsSet(DefaultsArgs),
    /// Make a project on this PC (a board, or a folder when no task board is connected).
    ProjectCreate(ProjectCreateArgs),
    /// Pin a project here, or give a board one of this PC's folders to start its chats in.
    ProjectSet(ProjectSetArgs),
    /// A gallery an agent showed in the chat: its title and pictures' names. A read.
    ShownGallery(GalleryArgs),
    /// The thumbnail of one picture of a gallery or option board. A read.
    ShownImage(ShownImageArgs),
    /// An option board and its answer, if any. A read.
    OptionBoard(OptionBoardArgs),
    /// Answer an option board; the first answer stands.
    OptionBoardAnswer(OptionBoardAnswerArgs),
    /// One chunk of a big picture, a project file or a tool's whole output. Pictures are reads;
    /// a file or an output reads this PC's disk and runs with the switch.
    BlobRead(BlobReadArgs),
    /// Forward a port of this PC to the asking PC: `{forwardId}`. Runs with the switch, for a
    /// chat the acting person owns or that is shared with them; its bytes then travel as pushes
    /// (see [`crate::tunnel`]).
    ForwardOpen(ForwardOpenArgs),
    /// End one of the asking PC's forwards. Needs no switch: stopping is always allowed.
    ForwardClose(ForwardCloseArgs),
    /// The MCP tool servers set up on this PC, for Settings → MCP's "Copy from another PC".
    /// Every secret value, saved secret id and credential-file path is blanked on the way out;
    /// environment names, commands and addresses cross as they stand. Answered only to a PC
    /// acting as this PC's own person. A read.
    McpListGet,
}

fn args<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, WireError> {
    // A missing argument object reads as an empty one.
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| WireError::bad_args(e.to_string()))
}

/// Distinguishes "absent" (`None`) from an explicit `null` (`Some(None)`).
fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
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
            "chat_stack" => Op::ChatStack(args(a)?),
            "stack_find" => Op::StackFind(args(a)?),
            "attach_put" => Op::AttachPut(args(a)?),
            "send" => Op::Send(args(a)?),
            "stop" => Op::Stop(args(a)?),
            "answer" => Op::Answer(args(a)?),
            "consent_decide" => Op::ConsentDecide(args(a)?),
            "consent_states" => Op::ConsentStates(args(a)?),
            "create" => Op::Create(args(a)?),
            "set_model" => Op::SetModel(args(a)?),
            "rename" => Op::Rename(args(a)?),
            "archive" => Op::Archive(args(a)?),
            "share" => Op::Share(args(a)?),
            "fork" => Op::Fork(args(a)?),
            "compact" => Op::Compact(args(a)?),
            "roll_over" => Op::RollOver(args(a)?),
            "cost" => Op::Cost(args(a)?),
            "export_markdown" => Op::ExportMarkdown(args(a)?),
            "organize" => Op::Organize(args(a)?),
            "mark_unread" => Op::MarkUnread(args(a)?),
            "mute" => Op::Mute(args(a)?),
            "move_profile" => Op::MoveProfile(args(a)?),
            "set_permission" => Op::SetPermission(args(a)?),
            "verbosity_get" => Op::VerbosityGet(args(a)?),
            "verbosity_set" => Op::VerbositySet(args(a)?),
            "context_on" => Op::ContextOn(args(a)?),
            "quick_open" => Op::QuickOpen(args(a)?),
            "chat_folder" => Op::ChatFolder(args(a)?),
            "attachment_get" => Op::AttachmentGet(args(a)?),
            "pages_list" => {
                args::<NoArgs>(a)?;
                Op::PagesList
            }
            "pages_files" => Op::PagesFiles(args(a)?),
            "pages_read" => Op::PagesRead(args(a)?),
            "file_view" => Op::FileView(args(a)?),
            "file_stat" => Op::FileStat(args(a)?),
            "changes" => Op::Changes(args(a)?),
            "diff" => Op::Diff(args(a)?),
            "stage" => Op::Stage(args(a)?),
            "unstage" => Op::Unstage(args(a)?),
            "archive_list" => Op::ArchiveList(args(a)?),
            "path_resolve" => Op::PathResolve(args(a)?),
            "board_connection" => Op::BoardConnection(args(a)?),
            "board_read" => {
                let p: BoardReadArgs = args(a)?;
                if p.params.get("action").and_then(Value::as_str).is_none() {
                    return Err(WireError::bad_args("A board read names an action."));
                }
                Op::BoardRead(p)
            }
            "board_cancel" => Op::BoardCancel(args(a)?),
            "warm" => Op::Warm(args(a)?),
            "tool_stop" => Op::ToolStop(args(a)?),
            "submission" => Op::Submission(args(a)?),
            "checkpoint" => Op::Checkpoint(args(a)?),
            "edit_open" => Op::EditOpen(args(a)?),
            "resend" => Op::Resend(args(a)?),
            "delete" => Op::Delete(args(a)?),
            "side_resolve" => Op::SideResolve(args(a)?),
            "skills" => Op::Skills(args(a)?),
            "title_suggest" => Op::TitleSuggest(args(a)?),
            "attention" => Op::Attention(args(a)?),
            "item_links" => Op::ItemLinks(args(a)?),
            "snooze" => Op::Snooze(args(a)?),
            "address" => Op::Address(args(a)?),
            "defaults_get" => Op::DefaultsGet(args(a)?),
            "defaults_set" => Op::DefaultsSet(args(a)?),
            "project_create" => Op::ProjectCreate(args(a)?),
            "project_set" => Op::ProjectSet(args(a)?),
            "shown_gallery" => Op::ShownGallery(args(a)?),
            "shown_image" => Op::ShownImage(args(a)?),
            "option_board" => Op::OptionBoard(args(a)?),
            "option_board_answer" => Op::OptionBoardAnswer(args(a)?),
            "blob_read" => {
                let r: BlobReadArgs = args(a)?;
                if !r.blob.is_valid() {
                    return Err(WireError::bad_args(
                        "A read names one attachment, gallery picture, file or output.",
                    ));
                }
                Op::BlobRead(r)
            }
            "forward_open" => Op::ForwardOpen(args(a)?),
            "forward_close" => Op::ForwardClose(args(a)?),
            "mcp_list_get" => {
                args::<NoArgs>(a)?;
                Op::McpListGet
            }
            _ => return Err(WireError::elsewhere()),
        })
    }

    /// Whether the op changes something on the PC that runs the chat, or reads this PC's disk
    /// (the project folder's files and changes). These need the PC's switch "Let other devices
    /// use chats on this PC"; the other reads need only the pairing.
    pub fn is_run(&self) -> bool {
        if let Op::BlobRead(a) = self {
            return a.blob.reads_disk();
        }
        matches!(
            self,
            Op::AttachPut(_)
                | Op::Send(_)
                | Op::Stop(_)
                | Op::Answer(_)
                | Op::ConsentDecide(_)
                | Op::Create(_)
                | Op::SetModel(_)
                | Op::Rename(_)
                | Op::Archive(_)
                | Op::Share(_)
                | Op::Fork(_)
                | Op::Compact(_)
                | Op::RollOver(_)
                | Op::ExportMarkdown(_)
                | Op::Organize(_)
                | Op::MarkUnread(_)
                | Op::Mute(_)
                | Op::MoveProfile(_)
                | Op::SetPermission(_)
                | Op::VerbositySet(_)
                | Op::Stage(_)
                | Op::Unstage(_)
                | Op::Warm(_)
                | Op::ToolStop(_)
                | Op::Submission(_)
                | Op::Resend(_)
                | Op::Delete(_)
                | Op::SideResolve(_)
                | Op::Snooze(_)
                | Op::Address(_)
                | Op::DefaultsSet(_)
                | Op::ProjectCreate(_)
                | Op::ProjectSet(_)
                | Op::QuickOpen(_)
                | Op::PagesList
                | Op::PagesFiles(_)
                | Op::PagesRead(_)
                | Op::FileView(_)
                | Op::FileStat(_)
                | Op::Changes(_)
                | Op::Diff(_)
                | Op::ArchiveList(_)
                | Op::PathResolve(_)
                | Op::OptionBoardAnswer(_)
                | Op::ForwardOpen(_)
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
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub colour: String,
    pub kind: String,
    pub path: String,
    /// A board's folder project: where its new chats start. Empty when it has none yet (and
    /// for a folder project, which is a folder itself).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// One of this device's profiles, as another device lists it. A chat can be labelled with one
/// from afar (`move_profile`); these are the ids it takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInfo {
    pub profile_id: String,
    pub name: String,
    /// The hub person this profile is linked to, when it is linked. A watcher ticks the
    /// profile that owns a chat by matching it against the chat's person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
}

/// The answer to `hello`: who this device is, who uses it, where chats can start and which
/// models they can run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub device: DeviceInfo,
    pub people: Vec<PersonInfo>,
    pub projects: Vec<ProjectInfo>,
    /// This device's profiles. Absent (an older Helm) means the watcher offers no
    /// "Move to profile" for this device's chats.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<ProfileInfo>,
    /// `null` when this device's `model_catalogue` op failed at hello time.
    pub catalogue: Option<Value>,
}

/// A chat as another device sees it: the chat record with the profile replaced by the person it
/// belongs to (the hub's id, or `null` when that profile is not linked to one), `sharedWith` as
/// person ids, and a few fields that spare the watcher a project lookup — its folder's name and
/// colour and, when the chat is shown under a board there, that board's id, name and colour as
/// `boardId`, `boardName` and `boardColour`. A watcher groups by the board when it is set and by
/// the folder when it is not. The draft is the owner's own and is not sent.
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

    /// Say the board this chat is shown under on its home PC. A board groups chats of several
    /// folders; a chat with no board keeps its folder, so the watcher groups by this when set.
    pub fn shown_on_board(mut self, id: &str, name: &str, colour: &str) -> RemoteChat {
        self.0.insert("boardId".into(), json!(id));
        self.0.insert("boardName".into(), json!(name));
        self.0.insert("boardColour".into(), json!(colour));
        self
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The ops this build added parse with their arguments, refuse what they do not take, and
    /// fall on the right side of the run switch.
    #[test]
    fn the_newer_ops_parse_and_split_into_reads_and_runs() {
        let rows: Vec<(&str, Value, Option<bool>)> = vec![
            ("stop", json!({"chatId": "c1"}), Some(true)),
            (
                "stop",
                json!({"chatId": "c1", "noResend": true}),
                Some(true),
            ),
            ("stop", json!({"chatId": "c1", "noResend": "yes"}), None),
            (
                "answer",
                json!({"chatId": "c1", "requestId": "q", "goto": 0, "label": "Back"}),
                Some(true),
            ),
            (
                "shown_gallery",
                json!({"chatId": "c1", "galleryId": "g"}),
                Some(false),
            ),
            (
                "shown_image",
                json!({"chatId": "c1", "galleryId": "g", "index": 2}),
                Some(false),
            ),
            (
                "shown_image",
                json!({"chatId": "c1", "galleryId": "g"}),
                None,
            ),
            (
                "option_board",
                json!({"chatId": "c1", "boardId": "b"}),
                Some(false),
            ),
            (
                "option_board_answer",
                json!({"chatId": "c1", "boardId": "b", "labels": ["A"]}),
                Some(true),
            ),
            (
                "option_board_answer",
                json!({"chatId": "c1", "boardId": "b", "other": "mine", "extra": 1}),
                None,
            ),
            (
                "blob_read",
                json!({"chatId": "c1", "ref": {"attachment": "a"}}),
                Some(false),
            ),
            (
                "blob_read",
                json!({"chatId": "c1", "ref": {"gallery": "g", "index": 0}, "offset": 655360}),
                Some(false),
            ),
            (
                "blob_read",
                json!({"chatId": "c1", "ref": {"file": "/work/a.txt"}}),
                Some(true),
            ),
            (
                "blob_read",
                json!({"chatId": "c1", "ref": {"output": 12}}),
                Some(true),
            ),
            ("blob_read", json!({"chatId": "c1", "ref": {}}), None),
            (
                "blob_read",
                json!({"chatId": "c1", "ref": {"gallery": "g"}}),
                None,
            ),
            (
                "blob_read",
                json!({"chatId": "c1", "ref": {"attachment": "a", "index": 1}}),
                None,
            ),
            (
                "blob_read",
                json!({"chatId": "c1", "ref": {"file": "/a", "output": 1}}),
                None,
            ),
            (
                "blob_read",
                json!({"chatId": "c1", "ref": {"path": "/a"}}),
                None,
            ),
            (
                "forward_open",
                json!({"chatId": "c1", "port": 5173}),
                Some(true),
            ),
            ("forward_open", json!({"chatId": "c1", "port": 70000}), None),
            ("forward_open", json!({"chatId": "c1"}), None),
            ("forward_close", json!({"forwardId": "f1"}), Some(false)),
            ("skills", json!({"chatId": "c1"}), Some(false)),
            (
                "side_resolve",
                json!({"chatId": "c1", "action": "discard"}),
                Some(true),
            ),
            (
                "side_resolve",
                json!({"chatId": "c1", "action": "promote", "confirmed": true}),
                Some(true),
            ),
            ("side_resolve", json!({"chatId": "c1"}), None),
        ];
        for (op, args, run) in rows {
            let got = Op::parse(op, args.clone());
            match run {
                Some(run) => assert_eq!(got.unwrap().is_run(), run, "{op} {args}"),
                None => assert_eq!(got.unwrap_err().code, code::BAD_ARGS, "{op} {args}"),
            }
        }
        // A stop without the flag is the stop an older Helm sends, and carries none on the wire.
        let plain = StopArgs {
            chat_id: "c1".into(),
            no_resend: false,
        };
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            json!({"chatId": "c1"})
        );
    }
}
