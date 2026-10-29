//! The shapes shared with the UI (generated into `ui/src/api/generated/localmodels/`).

use crate::settings::{CompareModel, ToolsSetup};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One configured server as Settings shows it. The key is never here, only whether one is set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ServerView {
    pub id: String,
    pub name: String,
    #[ts(type = "'strata' | 'ollama'")]
    pub kind: helm_chats::types::LocalServerKind,
    pub base_url: String,
    pub enabled: bool,
    /// The model to offer when a Strata server does not list its models.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    pub has_key: bool,
    /// Whether the server answered the last time Helm asked (a refresh or a test). Absent until
    /// it has been asked; Helm never asks at start-up.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reachable: Option<bool>,
    /// Why not, in a sentence, when it did not answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// The allowed range of a number setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    pub min: u32,
    pub max: u32,
    pub default: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelsState {
    pub servers: Vec<ServerView>,
    /// Ollama's context window for Helm's variant of a model, in tokens.
    pub ollama_context: u32,
    pub ollama_context_range: Range,
    /// How long Ollama keeps a model loaded after a turn, in minutes.
    pub keep_alive_minutes: u32,
    pub keep_alive_range: Range,
    /// Memory (GB) a Strata model needs, for the check before it loads.
    pub strata_need_gb: u32,
    pub strata_need_range: Range,
    pub tools: ToolsSetup,
    pub compare_with: CompareModel,
}

/// What `local_server_test` found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    /// A sentence either way: what answered, or why nothing useful did.
    pub message: String,
    /// How many models the server lists, when it could be asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub models: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RefreshResult {
    #[ts(type = "Array<import('../LocalModelInfo.ts').LocalModelInfo>")]
    pub models: Vec<helm_chats::types::LocalModelInfo>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerIdArgs {
    pub server_id: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyArgs {
    pub server_id: String,
    pub key: String,
}

impl std::fmt::Debug for KeyArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyArgs")
            .field("server_id", &self.server_id)
            .field("key", &"<hidden>")
            .finish()
    }
}
