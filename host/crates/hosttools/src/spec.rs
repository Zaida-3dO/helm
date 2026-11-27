//! What one host tool declares.

use serde::Serialize;
use serde_json::Value;

/// The reserved prefix every host-tool name carries.
pub const PREFIX: &str = "app_";
/// Longest name after the prefix.
pub const MAX_NAME_TAIL: usize = 60;
/// The longest tool name, in bytes, accepted anywhere one enters: a host tool's own name, the
/// name a worker asks to run, and (in the worker) a name an MCP server offers. A longer name is
/// refused whole rather than cut, and is never echoed back.
pub const MAX_TOOL_NAME_BYTES: usize = 256;
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
    /// Projects mode on and "Chats can change all settings" on (`app_update_repo`); a chat reaches
    /// that switch by staging `agentSettingsAll`.
    ProjectsModeAndAllSettings,
    /// At least one enabled MCP server.
    McpEnabled,
    /// Gmail is served by Helm for this launch: an enabled Google accounts server with at least
    /// one signed-in account ([`LaunchKeys::gmail_accounts`]).
    GmailAccounts,
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
    /// The group `app_list_tools` files this tool under (`chats`, `schedules`, ...). Empty: the
    /// tool is listed under its own name.
    pub group: &'static str,
    /// In the chat's own tool list. Every other tool is found with `app_list_tools` and called
    /// with `app_mcp_call`, so a chat pays for the few tools it uses all the time and for nothing
    /// else.
    pub core: bool,
    /// In the chat's own tool list while Orchestrator is on for it, and on demand otherwise.
    pub core_with_orchestrator: bool,
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
            group: "",
            core: false,
            core_with_orchestrator: false,
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

    /// File the tool under a group of `app_list_tools`.
    pub fn group(mut self, g: &'static str) -> Self {
        self.group = g;
        self
    }

    /// List the tool in every chat's own tool list.
    pub fn core(mut self) -> Self {
        self.core = true;
        self
    }

    /// List the tool in the chat's own tool list only while Orchestrator is on for it.
    pub fn core_with_orchestrator(mut self) -> Self {
        self.core_with_orchestrator = true;
        self
    }

    /// Why this declaration is invalid, or `None`.
    pub fn problem(&self) -> Option<String> {
        let n = self.name;
        if n.len() > MAX_TOOL_NAME_BYTES {
            return Some(format!(
                "host-tool names are at most {MAX_TOOL_NAME_BYTES} bytes"
            ));
        }
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
    /// The group `app_list_tools` files the tool under; absent when it has none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// Listed in the chat's own tool list (see [`ToolSpec::core`]).
    pub core: bool,
    /// Listed in the chat's own tool list while Orchestrator is on for it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub core_with_orchestrator: bool,
    /// Set on a tool that a merged tool took over: the old name still works, but it is neither
    /// listed nor searched, and a search for it names the merged tool and its action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<AliasOf>,
    /// On a merged tool: what each action does (the member tools' own words).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<crate::facade::ActionInfo>,
}

/// Where an old tool name went: the merged tool and the `action` that does what it did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AliasOf {
    pub tool: String,
    pub action: String,
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
            group: s.group.to_owned(),
            core: s.core,
            core_with_orchestrator: s.core_with_orchestrator,
            alias_of: None,
            actions: Vec::new(),
        }
    }
}

/// Make the tools that count as core only while Orchestrator is on core, in a launch's catalogue
/// (`hostTools[]` as JSON). The catalogue is built before the launch knows whether Orchestrator
/// is on, so the launch applies this once it does.
pub fn promote_for_orchestrator(tools: &mut Value) {
    let Some(list) = tools.as_array_mut() else {
        return;
    };
    for t in list {
        if t.get("coreWithOrchestrator").and_then(Value::as_bool) == Some(true) {
            t["core"] = Value::Bool(true);
        }
    }
}

