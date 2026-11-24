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

/// What a busy chat is doing right now, as the maintenance tick last saw it: read by the chip,
/// the chat list and the agent tools, and the base of the silent check. `null` while no turn is
/// open. Only `silent` raises a flag; a queue wait is never one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ChatState {
    /// A turn start that waits at its chat's start: how many of Helm's own chats would be
    /// answered first (`null` when the count is not known); why it waits, the same reason the
    /// queue keeps ([`crate::queue`]); and for a Claude hold the gating plan window's percent
    /// used and when it reopens, so the copy can name both.
    Queued {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[ts(optional)]
        #[ts(type = "number")]
        ahead: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[ts(optional)]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[ts(optional)]
        pct: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[ts(optional)]
        #[ts(type = "number")]
        until: Option<i64>,
    },
    /// The model is answering: `since` is the last output, Unix ms.
    Generating {
        #[ts(type = "number")]
        since: i64,
    },
    /// A tool call is running: its name and when it began, Unix ms (the oldest one when more
    /// than one runs at once).
    Tool {
        name: String,
        #[ts(type = "number")]
        since: i64,
    },
    /// A card — approval, question or plan — waits for the person.
    WaitingUser,
    /// The turn is open with no output, no tool call, no card and no queue wait for longer than
    /// the silent limit: `since` is the last output, Unix ms. This is the one state Helm flags.
    Silent {
        #[ts(type = "number")]
        since: i64,
    },
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
    /// A local chat whose turn is open but has said nothing, while the model server (which
    /// answers one request at a time) is busy with something else: how many of Helm's own local
    /// chats would be answered first, or 0 when the server is busy but the count is not known.
    /// `null` when the chat is not waiting — which begins the moment its first words arrive or
    /// its turn ends.
    #[ts(type = "number | null")]
    pub local_ahead: Option<i64>,
    /// Why this chat's turn start waits at its start, while it does: `local_cap`,
    /// `claude_capacity`, `at_limit` or `manual` (see [`crate::queue`], which the queue panel
    /// reads alongside this). `null` when the chat is not waiting for capacity.
    pub queue_reason: Option<String>,
    /// What the busy chat is doing right now ([`ChatState`]); `null` while no turn is open.
    /// Refreshed by the maintenance tick, never by a model call. Boxed so the record keeps its
    /// size: the state is read by the chip and the tools, not by every event hop.
    pub state: Option<Box<ChatState>>,
    /// The chat's first turn has run (or it was forked from one that did). The picker uses it to
    /// tell a chat whose conversation exists and will carry across a model change; a started chat
    /// can still move between any models, the relaunch keeps the conversation.
    pub started: bool,
    /// The profile the chat belongs to (never empty once the chat is stored).
    pub profile_id: String,
    /// The device the chat runs on: its worker, sign-in and plan are that device's.
    pub host_device_id: String,
    /// Profile ids, besides its own, that the chat is shown to.
    pub shared_with: Vec<String>,
    /// The roll-over stack this chat belongs to: an id shared by every session of a chat that
    /// rolled over, named for the oldest of them. `null` when this chat has never taken part in
    /// a roll over.
    pub stack_id: Option<String>,
    /// The session this one was rolled over from; `null` for the stack's oldest session.
    pub prev_session_id: Option<String>,
    /// The session that rolled over from this one; `null` for the newest, where the chat
    /// carries on.
    pub next_session_id: Option<String>,
    /// This chat's own pick for Claude Code's fast mode; `null` means it follows the new-chat
    /// default (Settings → Runtime). The composer's lightning toggle sets it; the value reaches
    /// the chat's agent when its worker next starts.
    pub fast_mode: Option<bool>,
    /// The chat's own open-item state — which transcript cards are opened and whether its
    /// sidebar row is expanded — so the chat shows the same on every PC that views it.
    /// `null` is the default: everything folded. The sidebar sets it through
    /// `chat_set_sidebar_state`; last write wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sidebar_state: Option<SidebarState>,
    /// The hover card's two-sentence summary ("what this chat is about, where it stands now"),
    /// made on demand by a local model ([`crate::summary`]). Left out when there is none. The
    /// row carries it so a chat seen on another PC shows the one made where it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    /// When that summary was made, Unix ms; left out with it. The card calls it stale once
    /// `last_activity_at` passes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub summary_at: Option<i64>,
}

