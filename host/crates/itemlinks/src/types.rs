//! The shapes the sidebar and the picker share with the host. Generated TypeScript lives in
//! `ui/src/api/generated/itemlinks/` (see [`crate::bindings`]).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LinkedBy {
    Agent,
    User,
}

/// One chat linked to one item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemLink {
    /// Lowercase hex without hyphens: the full id when it was ever given in full, else a prefix.
    pub item_id: String,
    pub chat_id: String,
    pub is_main: bool,
    pub linked_by: LinkedBy,
    #[ts(type = "number")]
    pub linked_at: i64,
    /// The board project's title when the item is one the board mirror holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub item_title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemLinksForChats {
    /// Every link of the asked chats, plus the other chats linked to the same items, so a
    /// sidebar can group an item's chats without a second read.
    pub links: Vec<ItemLink>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemCandidate {
    pub chat_id: String,
    pub title: String,
    pub is_main: bool,
    pub archived: bool,
    #[ts(type = "number")]
    pub linked_at: i64,
}

/// The answer of `item_open`: the one chat to open, or the chats to choose from, main first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemOpen {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<ItemCandidate>,
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub item_title: Option<String>,
}
