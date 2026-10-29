//! Chats on local model servers, from the chats crate's side.
//!
//! A local chat keeps the name of its server in the `provider` column and the server's own model
//! name in `model`. Everything that carries a model as one string (the picker, the new-chat
//! default, `chat_create`, the agent tools) uses one id for both, `local/<serverId>/<model>`, and
//! [`columns`] and [`id`] turn it into the two columns and back. A model name can hold `/` and `:`
//! (Ollama's do), so an id is split on its first two `/` only.
//!
//! The servers themselves, their settings, the memory check and the model loading live in the
//! `helm-localmodels` crate, which implements [`LocalModels`] and installs it with
//! [`crate::ChatRuntime::set_local_models`]. With nothing installed (tests, a build without the
//! crate) a local id is still stored and reported; starting such a chat is refused with a plain
//! sentence.

use crate::types::LocalModelInfo;
use std::sync::Arc;

/// The start of every local model id.
pub const PREFIX: &str = "local/";

/// `local/<server>/<model>`: the server id and the model name, if `id` is one.
pub fn split(id: &str) -> Option<(&str, &str)> {
    let rest = id.strip_prefix(PREFIX)?;
    let (server, model) = rest.split_once('/')?;
    (!server.is_empty() && !model.is_empty()).then_some((server, model))
}

/// The one-string id of a model on a server.
pub fn id(server: &str, model: &str) -> String {
    format!("{PREFIX}{server}/{model}")
}

/// The `(provider, model)` columns for a model id as the UI sends it. A Claude model id gives no
/// provider; something that starts like a local id but is not one is refused.
pub fn columns(model: Option<&str>) -> Result<(Option<String>, Option<String>), String> {
    let Some(m) = model else {
        return Ok((None, None));
    };
    if !m.starts_with(PREFIX) {
        return Ok((None, Some(m.to_owned())));
    }
    match split(m) {
        Some((server, name)) => Ok((Some(server.to_owned()), Some(name.to_owned()))),
        None => Err("That local model id is not valid.".to_owned()),
    }
}

/// SQL for a chat's one-string model id (`c` is the chat): the model for a Claude chat, the local
/// id for a chat on a local server.
macro_rules! model_id_sql {
    () => {
        "CASE WHEN c.provider IS NULL THEN c.model ELSE 'local/' || c.provider || '/' || c.model END"
    };
}
pub(crate) use model_id_sql;

/// [`model_id_sql!`] as a value, for queries built at run time.
pub const MODEL_ID_SQL: &str = model_id_sql!();

/// SQL for "this chat has started" (`c` is the chat): its first turn ran, or it was forked from a
/// chat that did and carries that session on. From then on its model can't cross between Claude
/// and local models, or between local servers; the picker and `chat_set_model` both read this.
macro_rules! started_sql {
    () => {
        "(EXISTS(SELECT 1 FROM runs r WHERE r.chat_id = c.id) OR EXISTS(SELECT 1 FROM chat_forks f WHERE f.chat_id = c.id))"
    };
}
pub(crate) use started_sql;

/// [`started_sql!`] as a value, for queries built at run time.
pub const STARTED_SQL: &str = started_sql!();

/// Thinking budget for a local model, from the chat's effort. A local model has no effort levels;
/// Claude Code is told how many tokens it may think with instead.
pub fn thinking_tokens(effort: Option<&str>) -> u32 {
    match effort {
        Some("low") => 0,
        Some("medium") => 4096,
        Some("high") => 8192,
        Some("max") => 32768,
        // xhigh, which is also what an unset effort means.
        _ => 16384,
    }
}

/// What a worker is launched with to talk to a local server. Resolved by the implementation of
/// [`LocalModels`] for one chat start.
#[derive(Clone, PartialEq)]
pub struct LocalServer {
    pub base_url: String,
    /// The server's key, or a dummy for a keyless server. Goes to the worker's environment only.
    pub key: String,
    pub context_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// Native Claude Code MCP servers off (lean tools, the default).
    pub lean: bool,
}