/// The chat's open-item state, stored whole per chat. A write merges field by field: a
/// field left out keeps what the chat already had (the sidebar writes only its `expanded`,
/// the transcript only its `openCards`), a field sent replaces it whole, and last write
/// wins. Small by design: the renderer trims before sending.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SidebarState {
    /// The shape's version; this build writes and reads 1.
    pub v: u32,
    /// The chat's row in the sidebar is expanded (its children are shown). Left out means
    /// keep what it was; the stored value leaves `false` out, since folded is the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expanded: Option<bool>,
    /// Ids of the transcript cards (note cards, page cards) the person has opened, most
    /// recently opened last. A card whose id is not listed is folded. Left out means keep
    /// what was; an empty list closes every card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number[]")]
    pub open_cards: Option<Vec<i64>>,
}

/// The roll-over stack of a chat, as `chat_stack` reports it: every session sharing the chat's
/// `stack_id`, oldest first, and the newest of them — the one where the chat lives now. A chat
/// that never rolled over answers with a stack of one: itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatStack {
    /// The stack's id — the oldest session's id; `null` for a chat no roll over has touched.
    pub stack_id: Option<String>,
    /// The newest session of the stack.
    pub latest_id: String,
    /// Every session of the stack, oldest first.
    pub sessions: Vec<StackSession>,
}

/// One session of a roll-over stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StackSession {
    pub id: String,
    pub title: String,
    pub archived: bool,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// A turn start held in the queue, as `queue_list` reports it (see [`crate::queue`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub chat_id: String,
    pub title: String,
    /// Why it waits: `local_cap`, `claude_capacity`, `at_limit` or `manual`.
    pub reason: String,
    /// `interactive`, `normal` or `background`.
    pub class: String,
    /// The local model server it waits on; nothing else has one.
    pub server_id: Option<String>,
    /// When the wait began, Unix ms.
    #[ts(type = "number")]
    pub since: i64,
    /// The queue pushed it to the front of its line.
    pub top: bool,
    /// The profile this chat belongs to, for the panel's tag when it is not the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub profile_id: Option<String>,
}

/// A chat with a turn open right now: the queue panel shows these above the waiting rows,
/// each with its own pause and stop. See [`crate::queue::summary`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueActive {
    pub chat_id: String,
    pub title: String,
    /// The local model server its turn runs on; nothing for a Claude chat.
    pub server_id: Option<String>,
    /// When the turn opened, Unix ms.
    #[ts(type = "number")]
    pub since: i64,
    /// The profile this chat belongs to, for the panel's tag when it is not the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub profile_id: Option<String>,
}

/// One local model server's load right now: chats working on it, and chats whose turn start
/// waits at its start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueServer {
    pub server_id: String,
    #[ts(type = "number")]
    pub working: i64,
    #[ts(type = "number")]
    pub queued: i64,
}

/// `queue_list`: the queue whole in one read — every held start, front of the lines first,
/// the chats with a turn open right now (oldest first), plus the load each place it waits on
/// carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueSummary {
    pub entries: Vec<QueueEntry>,
    /// Chats with a turn open right now, oldest turn first.
    pub active: Vec<QueueActive>,
    pub servers: Vec<QueueServer>,
    /// Chats waiting for the Claude plan windows to open again.
    #[ts(type = "number")]
    pub claude_queued: i64,
    /// Whether those windows read full right now.
    pub claude_full: bool,
    /// When they reopen, Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub claude_until: Option<i64>,
}

