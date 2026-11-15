//! The host-tool registry: each crate registers its tools next
//! to their handlers through `pub fn register_host_tools(r: &mut HostToolRegistry)` and one line
//! in `host/app/src/registrations.rs`.

use crate::facade::{self, FacadeSpec};
use crate::reserved;
use crate::spec::{CatalogueEntry, LaunchKeys, ToolSpec};
use helm_ipc::{EventSink, Services};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Where a call came from, as the worker reports it. Never taken from the tool's arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOrigin {
    /// The chat's own agent.
    Agent,
    /// A subagent (child) of the chat's agent.
    Subagent,
}

/// What a handler knows about its caller.
#[derive(Clone)]
pub struct ToolCtx {
    /// The chat whose worker sent the request.
    pub caller_chat_id: String,
    pub is_child: bool,
    pub tool_call_id: Option<String>,
    pub origin: CallOrigin,
    pub services: Arc<Services>,
    pub events: Arc<dyn EventSink>,
    /// The app's `api` ops, for tools that act through them; `None` in tests without them.
    pub ops: Option<Arc<dyn OpInvoker>>,
}

impl ToolCtx {
    /// Run an `api` op for this tool. An op refusal becomes a "nothing changed" refusal.
    pub async fn op(&self, op: &str, args: Value) -> Result<Value, ToolError> {
        let Some(ops) = &self.ops else {
            return Err(ToolError::refused("This tool is not available here."));
        };
        // A change to a chat that runs on another PC would be made as the person the sidebar
        // shows, not the person the calling chat belongs to: an agent leaves those to the user.
        if CHANGES_A_CHAT.contains(&op)
            && args
                .get("chatId")
                .and_then(Value::as_str)
                .is_some_and(|id| {
                    self.services
                        .get::<RemoteChatGuard>()
                        .is_some_and(|g| (g.0)(id))
                })
        {
            return Err(ToolError::refused(
                "That chat runs on another PC: change it from there, or ask the user to.",
            ));
        }
        ops.invoke(op, args)
            .await
            .map_err(|e| match e.code.as_str() {
                "internal" => ToolError::maybe(e.message),
                _ => ToolError::refused(e.message),
            })
    }
}

/// Says whether a chat id belongs to a chat that runs on another PC (installed by the app).
pub struct RemoteChatGuard(pub Arc<dyn Fn(&str) -> bool + Send + Sync>);

/// The ops through which a host tool changes one chat, named by `chatId`.
const CHANGES_A_CHAT: &[&str] = &[
    "attention_address",
    "attention_snooze",
    "chat_archive",
    "chat_delete",
    "chat_fork",
    "chat_mute",
    "chat_organize",
    "chat_rename",
    "chat_restore",
    "chat_set_model",
    "chat_stop",
];

/// Runs `api` ops for host tools (the op dispatcher, past its window check).
pub trait OpInvoker: Send + Sync {
    fn invoke<'a>(
        &'a self,
        op: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, helm_ipc::ApiError>> + Send + 'a>>;
}

impl OpInvoker for helm_ipc::Dispatcher {
    fn invoke<'a>(
        &'a self,
        op: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, helm_ipc::ApiError>> + Send + 'a>> {
        Box::pin(self.call_host(op, args))
    }
}

/// Whether a failed call may still have changed something.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Changed {
    No,
    Maybe,
}

/// A tool failure as the agent sees it. `public` is safe to show the model (≤ 2,000 bytes);
/// the worker adds the "nothing changed" or "may have happened" wording from `changed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolError {
    pub public: String,
    pub changed: Changed,
}

/// Refuse a label an agent supplies (a title, caption, option label...) that holds an invisible or
/// direction-changing character, since it could read as something it is not.
pub(crate) fn screen_label(what: &str, s: &str) -> Result<(), ToolError> {
    if s.chars().any(helm_core::hidden::is_hidden_char) {
        return Err(ToolError::refused(format!(
            "{what} cannot hold invisible or direction-changing characters."
        )));
    }
    Ok(())
}

/// Longest error text returned to the agent.
pub const MAX_ERROR_BYTES: usize = 2_000;

impl ToolError {
    /// A refusal or validation failure before anything changed.
    pub fn refused(msg: impl Into<String>) -> Self {
        Self {
            public: msg.into(),
            changed: Changed::No,
        }
    }

    /// A failure after the change may have started.
    pub fn maybe(msg: impl Into<String>) -> Self {
        Self {
            public: msg.into(),
            changed: Changed::Maybe,
        }
    }

