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
    /// Strata only: Helm may start the server itself when one of its chats needs it.
    pub auto_start: bool,
    /// Strata only: the install folder to start from; absent means the standard one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub folder: Option<String>,
    /// Strata only: the preset to launch with; absent means the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preset: Option<String>,
    /// How many of our chats may work on this server at once; absent follows the global
    /// default, 0 switches the cap off.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chat_cap: Option<u32>,
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

/// One listed model and what it is called, for the "Model names" card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelNameView {
    /// `local/<serverId>/<model>`.
    pub id: String,
    /// The name as the server gives it.
    pub model: String,
    /// The server's display name.
    pub server: String,
    /// What it is called when the user has given it no name.
    pub default_name: String,
    /// The name the user gave it, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub alias: Option<String>,
    /// The chat size the user says this model works well up to, in tokens; absent means unset,
    /// and the app falls back to a share of the model's window.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub comfort_tokens: Option<u64>,
    /// The other local model suggested when a chat outgrows this one, as its id; absent means no
    /// suggestion.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub step_up: Option<String>,
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
    /// The range of a server's own working-chat cap: 0 switches the cap off; leaving a server
    /// unset follows the global default (`localChatCap`, which sits in preferences).
    pub chat_cap_range: Range,
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
    /// Program names (`game.exe`) that unload local models while they run.
    pub games: Vec<String>,
    /// Minutes after a listed game closes before local models come back.
    pub game_lift_minutes: u32,
    pub game_lift_range: Range,
    /// The models the servers last listed, with their names.
    pub model_names: Vec<ModelNameView>,
    /// The longest a model's name may be, in characters.
    pub alias_max: u32,
    /// The range of a model's "works well up to" size (tokens): clearing one falls back to 70%
    /// of its window.
    pub comfort_range: Range,
    /// The scheduled task that restarts a model server, paused while local models are unloaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub restart_task: Option<String>,
    /// The file written while local models are unloaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pause_file: Option<String>,
}

/// Whether local models may load: `on`, or unloaded by the user (`manual`) or by a game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum LockKind {
    On,
    Manual,
    Game,
}

/// The graphics-card lock as the top bar and Settings show it (`local_lock_state`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LockView {
    pub state: LockKind,
    /// When the lock was set, in ms since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub since: Option<i64>,
    /// The game that set it, for a game lock.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub game: Option<String>,
    /// An unload or a turn back on is still running.
    pub working: bool,
    /// One sentence per step of the last unload or turn back on.
    pub report: Vec<String>,
    /// At least one server is switched on and has a known model, so there is something to unload.
    pub any_local: bool,
    /// Minutes after a game closes before local models come back.
    pub lift_minutes: u32,
    /// The restart task that is paused while local models are unloaded, if one is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub restart_task: Option<String>,
    /// Why a chat says the card is in use, while it does: sending to a local-model chat would reload the model server.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hold_reason: Option<String>,
    /// When that note ends by itself, in ms since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub hold_until: Option<i64>,
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
    /// Sentences a server that answered has for the user (for example it lists no models).
    pub notes: Vec<String>,
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