/// `queue_action`: one move on one held start; answers with the fresh [`QueueSummary`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueActionArgs {
    pub chat_id: String,
    /// `top`, `up`, `down`, `move`, `hold`, `release`, `cancel`, `pause`, `stop` or
    /// `start_anyway`. `pause` tells a working chat to halt at its next boundary (and parks a
    /// waiting one); `stop` takes down a working chat's worker at once (and ends a waiting
    /// one's wait). Both stamp the acting profile on the target's transcript.
    pub action: String,
    /// For `move`: stand just before this waiting chat (at the end when given as none).
    #[serde(default)]
    #[ts(optional)]
    pub before_chat_id: Option<String>,
}

/// One entry of the build queue: a feature to run, in the order the queue keeps them
/// ([`crate::buildqueue`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BuildQueueEntry {
    pub id: String,
    /// Who added it (the empty string when nobody was named).
    pub profile_id: String,
    pub title: String,
    /// The brief the started chat opens with; `None` means the board item line instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub brief: Option<String>,
    /// The board item to work on, named in the first message when there is no brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub item_id: Option<String>,
    /// The project the chat runs in (a folder project, or a board with a folder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_id: Option<String>,
    /// The model to run it on; `None` is the new-chat default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// Its place in the line, smallest first.
    #[ts(type = "number")]
    pub position: i64,
    /// `waiting`, `running`, `paused`, `done` or `removed`.
    pub state: String,
    /// The chat carrying it out, while one runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub finished_at: Option<i64>,
}

/// `build_queue_add`: join the back of the line. It needs a title and either a brief or a board
/// item to work on; the project and model are optional.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildQueueAddArgs {
    pub title: String,
    #[serde(default)]
    #[ts(optional)]
    pub brief: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub item_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub project_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<String>,
}

/// `build_queue_move`: stand this entry just before another (`None` is the back of the line).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildQueueMoveArgs {
    pub id: String,
    #[serde(default)]
    #[ts(optional)]
    pub before_id: Option<String>,
}

/// `build_queue_pause`, `build_queue_resume` and `build_queue_remove`: one entry, by id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildQueueIdArgs {
    pub id: String,
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
    /// The profile the chat belongs to; empty for a chat nobody owns (the home profile's). The
    /// window keeps the rows of the profile it shows; the badge, tray and keep-awake count all.
    pub profile_id: String,
}

/// What a muted chat is holding for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum MutedKind {
    Approval,
    Question,
    Plan,
    /// A question its agent flagged with `app_flag_attention`.
    Flag,
}

/// A muted chat with something waiting: listed in the inbox's Muted group, never counted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MutedRow {
    pub chat_id: String,
    pub title: String,
    pub project: String,
    pub kind: MutedKind,
    /// What it is waiting for, in plain words.
    pub reason: String,
}

/// What the idle check holds about a chat it listed or gave up on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TriageRow {
    pub chat_id: String,
    pub title: String,
    pub project: String,
    /// `normal` or `high` when the check judged the chat needs you; otherwise `null`.
    pub priority: Option<String>,
    /// All its tries ended without a verdict: the inbox offers Retry.
    pub failed: bool,
    /// `decision` or `stalled` when the check judged the chat needs you (a stall offers Resume);
    /// `null` for a verdict from before kinds, which reads as a decision.
    pub kind: Option<String>,
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
    /// Chats the user marked unread.
    pub unread: Vec<AttentionRow>,
    /// Chats that finished a reply the user has not read. Not counted in the badge and not a
    /// group the inbox lists: it exists so the finished notices stay tied to the reply's read
    /// state — a finished toast comes back down once the chat is read, and an away alert can
    /// still name the finish.
    #[serde(default)]
    pub finished: Vec<AttentionRow>,
    /// Running chats with no output for the stall limit, and quiet chats the idle check judged
    /// stalled (the reason is the check's own, and the inbox offers Resume); they count in the
    /// badge.
    pub stalled: Vec<AttentionRow>,
    /// Quiet chats the idle check thinks need a decision from you; the reason is the check's own
    /// and leads with the decision.
    pub flagged: Vec<AttentionRow>,
    /// Muted chats with a flag, question, approval or plan waiting. Kept so the user can find
    /// them; they raise nothing and are not in the badge.
    #[serde(default)]
    pub muted: Vec<MutedRow>,
    /// The idle check's priority for chats it listed, and the chats it gave up on.
    #[serde(default)]
    pub triage: Vec<TriageRow>,
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LiveTool {
    pub tool_call_id: String,
    pub name: String,
    /// A few plain words on what the call is doing, read from its own arguments; a call whose
    /// arguments say nothing usable has none.
    pub summary: Option<String>,
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
    /// What the question gate has made of this card, while it checks it or once it has judged
    /// it. Absent when no gate took it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gate: Option<GateState>,
}

