//! The shapes the UI sees. camelCase on the wire; exported with ts-rs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Longest query accepted, in bytes (chats section 15 item 5 uses the same bound for the agent tool).
pub const MAX_QUERY_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchArgs {
    pub query: String,
    /// Only chats in this project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Include archived chats.
    #[serde(default)]
    pub archived: bool,
    /// The project in view, for the ranking boost.
    #[serde(default)]
    pub current_project_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenArgs {
    pub chat_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub chat_id: String,
    pub title: String,
    /// The project's name.
    pub project: String,
    /// The matched message, to open the chat at; null for a title-only hit.
    pub message_id: Option<String>,
    /// About 160 characters of the matched text around the first matching word; empty for a
    /// title-only hit.
    pub snippet: String,
    /// The chat matched by title and has no matching text.
    pub title_only: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
}
