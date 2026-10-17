//! Chat search.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `chat_search` | heavy | [`SearchArgs`] to [`SearchResult`] |
//! | `search_open_record` | maintenance | [`OpenArgs`] to `null` (counts one open of a chat for the ranking boost) |
//! | `search_signals_clear` | maintenance | `{}` to `null` (forgets every recorded open) |
//!
//! Message text comes from `helm_chats`' `chat_search_source` view, which already leaves out
//! inactive edit branches, hidden side chats and, unless asked for, archived chats. Only the first
//! twelve words of a query count; each becomes a quoted prefix term and all must match. A chat also
//! matches when its title contains the whole query, ignoring case, and those chats come first.
//! Ranking applies the universal-search boost (see `signals`).

pub mod bindings;
mod ops;
mod query;
mod rank;
mod search;
mod signals;
pub mod types;

pub use ops::register_ops;
pub use types::{OpenArgs, SearchArgs, SearchHit, SearchResult};

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("search-opens", signals::init);
}