/// Where the question gate has got to with a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum GateStatus {
    /// The gate is judging it: the chat shows "Checking whether this needs you…", and the card
    /// raises no badge, toast or alert until the hold ends.
    Checking,
    /// The gate decided this one needs the user; the brief sits above the options.
    Escalated,
    /// The gate could not judge it (or ran out of time): a normal card with a quiet note.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GateState {
    pub state: GateStatus,
    /// The gate's write-up of the question, for an escalated card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub brief: Option<GateBrief>,
    /// The quiet line a failed hold leaves on the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// One option in the gate's brief of a question, written in the user's terms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GateBriefOption {
    pub label: String,
    /// What choosing it means, in one plain line.
    pub means: String,
    pub pros: String,
    pub cons: String,
    /// Rough cost: time, money or effort.
    pub cost: String,
}

/// The gate's rewrite of an escalated question: the problem, the real choices, and what the gate
/// would pick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GateBrief {
    pub problem: String,
    pub options: Vec<GateBriefOption>,
    pub recommendation: String,
    /// Why that recommendation, in one line.
    pub why: String,
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
    /// The end of the chat's last turn is being checked (Helm decides whether the stop needs the
    /// user): the chat shows "Checking whether this stop needs you…" until the check ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_check: Option<StopCheck>,
    pub capabilities: Capabilities,
    pub send_behavior: SendBehavior,
    #[ts(type = "number")]
    pub revision: i64,
}

/// A turn end being checked, with when the check began.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StopCheck {
    #[ts(type = "number")]
    pub started_at: i64,
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
    /// Something the question gate did with an ask: an answer it chose for the user, which they
    /// can review and change later.
    Gatekeeper,
    /// The chat stopped at a blocker only the user can clear, and Helm checked and agreed.
    StopBlocker,
    /// The chat stopped at the plan's usage limit, with nothing left that can run elsewhere.
    StopLimit,
    /// The chat stopped after a failure that persisted through retries.
    StopFailure,
    /// The chat stopped to ask, and Helm would not answer for the user.
    StopAsked,
    /// A gap in the chat's work that was not its doing: the computer slept, the connection was
    /// down, or Helm asked the chat to carry on after one.
    Interruption,
    /// Claude Code could not refresh its sign-in after Helm's tries: the row carries the
    /// "Sign in again" offer.
    SignIn,
    /// The project's startup instructions were added to the chat's first message (or, after a
    /// roll over, the note to carry on with the startup that was already done).
    Startup,
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
    /// For a user row written by a page (`origin: page`): the page's name and the short summary
    /// the collapsed card shows, kept here so the card survives reloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_overview: Option<String>,
    /// For a user row from another chat or a note Helm wrote: its kind ([`MessageKind`]), shown
    /// as a small tag on the card. Absent for the user's own sends and older rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<MessageKind>,
    /// For a `wait-finished` note about a chat: the watched chat, so the card's short line can
    /// name and link it. Absent otherwise and when that chat is already gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub wait_chat: Option<WatchedChat>,
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
    /// For a tool row: a few plain words on what the call is doing, read from its own
    /// arguments (see `toolsummary`), so later views of the row can show the same words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_summary: Option<String>,
    /// For a user row: the images sent with the message, shown as thumbnails in the bubble. For a
    /// tool row: the pictures the tool returned. The bytes are not in the row; `attachment_get`
    /// reads them (see `message_images`).
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
}