/// Hide every tool a lean chat was not given, in a launch's catalogue (`hostTools[]` as JSON):
/// entries outside `names` get `"hidden": true`. The worker leaves hidden tools off the chat's
/// list and out of its prompt, but they stay callable through the always-on pair `app_list_tools`
/// (which still finds them) and `app_mcp_call` — the escape hatch a lean chat uses to load more
/// of Helm's tools on demand. A merged tool's old names are left as they are: they were never
/// listed anyway, and the facade that replaced them follows the same hidden flag. The entries
/// the launch named are promoted to `core`, so a lean chat running on a local model — which is
/// only ever given core tools — still gets its whole list.
pub fn hide_outside(tools: &mut Value, names: &[String]) {
    if let Some(list) = tools.as_array_mut() {
        for t in list.iter_mut() {
            let shown = t
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| names.iter().any(|k| k == n));
            if shown {
                // A local-model chat lists only core tools, and a lean list is that chat's whole
                // tool set — small enough to ride every request. Promote what the launch named,
                // or a lean chat on a local model would silently lose the tools it was given.
                t["core"] = Value::Bool(true);
            } else {
                t["hidden"] = Value::Bool(true);
            }
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
    /// Gmail is served by Helm: an enabled Google accounts server and at least one signed-in
    /// account (see [`McpLaunch::gmail_accounts`]).
    pub gmail_accounts: bool,
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
            Condition::GmailAccounts => self.gmail_accounts,
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
    /// An enabled Google accounts server and at least one signed-in account (lists the
    /// Gmail host tools).
    pub gmail_accounts: bool,
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

/// Which of Helm's MCP servers a chat may reach, when its project says so. Registered as the
/// [`McpServerFilterGate`] service by the crate that owns projects; the MCP crate applies it to
/// each chat's server list.
pub trait McpServerFilter: Send + Sync {
    /// The ids of the servers the chat's project allows, or `None` for every server (the project
    /// names none).
    fn allowed(&self, chat_id: &str) -> Option<Vec<String>>;
    /// Whether any project names servers at all, so one list for every chat no longer fits.
    fn any(&self) -> bool;
}

/// The service wrapper for [`McpServerFilter`]. With none registered, every chat gets every server.
#[derive(Clone)]
pub struct McpServerFilterGate(pub std::sync::Arc<dyn McpServerFilter>);

/// One of a chat's MCP servers it cannot use right now: the server's display name, what its
/// connection looks like now (as the worker last reported), and the reason words for the person.
#[derive(Clone, Debug)]
pub struct McpChatProblem {
    pub name: String,
    pub state: String,
    pub reason: String,
}

/// The MCP servers one chat was given that are not connected. Registered as the
/// [`McpChatProblemsGate`] service by the MCP crate; a tool crate asks it when an agent wonders
/// why a tool it expected is missing.
pub trait McpChatProblems: Send + Sync {
    fn problems(&self, chat_id: &str) -> Vec<McpChatProblem>;
}

/// The service wrapper for [`McpChatProblems`]. With none registered, chats get no report.
#[derive(Clone)]
pub struct McpChatProblemsGate(pub std::sync::Arc<dyn McpChatProblems>);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_lean_launch_hides_everything_it_did_not_name() {
        let all = json!([
            {"name": "app_telegram_send"},
            {"name": "app_schedule", "aliasOf": null, "core": false},
            {"name": "app_help"}
        ]);
        let mut tools = all.clone();
        hide_outside(&mut tools, &["app_telegram_send".to_owned()]);
        // The named tool is kept and promoted to core: a local-model chat lists only core
        // tools, and the lean list is exactly what that chat needs — app_schedule on it must
        // survive. Everything else carries the hidden flag and stays in the catalogue, so the
        // escape hatch can still find and call it.
        assert_eq!(tools[0], json!({"name": "app_telegram_send", "core": true}));
        assert_eq!(tools[1]["hidden"], json!(true));
        assert_eq!(
            tools[1]["core"],
            json!(false),
            "a hidden tool keeps its core flag"
        );
        assert_eq!(tools[2]["hidden"], json!(true));
        // None named: all hidden, none removed; a catalogue that is not a list is left alone.
        let mut none = all;
        hide_outside(&mut none, &[]);
        assert_eq!(none.as_array().unwrap().len(), 3);
        assert!(none
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["hidden"] == json!(true)));
        let mut odd = json!({"name": "app_help"});
        hide_outside(&mut odd, &["app_help".to_owned()]);
        assert_eq!(odd, json!({"name": "app_help"}));
    }
}
