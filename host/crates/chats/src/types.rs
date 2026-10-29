//! The record shapes the renderer sees. Every type here derives `TS` and is
//! exported to `ui/src/api/generated/` by [`crate::bindings`]; the UI imports them from there and
//! never hand-writes a copy.
//!
//! Integer revisions, sequences, positions and Unix-millisecond timestamps are `i64` in Rust and
//! `number` on the wire (they stay below 2^53, see `helm_core::revisions::MAX_REVISION`), so each
//! one carries `#[ts(type = "number")]`; without it ts-rs would emit `bigint`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// Chat status. A clean settle after a user Stop is `stopped`, any other abnormal
/// end is `interrupted`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ChatStatus {
    Idle,
    Starting,
    Running,
    Stopping,
    Stopped,
    Interrupted,
}

impl ChatStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Interrupted => "interrupted",
        }
    }

    /// The stored text, leniently: anything unknown reads as `interrupted` (the safe state to show).
    pub fn parse(s: &str) -> Self {
        match s {
            "idle" => Self::Idle,
            "starting" => Self::Starting,
            "running" => Self::Running,
            "stopping" => Self::Stopping,
            "stopped" => Self::Stopped,
            _ => Self::Interrupted,
        }
    }

    /// A worker is (or is about to be) running a turn.
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }
}

/// Only Claude Code for now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum Harness {
    Claude,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatRecord {
    pub id: String,
    pub project_id: String,
    pub cwd: String,
    pub title: String,
    pub status: ChatStatus,
    pub error: Option<String>,
    pub harness: Harness,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub draft: String,
    #[ts(type = "number")]
    pub draft_revision: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    #[ts(type = "number")]
    pub revision: i64,
    pub pinned: bool,
    pub archived: bool,
    /// The chat this one is nested under, or null.
    pub parent_chat_id: Option<String>,
    /// Non-null for an unpromoted side chat: the chat it was opened from. Side chats are
    /// hidden from `chat_list` until promoted.
    pub side_source_id: Option<String>,
    pub title_state: TitleState,
    pub unread: UnreadState,
    /// Muted chats stay out of the badge, needs-you, toasts and the tray.
    pub muted: bool,
    /// The last worker output or chat change, Unix ms.
    #[ts(type = "number")]
    pub last_activity_at: i64,
    /// A running chat with no worker output for `stallMinutes` (default 5). Stop still works; the
    /// flag clears on the next output or when the turn ends.
    pub stalled: bool,
}

/// Where the chat's title came from. A manual rename always wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum TitleState {
    Fallback,
    Generated,
    Manual,
}

impl TitleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fallback => "fallback",
            Self::Generated => "generated",
            Self::Manual => "manual",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "generated" => Self::Generated,
            "manual" => Self::Manual,
            _ => Self::Fallback,
        }
    }
}

/// The chat's unread marks: a manual mark's token, and the id of an unread
/// completion. `unread_clear` and `completion_ack` take these back, so a stale read never clears
/// a newer mark.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UnreadState {
    pub mark_token: Option<String>,
    pub completion_id: Option<String>,
}

