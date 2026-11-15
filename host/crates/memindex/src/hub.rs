//! What the indexer needs of the hub line, so it can be driven against a stand-in in tests.
//!
//! The relay crate provides the real one (a line that sends a frame and waits for the answer
//! carrying its id). Nothing here queues anything for a later line: with the line down a frame
//! fails at once, and the next pass starts again from what the hub says it holds.

use serde_json::Value;
use std::sync::Arc;

/// Whether frames can go to the hub, and whether it can take them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HubStatus {
    /// No line is up.
    Down,
    /// The line is up but the hub does not index (an older hub).
    TooOld,
    Ready,
}

/// The line to the hub, as the indexer uses it.
pub trait IndexHub: Send + Sync {
    /// Send `frame` (the line gives it an `id`) and wait for the frame that answers it.
    fn ask(&self, frame: Value) -> Result<Value, String>;
    fn status(&self) -> HubStatus;
    /// Changes whenever a new line comes up: what the hub holds must be asked again.
    fn epoch(&self) -> u64;
    /// Call `f` whenever the line comes up or goes down.
    fn on_change(&self, f: Arc<dyn Fn() + Send + Sync>);
    /// Who answers the hub's own questions (`ix_embed`, `ix_trace`).
    fn set_responder(&self, r: Arc<dyn Responder>);
}

/// The answer to an `ix_embed`.
#[derive(Clone, Debug, PartialEq)]
pub enum Embedded {
    /// The vector and the model's name.
    Vector(Vec<f32>, String),
    /// The graphics card is reserved: the hub asks its own embedder instead.
    Locked,
    Failed,
}

/// What this PC answers when the hub asks.
pub trait Responder: Send + Sync {
    /// Embed `text` as it stands (the hub already put the query prefix on it).
    fn embed(&self, text: &str) -> Embedded;
    /// Search one chat's work trace for the viewer named in `frame`; the frame the hub sends
    /// goes in, the body of the `ix_trace_result` comes out.
    fn trace(&self, frame: &Value) -> Value;
}
