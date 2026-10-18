//! The host-tool registry: each crate registers its tools next
//! to their handlers through `pub fn register_host_tools(r: &mut HostToolRegistry)` and one line
//! in `host/app/src/registrations.rs`.

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
        ops.invoke(op, args)
            .await
            .map_err(|e| match e.code.as_str() {
                "internal" => ToolError::maybe(e.message),
                _ => ToolError::refused(e.message),
            })
    }
}

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

    pub fn get(&self, name: &str) -> Option<&RegisteredTool> {
        self.tools.get(name)
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

    /// The tools a launch lists, in registration order (`launch.json` `hostTools[]`).
    pub fn for_launch(&self, keys: &LaunchKeys) -> Vec<CatalogueEntry> {
        if keys.restricted {
            return Vec::new();
        }
        self.order
            .iter()
            .filter_map(|n| self.tools.get(n))
            .filter(|t| keys.admits(t.spec.availability.when))
            .map(|t| CatalogueEntry::from(&t.spec))
            .collect()
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
        let restricted = LaunchKeys {
            restricted: true,
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
