//! The record shapes the UI sees. camelCase on the wire; exported with ts-rs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const MAX_BUTTONS: usize = 24;
pub const MAX_LABEL_CHARS: usize = 40;
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_ID_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Sends the text into the current chat.
    Prompt,
    /// Starts a chat in the bound project, then sends the text there.
    NewChat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    Topbar,
    Overflow,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionButton {
    pub id: String,
    /// 1 to 40 characters.
    pub label: String,
    pub kind: ActionKind,
    /// 1 byte to 16 KiB: the exact message sent.
    pub text: String,
    /// The project a `new_chat` button starts its chat in; null until the user binds one.
    pub project_id: Option<String>,
    pub placement: Placement,
    pub hidden: bool,
    /// Which built-in seed this button came from, if any.
    pub seed_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ActionSet {
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub seed_version: i64,
    pub buttons: Vec<ActionButton>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionsSaveArgs {
    /// The revision the editor started from; a different stored revision is a conflict.
    #[ts(type = "number")]
    pub revision: i64,
    pub buttons: Vec<ActionButton>,
}