    /// The text the agent reads: the message cut to the byte limit on a character boundary, then
    /// the fixed sentence for `changed`.
    pub fn text(&self) -> String {
        let tail = match self.changed {
            Changed::No => " Nothing changed.",
            Changed::Maybe => {
                " The change may have happened: check the current state before retrying."
            }
        };
        let room = MAX_ERROR_BYTES.saturating_sub(tail.len());
        let mut msg = self.public.as_str();
        if msg.len() > room {
            let mut cut = room;
            while !msg.is_char_boundary(cut) {
                cut -= 1;
            }
            msg = &msg[..cut];
        }
        format!("{msg}{tail}")
    }
}

pub type ToolResult = Result<Value, ToolError>;
pub type ToolFuture = Pin<Box<dyn Future<Output = ToolResult> + Send>>;
pub type BlockingToolFn = Arc<dyn Fn(&ToolCtx, Value) -> ToolResult + Send + Sync>;
pub type AsyncToolFn = Arc<dyn Fn(ToolCtx, Value) -> ToolFuture + Send + Sync>;

/// How a handler runs: on the blocking pool (most tools: SQLite reads and writes) or on the async
/// runtime. The dispatcher decides the lock (maintenance for `mutating`) from the tool definition.
#[derive(Clone)]
pub enum ToolHandler {
    Blocking(BlockingToolFn),
    Async(AsyncToolFn),
}

#[derive(Clone)]
pub struct RegisteredTool {
    pub spec: ToolSpec,
    pub handler: ToolHandler,
}

/// Every host tool the app serves. A bad or duplicate registration panics, so the app's first test
/// that builds the registry catches it (as `OpRegistry` does for ops).
#[derive(Clone, Default)]
pub struct HostToolRegistry {
    tools: BTreeMap<&'static str, RegisteredTool>,
    /// Registration order: the catalogue lists tools in it (built-ins first).
    order: Vec<&'static str>,
    /// The merged tools, in registration order.
    facades: Vec<FacadeSpec>,
}

