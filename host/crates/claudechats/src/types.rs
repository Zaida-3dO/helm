//! The shapes the UI sees. camelCase on the wire; exported with ts-rs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Longest query accepted, in bytes (the same bound the chat search uses).
pub const MAX_QUERY_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchArgs {
    pub query: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadArgs {
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkArgs {
    pub session_id: String,
}

/// One record as the list and the search rows show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatRow {
    pub session_id: String,
    /// The record's own title line, else its first prompt.
    pub title: String,
    /// The folder the chat worked in.
    pub folder: String,
    /// Last activity, milliseconds.
    #[ts(type = "number")]
    pub updated_at: i64,
    /// User and assistant messages counted.
    #[ts(type = "number")]
    pub message_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatList {
    pub chats: Vec<ChatRow>,
    /// The records folder was not found, so the list is whatever was indexed before (usually
    /// nothing): the UI can say why it is empty.
    pub source_missing: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub session_id: String,
    pub title: String,
    pub folder: String,
    #[ts(type = "number")]
    pub updated_at: i64,
    #[ts(type = "number")]
    pub message_count: i64,
    /// About 160 characters around the first matching word; empty for a title-only hit.
    pub snippet: String,
    pub title_only: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Message,
    Tool,
    Context,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum LineRole {
    User,
    Assistant,
}

/// One line of the read-only view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub kind: LineKind,
    /// Set for a message line; tool and context lines have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<LineRole>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Milliseconds; 0 when the record carried no timestamp.
    #[ts(type = "number")]
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatRead {
    pub session_id: String,
    pub title: String,
    pub folder: String,
    #[ts(type = "number")]
    pub updated_at: i64,
    /// Continuing is offered only when the folder it worked in still exists.
    pub forkable: bool,
    /// The view holds only the newest part of a long chat.
    pub truncated: bool,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Forked {
    pub chat_id: String,
}
