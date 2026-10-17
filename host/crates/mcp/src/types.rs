//! The MCP config shapes. camelCase on the wire; the UI-facing ones
//! are exported with ts-rs.
//!
//! The stored form never holds a literal secret: a `literal` reference keeps only a `secretId`,
//! and the value lives in the credential store. Input forms may carry `value` for a new
//! literal; [`crate::config`] moves it into the store before anything is written.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

pub const MAX_SERVERS: usize = 16;
pub const MAX_CONFIG_BYTES: usize = 128 * 1024;
pub const MAX_ID_CHARS: usize = 24;
pub const MAX_NAME_CHARS: usize = 120;
pub const MAX_COMMAND_CHARS: usize = 4096;
pub const MAX_ARGS: usize = 64;
pub const MAX_ARG_CHARS: usize = 4096;
pub const MAX_ENV: usize = 32;
pub const MAX_HEADERS: usize = 32;
pub const MAX_HEADER_NAME: usize = 128;
pub const MAX_URL_CHARS: usize = 4096;
pub const MAX_LITERAL_BYTES: usize = 16 * 1024;
pub const MAX_REF_VALUE_CHARS: usize = 4096;
pub const MAX_PREFIX_BYTES: usize = 64;
pub const MAX_OAUTH_FIELD: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Stdio,
    Http,
    Sse,
}

impl TransportKind {
    pub fn is_remote(self) -> bool {
        !matches!(self, TransportKind::Stdio)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            TransportKind::Stdio => "stdio",
            TransportKind::Http => "http",
            TransportKind::Sse => "sse",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// `value` names an environment variable, read on the host at connection time.
    Environment,
    /// `value` is an absolute path to a credential file (≤ 16 KiB, trimmed).
    File,
    /// The secret itself, kept in the credential store under `secretId`.
    Literal,
}

/// An env or header value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValueRef {
    pub kind: RefKind,
    /// The variable name or file path; for a literal, the secret on input only (never stored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub value: Option<String>,
    /// A stored literal's credential id. Send it back unchanged to keep the saved secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub secret_id: Option<String>,
    /// Prepended to the resolved value, for example `Bearer `. At most 64 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefix: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuthConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub redirect_uri: Option<String>,
}

/// One flat transport object, as the admin tools take it. Which fields apply
/// depends on `type`; the others must be absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transport {
    #[serde(rename = "type")]
    pub kind: TransportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(as = "Option<BTreeMap<String, ValueRef>>", optional)]
    pub env: BTreeMap<String, ValueRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(as = "Option<BTreeMap<String, ValueRef>>", optional)]
    pub headers: BTreeMap<String, ValueRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub oauth: Option<OAuthConfig>,
}

/// A configured server, in its stored form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServer {
    /// 1–24 of `[a-z0-9_]`; immutable once created.
    pub id: String,
    /// Display name, ≤ 120 characters, no control characters.
    pub name: String,
    pub enabled: bool,
    pub transport: Transport,
    /// The user's per-server "Allow unencrypted on my network" opt-in. User-only.
    #[serde(default)]
    pub allow_private_http: bool,
    /// The preset this server was created from, if any (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preset: Option<String>,
}

/// What Settings → MCP reads: the stored servers (no secret is ever in them) and the revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpEditorSnapshot {
    pub revision: String,
    pub servers: Vec<McpServer>,
}

/// What an agent sees: no argument, env or header values, and no credentials.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentServerView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    #[serde(rename = "type")]
    pub kind: TransportKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    pub arg_count: usize,
    pub env_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    pub header_names: Vec<String>,
    pub oauth: bool,
    pub allow_private_http: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentSnapshot {
    pub revision: String,
    pub servers: Vec<AgentServerView>,
}

/// Settings → MCP: add a server, or replace one with the same id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerSaveArgs {
    /// The revision the editor started from. Optional for the main window.
    #[serde(default)]
    #[ts(optional)]
    pub revision: Option<String>,
    pub server: McpServer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerRemoveArgs {
    #[serde(default)]
    #[ts(optional)]
    pub revision: Option<String>,
    pub server_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerEnableArgs {
    #[serde(default)]
    #[ts(optional)]
    pub revision: Option<String>,
    pub server_id: String,
    pub enabled: bool,
}

/// One enabled server with every reference resolved, for the worker only. Never
/// written to disk or to `launch.json`; it travels in the `mcp_config` stdin line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedServer {
    pub id: String,
    pub name: String,
    pub transport: TransportKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    pub allow_private_http: bool,
    /// Every resolved secret, so the worker can redact it everywhere.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// The preset's guidance for the agent, shown with this server's discovered tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    /// Set when a reference could not be resolved: the worker reports the server failed with
    /// this text and never connects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
