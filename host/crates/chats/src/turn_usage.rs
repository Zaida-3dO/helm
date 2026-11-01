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
//! `chat_turn_usage.running`, so it survives a worker restart). The running totals started over
//! (a `/clear`, or a resume that restored nothing), and the whole of them is new, when they went
//! down, or when the difference is smaller than the turn's own main-conversation tokens, which
//! it always contains: a fresh process whose first turn happens to pass the saved totals.
//!
//! What cannot be told apart: a hard-killed process may have saved older totals than Helm last
//! saw, and a resume then restores those. The drop reads as a fresh start, so that first turn can
//! count part of the earlier spend again.

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
/// previous turn). `main` is the turn's own token count for the main conversation: a share
/// always holds at least that much, and it stands in when the running totals carry no token counts.
pub fn share(base: &Usage, running: &Usage, main: &Usage) -> Usage {
    let counted = tokens(running) > 0;
    // Every count grew, and by at least the main conversation's tokens in all (compared as one
    // sum, so a count the two reports file differently cannot trip it).
    let grew = running.cost_usd + 1e-12 >= base.cost_usd
        && running.input_tokens >= base.input_tokens
        && running.output_tokens >= base.output_tokens
        && running.cache_read_tokens >= base.cache_read_tokens
        && running.cache_write_tokens >= base.cache_write_tokens
        && (!counted || tokens(running) - tokens(base) >= tokens(main));
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
    if !counted {
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

/// What the chat's conversation now holds in its context, and when it was measured: the input,
/// cache-read and cache-write counts of the chat's newest turn — that turn's own request, which
/// is the whole conversation as the model last saw it — with that turn's time. The session's
/// running totals are not it: they add every turn's reading together, so a chat of a few turns
/// would read as several times its real size. `None` when no turn of the chat has reported
/// usage. This is the same reading the token chip, the context bar and the model-switch
/// capacity check use, so an answer from a tool never disagrees with what the chat shows.
pub fn context_reported_at(tx: &Transaction<'_>, chat_id: &str) -> Option<(u64, i64)> {
    let row: Option<(i64, i64, i64, i64)> = tx
        .query_row(
            "SELECT input_tokens, cache_read_tokens, cache_write_tokens, timestamp
             FROM chat_turn_usage WHERE chat_id = ?1
             ORDER BY timestamp DESC, rowid DESC LIMIT 1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .ok()?;
    let (input, read, write, at) = row?;
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    Some((n(input) + n(read) + n(write), at))
}

/// The context window a Claude model id gets: the ids that name the longer window take it
/// (a million tokens), everything else the standard two hundred thousand. Helm's own working
/// figure — a size rule only ever needs a close enough bound, and the binary's model names are
/// not asked for their windows.
pub fn claude_window(model_id: &str) -> u64 {
    if model_id.to_ascii_lowercase().contains("[1m]") {
        1_000_000
    } else {
        200_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_window_rule_reads_the_model_id() {
        assert_eq!(claude_window("claude-fable-5.5[1m]"), 1_000_000);
        assert_eq!(claude_window("Claude-Opus-9.9[1M]"), 1_000_000);
        assert_eq!(claude_window("claude-fable-5.5"), 200_000);
        // Nothing to switch on, and the picker's placeholder.
        assert_eq!(claude_window(""), 200_000);
        assert_eq!(claude_window("default"), 200_000);
    }
}
