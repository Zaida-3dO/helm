//! Claude Code's own chat records, read through Helm.
//!
//! Claude Code keeps every chat it has run as a JSONL record under `<home>/.claude/projects`,
//! one folder per working directory. This crate lists those chats, searches their text, shows one
//! read-only, and — the point of the whole thing — starts a new Helm chat that carries on from one
//! with Claude's full context (the new chat's session binding names the record's session id, so
//! the first launch resumes it as a fresh session).
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `claude_chat_list` | heavy | `{}` to [`ChatList`] (re-indexes first, newest first) |
//! | `claude_chat_search` | heavy | [`SearchArgs`] to [`SearchResults`] |
//! | `claude_chat_read` | heavy | [`ReadArgs`] to [`ChatRead`] |
//! | `claude_chat_fork` | maintenance | [`ForkArgs`] to [`Forked`] |
//!
//! The records are read-only to Helm: nothing here ever writes into `.claude`. The index lives in
//! two tables refreshed when the list or a search runs: one row per record, and an FTS5 table over
//! its user and assistant text. The newest records are indexed up to a file count and size limit;
//! bigger files and unparsable lines are skipped rather than failing the run.

pub mod bindings;
mod fork;
mod ops;
mod parse;
mod query;
mod store;
pub mod types;

pub use ops::register_ops;
pub use types::{
    ChatList, ChatRead, ChatRow, ForkArgs, Forked, SearchArgs, SearchHit, SearchResults,
};

use std::path::PathBuf;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("claudechats", store::init);
}

/// `<home>/.claude/projects`, when the home folder can be found. A debug-build environment
/// override lets tests use a fixture folder instead of the real one.
pub(crate) fn projects_root() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("HELM_TEST_CLAUDE_PROJECTS") {
        return Some(PathBuf::from(p));
    }
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)?;
    Some(home.join(".claude").join("projects"))
}
