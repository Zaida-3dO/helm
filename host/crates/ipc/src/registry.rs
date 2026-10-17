//! The op registry: the seam through which every crate adds `api` operations.

use crate::events::EventSink;
use crate::services::Services;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// How an op runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpClass {
    /// On the async runtime (send, stop, answer and similar).
    Async,
    /// On the blocking pool.
    Blocking,
    /// On the blocking pool, holding the global `maintenance` lock (not reentrant).
    Maintenance,
    /// On the blocking pool, holding one of the two `inspections` permits (expensive reads).
    Heavy,
}

/// An op failure as the renderer sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
    /// The one generic refusal for unknown ops and unauthorised callers. It names no op.
    pub fn refused() -> Self {
        Self::new("refused", "This operation is not available here.")
    }
    pub fn bad_args(message: impl Into<String>) -> Self {
        Self::new("bad_args", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new("conflict", message)
    }
}

/// What a handler receives besides its arguments.
#[derive(Clone)]
pub struct OpCtx {
    pub services: Arc<Services>,
    pub events: Arc<dyn EventSink>,
}

pub type OpResult = Result<Value, ApiError>;
pub type BoxFuture = Pin<Box<dyn Future<Output = OpResult> + Send>>;
pub(crate) type BlockingFn = Arc<dyn Fn(&OpCtx, Value) -> OpResult + Send + Sync>;
pub(crate) type AsyncFn = Arc<dyn Fn(OpCtx, Value) -> BoxFuture + Send + Sync>;

#[derive(Clone)]
pub(crate) enum Handler {
    Blocking(BlockingFn),
    Async(AsyncFn),
}

#[derive(Clone)]
pub(crate) struct Op {
    pub class: OpClass,
    pub handler: Handler,
}

/// Every op the host serves. Names are unique; registering a name twice panics at startup, so a
/// clash between crates is caught by the first test that builds the registry.
#[derive(Default, Clone)]
pub struct OpRegistry {
    pub(crate) ops: BTreeMap<String, Op>,
}

impl OpRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a blocking op in `class` (`Blocking`, `Maintenance` or `Heavy`).
    pub fn blocking(
        &mut self,
        name: &str,
        class: OpClass,
        f: impl Fn(&OpCtx, Value) -> OpResult + Send + Sync + 'static,
    ) -> &mut Self {
        assert_ne!(
            class,
            OpClass::Async,
            "use OpRegistry::async_op for async ops"
        );
        self.add(
            name,
            Op {
                class,
                handler: Handler::Blocking(Arc::new(f)),
            },
        )
    }

    /// Add an op that runs on the async runtime.
    pub fn async_op<F, Fut>(&mut self, name: &str, f: F) -> &mut Self
    where
        F: Fn(OpCtx, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = OpResult> + Send + 'static,
    {
        let f: AsyncFn = Arc::new(move |ctx, args| Box::pin(f(ctx, args)));
        self.add(
            name,
            Op {
                class: OpClass::Async,
                handler: Handler::Async(f),
            },
        )
    }

    fn add(&mut self, name: &str, op: Op) -> &mut Self {
        if self.ops.insert(name.to_owned(), op).is_some() {
            panic!("api op registered twice: {name}");
        }
        self
    }

    pub fn contains(&self, name: &str) -> bool {
        self.ops.contains_key(name)
    }

    pub fn class_of(&self, name: &str) -> Option<OpClass> {
        self.ops.get(name).map(|o| o.class)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.ops.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_names_panic() {
        let mut r = OpRegistry::new();
        r.blocking("x", OpClass::Blocking, |_, _| Ok(Value::Null));
        r.blocking("x", OpClass::Heavy, |_, _| Ok(Value::Null));
    }

    #[test]
    fn classes_are_recorded() {
        let mut r = OpRegistry::new();
        r.blocking("m", OpClass::Maintenance, |_, _| Ok(Value::Null));
        r.async_op("a", |_, _| async { Ok(Value::Null) });
        assert_eq!(r.class_of("m"), Some(OpClass::Maintenance));
        assert_eq!(r.class_of("a"), Some(OpClass::Async));
        assert_eq!(r.names().collect::<Vec<_>>(), ["a", "m"]);
    }
}