impl std::fmt::Debug for LocalServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalServer")
            .field("base_url", &self.base_url)
            .field("key", &"<hidden>")
            .field("context_tokens", &self.context_tokens)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("lean", &self.lean)
            .finish()
    }
}

/// One turn about to start on a local server, as the preparation step sees it.
#[derive(Clone, Debug)]
pub struct LocalTurn {
    pub chat_id: String,
    pub server_id: String,
    /// The model name as the chat stores it.
    pub model: String,
    /// The name the chat's running worker was launched with, if it has one. For Ollama this is
    /// Helm's variant of the model, which can differ from `model`.
    pub launched: Option<String>,
    /// Names launched workers of any chat are using right now, so an older variant of one is
    /// never deleted under it.
    pub in_use: Vec<String>,
}

/// The local model servers, as the chats crate uses them. Calls marked blocking are made off the
/// chat's task, on the blocking pool.
pub trait LocalModels: Send + Sync {
    /// Whether a server with this id is set up (a chat may be created on it).
    fn knows(&self, server_id: &str) -> bool;

    /// Whether the server is set up and switched on, so a new chat can rely on it by default.
    fn available(&self, server_id: &str) -> bool {
        self.knows(server_id)
    }

    /// The server's address, key and limits for a worker launching with `model`. An `Err` is a
    /// sentence for the chat.
    fn server(&self, server_id: &str, model: &str) -> Result<LocalServer, String>;

    /// Blocking. Before each turn: make sure the model can run (memory check, the model loaded,
    /// Ollama's variant made) and answer the name the worker should ask the server for. `progress`
    /// is called with a short line when something slow starts ("Loading <model>…"). An `Err` is a
    /// sentence for the chat, and no turn is started.
    fn prepare(&self, turn: &LocalTurn, progress: &dyn Fn(&str)) -> Result<String, String>;

    /// A turn on `model` (the name the worker used) ended. Quick; slow work goes on another thread.
    fn finished(&self, server_id: &str, model: &str);

    /// The models on the servers as last seen. With `refresh`, asks the servers first.
    fn catalogue(&self, refresh: bool) -> Vec<LocalModelInfo>;
}

/// The refusal for a local chat when no server list is installed.
pub const NOT_AVAILABLE: &str = "Local models are not available in this build of Helm.";

pub type SharedLocal = Arc<dyn LocalModels>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_split_on_the_first_two_slashes_and_round_trip() {
        for (id_str, want) in [
            ("local/ollama/qwen3:8b", Some(("ollama", "qwen3:8b"))),
            (
                "local/ollama/hf.co/user/model:Q4_K_M",
                Some(("ollama", "hf.co/user/model:Q4_K_M")),
            ),
            (
                "local/strata/qwen-3.8-flash",
                Some(("strata", "qwen-3.8-flash")),
            ),
            ("local//m", None),
            ("local/s/", None),
            ("local/s", None),
            ("opus", None),
        ] {
            assert_eq!(split(id_str), want, "{id_str}");
            if let Some((s, m)) = want {
                assert_eq!(id(s, m), id_str);
            }
        }
        assert_eq!(columns(None), Ok((None, None)));
        assert_eq!(
            columns(Some("opus[1m]")),
            Ok((None, Some("opus[1m]".into())))
        );
        assert_eq!(
            columns(Some("local/a/b/c")),
            Ok((Some("a".into()), Some("b/c".into())))
        );
        assert!(columns(Some("local/a")).is_err());
        assert!(columns(Some("local/")).is_err());
        assert_eq!(
            [
                Some("low"),
                Some("medium"),
                Some("high"),
                Some("xhigh"),
                Some("max"),
                None
            ]
            .map(thinking_tokens),
            [0, 4096, 8192, 16384, 32768, 16384]
        );
    }
}
