//! What each turn used: its tokens and the cost Claude Code reported.
//!
//! A reply's own usage (from the request that produced it) stays on its message row, where the
//! token chip and the context size read it. What a turn cost is worked out once, when Claude Code
//! reports the turn's result, and written to the session log as a `usage` entry, projected into
//! `chat_turn_usage`. Cost reports read that table only, so nothing is counted twice.
//!
//! Claude Code reports cost as a running total for the whole process, subagents included, and a
//! process that resumes a session starts from that session's saved total. So a turn's share is the
//! running total minus the one seen after the previous turn of the same session (kept in
//! `chat_turn_usage.running`, so it survives a worker restart). When the running total goes down,
//! it started over (a `/clear`, or a resume that restored nothing), and the whole of it is new.

use crate::types::Usage;
use rusqlite::{OptionalExtension, Transaction};
use serde_json::Value;

/// A usage object in either spelling: Helm's (`inputTokens`, `costUsd`) or the Messages API's
/// (`input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`). `None` when it has
/// no token count at all.
pub fn parse(u: &Value) -> Option<Usage> {
    let n = |camel: &str, snake: &str| {
        u.get(camel)
            .or_else(|| u.get(snake))
            .and_then(Value::as_i64)
    };
    let input = n("inputTokens", "input_tokens");
    let output = n("outputTokens", "output_tokens");
    let read = n("cacheReadTokens", "cache_read_input_tokens");
    let write = n("cacheWriteTokens", "cache_creation_input_tokens");
    if input.is_none() && output.is_none() && read.is_none() && write.is_none() {
        return None;
    }
    Some(Usage {
        input_tokens: input.unwrap_or(0),
        output_tokens: output.unwrap_or(0),
        cache_read_tokens: read.unwrap_or(0),
        cache_write_tokens: write.unwrap_or(0),
        cost_usd: u.get("costUsd").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

fn tokens(u: &Usage) -> i64 {
    u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens
}

/// The turn's share of `running`, measured from `base` (the running totals after the session's
/// previous turn). `main` is the turn's own token count for the main conversation, used when the
/// running totals carry no token counts.
pub fn share(base: &Usage, running: &Usage, main: &Usage) -> Usage {
    let grew = running.cost_usd + 1e-12 >= base.cost_usd
        && running.input_tokens >= base.input_tokens
        && running.output_tokens >= base.output_tokens
        && running.cache_read_tokens >= base.cache_read_tokens
        && running.cache_write_tokens >= base.cache_write_tokens;
    let mut d = if grew {
        Usage {
            input_tokens: running.input_tokens - base.input_tokens,
            output_tokens: running.output_tokens - base.output_tokens,
            cache_read_tokens: running.cache_read_tokens - base.cache_read_tokens,
            cache_write_tokens: running.cache_write_tokens - base.cache_write_tokens,
            cost_usd: (running.cost_usd - base.cost_usd).max(0.0),
        }
    } else {
        running.clone()
    };
    if tokens(running) == 0 {
        d.input_tokens = main.input_tokens;
        d.output_tokens = main.output_tokens;
        d.cache_read_tokens = main.cache_read_tokens;
        d.cache_write_tokens = main.cache_write_tokens;
    }
    d
}

/// True when the share holds nothing worth a record.
pub fn is_empty(u: &Usage) -> bool {
    tokens(u) == 0 && u.cost_usd <= 0.0
}

/// The running totals recorded after the newest turn of `session`, in any chat (a fork resumes
/// its source's session, and Claude Code restores that session's totals).
pub fn baseline(tx: &Transaction<'_>, session: &str) -> rusqlite::Result<Option<Usage>> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT running FROM chat_turn_usage WHERE native_session = ?1
             ORDER BY timestamp DESC, rowid DESC LIMIT 1",
            [session],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
}
