//! Seams other crates implement and register as services. The chats crate
//! defines the traits and calls them; it never depends on the crates that implement them.
//!
//! | Seam | Implemented by | Called from |
//! |---|---|---|
//! | [`SendContextBuilder`] (service [`SendContext`]) | `helm_panel` (panel context) | `chat_send`, for a composer send that carries `context` |
//! | [`ProjectGate`] (service [`ProjectGates`]) | `helm_projects` | `chat_restore` and `chat_create`: an archived project refuses both |
//! | [`AfkPolicy`] (installed with [`crate::ChatRuntime::set_afk_policy`]) | `helm_afk` | each new interaction a worker raises, and each turn's context block |
//!
//! A seam with nothing registered is skipped: a context payload is dropped (logged once per send)
//! and every project counts as open.

use crate::archive::Refusal;
use rusqlite::Transaction;
use serde_json::Value;
use std::sync::Arc;

/// The per-chat context generation: bumped on every worker launch and compaction, so a builder
/// that dedupes ("unchanged") knows when it must send the full block again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContextGen(pub u64);

/// The block attached to the user's message. The worker sends it to Claude Code as a separate
/// text content block after the message text; the transcript row shows only the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextBlock {
    pub text: String,
}

pub trait SendContextBuilder: Send + Sync {
    /// Build the block for `payload` (the renderer's strictly shaped panel-context value). An
    /// `Err` refuses the whole send, with the draft kept.
    fn build(
        &self,
        tx: &Transaction<'_>,
        chat_id: &str,
        payload: &Value,
        gen: ContextGen,
    ) -> Result<ContextBlock, Refusal>;
}

/// The service wrapper: `services.insert(Arc::new(SendContext(Arc::new(MyBuilder))))`.
#[derive(Clone)]
pub struct SendContext(pub Arc<dyn SendContextBuilder>);

pub trait ProjectGate: Send + Sync {
    /// True when the project is archived: new chats and single-chat restores are refused.
    fn project_archived(&self, tx: &Transaction<'_>, project_id: &str) -> rusqlite::Result<bool>;
}

/// The service wrapper for [`ProjectGate`].
#[derive(Clone)]
pub struct ProjectGates(pub Arc<dyn ProjectGate>);

/// What AFK does with one pending interaction.
#[derive(Clone, Debug, PartialEq)]
pub enum AfkAction {
    /// Not AFK's to handle: the card waits for the user as usual.
    Leave,
    /// Answer it now. `fields` are the answer's fields (`value`, `confirmed`); `marker` is the
    /// transcript line recording what AFK did.
    Answer {
        fields: serde_json::Map<String, Value>,
        marker: String,
    },
    /// Leave it waiting for the user, and say so in the transcript.
    Defer { marker: String },
}

/// AFK's say over a chat, asked by the chat's actor. Implemented by the AFK crate; with nothing
/// installed every chat behaves as if AFK were off.
pub trait AfkPolicy: Send + Sync {
    /// Text every new turn of this chat carries while AFK is on for it, `None` otherwise.
    fn turn_text(&self, chat_id: &str) -> Option<String>;
    /// What to do with an interaction the worker raised (or, with `at_flip`, one already waiting
    /// when AFK turned on). The implementation records what it decides.
    fn decide(
        &self,
        chat_id: &str,
        interaction: &crate::types::Interaction,
        at_flip: bool,
    ) -> AfkAction;
}