impl HostToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool answered on the blocking pool.
    pub fn blocking(
        &mut self,
        spec: ToolSpec,
        f: impl Fn(&ToolCtx, Value) -> ToolResult + Send + Sync + 'static,
    ) -> &mut Self {
        self.add(spec, ToolHandler::Blocking(Arc::new(f)))
    }

    /// Register a tool answered on the async runtime.
    pub fn async_tool<F, Fut>(&mut self, spec: ToolSpec, f: F) -> &mut Self
    where
        F: Fn(ToolCtx, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        let f: AsyncToolFn = Arc::new(move |ctx, args| Box::pin(f(ctx, args)));
        self.add(spec, ToolHandler::Async(f))
    }

    fn add(&mut self, spec: ToolSpec, handler: ToolHandler) -> &mut Self {
        if let Some(p) = spec.problem() {
            panic!("bad host tool: {p}");
        }
        let name = spec.name;
        if reserved::is_dropped(name) {
            panic!("{name} is dropped and must not be registered");
        }
        if !reserved::is_reserved(name) {
            panic!("{name} is not in host/crates/hosttools/reserved.json");
        }
        if self
            .tools
            .insert(name, RegisteredTool { spec, handler })
            .is_some()
        {
            panic!("host tool registered twice: {name}");
        }
        self.order.push(name);
        self
    }

    /// Set a registered tool's group and where it is listed (see [`crate::surface`]).
    pub fn set_surface(
        &mut self,
        name: &str,
        group: &'static str,
        core: bool,
        with_orchestrator: bool,
    ) {
        let t = self
            .tools
            .get_mut(name)
            .unwrap_or_else(|| panic!("{name} is not a registered tool"));
        t.spec.group = group;
        t.spec.core = core;
        t.spec.core_with_orchestrator = with_orchestrator;
    }

    /// Cut every listed tool's description to its first sentences (at most `max` characters) and
    /// move the whole original to the front of its guidelines, which `app_list_tools` and
    /// `app_help` return. A tool a merged tool took over is left whole: it is never listed, and
    /// the merged tool shows its text when asked. Nothing is lost, it only moves.
    pub fn slim_descriptions(&mut self, max: usize) {
        let merged: Vec<&'static str> = self
            .facades
            .iter()
            .flat_map(|f| f.actions.iter().map(|(_, m)| *m))
            .collect();
        for (name, t) in self.tools.iter_mut() {
            if merged.contains(name)
                || t.spec.core_with_orchestrator
                || t.spec.description.chars().count() <= max
            {
                continue;
            }
            let full = std::mem::take(&mut t.spec.description);
            t.spec.description =
                format!("{} More: app_help tool/{name}.", facade::lead(&full, max));
            t.spec.guidelines = Some(match t.spec.guidelines.take() {
                Some(g) => format!(
                    "{full}

{g}"
                ),
                None => full,
            });
        }
    }

    /// Merge a family of registered tools behind one tool with an `action` argument (see
    /// [`crate::facade`]). A bad merge panics, as a bad tool does.
    pub fn facade(&mut self, f: FacadeSpec) -> &mut Self {
        let name = f.name;
        if !reserved::is_reserved(name) {
            panic!("{name} is not in host/crates/hosttools/reserved.json");
        }
        if self.tools.contains_key(name) || self.facades.iter().any(|x| x.name == name) {
            panic!("host tool registered twice: {name}");
        }
        let len = f.description.chars().count();
        if len == 0 || len > facade::MAX_LISTED_DESCRIPTION {
            panic!(
                "{name}: description is {len} characters (1-{})",
                facade::MAX_LISTED_DESCRIPTION
            );
        }
        if f.actions.len() < 2 {
            panic!("{name}: a merged tool has at least two actions");
        }
        for (i, (action, member)) in f.actions.iter().enumerate() {
            if action.is_empty() || !action.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                panic!("{name}: action {action:?} uses a-z and _ only");
            }
            if f.actions[..i].iter().any(|(a, _)| a == action) {
                panic!("{name}: action {action} is listed twice");
            }
            if !self.tools.contains_key(member) {
                panic!("{name}: {member} is not a registered tool");
            }
            if self.alias_of(member).is_some() {
                panic!("{member} is already merged into another tool");
            }
            // The worker admits a call by the merged name alone, so a member that a launch may
            // not list cannot sit behind one.
            if self.tools[member].spec.availability.when != crate::spec::Condition::Always {
                panic!("{name}: {member} is not always listed, so it cannot be merged");
            }
            if f.actions[..i].iter().any(|(_, m)| m == member) && f.fixed_for(action).is_none() {
                panic!("{name}: {member} is listed twice without a fixed argument");
            }
        }
        self.facades.push(f);
        self
    }

    pub fn get(&self, name: &str) -> Option<&RegisteredTool> {
        self.tools.get(name)
    }

    /// The merged tools.
    pub fn facades(&self) -> &[FacadeSpec] {
        &self.facades
    }

    /// The merged tool and action that took over `tool`, when one did.
    pub fn alias_of(&self, tool: &str) -> Option<(&FacadeSpec, &'static str)> {
        self.facades.iter().find_map(|f| {
            f.actions
                .iter()
                .find(|(_, m)| *m == tool)
                .map(|(a, _)| (f, *a))
        })
    }

    /// A call to a merged tool, rewritten to the member tool its action names. `Ok(None)` when
    /// `name` is not a merged tool: the call stands as it is.
    pub fn rewrite(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<Option<(&'static str, Value)>, ToolError> {
        match self.facades.iter().find(|f| f.name == name) {
            Some(f) => facade::rewrite(f, args).map(Some),
            None => Ok(None),
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.order.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The tools a launch lists, in registration order (`launch.json` `hostTools[]`), then the
    /// merged tools. A tool a merged tool took over stays in the list, marked with where it went:
    /// it still answers to its old name but is neither listed to the model nor searched.
    pub fn for_launch(&self, keys: &LaunchKeys) -> Vec<CatalogueEntry> {
        if keys.restricted {
            return Vec::new();
        }
        let mut out: Vec<CatalogueEntry> = self
            .order
            .iter()
            .filter_map(|n| self.tools.get(n))
            .filter(|t| keys.admits(t.spec.availability.when))
            .map(|t| {
                let mut e = CatalogueEntry::from(&t.spec);
                if let Some((f, action)) = self.alias_of(t.spec.name) {
                    e.alias_of = Some(facade::alias(f, action));
                }
                e
            })
            .collect();
        for f in &self.facades {
            let members: Vec<_> = f
                .actions
                .iter()
                .filter_map(|(a, m)| self.tools.get(m).map(|t| (*a, t)))
                .filter(|(_, t)| keys.admits(t.spec.availability.when))
                .collect();
            out.extend(facade::entry(f, &members));
        }
        out
    }

    /// The largest request any listed tool accepts: the channel's cap must be at least this.
    pub fn largest_request(&self) -> usize {
        self.tools
            .values()
            .map(|t| t.spec.request_limit.bytes())
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Condition;
    use serde_json::json;

    fn obj() -> Value {
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    }

    fn ok(_: &ToolCtx, _: Value) -> ToolResult {
        Ok(Value::Null)
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_names_panic() {
        let mut r = HostToolRegistry::new();
        r.blocking(ToolSpec::read("app_list_chats", "List chats.", obj()), ok);
        r.blocking(ToolSpec::read("app_list_chats", "List chats.", obj()), ok);
    }

    #[test]
    #[should_panic(expected = "not in host/crates/hosttools/reserved.json")]
    fn unreserved_names_panic() {
        let mut r = HostToolRegistry::new();
        r.blocking(ToolSpec::read("app_made_up", "No.", obj()), ok);
    }

    #[test]
    #[should_panic(expected = "is dropped and must not be registered")]
    fn dropped_names_panic() {
        let mut r = HostToolRegistry::new();
        r.blocking(ToolSpec::read("app_request_mac_step", "No.", obj()), ok);
    }

    #[test]
    #[should_panic(expected = "description is 2049 characters")]
    fn long_descriptions_panic() {
        let mut r = HostToolRegistry::new();
        r.blocking(
            ToolSpec::read("app_list_chats", "x".repeat(2_049), obj()),
            ok,
        );
    }

    #[test]
    fn a_2048_character_description_is_fine() {
        let mut r = HostToolRegistry::new();
        r.blocking(
            ToolSpec::read("app_list_chats", "x".repeat(2_048), obj()),
            ok,
        );
        assert!(r.contains("app_list_chats"));
    }

    #[test]
    #[should_panic(expected = "inputSchema must be a JSON Schema object")]
    fn non_object_schemas_panic() {
        let mut r = HostToolRegistry::new();
        r.blocking(
            ToolSpec::read("app_list_chats", "List.", json!({"type": "string"})),
            ok,
        );
    }

    #[test]
    fn the_catalogue_follows_launch_keys_and_order() {
        let mut r = HostToolRegistry::new();
        r.blocking(ToolSpec::read("app_list_projects", "P.", obj()), ok)
            .blocking(
                ToolSpec::read("app_repo_locations", "R.", obj()).when(Condition::ProjectsMode),
                ok,
            )
            .blocking(
                ToolSpec::write("app_update_repo", "U.", obj())
                    .large()
                    .when(Condition::ProjectsModeAndAllSettings),
                ok,
            )
            .blocking(
                ToolSpec::read("app_web_search", "W.", obj()).when(Condition::WebTools),
                ok,
            );
        let names = |k: LaunchKeys| {
            r.for_launch(&k)
                .into_iter()
                .map(|e| e.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(LaunchKeys::default()), ["app_list_projects"]);
        let pm = LaunchKeys {
            projects_mode: true,
            ..Default::default()
        };
        assert_eq!(names(pm), ["app_list_projects", "app_repo_locations"]);
        let all = LaunchKeys {
            projects_mode: true,
            all_settings: true,
            ..Default::default()
        };
        assert_eq!(
            names(all),
            ["app_list_projects", "app_repo_locations", "app_update_repo"]
        );
        // Web search is listed on its own switch, whatever the other keys say.
        let web = LaunchKeys {
            web_tools: true,
            ..Default::default()
        };
        assert_eq!(names(web), ["app_list_projects", "app_web_search"]);
        let local_only = LaunchKeys {
            local_chat: true,
            ..Default::default()
        };
        assert_eq!(names(local_only), ["app_list_projects"]);
        let restricted = LaunchKeys {
            restricted: true,
            web_tools: true,
            ..all
        };
        assert!(names(restricted).is_empty());
        assert_eq!(r.largest_request(), 32 * 1024);
    }

    #[test]
    fn catalogue_entries_are_camel_case() {
        let s = ToolSpec::write("app_update_settings", "S.", obj()).large();
        let v = serde_json::to_value(CatalogueEntry::from(&s)).unwrap();
        assert_eq!(v["inputSchema"]["type"], "object");
        assert_eq!(v["requestLimitBytes"], 32_768);
        assert_eq!(v["timeoutMs"], 20_000);
        assert_eq!(v["mutating"], true);
        assert!(v.get("guidelines").is_none());
    }

    #[test]
    fn error_text_names_what_changed_and_fits() {
        assert_eq!(
            ToolError::refused("No such chat.").text(),
            "No such chat. Nothing changed."
        );
        assert!(ToolError::maybe("Timed out.")
            .text()
            .contains("may have happened"));
        let long = ToolError::refused("é".repeat(2_000)).text();
        assert!(long.len() <= MAX_ERROR_BYTES);
        assert!(long.ends_with(" Nothing changed."));
    }
}
