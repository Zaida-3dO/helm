//! The record shapes and op arguments the renderer sees.
//! Every type here derives `TS` and is exported to `ui/src/api/generated/projects/`
//! by [`crate::bindings`].
//!
//! Integers that are revisions, positions or Unix-millisecond times carry `#[ts(type = "number")]`
//! so ts-rs never emits `bigint` (they stay below 2^53).

use helm_chats::types::ChatStatus;
use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

/// A project's kind: a folder project made in Helm, or a board project mirrored read-only from
/// Agent Standup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ProjectKind {
    Folder,
    Board,
}

impl ProjectKind {
    pub fn parse(s: &str) -> Self {
        if s == "board" {
            Self::Board
        } else {
            Self::Folder
        }
    }
}

/// Where a project's parent came from. Agent Standup projects are always top-level on the
/// board, so in this wave every stored parent is `local`; `board` is kept for a later board that
/// does report parents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ParentSource {
    Board,
    Local,
}

/// What the mirror knows about a board project (from `GET /api/projects`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BoardInfo {
    pub board_project_id: String,
    /// The normalised board URL the project was read from.
    pub url: String,
    /// The board's own title (the node's `name` shows a local rename when there is one).
    pub title: String,
    pub headline: Option<String>,
    pub area: Option<String>,
    pub repo: Option<String>,
    pub priority: Option<String>,
    /// ISO 8601, the newest activity in the project's subtree. Stands in for a
    /// verified-at time: an upsert carrying an older value is ignored.
    pub last_activity: Option<String>,
    /// Missing from the latest complete sync: kept, chats stay put, shown with a note.
    pub stale: bool,
    /// A local override (name or icon) is in force.
    pub edited_locally: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectNode {
    pub id: String,
    pub kind: ProjectKind,
    /// The name to show: a folder project's name, or a board project's local rename or board title.
    pub name: String,
    /// A folder project's primary folder. `null` for a board project and for a local group that has
    /// no folder of its own.
    pub path: Option<String>,
    pub parent_id: Option<String>,
    pub parent_source: Option<ParentSource>,
    /// An emoji or a library icon name (`lib:<name>`); `null` = the default glyph.
    pub icon: Option<String>,
    /// A named colour or `#rrggbb`, lowercase.
    pub colour: Option<String>,
    pub pinned: bool,
    pub collapsed: bool,
    #[ts(type = "number | null")]
    pub position: Option<i64>,
    pub archived: bool,
    pub board: Option<BoardInfo>,
    /// For a board project or a folderless group: where its new chats start.
    pub primary_folder_project_id: Option<String>,
    /// Non-archived, non-side chats shown under this group.
    #[ts(type = "number")]
    pub chat_count: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    /// Compare-and-set revision for `project_update`.
    #[ts(type = "number")]
    pub revision: i64,
}

/// The one row-level mark a chat row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum RowAttention {
    Running,
    Unread,
    Lingering,
    Interrupted,
    Pinned,
}

/// Running > unread > lingering > interrupted > pinned. Pure; the UI has the same rule
/// in `ui/src/sidebar/status.ts` and folds in the attention summary's unread marks.
pub fn row_attention(
    status: ChatStatus,
    unread: bool,
    lingering: bool,
    pinned: bool,
) -> Option<RowAttention> {
    if status.is_busy() {
        Some(RowAttention::Running)
    } else if unread {
        Some(RowAttention::Unread)
    } else if lingering {
        Some(RowAttention::Lingering)
    } else if status == ChatStatus::Interrupted {
        Some(RowAttention::Interrupted)
    } else if pinned {
        Some(RowAttention::Pinned)
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SidebarRow {
    pub chat_id: String,
    pub title: String,
    /// The group the chat is shown under: its `chat_groups` row, else its folder project.
    pub group_project_id: String,
    /// Where the chat runs (`chats.project_id`, untouched by grouping).
    pub folder_project_id: String,
    pub folder_path: String,
    /// Stored nesting depth, 0–2. The sidebar recomputes the drawn depth from the rows it holds.
    pub depth: u8,
    pub parent_chat_id: Option<String>,
    pub has_children: bool,
    pub status: ChatStatus,
    pub attention: Option<RowAttention>,
    pub pinned: bool,
    pub archived: bool,
    #[ts(type = "number | null")]
    pub position: Option<i64>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SidebarView {
    Projects,
    Activity,
    Archived,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SidebarPage {
    pub rows: Vec<SidebarRow>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BoardSyncState {
    pub configured: bool,
    pub syncing: bool,
    #[ts(type = "number | null")]
    pub last_sync_at: Option<i64>,
    pub error: Option<String>,
    #[ts(type = "number")]
    pub project_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BoardConnection {
    pub url: String,
    /// The token itself never leaves the host; the UI only learns whether one is set.
    pub has_token: bool,
}

// ---- op arguments -------------------------------------------------------------------------

/// Distinguishes "absent" (`None`) from an explicit `null` (`Some(None)`).
fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectListArgs {
    #[serde(default)]
    #[ts(optional)]
    pub include_archived: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectCreateArgs {
    pub name: String,
    #[serde(default)]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub parent_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectUpdateArgs {
    pub project_id: String,
    #[ts(type = "number")]
    pub revision: i64,
    #[serde(default)]
    #[ts(optional)]
    pub name: Option<String>,
    /// `null` clears.
    #[serde(default, deserialize_with = "double_option")]
    #[ts(optional, type = "string | null")]
    pub icon: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    #[ts(optional, type = "string | null")]
    pub colour: Option<Option<String>>,
    #[serde(default)]
    #[ts(optional)]
    pub pinned: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub collapsed: Option<bool>,
    /// Reorder within the sibling pin group: place before this project (`null` = last). Rewrites the
    /// group's positions 0..n atomically; a no-op move writes nothing.
    #[serde(default, deserialize_with = "double_option")]
    #[ts(optional, type = "string | null")]
    pub before_project_id: Option<Option<String>>,
    /// Nest under another project (`null` = top level). Depth index ≤ 2, no cycles.
    #[serde(default, deserialize_with = "double_option")]
    #[ts(optional, type = "string | null")]
    pub parent_id: Option<Option<String>>,
    /// A folder project's primary folder. Every chat in it must be idle.
    #[serde(default, deserialize_with = "double_option")]
    #[ts(optional, type = "string | null")]
    pub path: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    #[ts(optional, type = "string | null")]
    pub primary_folder_project_id: Option<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectIdArgs {
    pub project_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SidebarPageArgs {
    pub view: SidebarView,
    #[serde(default)]
    #[ts(optional)]
    pub group_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SidebarRowsArgs {
    pub chat_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatOrganizeArgs {
    pub chat_id: String,
    #[serde(default)]
    #[ts(optional)]
    pub pinned: Option<bool>,
    /// Reorder within the chat's group and pin state: before this chat.
    #[serde(default)]
    #[ts(optional)]
    pub before_chat_id: Option<String>,
    /// Reorder to the end of the chat's group and pin state.
    #[serde(default)]
    #[ts(optional)]
    pub to_end: Option<bool>,
    /// Show the chat under this group (writes `chat_groups`; where it runs is unchanged).
    #[serde(default)]
    #[ts(optional)]
    pub group_project_id: Option<String>,
    /// Nest under another chat (`null` = top level).
    #[serde(default, deserialize_with = "double_option")]
    #[ts(optional, type = "string | null")]
    pub parent_chat_id: Option<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardConnectionSaveArgs {
    pub url: String,
    /// Write-only. Absent keeps the stored token; an empty string clears it.
    #[serde(default)]
    #[ts(optional)]
    pub token: Option<String>,
}