/// Submission states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SubmissionState {
    Saved,
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
    /// Spoken or typed on the phone's voice screen and forwarded by the hub. Like `telegram`, it
    /// never counts as consent, and approvals, plans and trust prompts are never answered by it.
    Voice,
    /// Heard on a room speaker (a Home Assistant voice satellite) and forwarded by the hub.
    /// Like `voice` and `telegram`: never consent, and never answers a prompt.
    Speaker,
    /// Typed on another device of the household for a chat that runs here. The person at that
    /// device owns the chat, so it counts as their own words.
    Remote,
    /// Written by a page running in the side panel (or an app integration speaking for one). It
    /// follows the user's send behaviour like an action, but it is not the user's own words and
    /// never answers a question or approves anything on their behalf. The transcript shows it as
    /// a card naming the page, not as a typed bubble.
    Page,
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
            Self::Voice => "voice",
            Self::Speaker => "speaker",
            Self::Remote => "remote",
            Self::Page => "page",
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
            "voice" => Self::Voice,
            "speaker" => Self::Speaker,
            "remote" => Self::Remote,
            "page" => Self::Page,
            _ => Self::App,
        }
    }

    /// The user's own send from this window: it follows the send mode and the wait limit. A
    /// page's send counts here (the user started it from a page they are running), though it is
    /// never the user's words and never counts as consent — consent reads the stored origin for
    /// that, and only `typed` qualifies.
    pub fn is_users_own(self) -> bool {
        matches!(
            self,
            Self::Typed
                | Self::Action
                | Self::Notification
                | Self::Answer
                | Self::Remote
                | Self::Page
        )
    }
}

impl From<SendOrigin> for SubmissionOrigin {
    fn from(o: SendOrigin) -> Self {
        match o {
            SendOrigin::Composer => Self::Typed,
            SendOrigin::Action => Self::Action,
            SendOrigin::Notification => Self::Notification,
            SendOrigin::Page => Self::Page,
        }
    }
}

/// What kind of message a chat-to-chat or Helm-written message is. A closed set: `app_send_message`
/// requires one of the inter-chat kinds and teaches the agents which to use; the code inside Helm
/// that writes a note for an agent stamps a specific kind of its own (`wait-finished`,
/// `request-expired`) rather than one catch-all "system" kind. The user's own typing never carries
/// one, and neither does any send that is none of these: where a kind is asked for, a word
/// outside the set is refused, and one already stored reads back as none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum MessageKind {
    /// The task that starts a sub-chat.
    InitialPrompt,
    /// An instruction or correction into a chat that is working.
    Steer,
    /// A small prompt carrying no new work, between chats or from Helm.
    Nudge,
    /// A milestone or progress report to the chat that is waiting on it.
    Update,
    /// What the user did somewhere else, relayed to the agent that needs it.
    UserAction,
    /// The hand-over summary at the end of finished work.
    Completed,
    /// Helm's wait watcher: the wait the chat declared has finished.
    WaitFinished,
    /// Helm: a request the chat made expired without an answer.
    RequestExpired,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InitialPrompt => "initial-prompt",
            Self::Steer => "steer",
            Self::Nudge => "nudge",
            Self::Update => "update",
            Self::UserAction => "user-action",
            Self::Completed => "completed",
            Self::WaitFinished => "wait-finished",
            Self::RequestExpired => "request-expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "initial-prompt" => Some(Self::InitialPrompt),
            "steer" => Some(Self::Steer),
            "nudge" => Some(Self::Nudge),
            "update" => Some(Self::Update),
            "user-action" => Some(Self::UserAction),
            "completed" => Some(Self::Completed),
            "wait-finished" => Some(Self::WaitFinished),
            "request-expired" => Some(Self::RequestExpired),
            _ => None,
        }
    }

    /// The kinds a chat may stamp on a message it sends to another chat. Helm's own note kinds
    /// (`wait-finished`, `request-expired`) are stamped only by the code that writes them.
    pub fn sendable(self) -> bool {
        !matches!(self, Self::WaitFinished | Self::RequestExpired)
    }
}