/// A finished turn, not yet seen. At most one per chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub id: String,
    pub chat_id: String,
    pub title: String,
    pub project: String,
    /// The reply's opening, at most 240 characters.
    pub preview: String,
    #[ts(type = "number")]
    pub completed_at: i64,
    pub read: bool,
    pub dismissed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AttentionKind {
    Approval,
    Question,
    Plan,
    Running,
    Unread,
    Stalled,
    /// The idle check thinks the chat still needs you.
    Triage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttentionRow {
    pub chat_id: String,
    pub title: String,
    pub project: String,
    pub kind: AttentionKind,
    /// Why the chat is listed, in plain words.
    pub reason: String,
}

/// The one attention summary: the badge count (each chat once), the
/// busy count, and at most 8 rows per group. Published as an `attention` event on every
/// change and read by `attention_get`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttentionSummary {
    pub badge: u32,
    pub busy: u32,
    pub needs_you: Vec<AttentionRow>,
    pub running: Vec<AttentionRow>,
    pub unread: Vec<AttentionRow>,
    /// Running chats with no output for the stall limit; they count in the badge.
    pub stalled: Vec<AttentionRow>,
    /// Quiet chats the idle check thinks still need you; the reason is the check's own.
    pub flagged: Vec<AttentionRow>,
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LiveTool {
    pub tool_call_id: String,
    pub name: String,
    #[ts(type = "number")]
    pub started_at: i64,
    pub stoppable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundTaskKind {
    Shell,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTask {
    pub id: String,
    pub title: String,
    pub command: Option<String>,
    pub kind: BackgroundTaskKind,
    #[ts(type = "number")]
    pub started_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Compacting {
    #[ts(type = "number")]
    pub started_at: i64,
    pub retrying: bool,
}

/// What the running worker can do. The UI hides a control whose capability is false.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub withdraw: bool,
    pub deliver_now: bool,
    pub stop_tool: bool,
    pub compact: bool,
    /// `tool_stop` with a subagent's Agent call id stops that one agent. Absent
    /// until the worker reports it, so the per-agent Stop stays hidden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_subagent: Option<bool>,
}

/// A native subagent's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SubagentStatus {
    #[default]
    Running,
    Completed,
    Failed,
    Stopped,
}

/// One subagent, keyed by the parent's Agent tool call. The live list is on [`ThreadState`] and
/// is never written per step; the final state is kept on the call's tool row
/// ([`MessageDetails::subagent`]), so it survives a restart.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct Subagent {
    pub tool_call_id: String,
    /// The agent definition it runs as (`general-purpose`, `reviewer`, ...).
    pub agent_type: String,
    pub name: String,
    pub description: String,
    /// The assignment prompt's first lines.
    pub assignment: String,
    pub status: SubagentStatus,
    /// The tool of the newest step; absent when the newest step is narration.
    pub step_tool: Option<String>,
    /// The newest step: the tool's target, or the first line of the agent's narration.
    pub step: Option<String>,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub ended_at: Option<i64>,
    #[ts(type = "number | null")]
    pub duration_ms: Option<i64>,
    #[ts(type = "number | null")]
    pub tokens: Option<i64>,
    #[ts(type = "number | null")]
    pub tool_uses: Option<i64>,
    pub error: Option<String>,
    /// The final report: at most 2 KiB on the live thread, 16 KiB on the tool row.
    pub report: Option<String>,
    pub report_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum InteractionMethod {
    Select,
    Confirm,
    Input,
    Editor,
    Dismiss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum InteractionAttention {
    Approval,
    Question,
    Plan,
    Trust,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InteractionContext {
    pub label: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub markdown: Option<bool>,
}

/// One pending interaction: the worker's `extension_ui_request` (as
/// `worker/PROTOCOL-NOTES.md` describes it) plus `chatId`. `id` is the request id answered through
/// `interaction_answer`. `step` is 0-based.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Interaction {
    pub chat_id: String,
    pub id: String,
    pub method: InteractionMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attention: Option<InteractionAttention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context: Option<Vec<InteractionContext>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub option_descriptions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub allow_other: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub secret: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefill: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SendMode {
    Asap,
    Interrupt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum LongCommands {
    Off,
    Warn,
    Redirect,
}

/// The resolved send behaviour (settings-schema `sendBehavior`): always the full
/// object, with the defaults ([`crate::send_behavior::HOME_DEFAULTS`]) filled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SendBehavior {
    pub mode: SendMode,
    pub show_choices: bool,
    pub stop_sends_waiting: bool,
    pub wait_limit_seconds: Option<u32>,
    pub tool_stop: bool,
    pub long_commands: LongCommands,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadState {
    pub chat_id: String,
    pub status: ChatStatus,
    pub permission_mode: Option<String>,
    pub interactions: Vec<Interaction>,
    pub live_tools: Vec<LiveTool>,
    pub background_tasks: Vec<BackgroundTask>,
    /// This worker's subagents, newest last, at most 30.
    pub subagents: Vec<Subagent>,
    /// How many of `subagents` are running: the sidebar's "working" signal for a chat that is
    /// otherwise idle.
    pub subagents_running: u32,
    pub compacting: Option<Compacting>,
    /// A chat on a local model: what Helm is doing before the model's first words, until they
    /// arrive ("Loading <model>…"). Absent for a Claude chat and once the reply has started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub loading: Option<LocalLoading>,
    pub capabilities: Capabilities,
    pub send_behavior: SendBehavior,
    #[ts(type = "number")]
    pub revision: i64,
}

/// The line shown while a local model is made ready for a turn, with when it began.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LocalLoading {
    /// For example "Loading qwen3:8b…".
    pub text: String,
    #[ts(type = "number")]
    pub started_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum MessageRole {
    User,
    Assistant,
    ToolResult,
    Context,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum MessageStatus {
    Saved,
    Streaming,
    Complete,
    Error,
    Interrupted,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub cache_read_tokens: i64,
    #[ts(type = "number")]
    pub cache_write_tokens: i64,
    pub cost_usd: f64,
}

/// A tool call made by an assistant row. `args` is the call's JSON, cut to 4 KiB on a whole
/// character, with `truncated` set when it was cut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallSummary {
    pub id: String,
    pub name: String,
    pub args: String,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum RetryState {
    Retrying,
    Waiting,
    Recovered,
    Retried,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RetryInfo {
    pub state: RetryState,
    pub attempt: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ContextKind {
    Compaction,
    ModelChange,
    EffortChange,
    Notice,
    Restart,
    /// The boundary a fork's history ends with ("Forked from <title>").
    Fork,
    /// The marker an edit leaves where the new branch starts.
    Edit,
    /// A change to the chat's Orchestrator state ("You turned Orchestrator on...").
    Orchestrator,
    /// Something that happened on Telegram outside this chat's turns: an alert another chat
    /// sent the user, or an approval answered from the phone.
    Telegram,
    /// Something AFK did with an ask while the user was away (an automatic answer, or an ask it
    /// left waiting for them).
    Afk,
}

/// The `details` bag of a message row. Every field is optional and absent when unset.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MessageDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tools: Option<Vec<ToolCallSummary>>,
    /// For a `toolResult` row: the call's arguments, bounded like [`ToolCallSummary::args`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_args: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<SubmissionMode>,
    /// For a user row whose message was not typed in Helm's composer: where it came from (a
    /// Telegram message, a schedule, another chat...). Absent for typed messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub origin: Option<SubmissionOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retry: Option<RetryInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_kind: Option<ContextKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stoppable: Option<bool>,
    /// For a tool row: which server the tool came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_source: Option<ToolSource>,
    /// For a user row: the images sent with the message, shown as thumbnails in the bubble. The
    /// bytes are not in the row; `attachment_get` reads them (see `message_images`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub images: Option<Vec<MessageImage>>,
    /// For a user row: the files, folders and chats attached by reference, shown as chips in the
    /// bubble.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refs: Option<Vec<MessageRef>>,
    /// For an Agent tool row: the subagent's final state and report, written when it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub subagent: Option<Box<Subagent>>,
}

/// One image sent with a user message: what the bubble needs to ask for it and lay it out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MessageImage {
    /// The attachment id (`attachment_get` takes it).
    pub id: String,
    pub name: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

/// One file, folder or chat attached to a user message by reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MessageRef {
    /// `file`, `folder` or `chat`.
    pub kind: String,
    /// The file or folder name, or the chat's title when it was attached.
    pub name: String,
    /// The absolute path, or the referenced chat's id.
    pub target: String,
}

/// Which server a tool came from: Helm's own host tools, an MCP server, or Claude Code itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolSource {
    /// `host`, `mcp` or `native`.
    pub kind: String,
    pub server: String,
    pub tool: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MessageRow {
    #[ts(type = "number")]
    pub id: i64,
    pub chat_id: String,
    pub source_key: String,
    pub role: MessageRole,
    pub text: String,
    pub thinking: String,
    /// About the last 2,400 characters of thinking when the row's thinking was cut.
    pub thinking_tail: Option<String>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub status: MessageStatus,
    #[ts(type = "number")]
    pub timestamp: i64,
    #[ts(type = "number")]
    pub sequence: i64,
    #[ts(type = "number")]
    pub revision: i64,
    /// File name of the content-addressed artifact holding the full record, when it was too big.
    pub artifact: Option<String>,
    pub details: MessageDetails,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SubmissionMode {
    Prompt,
    Steer,
    FollowUp,
}

/// Submission states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SubmissionState {
    Saved,
    Queued,
    Dispatching,
    Accepted,
    Delivered,
    Complete,
    Rejected,
    Uncertain,
    Withdrawn,
    Superseded,
}

/// Who a submission came from. Only `typed` is the user
/// typing in the composer. Button and notification sends are still the user's own sends (they
/// follow `sendBehavior`), but they never count as typed consent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionOrigin {
    Typed,
    Action,
    Notification,
    /// An answer to a question card, sent as a message.
    Answer,
    /// The app itself (the restart continuation).
    App,
    OtherChat,
    Schedule,
    Telegram,
}

impl SubmissionOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Action => "action",
            Self::Notification => "notification",
            Self::Answer => "answer",
            Self::App => "app",
            Self::OtherChat => "other_chat",
            Self::Schedule => "schedule",
            Self::Telegram => "telegram",
        }
    }

    /// The stored text. Older rows say `user` (typed) or `harness` (the app). Anything unknown
    /// reads as `app`, which is never treated as the user's.
    pub fn parse(s: &str) -> Self {
        match s {
            "typed" | "user" => Self::Typed,
            "action" => Self::Action,
            "notification" => Self::Notification,
            "answer" => Self::Answer,
            "other_chat" => Self::OtherChat,
            "schedule" => Self::Schedule,
            "telegram" => Self::Telegram,
            _ => Self::App,
        }
    }

    /// The user's own send from this window: it follows the send mode and the wait limit.
    pub fn is_users_own(self) -> bool {
        matches!(
            self,
            Self::Typed | Self::Action | Self::Notification | Self::Answer
        )
    }
}

impl From<SendOrigin> for SubmissionOrigin {
    fn from(o: SendOrigin) -> Self {
        match o {
            SendOrigin::Composer => Self::Typed,
            SendOrigin::Action => Self::Action,
            SendOrigin::Notification => Self::Notification,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    pub id: String,
    pub chat_id: String,
    pub text: String,
    pub mode: SubmissionMode,
    pub state: SubmissionState,
    pub origin: SubmissionOrigin,
    /// Queue position for a queued follow-up; `null` otherwise.
    #[ts(type = "number | null")]
    pub position: Option<i64>,
    #[ts(type = "number")]
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: String,
    pub status: String,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub ended_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPage {
    pub chat_id: String,
    /// At most 100, oldest first.
    pub messages: Vec<MessageRow>,
    pub has_older: bool,
    pub has_newer: bool,
    pub older_cursor: Option<String>,
    pub newer_cursor: Option<String>,
    pub runs: Vec<RunSummary>,
    /// At most 100 submissions not yet delivered, in queue order.
    pub pending: Vec<Submission>,
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum CatalogueSource {
    Cli,
    Cache,
    Fallback,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogue {
    pub harness: Harness,
    pub models: Vec<ModelInfo>,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub source: CatalogueSource,
    /// Models on the local model servers configured in Helm, listed after the Claude models.
    #[serde(default)]
    pub local: Vec<LocalModelInfo>,
}

/// Which kind of local model server a model lives on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum LocalServerKind {
    Strata,
    Ollama,
}

/// One model on a local model server. Its `id` is `local/<serverId>/<model>`: the model part may
/// itself hold `/` and `:`, so split on the first two `/` only. A chat on it stores the server id
/// in `provider` and the model part in `model`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelInfo {
    pub id: String,
    pub name: String,
    /// The server's display name.
    pub server: String,
    pub kind: LocalServerKind,
    #[ts(optional, type = "number")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[ts(optional, type = "number")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded: Option<bool>,
    /// False when the server did not answer; the picker greys the row.
    pub reachable: bool,
}

// ---------------------------------------------------------------------------------------------
// Op arguments and results. Arguments are deserialised strictly: an unknown field is
// a `bad_args` refusal, so a renamed field shows up at once instead of being ignored.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatCreateArgs {
    pub cwd: String,
    /// The folder project to put the chat in; it must be a folder project over `cwd`. Without it
    /// the folder's project is found or made.
    #[serde(default)]
    #[ts(optional)]
    pub project_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatIdArgs {
    pub chat_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatGetResult {
    pub chat: ChatRecord,
    pub thread: ThreadState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptPageArgs {
    pub chat_id: String,
    /// An `olderCursor` from an earlier page.
    #[serde(default)]
    #[ts(optional)]
    pub before: Option<String>,
    /// A `newerCursor` from an earlier page.
    #[serde(default)]
    #[ts(optional)]
    pub after: Option<String>,
    /// A message id to centre the page on.
    #[serde(default)]
    #[ts(type = "number")]
    #[ts(optional)]
    pub around: Option<i64>,
    /// Include the cursor's own row (for jumps).
    #[serde(default)]
    #[ts(optional)]
    pub inclusive: Option<bool>,
}

/// Find in chat over the whole stored history (`chat_find`): one scan step of the active branch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatFindArgs {
    pub chat_id: String,
    pub query: String,
    /// Also search tool calls, thinking and context rows.
    #[serde(default)]
    #[ts(optional)]
    pub include_activity: Option<bool>,
    /// The `nextCursor` of the previous step; absent for the first.
    #[serde(default)]
    #[ts(optional)]
    pub after: Option<String>,
}

/// A row that matches, and how many times (counted per row, capped).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FoundRow {
    #[ts(type = "number")]
    pub id: i64,
    #[ts(type = "number")]
    pub count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatFindResult {
    /// Matching rows of this step, oldest first.
    pub rows: Vec<FoundRow>,
    /// Where the next step continues; absent when the whole history has been scanned.
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SendAction {
    /// Apply `sendBehavior.mode` while busy; a plain prompt when idle.
    Default,
    Steer,
    Now,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSendArgs {
    pub chat_id: String,
    /// A fresh UUID per send; a repeat with the same id is answered without sending twice.
    pub submission_id: String,
    pub text: String,
    pub action: SendAction,
    /// The draft revision the composer held when it sent; a newer stored draft is kept. Ignored
    /// for a send whose origin is not the composer.
    #[ts(type = "number")]
    pub draft_revision: i64,
    /// Who pressed send. Only `composer` (the default) touches the draft.
    #[serde(default)]
    #[ts(optional)]
    pub origin: Option<SendOrigin>,
    /// Ids from `attachment_put`.
    #[serde(default)]
    #[ts(optional)]
    pub attachments: Option<Vec<String>>,
    /// The panel-context payload, handed to the registered context builder (composer sends only).
    #[serde(default)]
    #[ts(optional, type = "unknown")]
    pub context: Option<Value>,
}

/// Where a user's send came from. Every origin is still the user's own send
/// and follows `sendBehavior`; only the composer's clears the draft.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SendOrigin {
    #[default]
    Composer,
    Action,
    Notification,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    pub submission: Submission,
    pub chat: ChatRecord,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatStopArgs {
    pub chat_id: String,
    /// `false` is Stop (the queue becomes uncertain); `true` is Esc (the queue continues).
    pub continue_queue: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatStopResult {
    pub status: ChatStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SubmissionActionKind {
    Withdraw,
    SteerNow,
    SendNow,
    Recover,
    Move,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmissionActionArgs {
    pub submission_id: String,
    pub action: SubmissionActionKind,
    /// For `move`: the new 0-based queue position.
    #[serde(default)]
    #[ts(type = "number")]
    #[ts(optional)]
    pub position: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionActionResult {
    pub submission: Submission,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InteractionAnswerArgs {
    pub chat_id: String,
    pub request_id: String,
    #[serde(default)]
    #[ts(optional)]
    pub value: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub confirmed: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub cancelled: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub goto: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftSaveArgs {
    pub chat_id: String,
    pub text: String,
    /// Proposed by the client as `max(now, prev + 1)`; refused unless greater than the stored one.
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DraftSaveResult {
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSetModelArgs {
    pub chat_id: String,
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCatalogueArgs {
    pub harness: Harness,
    #[serde(default)]
    #[ts(optional)]
    pub refresh: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolStopArgs {
    pub chat_id: String,
    pub tool_call_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolStopResult {
    pub stopped: bool,
}

/// `unread_clear`: the token the UI saw; a newer mark has another token and stays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnreadClearArgs {
    pub chat_id: String,
    pub token: String,
}

/// `chat_fork`: `forkId` is a fresh UUID per request; a repeat answers the same
/// fork. `messageId` forks from that assistant reply; `side` with an `excerpt` makes a side chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatForkArgs {
    pub chat_id: String,
    pub fork_id: String,
    #[serde(default)]
    #[ts(optional, type = "number")]
    pub message_id: Option<i64>,
    #[serde(default)]
    #[ts(optional)]
    pub side: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub excerpt: Option<String>,
}

/// `message_edit_open`: `messageId` is a user row's id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageEditOpenArgs {
    pub chat_id: String,
    #[ts(type = "number")]
    pub message_id: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MessageEditOpenResult {
    #[ts(type = "number")]
    pub message_id: i64,
    /// Pass back on resend; a message that changed meanwhile refuses it.
    #[ts(type = "number")]
    pub revision: i64,
    pub text: String,
}

/// `message_resend`: replaces the message and everything after it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageResendArgs {
    pub chat_id: String,
    #[ts(type = "number")]
    pub message_id: i64,
    #[ts(type = "number")]
    pub revision: i64,
    pub submission_id: String,
    pub text: String,
}

/// `chat_rename`: a manual title, 1–512 bytes; it always wins over an automatic one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatRenameArgs {
    pub chat_id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TitleSuggestResult {
    pub title: String,
}

/// `attachment_put`: one image, base64. PNG, JPEG, GIF or WebP.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentPutArgs {
    pub chat_id: String,
    pub name: String,
    pub mime: String,
    /// The file's bytes, base64.
    pub bytes: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentPutResult {
    pub attachment_id: String,
}

/// One of the CLI's commands or skills, as `get_state` reported it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

/// `autocomplete_suggest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocompleteArgs {
    pub chat_id: String,
    /// A fresh UUID per request.
    pub request_id: String,
    pub draft: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AutocompleteResult {
    /// The rest of the word being typed; empty when there is nothing to show.
    pub completion: String,
    /// Set when the request was not run (budget, busy, cooldown): ask again after this long.
    #[ts(type = "number | null")]
    pub retry_after_ms: Option<i64>,
}

/// `side_chat_resolve`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SideChatAction {
    Promote,
    Discard,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SideChatResolveArgs {
    pub chat_id: String,
    pub action: SideChatAction,
    /// Discarding a side chat that has sent anything needs this.
    #[serde(default)]
    #[ts(optional)]
    pub confirmed: Option<bool>,
}

/// `chat_mute`: mute or unmute (user-only: no agent tool reads or writes it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatMuteArgs {
    pub chat_id: String,
    pub muted: bool,
}

/// `attention_snooze`: leave the chat out of the attention groups until `until` (Unix ms, at most
/// 367 days ahead); `null` unsnoozes. New activity ends a snooze early.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttentionSnoozeArgs {
    pub chat_id: String,
    #[ts(type = "number | null")]
    pub until: Option<i64>,
}

/// `completion_ack`: the completion the UI saw; a newer one stays unread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletionAckArgs {
    pub chat_id: String,
    pub completion_id: String,
}

// ---------------------------------------------------------------------------------------------
// Events, on the one `app-event` channel, tagged by `kind`.
//
// Ordering rule for the UI: `chat`, `thread`, `message` and `queue` carry a `revision` and the UI
// drops one that is not newer than what it holds for that chat. `refresh` and `catalogue` carry
// none and always apply: both only ask the UI to re-read.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatEvent {
    /// Any change to the chat row. The revision is `chat.revision`.
    Chat { chat: ChatRecord },
    /// The thread state changed. The revision is `thread.revision`.
    Thread {
        chat_id: String,
        thread: ThreadState,
    },
    /// One per 80 ms flush while rows are dirty, plus tool start, tool end and row finalisation.
    Message {
        chat_id: String,
        #[ts(type = "number")]
        revision: i64,
        rows: Vec<MessageRow>,
    },
    /// The full pending list after any submission state or position change.
    Queue {
        chat_id: String,
        #[ts(type = "number")]
        revision: i64,
        pending: Vec<Submission>,
    },
    /// Re-read the transcript page (after reprojection or recovery). Always applies.
    Refresh { chat_id: String },
    /// The model catalogue was refreshed. Always applies.
    Catalogue { harness: Harness },
    /// The attention summary changed. The newest `summary.revision` wins.
    Attention { summary: AttentionSummary },
    /// A turn finished and its completion was announced, once per completion id.
    Completion { completion: Completion },
    /// The chat is gone (a hard delete, a discarded side chat, a chat dropped after a failed
    /// first send). Drop everything held for it, including its sidebar row.
    #[serde(rename = "chat_removed")]
    ChatRemoved { chat_id: String },
}

impl ChatEvent {
    /// As an [`helm_ipc::AppEvent`] for the event channel.
    pub fn to_app_event(&self) -> helm_ipc::AppEvent {
        let v = serde_json::to_value(self).expect("chat events serialise");
        let Value::Object(mut map) = v else {
            unreachable!("a tagged enum serialises to an object")
        };
        let kind = map
            .remove("kind")
            .and_then(|k| k.as_str().map(str::to_owned))
            .expect("tagged by kind");
        let mut e = helm_ipc::AppEvent::new(&kind);
        e.fields = map;
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_keep_the_kind_tag_and_camel_case_fields() {
        let e = ChatEvent::Queue {
            chat_id: "c1".into(),
            revision: 7,
            pending: vec![],
        };
        assert_eq!(
            e.to_app_event().to_value(),
            json!({"kind": "queue", "chatId": "c1", "revision": 7, "pending": []})
        );
        assert_eq!(
            ChatEvent::Refresh {
                chat_id: "c".into()
            }
            .to_app_event()
            .kind,
            "refresh"
        );
    }

    #[test]
    fn op_args_refuse_unknown_fields() {
        let ok: Result<ChatStopArgs, _> =
            serde_json::from_value(json!({"chatId": "c", "continueQueue": true}));
        assert!(ok.is_ok());
        let bad: Result<ChatStopArgs, _> =
            serde_json::from_value(json!({"chatId": "c", "continueQueue": true, "x": 1}));
        assert!(bad.is_err());
    }

    #[test]
    fn empty_details_serialise_as_an_empty_object() {
        assert_eq!(
            serde_json::to_value(MessageDetails::default()).unwrap(),
            json!({})
        );
    }

    #[test]
    fn unknown_stored_status_reads_as_interrupted() {
        assert_eq!(ChatStatus::parse("idle"), ChatStatus::Idle);
        assert_eq!(ChatStatus::parse("importing"), ChatStatus::Interrupted);
        for s in [
            ChatStatus::Idle,
            ChatStatus::Starting,
            ChatStatus::Running,
            ChatStatus::Stopping,
            ChatStatus::Stopped,
            ChatStatus::Interrupted,
        ] {
            assert_eq!(ChatStatus::parse(s.as_str()), s);
        }
    }
}
