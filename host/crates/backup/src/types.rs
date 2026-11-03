//! The shapes the UI sees. camelCase on the wire; exported with ts-rs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateArgs {
    /// Where to write the archive; the default is a `Helm Backups` folder under Documents.
    #[serde(default)]
    pub dest_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    /// The archive folder itself, plain and readable.
    pub archive_dir: String,
    /// The profile entries carried besides the database (folders and loose files).
    #[ts(type = "number")]
    pub entries: i64,
    #[ts(type = "number")]
    pub db_bytes: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListArgs {
    #[serde(default)]
    pub dir: Option<String>,
}

/// One archive found on disk, read from its manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    pub name: String,
    pub path: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub db_bytes: i64,
    #[ts(type = "number")]
    pub entries: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackupList {
    pub default_dir: String,
    /// Newest first.
    pub backups: Vec<BackupEntry>,
    /// The archive a pending restore will apply at the next start, if one is staged.
    pub pending: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreArgs {
    pub archive_dir: String,
    /// Must match the host's confirmation phrase; the UI puts it on the confirm button.
    pub confirm: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RestoreStaged {
    pub archive_dir: String,
    /// Nothing has changed yet: it takes effect at the next start.
    pub restart_required: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportArgs {
    pub chat_id: String,
    #[serde(default)]
    pub dest_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Exported {
    pub path: String,
    /// The message lines written (a branch never taken and empty lines are left out).
    #[ts(type = "number")]
    pub messages: i64,
}