/// For a `wait-finished` note about a chat: the watched chat's id and title, stored beside the
/// message so the transcript's one-line form can name and link that chat without showing its id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WatchedChat {
    pub chat_id: String,
    pub title: String,
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
    #[ts(type = "number")]
    pub created_at: i64,
    /// Only in the waiting list (`pending`): the images linked to this send, so its bubble can
    /// show them while it waits. Absent when it carried none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub images: Option<Vec<MessageImage>>,
    /// Only in the waiting list (`pending`): the files, folders and chats attached to this send
    /// by reference. Absent when it carried none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refs: Option<Vec<MessageRef>>,
    /// For a page's send: the page's name and its short summary, so a waiting bubble can draw
    /// the same card the sent row does. Absent for any other send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_overview: Option<String>,
    /// What kind of message this is, for a chat-to-chat send or a note Helm wrote
    /// ([`MessageKind`]). Absent for the user's own sends and for notes that name no kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<MessageKind>,
    /// For a `wait-finished` note about a chat: the chat it watched, so the card can name it.
    /// Absent otherwise and when that chat is already gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub wait_chat: Option<WatchedChat>,
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
    /// At most 100 messages sent but not yet read or settled, oldest first.
    pub pending: Vec<Submission>,
    /// Where the roll-over sessions meet inside `messages` (only ever filled when the page
    /// asked for the stack). Oldest first, like the rows.
    #[serde(default)]
    pub session_marks: Vec<SessionMark>,
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
    /// What the model is called in the app: the name the user gave it, else a short default.
    pub name: String,
    /// The name as the server gives it.
    pub model: String,
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
    /// True when the server did not answer but Helm can start it when a chat uses it: the picker
    /// keeps the row choosable.
    pub startable: bool,
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
    /// The board to show the chat under. Without it the chat goes to the one board whose folder
    /// it runs in, else to the default board.
    #[serde(default)]
    #[ts(optional)]
    pub board_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub effort: Option<String>,
    /// The new-chat page: start in the permission mode the user last picked with a chat's chip.
    /// Unset for chats that agents, crews, roll overs, other devices and Telegram start.
    #[serde(default)]
    #[ts(optional)]
    pub start_in_last_mode: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatIdArgs {
    pub chat_id: String,
}

/// `chat_summarize`: what the hover card shows now and whether an answer is on its way.
/// The call never waits on the model — see [`crate::summary`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    /// The stored two-sentence summary, or `null` when there is none yet.
    pub summary: Option<String>,
    /// When it was made, Unix ms; `null` with it. The card calls it stale once its own
    /// `lastActivityAt` passes it.
    #[ts(type = "number | null")]
    pub summary_at: Option<i64>,
    /// A summary is being made on this PC right now: the card can ask again shortly.
    pub generating: bool,
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
    /// Page the whole roll-over stack as one window: when a walk runs past the end of one
    /// session it continues in the session before or after it, and the page says where the
    /// sessions meet with `sessionMarks`. Without it a chat answers from its own rows alone.
    #[serde(default)]
    #[ts(optional)]
    pub stack: Option<bool>,
}

/// Where two roll-over sessions meet in a stack page. The divider it names sits between the
/// two rows it anchors (a missing side means the other side of the boundary is in the page
/// before or after this one); it names the older session, the one the reader meets going up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionMark {
    /// The row shown above the divider, when this page carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    #[ts(optional)]
    pub above_message_id: Option<i64>,
    /// The row shown below the divider, when this page carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    #[ts(optional)]
    pub below_message_id: Option<i64>,
    /// The older session the divider names.
    pub session_id: String,
    /// Its title, for the divider's words.
    pub title: String,
    /// When that session rolled over into the next one.
    #[ts(type = "number")]
    pub rolled_over_at: i64,
}

/// Find across a chat's whole roll-over stack (`stack_find`): every session's message text,
/// with the same case-insensitive rule as the find bar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StackFindArgs {
    pub chat_id: String,
    pub query: String,
    /// Also search tool calls, thinking and context rows.
    #[serde(default)]
    #[ts(optional)]
    pub include_activity: Option<bool>,
    /// At most this many rows (default 50, capped at 200).
    #[serde(default)]
    #[ts(type = "number")]
    #[ts(optional)]
    pub limit: Option<i64>,
}

