//! What one host tool declares.

use serde::Serialize;
use serde_json::Value;

/// The reserved prefix every host-tool name carries.
pub const PREFIX: &str = "app_";
/// Longest name after the prefix.
pub const MAX_NAME_TAIL: usize = 60;
/// Longest description: Claude Code truncates longer MCP tool descriptions.
pub const MAX_DESCRIPTION: usize = 2_048;
/// The default host-side timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 20_000;

/// The request-size class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum RequestLimit {
    /// 8 KiB, the default.
    Small,
    /// 32 KiB: schedules, settings updates, MCP server add and edit.
    Large,
}

impl RequestLimit {
    pub const fn bytes(self) -> usize {
        match self {
            RequestLimit::Small => 8 * 1024,
            RequestLimit::Large => 32 * 1024,
        }
    }
}

/// The launch condition a tool needs to be listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// Every unrestricted root chat.
    Always,
    /// Projects mode on and the chat eligible (`app_repo_locations`).
    ProjectsMode,
    /// Projects mode on and "Chats can change all settings" on (`app_update_repo`).
    ProjectsModeAndAllSettings,
    /// At least one enabled MCP server.
    McpEnabled,
    /// Helm's own web search is offered to this launch ([`LaunchKeys::web_tools`]).
    WebTools,
    /// Never listed: the worker itself calls it (a hook asking the host), agents never see it.
    Worker,
}

/// Who may call a listed tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Availability {
    /// Children (subagents) may call it. On Claude Code a child sees the parent's whole list, and a
    /// parent-only call is refused at the gate with the fixed "only available to the parent chat"
    /// error, so this does not filter the catalogue.
    pub children: bool,
    pub when: Condition,
}

impl Availability {
    pub const ROOT: Availability = Availability {
        children: false,
        when: Condition::Always,
    };
}

/// One host tool's declaration, registered beside its Rust handler.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    /// `app_` plus at most 60 characters of `[a-z0-9_]`, listed in `reserved.json`.
    pub name: &'static str,
    /// At most 2,048 characters (checked when the tool registers, so the first test that builds
    /// the registry fails on an overlong one).
    pub description: String,
    /// A JSON Schema whose top level is `{"type": "object",...}`.
    pub input_schema: Value,
    /// Usage guidance that does not fit the description.
    pub guidelines: Option<String>,
    pub request_limit: RequestLimit,
    pub timeout_ms: u64,
    pub availability: Availability,
    /// Changes state: runs under the maintenance lock, and its guidance carries the
    /// explicit-request rule and "never retry after a lost reply".
    pub mutating: bool,
    /// The handler calls `api` ops that take their own lock ([`crate::OpInvoker`]), so the
    /// dispatcher must not hold the maintenance lock around it (it is not reentrant).
    pub self_locking: bool,
}

impl ToolSpec {
    /// A read-only, root-only tool with the default limit and timeout.
    pub fn read(name: &'static str, description: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name,
            description: description.into(),
            input_schema,
            guidelines: None,
            request_limit: RequestLimit::Small,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            availability: Availability::ROOT,
            mutating: false,
            self_locking: false,
        }
    }

    /// A mutating, root-only tool with the default limit and timeout.
    pub fn write(name: &'static str, description: impl Into<String>, input_schema: Value) -> Self {
        Self {
            mutating: true,
            ..Self::read(name, description, input_schema)
        }
    }

    pub fn large(mut self) -> Self {
        self.request_limit = RequestLimit::Large;
        self
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    pub fn children(mut self) -> Self {
        self.availability.children = true;
        self
    }

    pub fn when(mut self, c: Condition) -> Self {
        self.availability.when = c;
        self
    }

    /// The handler delegates to `api` ops, which lock for themselves.
    pub fn self_locking(mut self) -> Self {
        self.self_locking = true;
        self
    }

    pub fn guidelines(mut self, g: impl Into<String>) -> Self {
        self.guidelines = Some(g.into());
        self
    }

    /// Why this declaration is invalid, or `None`.
    pub fn problem(&self) -> Option<String> {
        let n = self.name;
        let Some(tail) = n.strip_prefix(PREFIX) else {
            return Some(format!("{n}: host-tool names start with {PREFIX}"));
        };
        if tail.is_empty() || tail.chars().count() > MAX_NAME_TAIL {
            return Some(format!(
                "{n}: the name after {PREFIX} must be 1-{MAX_NAME_TAIL} characters"
            ));
        }
        if !tail
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Some(format!("{n}: names use a-z, 0-9 and _ only"));
        }
        let len = self.description.chars().count();
        if len == 0 || len > MAX_DESCRIPTION {
            return Some(format!(
                "{n}: description is {len} characters (1-{MAX_DESCRIPTION})"
            ));
        }
        if self.input_schema.get("type").and_then(Value::as_str) != Some("object") {
            return Some(format!("{n}: inputSchema must be a JSON Schema object"));
        }
        if self.timeout_ms == 0 {
            return Some(format!("{n}: timeout must be positive"));
        }
        None
    }
}

