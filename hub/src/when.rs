//! Dates in a search query ("a few months ago", "last week", "in March 2026").
//!
//! This is the contract the search calls; the phrase table behind it is filled in separately. Until
//! then no phrase is recognised and the query comes back unchanged with no window.

/// A span of time in UTC milliseconds. `vague` marks a window made from an imprecise phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from_ms: i64,
    pub to_ms: i64,
    pub vague: bool,
}

/// The query without its date phrase, and the window the phrase meant. `utc_offset_minutes` is
/// how far local time is ahead of UTC at `now_ms`.
pub fn parse(query: &str, _now_ms: i64, _utc_offset_minutes: i32) -> (String, Option<Window>) {
    (query.to_owned(), None)
}