/// One matching message of a stack session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StackFound {
    /// The session whose history holds the row.
    pub session_id: String,
    #[ts(type = "number")]
    pub id: i64,
    /// How many times the query appears in the row (counted per row, capped).
    #[ts(type = "number")]
    pub count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StackFindResult {
    /// Matching rows: newest session first, newest row first within a session.
    pub rows: Vec<StackFound>,
    /// True when the limit cut the list short.
    pub truncated: bool,
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
    /// For a send whose origin is `page`: the page's name, shown on the card the transcript
    /// draws instead of a typed bubble. Kept in the message's details so the card survives a
    /// reload. Ignored for any other origin.
    #[serde(default)]
    #[ts(optional)]
    pub page_title: Option<String>,
    /// For a send whose origin is `page`: a short summary the card shows while collapsed.
    #[serde(default)]
    #[ts(optional)]
    pub page_overview: Option<String>,
    /// For a send from another chat or a paired device's agent: the kind of message
    /// ([`MessageKind`]), stored with it and shown on the transcript card. Absent for the user's
    /// own sends.
    #[serde(default)]
    #[ts(optional)]
    pub kind: Option<MessageKind>,
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
    /// A page running in the side panel wrote the message; it shows as a card naming the page.
    Page,
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
    Recover,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmissionActionArgs {
    pub submission_id: String,
    pub action: SubmissionActionKind,
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
    /// Why the model changed, in the changer's own sentence. Set when an agent changes a chat's
    /// model; it is written into the transcript's model-change row.
    #[serde(default)]
    #[ts(optional)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSetFastModeArgs {
    pub chat_id: String,
    /// The chat's own pick: `true`/`false` sets it, `null` drops it so the chat follows the
    /// new-chat default again.
    #[serde(default)]
    #[ts(optional)]
    pub on: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSetSidebarStateArgs {
    pub chat_id: String,
    /// The chat's open-item state. Fields merge one at a time: a field left out keeps what
    /// the chat had, a field sent replaces it whole (last write wins); `null` — or no state
    /// named at all — resets, everything folded.
    pub state: Option<SidebarState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCatalogueArgs {
    pub harness: Harness,
    #[serde(default)]
    #[ts(optional)]
    pub refresh: Option<bool>,
    /// Another device's id: the models that device last reported, read from this PC's copy.
    #[serde(default)]
    #[ts(optional)]
    pub device_uid: Option<String>,
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
    /// The message carries pictures or file references, which ride the resend: with them an
    /// empty text is still a sendable message. Default keeps an older PC's answer readable.
    #[serde(default)]
    pub has_attachments: bool,
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
    /// The first option.
    pub title: String,
    /// Up to three options, best first.
    pub titles: Vec<String>,
    /// Set when the options are not the model's own (it could not answer), saying why in plain
    /// words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
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
    /// Where the skill comes from, when Helm can say: `helm-skill` for one from Helm's own
    /// skills folder, empty otherwise. The `/` menu shows the source beside the name.
    #[serde(default)]
    pub scope: String,
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
// Ordering rule for the UI: `chat`, `thread`, `message` and `pending` carry a `revision` and the UI
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
    Chat { chat: Box<ChatRecord> },
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
    /// The full in-flight and not-yet-delivered list after any submission state change.
    Pending {
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
        let e = ChatEvent::Pending {
            chat_id: "c1".into(),
            revision: 7,
            pending: vec![],
        };
        assert_eq!(
            e.to_app_event().to_value(),
            json!({"kind": "pending", "chatId": "c1", "revision": 7, "pending": []})
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
        let ok: Result<ChatStopArgs, _> = serde_json::from_value(json!({"chatId": "c"}));
        assert!(ok.is_ok());
        let bad: Result<ChatStopArgs, _> = serde_json::from_value(json!({"chatId": "c", "x": 1}));
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