/// One `launch.json` `hostTools[]` entry: what the worker's in-process server lists and enforces.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueEntry {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidelines: Option<String>,
    /// The worker refuses a larger request before it is written (both sides enforce it).
    pub request_limit_bytes: usize,
    pub timeout_ms: u64,
    pub children: bool,
    pub mutating: bool,
}

impl From<&ToolSpec> for CatalogueEntry {
    fn from(s: &ToolSpec) -> Self {
        Self {
            name: s.name.to_owned(),
            description: s.description.clone(),
            input_schema: s.input_schema.clone(),
            guidelines: s.guidelines.clone(),
            request_limit_bytes: s.request_limit.bytes(),
            timeout_ms: s.timeout_ms,
            children: s.availability.children,
            mutating: s.mutating,
        }
    }
}

/// The per-launch switches that decide which conditional tools are listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaunchKeys {
    pub projects_mode: bool,
    pub all_settings: bool,
    pub mcp_enabled: bool,
    /// A restricted chat lists nothing here; its posture decides at the gate.
    pub restricted: bool,
    /// The chat runs on a local model server configured in Helm, not on Claude.
    pub local_chat: bool,
    /// Helm's own web search is offered to this launch (see [`WebToolsGate`]).
    pub web_tools: bool,
}

impl LaunchKeys {
    pub fn admits(&self, c: Condition) -> bool {
        match c {
            Condition::Always => true,
            Condition::ProjectsMode => self.projects_mode,
            Condition::ProjectsModeAndAllSettings => self.projects_mode && self.all_settings,
            Condition::McpEnabled => self.mcp_enabled,
            Condition::WebTools => self.web_tools,
            Condition::Worker => false,
        }
    }
}

/// What one launch's MCP setup is, decided by the MCP crate (its repository gate and the
/// native-servers switch) and applied by the launch flags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpLaunch {
    /// Load the user's native Claude Code MCP servers. False adds `--strict-mcp-config`, so only
    /// Helm's own server loads.
    pub native: bool,
    /// Native servers the CLI must not use (held or reserved repository servers).
    pub disabled: Vec<String>,
    /// At least one Helm-configured MCP server is enabled (lists `app_mcp_tools`).
    pub helm_servers: bool,
}

/// The MCP crate's side of the launch; registered as the [`McpLaunchGate`] service.
pub trait McpLaunchSource: Send + Sync {
    fn for_launch(&self, chat_id: &str, cwd: &std::path::Path) -> McpLaunch;
}

/// The web tools' side of the launch: whether Helm's own web search is offered to a chat.
/// Registered as the [`WebToolsGate`] service by the crate that owns web search.
pub trait WebToolsSource: Send + Sync {
    fn offered(&self, local_chat: bool) -> bool;
}

/// The service wrapper for [`WebToolsSource`]. With none registered, web search is not offered.
#[derive(Clone)]
pub struct WebToolsGate(pub std::sync::Arc<dyn WebToolsSource>);

/// The service wrapper for [`McpLaunchSource`]. With none registered, native MCP stays on and
/// nothing is disabled (the default).
#[derive(Clone)]
pub struct McpLaunchGate(pub std::sync::Arc<dyn McpLaunchSource>);
