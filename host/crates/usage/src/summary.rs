//! `app_usage_summary`: a bounded, read-only digest of this machine's own usage records.
//!
//! The records are the per-turn token counts and reported cost the chats crate keeps in
//! `chat_turn_usage` (subagents included), so the digest reads Helm's database and nothing else:
//! no network. Cost is what the binary reported, never an estimate. A turn's model is its chat's
//! current model, since the rows themselves do not record one. A fork's copied history counts for
//! the chat it came from only (see [`OWN_USAGE`]). Turns from before Helm recorded them are not
//! there at all; a cost report says so ([`CostReport::uncounted_replies`]). Periods named by words
//! (`today`, `morning`) and the day buckets follow this machine's clock, not the UTC day.
//!
//! `plan: true` attaches the meter's own plan answer ([`plan_block`]): the 5-hour and weekly
//! windows as percent used, percent left and reset time, whether a window is at its limit now, and
//! each reading's age and source. `fresh: true` asks the binary for a reading first, on the spot
//! ([`crate::service::UsageService::fresh_read`]), under the meter's one-read-per-minute floor.
//! Nothing the plan part carries is a key.
//!
//! A `chatId` (or a `session`, the Claude Code session id) narrows the figures to one chat and
//! adds a `chat` block: its token counts, its cost, an estimated share of the plan's weekly window
//! (the same estimate [`cost_report`] makes), and its current context size — the reading
//! [`helm_chats::turn_usage::context_reported_at`] gives the token chip and the context bar — over
//! its model's window. `tree: true` widens a chat's figures to its child chats, forks and chats
//! sharing its session.
//!
//! `usage_cost` ([`cost_report`]) reads the same records for one chat or project, and adds an
//! estimated share of the plan's weekly window from the meter's last reading.
//!
//! **Chats on local models are kept apart.** Their turns cost nothing and never count toward the
//! plan, so the totals, models, days and weekly shares here are Claude's alone. What the local
//! chats used is a separate `local` block, with what the same tokens would have cost on a Claude
//! model ([`crate::prices`]).

use crate::prices::{price, would_cost, Price, PRICES_READ};
use crate::service::FreshOutcome;
use crate::types::{
    ChatCost, CostReport, CostScope, CostTotals, LocalTrack, ReadinessState, UsageSnapshot,
    UsageSource, UsageWindow, WeeklyShare,
};
use helm_core::db::Database;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use rusqlite::ToSql;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Chats listed in a digest.
const TOP_CHATS: usize = 10;
/// Days listed in a digest (the newest).
const MAX_DAYS: usize = 31;
/// The answer's size bound.
const MAX_BYTES: usize = 8 * 1024;
/// Chats a tree takes in at most.
const TREE_MAX: usize = 100;
/// Plan windows listed.
const MAX_PER_MODEL: usize = 3;
/// The control read a `fresh: true` call may take, inside the tool's own 20-second answer.
const FRESH_BUDGET_SECS: u64 = 13;
const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;
const WEEK_MS: i64 = 7 * DAY_MS;

#[derive(Default, Clone, Copy)]
struct Sums {
    turns: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
}

impl Sums {
    fn add(&mut self, o: &Sums) {
        self.turns += o.turns;
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cost += o.cost;
    }

    fn json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("turns".into(), json!(self.turns));
        m.insert("inputTokens".into(), json!(self.input));
        m.insert("outputTokens".into(), json!(self.output));
        m.insert("cacheReadTokens".into(), json!(self.cache_read));
        m.insert("cacheWriteTokens".into(), json!(self.cache_write));
        m.insert(
            "costUsd".into(),
            json!((self.cost * 10_000.0).round() / 10_000.0),
        );
        m
    }

    fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// A local chat's (or a group's) use, priced against `p`.
fn track(s: &Sums, p: &Price) -> LocalTrack {
    LocalTrack {
        turns: s.turns,
        input_tokens: s.input,
        output_tokens: s.output,
        cache_read_tokens: s.cache_read,
        cache_write_tokens: s.cache_write,
        total_tokens: s.total(),
        would_cost_usd: would_cost(p, s.input, s.output, s.cache_read, s.cache_write),
        compared_with: p.key.to_owned(),
        compared_name: p.name.to_owned(),
        prices_read: PRICES_READ.to_owned(),
    }
}

fn refuse(m: impl Into<String>) -> ToolError {
    ToolError::refused(m)
}

/// An instant given as Unix milliseconds, a UTC date or an RFC 3339 time.
fn instant(v: &Value, what: &str) -> Result<i64, ToolError> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| refuse(format!("{what} must be a whole number of milliseconds."))),
        Value::String(s) => {
            let s = s.trim();
            if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
                return Ok(d.timestamp_millis());
            }
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|d| d.and_utc().timestamp_millis())
                .ok_or_else(|| refuse(format!("{what} must be YYYY-MM-DD or an RFC 3339 time.")))
        }
        _ => Err(refuse(format!("{what} must be a time."))),
    }
}

/// The `[since, until)` window in milliseconds the arguments ask for, and its label. `off` is the
/// machine's offset from UTC in milliseconds: the word periods and their labels follow the
/// machine's clock (a west-of-UTC chat used in its local morning would otherwise miss the
/// "today" window entirely).
fn window(a: &Value, now: i64, off: i64) -> Result<(i64, i64, String), ToolError> {
    let explicit = a.get("since").is_some() || a.get("until").is_some();
    let groups = explicit as u8
        + u8::from(a.get("lastHours").is_some())
        + u8::from(a.get("range").is_some());
    if groups > 1 {
        return Err(refuse("Give one of range, lastHours or since/until."));
    }
    if explicit {
        let since = a
            .get("since")
            .map(|v| instant(v, "since"))
            .transpose()?
            .unwrap_or(0);
        let until = a
            .get("until")
            .map(|v| instant(v, "until"))
            .transpose()?
            .unwrap_or(now + 1);
        if until <= since {
            return Err(refuse("until must be after since."));
        }
        return Ok((since, until, "custom".into()));
    }
    if let Some(v) = a.get("lastHours") {
        let n = v
            .as_i64()
            .filter(|n| (1..=744).contains(n))
            .ok_or_else(|| refuse("lastHours must be a whole number of hours from 1 to 744."))?;
        return Ok((now - n * HOUR_MS, now + 1, format!("{n}h")));
    }
    let range = a.get("range").and_then(Value::as_str).unwrap_or("7d");
    let local = now + off;
    let local_midnight = local - local.rem_euclid(DAY_MS);
    let day_start = local_midnight - off;
    let since = match range {
        "today" => day_start,
        // The morning starts at six o'clock by this machine's clock; before six, the young day
        // is the morning.
        "morning" => {
            let six = local_midnight + 6 * HOUR_MS - off;
            if six <= now {
                six
            } else {
                day_start
            }
        }
        "24h" => now - DAY_MS,
        "7d" => now - 7 * DAY_MS,
        "30d" => now - 30 * DAY_MS,
        "all" => 0,
        _ => return Err(refuse("range is today, morning, 24h, 7d, 30d or all.")),
    };
    Ok((since, now + 1, range.into()))
}

/// `?` for `n` values, comma separated, for an `IN (…)`.
fn qmarks(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// A chat, its title, model, day and sums, and whether the chat runs on a local model.
type Row = (String, String, String, i64, Sums, bool);

/// The chat ids and the note that came with resolving them.
struct Scope {
    /// The chats whose turns are counted.
    ids: Vec<String>,
    /// The chat the `chat` block describes.
    primary: String,
    /// Plain sentences about the scope (several chats share a session, the tree was cut).
    notes: Vec<String>,
}

fn read_rows(
    db: &Database,
    since: i64,
    until: i64,
    chats: Option<&[String]>,
    off: i64,
) -> Result<Vec<Row>, ToolError> {
    db.snapshot(|tx| {
        let pred = match chats {
            None => "1".to_owned(),
            Some(ids) => format!("c.id IN ({})", qmarks(ids.len())),
        };
        let mut st = tx.prepare(&format!(
            "SELECT c.id, c.title, COALESCE(c.model, 'default'), (u.timestamp + ?1) / 86400000,
                    COUNT(*), {SUMS}, u.provider <> 'claude'
             FROM chat_turn_usage u JOIN chats c ON c.id = u.chat_id
             WHERE {OWN_USAGE} AND u.timestamp >= ?2 AND u.timestamp < ?3 AND {pred}
             GROUP BY c.id, 3, 4, u.provider"
        ))?;
        let mut args: Vec<&dyn ToSql> = vec![&off, &since, &until];
        if let Some(ids) = chats {
            args.extend(ids.iter().map(|s| -> &dyn ToSql { s }));
        }
        let it = st.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                Sums {
                    turns: r.get(4)?,
                    input: r.get(5)?,
                    output: r.get(6)?,
                    cache_read: r.get(7)?,
                    cache_write: r.get(8)?,
                    cost: r.get(9)?,
                },
                r.get::<_, bool>(10)?,
            ))
        })?;
        it.collect::<rusqlite::Result<Vec<Row>>>()
    })
    .map_err(|e| refuse(format!("The usage records could not be read: {e}")))
}

/// Everything about the chats that are in scope: `None` when the answer covers the whole machine.
fn scope_of(db: &Database, a: &Value) -> Result<Option<Scope>, ToolError> {
    let chat_id = a.get("chatId").and_then(Value::as_str);
    let session = a.get("session").and_then(Value::as_str);
    let tree = a.get("tree").and_then(Value::as_bool).unwrap_or(false);
    if chat_id.is_some() && session.is_some() {
        return Err(refuse("Give chatId or session, not both."));
    }
    if chat_id.is_none() && session.is_none() {
        if tree {
            return Err(refuse("tree needs chatId or session."));
        }
        return Ok(None);
    }
    // One id can name a chat directly or a Claude Code session several chats carry.
    let found = db
        .snapshot::<_, rusqlite::Error>(|tx| {
            if let Some(id) = chat_id {
                let ok: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)",
                    [id],
                    |r| r.get(0),
                )?;
                return Ok(ok.then(|| (id.to_owned(), 1usize)));
            }
            let s = session.unwrap();
            let mut st = tx.prepare(
                "SELECT chat_id, MAX(timestamp) AS t FROM chat_turn_usage
                 WHERE native_session = ?1 GROUP BY chat_id ORDER BY t DESC",
            )?;
            let mut ids: Vec<String> = st
                .query_map([s], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if ids.is_empty() {
                let ok: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?1)",
                    [s],
                    |r| r.get(0),
                )?;
                if ok {
                    ids.push(s.to_owned());
                }
            }
            let multi = ids.len();
            Ok(ids.into_iter().next().map(|p| (p, multi)))
        })
        .map_err(|e| refuse(format!("The usage records could not be read: {e}")))?;
    let (primary, shared) = found.ok_or_else(|| {
        refuse(if chat_id.is_some() {
            "There is no such chat."
        } else {
            "No chat here carries that Claude Code session id."
        })
    })?;
    let mut notes = Vec::new();
    if shared > 1 {
        notes.push(format!(
            "The session id names {shared} chats; the `chat` block is the newest one's. Add tree: true to take in all of them."
        ));
    }
    let ids = if tree {
        let all = db
            .snapshot(|tx| tree_chats(tx, std::slice::from_ref(&primary)))
            .map_err(|e| refuse(format!("The usage records could not be read: {e}")))?;
        if all.len() >= TREE_MAX {
            notes.push(format!(
                "The tree was taken in up to {TREE_MAX} chats; the rest are not counted."
            ));
        }
        all
    } else {
        vec![primary.clone()]
    };
    Ok(Some(Scope {
        ids,
        primary,
        notes,
    }))
}

/// `from` and everything growing out of it: child chats, chats forked from them, and chats that
/// carry one of their Claude Code session ids (a fork resumes its source's session; an app-created
/// child gets its own). [`OWN_USAGE`] keeps an inherited turn with the chat it came from, so a
/// chat reached twice contributes its own turns once.
fn tree_chats(tx: &rusqlite::Transaction<'_>, from: &[String]) -> rusqlite::Result<Vec<String>> {
    let mut all: BTreeSet<String> = from.iter().cloned().collect();
    let mut frontier: Vec<String> = from.to_vec();
    while !frontier.is_empty() && all.len() < TREE_MAX {
        let ph = qmarks(frontier.len());
        let mut found: BTreeSet<String> = BTreeSet::new();
        let mut take = |sql: String| -> rusqlite::Result<()> {
            let mut st = tx.prepare(&sql)?;
            for id in st.query_map(rusqlite::params_from_iter(frontier.iter()), |r| {
                r.get::<_, String>(0)
            })? {
                found.insert(id?);
            }
            Ok(())
        };
        take(format!(
            "SELECT id FROM chats WHERE parent_chat_id IN ({ph})"
        ))?;
        take(format!(
            "SELECT chat_id FROM chat_forks WHERE source_chat_id IN ({ph})"
        ))?;
        take(format!(
            "SELECT DISTINCT chat_id FROM chat_turn_usage
             WHERE native_session IN (SELECT DISTINCT native_session FROM chat_turn_usage
                                      WHERE chat_id IN ({ph}) AND native_session IS NOT NULL)"
        ))?;
        frontier = found.difference(&all).cloned().collect();
        for id in frontier.iter() {
            if all.len() >= TREE_MAX {
                break;
            }
            all.insert(id.clone());
        }
    }
    Ok(all.into_iter().collect())
}

/// What the caller knows that the database does not: the instant to measure periods against, the
/// machine's UTC offset, the meter's weekly window `(percent used, resets at)`, and where to look
/// up a local model's context window (by its one-string `local/<server>/<model>` id).
pub struct DigestEnv<'a> {
    pub now: i64,
    pub local_offset_ms: i64,
    pub weekly: Option<(f64, i64)>,
    pub local_window: &'a dyn Fn(&str) -> Option<u64>,
}

/// Build the digest.
pub fn digest(db: &Database, a: &Value, compare: &str, env: &DigestEnv) -> ToolResult {
    let (since, until, label) = window(a, env.now, env.local_offset_ms)?;
    let scope = scope_of(db, a)?;
    let rows = read_rows(
        db,
        since,
        until,
        scope.as_ref().map(|s| s.ids.as_slice()),
        env.local_offset_ms,
    )?;
    let p = price(compare);
    let mut local = Sums::default();
    let mut local_models: BTreeMap<String, Sums> = BTreeMap::new();
    let mut local_chats: BTreeMap<String, (String, Sums)> = BTreeMap::new();

    let mut total = Sums::default();
    let mut by_model: BTreeMap<String, Sums> = BTreeMap::new();
    let mut by_day: BTreeMap<i64, Sums> = BTreeMap::new();
    let mut by_chat: BTreeMap<String, (String, Sums)> = BTreeMap::new();
    for (id, title, model, day, s, is_local) in &rows {
        if *is_local {
            local.add(s);
            local_models.entry(model.clone()).or_default().add(s);
            local_chats
                .entry(id.clone())
                .or_insert_with(|| (title.clone(), Sums::default()))
                .1
                .add(s);
            continue;
        }
        total.add(s);
        by_model.entry(model.clone()).or_default().add(s);
        by_day.entry(*day).or_default().add(s);
        by_chat
            .entry(id.clone())
            .or_insert_with(|| (title.clone(), Sums::default()))
            .1
            .add(s);
    }
    let mut chats: Vec<(String, (String, Sums))> = by_chat.into_iter().collect();
    chats.sort_by_key(|(_, (_, s))| std::cmp::Reverse(s.total()));
    let chats_total = chats.len();
    let chats: Vec<Value> = chats
        .into_iter()
        .take(TOP_CHATS)
        .map(|(id, (title, s))| {
            let mut m = s.json();
            m.insert("chatId".into(), json!(id));
            m.insert(
                "title".into(),
                json!(title.chars().take(80).collect::<String>()),
            );
            Value::Object(m)
        })
        .collect();
    let days: Vec<Value> = by_day
        .iter()
        .rev()
        .take(MAX_DAYS)
        .rev()
        .map(|(d, s)| {
            let mut m = s.json();
            // The bucket number is the turn's local wall-clock day (its milliseconds plus the
            // machine's offset, over a day), so `d` days read as a UTC date is the local date.
            let date = chrono::DateTime::from_timestamp_millis(d * DAY_MS)
                .map(|t| t.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            m.insert("day".into(), json!(date));
            Value::Object(m)
        })
        .collect();
    let mut models: Vec<(&String, &Sums)> = by_model.iter().collect();
    models.sort_by_key(|(_, s)| std::cmp::Reverse(s.total()));
    let models: Vec<Value> = models
        .into_iter()
        .map(|(name, s)| {
            let mut m = s.json();
            m.insert("model".into(), json!(name));
            Value::Object(m)
        })
        .collect();

    let mut out = json!({
        "range": label,
        "sinceMs": since,
        "untilMs": until.min(env.now + 1),
        "total": Value::Object(total.json()),
        "byModel": models,
        "byDay": days,
        "topChats": chats,
        "chatsWithUsage": chats_total,
        "notes": "Local records only: each turn's token counts (Claude Code subagents included) and the cost the binary reported. A model is the chat's current model. today, morning and the day buckets follow this machine's clock. Chats on local models are not in these figures; see local.",
    });
    if local.turns > 0 {
        out["local"] = local_block(&local, &local_models, local_chats, &p);
    }
    if let Some(s) = &scope {
        out["forChat"] = json!(s.primary);
        out["scopeChats"] = json!(s.ids.len());
        out["chat"] = chat_block(db, s, &rows, env)?;
    }
    trim(&mut out);
    Ok(out)
}

/// What the chat's conversation holds now and the window of the model it runs on: the newest
/// recorded turn's measured context ([`helm_chats::turn_usage::context_reported_at`] — the last
/// call that reported usage, not a running total) over the model's real window: the standard
/// figure for a Claude model id, the size the local server last reported for a local id. It comes
/// with the model id (one string; a local chat's carries its server) and provider. This one
/// reading backs both the digest's `chat` block and the `chat_context` op that feeds the chat's
/// context line, so the two never disagree.
pub fn chat_context(
    db: &Database,
    chat_id: &str,
    local_window: &dyn Fn(&str) -> Option<u64>,
) -> Result<(String, Option<String>, Value), rusqlite::Error> {
    let (model_id, provider, ctx) = db.snapshot::<_, rusqlite::Error>(|tx| {
        let (model_id, provider): (String, Option<String>) = tx.query_row(
            &format!(
                "SELECT COALESCE({}, 'default'), provider FROM chats c WHERE c.id = ?1",
                helm_chats::local::MODEL_ID_SQL
            ),
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok::<_, rusqlite::Error>((
            model_id,
            provider,
            helm_chats::turn_usage::context_reported_at(tx, chat_id),
        ))
    })?;
    // The context size and its window: the standard figure for a Claude model id, the server's own
    // last-reported figure for a local model, unknown when the server has not said.
    let (window, window_source) = match &provider {
        None => (
            Some(helm_chats::turn_usage::claude_window(&model_id)),
            "the standard window for this kind of Claude model id".to_owned(),
        ),
        Some(_) => (
            local_window(&model_id),
            "the size the local model server last reported".to_owned(),
        ),
    };
    let context = match ctx {
        Some((tokens, at)) => {
            let pct = window.map(|w| (tokens as f64 * 100.0 / w as f64 * 100.0).round() / 100.0);
            json!({
                "contextTokens": tokens,
                "measuredAtMs": at,
                "windowTokens": window,
                "windowPct": pct,
                "windowSource": window_source,
                "notes": "The conversation this chat's newest recorded turn reported in its context: the input, cache-read and cache-write tokens held at that moment, the same reading the chat's token chip shows. windowPct is that over the model's window (over 100 means it no longer fits).",
            })
        }
        None => json!({
            "contextTokens": null,
            "notes": "No turn of this chat has reported usage yet, so its context size has not been measured.",
        }),
    };
    Ok((model_id, provider, context))
}

/// The `chat` block: what the scoped chats used in the window, and the primary chat's context
/// size over its model's window.
fn chat_block(
    db: &Database,
    scope: &Scope,
    rows: &[Row],
    env: &DigestEnv,
) -> Result<Value, ToolError> {
    let mut toks = Sums::default();
    let mut cost = 0.0;
    let mut has_local = false;
    for (_, _, _, _, s, is_local) in rows {
        toks.turns += s.turns;
        toks.input += s.input;
        toks.output += s.output;
        toks.cache_read += s.cache_read;
        toks.cache_write += s.cache_write;
        if *is_local {
            has_local = true;
        } else {
            cost += s.cost;
        }
    }
    // The chat's title.
    let title = db
        .snapshot::<_, rusqlite::Error>(|tx| {
            tx.query_row(
                "SELECT title FROM chats WHERE id = ?1",
                [&scope.primary],
                |r| r.get::<_, String>(0),
            )
        })
        .map_err(|e| refuse(format!("The usage records could not be read: {e}")))?;
    let (model_id, provider, context) = chat_context(db, &scope.primary, env.local_window)
        .map_err(|e| refuse(format!("The usage records could not be read: {e}")))?;

    let mut notes = scope.notes.clone();

    let mut block = json!({
        "chatId": scope.primary,
        "title": title.chars().take(80).collect::<String>(),
        "model": model_id,
        "tokens": {
            "turns": toks.turns,
            "inputTokens": toks.input,
            "outputTokens": toks.output,
            "cacheReadTokens": toks.cache_read,
            "cacheWriteTokens": toks.cache_write,
            "totalTokens": toks.total(),
            "costUsd": (cost * 10_000.0).round() / 10_000.0,
        },
        "context": context,
    });
    // The estimated weekly share, the way a cost report estimates it: Claude's chats only, split
    // from the plan's percentage by cost since the window began.
    if provider.is_none() {
        if let Some((pct, resets)) = env.weekly.filter(|(_, r)| *r > env.now) {
            let (scope_cost, helm_cost) = week_costs(db, &scope.ids, resets - WEEK_MS)
                .map_err(|e| refuse(format!("The usage records could not be read: {e}")))?;
            let share = if helm_cost > 0.0 {
                (pct * scope_cost / helm_cost * 1000.0).round() / 1000.0
            } else {
                0.0
            };
            block["weeklyWindow"] =
                json!({"startMs": resets - WEEK_MS, "resetsAtMs": resets, "weekPctUsed": pct});
            block["weeklySharePct"] = json!(share);
            notes.push(
                "weeklySharePct is an estimate: the plan reports one percentage for the whole week, split by cost since the window began across all Helm chats.".to_owned(),
            );
        }
    } else {
        notes.push(
            "A chat on a local model costs nothing and takes no share of the plan.".to_owned(),
        );
    }
    if scope.ids.len() > 1 {
        notes.push(format!(
            "tokens, costUsd and weeklySharePct cover {} chats (this one, its children, forks and session siblings); context is this chat's own.",
            scope.ids.len()
        ));
    }
    if has_local {
        notes.push(
            "A chat on a local model is in the token counts but not the cost or the weekly share."
                .to_owned(),
        );
    }
    if !notes.is_empty() {
        block["notes"] = Value::Array(notes.into_iter().map(Value::String).collect());
    }
    Ok(block)
}

/// What the scoped Claude chats and all Helm chats cost since `from`.
fn week_costs(db: &Database, ids: &[String], from: i64) -> Result<(f64, f64), rusqlite::Error> {
    db.snapshot(|tx| {
        let ph = qmarks(ids.len());
        let mut args: Vec<&dyn ToSql> = vec![&from];
        args.extend(ids.iter().map(|s| -> &dyn ToSql { s }));
        let scope = tx.query_row(
            &format!(
                "SELECT COALESCE(SUM(u.cost_usd), 0.0)
                 FROM chat_turn_usage u JOIN chats c ON c.id = u.chat_id
                 WHERE {OWN_USAGE} AND u.provider = 'claude' AND u.timestamp >= ?1
                   AND c.id IN ({ph})"
            ),
            rusqlite::params_from_iter(args.iter()),
            |r| r.get(0),
        )?;
        let helm: f64 = tx.query_row(
            &format!(
                "SELECT COALESCE(SUM(u.cost_usd), 0.0)
                 FROM chat_turn_usage u JOIN chats c ON c.id = u.chat_id
                 WHERE u.timestamp >= ?1 AND {OWN_USAGE} AND u.provider = 'claude'"
            ),
            [from],
            |r| r.get(0),
        )?;
        Ok::<_, rusqlite::Error>((scope, helm))
    })
}

/// The `local` block of the digest: what chats on local models used, with what it would have cost
/// on Claude. The models and chats listed are the heaviest few.
fn local_block(
    total: &Sums,
    models: &BTreeMap<String, Sums>,
    chats: BTreeMap<String, (String, Sums)>,
    p: &Price,
) -> Value {
    let mut block = serde_json::to_value(track(total, p)).unwrap_or(Value::Null);
    block["costUsd"] = json!(0);
    let mut by_model: Vec<(&String, &Sums)> = models.iter().collect();
    by_model.sort_by_key(|(_, s)| std::cmp::Reverse(s.total()));
    block["byModel"] = Value::Array(
        by_model
            .into_iter()
            .take(TOP_LOCAL)
            .map(|(name, s)| {
                let mut v = serde_json::to_value(track(s, p)).unwrap_or(Value::Null);
                v["model"] = json!(name);
                v
            })
            .collect(),
    );
    let mut by_chat: Vec<(String, (String, Sums))> = chats.into_iter().collect();
    by_chat.sort_by_key(|(_, (_, s))| std::cmp::Reverse(s.total()));
    block["chats"] = Value::Array(
        by_chat
            .into_iter()
            .take(TOP_LOCAL)
            .map(|(id, (title, s))| {
                let mut v = serde_json::to_value(track(&s, p)).unwrap_or(Value::Null);
                v["chatId"] = json!(id);
                v["title"] = json!(title.chars().take(80).collect::<String>());
                v
            })
            .collect(),
    );
    block["notes"] = json!("Chats on local models cost nothing and are not in the plan's figures or in the totals above. wouldCostUsd is an estimate of what the same tokens would have cost on the Claude model named in comparedWith at published API prices read on pricesRead; it is not a bill.");
    block
}

/// Models and chats listed in the digest's local block.
const TOP_LOCAL: usize = 5;

/// Bounded: drop the tails of the longest lists first if the answer is over the size bound.
fn trim(out: &mut Value) {
    for key in ["topChats", "byDay"] {
        while serde_json::to_vec(out).map(|v| v.len()).unwrap_or(0) > MAX_BYTES {
            let Some(list) = out[key].as_array_mut().filter(|l| !l.is_empty()) else {
                break;
            };
            list.pop();
            out["truncated"] = json!(true);
        }
    }
}

/// The plan part of the answer, from the meter's snapshot: the two big windows as percent used and
/// left, reset time, age and source, whether any window is at its limit right now, and — when the
/// caller asked for a fresh reading — what that came down to. Nothing in here is a key.
fn plan_block(snap: &UsageSnapshot, now: i64, fresh: Option<FreshOutcome>) -> Value {
    let at_limit =
        |w: &UsageWindow| w.pct.is_some_and(|p| p >= 100.0) && w.resets_at.is_some_and(|t| t > now);
    let src = |s: UsageSource| match s {
        UsageSource::Control => "a control request to Claude Code",
        UsageSource::RateLimitEvent => "a rate-limit line a chat saw",
    };
    let win = |w: &UsageWindow| {
        json!({
            "pctUsed": w.pct,
            "pctLeft": w.pct.map(|p| ((100.0 - p).max(0.0) * 10.0).round() / 10.0),
            "resetsAtMs": w.resets_at,
            "readAtMs": w.at,
            "ageMs": now - w.at,
            "atLimit": at_limit(w),
            "source": src(w.source),
        })
    };
    let mut out = json!({
        "meterState": if snap.readiness.state == ReadinessState::Ready { "ready" } else { "inactive" },
        "meterReason": snap.readiness.reason,
        "refreshing": snap.refreshing,
        "notes": "Claude Code reports each plan window as a percentage used; Helm never learns the absolute limits. atLimit is true when a window has reached 100 percent with its reset still ahead; the plan is limited now when that is true for any window. source says where that window's newest number came from. The meter reads at most once a minute.",
    });
    if let Some(v) = &snap.readiness.binary_version {
        out["binaryVersion"] = json!(v);
    }
    match &snap.reading {
        Some(r) => {
            out["plan"] = json!(r.plan);
            out["fetchedAtMs"] = json!(r.fetched_at);
            out["ageMs"] = json!(now - r.fetched_at);
            out["stale"] = json!(r.stale);
            out["fiveHour"] = r.five_hour.as_ref().map(win).unwrap_or(Value::Null);
            out["weekly"] = r.weekly.as_ref().map(win).unwrap_or(Value::Null);
            out["atLimit"] = json!(
                r.five_hour.as_ref().is_some_and(at_limit)
                    || r.weekly.as_ref().is_some_and(at_limit)
                    || r.per_model.iter().any(|m| m.pct.is_some_and(|p| p >= 100.0)
                        && m.resets_at.is_some_and(|t| t > now))
            );
            out["perModel"] = Value::Array(
                r.per_model
                    .iter()
                    .take(MAX_PER_MODEL)
                    .map(|m| {
                        json!({
                            "model": m.model,
                            "pctUsed": m.pct,
                            "resetsAtMs": m.resets_at,
                            "readAtMs": m.at,
                            "ageMs": now - m.at,
                            "source": src(m.source),
                        })
                    })
                    .collect(),
            );
            out["perModelCount"] = json!(r.per_model.len());
        }
        None => {
            out["reading"] = Value::Null;
            out["notes"] = json!(format!(
                "{} There is no reading yet, so no percentages or reset times can be shown.",
                out["notes"].as_str().unwrap_or_default()
            ));
        }
    }
    if let Some(next) = snap.next_refresh_at {
        out["nextReadAtMs"] = json!(next);
    }
    if let Some(f) = fresh {
        let (outcome, note) = match f {
            FreshOutcome::Read => (
                "read",
                "A fresh read from Claude Code ran; the numbers in this block are its answer.",
            ),
            FreshOutcome::Failed => (
                "failed",
                "A fresh read was tried; Claude Code gave no numbers. meterReason says why.",
            ),
            FreshOutcome::InProgress => (
                "already running",
                "A read was already running, so the newest numbers on hand are shown.",
            ),
            FreshOutcome::TooSoon => (
                "too soon",
                "The meter reads at most once a minute, so nothing new was read; ageMs says how old what is shown is.",
            ),
        };
        out["fresh"] = json!({"outcome": outcome, "note": note});
    }
    out
}

const OWN_USAGE: &str = "u.timestamp >= c.created_at";

/// The token and cost sums over `chat_turn_usage u`.
const SUMS: &str = "SUM(u.input_tokens), SUM(u.output_tokens), SUM(u.cache_read_tokens),
     SUM(u.cache_write_tokens), SUM(u.cost_usd)";

fn totals(s: &Sums) -> CostTotals {
    CostTotals {
        input_tokens: s.input,
        output_tokens: s.output,
        cache_read_tokens: s.cache_read,
        cache_write_tokens: s.cache_write,
        total_tokens: s.total(),
        cost_usd: s.cost,
    }
}

/// `usage_cost`: what one chat, or every chat in a project and its sub-projects, used. Archived
/// chats count. `weekly` is the meter's weekly window as `(percent used, resets at)`.
///
/// The weekly share is an estimate: the plan reports only one percentage for everything, so it is
/// split in proportion to cost since the window began (its reset time minus seven days), across all
/// Helm chats. Cost rather than tokens, because cost already weighs model and cache pricing, which
/// is closer to how the limit is spent than a raw token count. It assumes the week's usage all
/// came through Helm.
pub fn cost_report(
    db: &Database,
    scope: CostScope,
    id: &str,
    weekly: Option<(f64, i64)>,
    compare: &str,
    now: i64,
) -> Result<CostReport, String> {
    let window = weekly.filter(|(_, resets)| *resets > now);
    let since = window.map_or(i64::MAX, |(_, r)| r - WEEK_MS);
    let p = price(compare);
    // A chat, its title, whether it is archived, its sums, its cost in the weekly window and
    // whether it runs on a local model.
    type Line = (String, String, bool, Sums, f64, bool);
    let read = db.snapshot(|tx| {
        let name: Option<String> = match scope {
            CostScope::Chat => tx.query_row("SELECT title FROM chats WHERE id = ?1", [id], |r| r.get(0)),
            CostScope::Project => tx.query_row("SELECT name FROM projects WHERE id = ?1", [id], |r| r.get(0)),
        }
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
        let Some(name) = name else {
            return Ok(None);
        };
        let chats = match scope {
            CostScope::Chat => "c.id = ?1",
            CostScope::Project => {
                "c.project_id IN (WITH RECURSIVE tree(id) AS (SELECT ?1 UNION
                     SELECT pp.project_id FROM project_parents pp JOIN tree t ON pp.parent_id = t.id)
                 SELECT id FROM tree)"
            }
        };
        let mut st = tx.prepare(&format!(
            "SELECT c.id, c.title, c.archived, COUNT(*), {SUMS},
                    SUM(CASE WHEN u.timestamp >= ?2 THEN u.cost_usd ELSE 0.0 END),
                    u.provider <> 'claude'
             FROM chat_turn_usage u JOIN chats c ON c.id = u.chat_id
             WHERE {chats} AND {OWN_USAGE}
             GROUP BY c.id, u.provider"
        ))?;
        let lines = st
            .query_map(rusqlite::params![id, since], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    Sums {
                        turns: r.get(3)?,
                        input: r.get(4)?,
                        output: r.get(5)?,
                        cache_read: r.get(6)?,
                        cache_write: r.get(7)?,
                        cost: r.get(8)?,
                    },
                    r.get::<_, f64>(9)?,
                    r.get::<_, bool>(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<Line>>>()?;
        let helm: f64 = tx.query_row(
            &format!(
                "SELECT COALESCE(SUM(u.cost_usd), 0.0)
                 FROM chat_turn_usage u JOIN chats c ON c.id = u.chat_id
                 WHERE u.timestamp >= ?1 AND {OWN_USAGE} AND u.provider = 'claude'"
            ),
            [since],
            |r| r.get(0),
        )?;
        // Replies here from before Helm recorded its first turn were never counted.
        let first: Option<i64> =
            tx.query_row("SELECT MIN(timestamp) FROM chat_turn_usage", [], |r| r.get(0))?;
        let uncounted: bool = tx.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM messages m JOIN chats c ON c.id = m.chat_id
                 WHERE {chats} AND m.role = 'assistant' AND m.timestamp >= c.created_at
                   AND m.timestamp < ?2)"
            ),
            rusqlite::params![id, first.unwrap_or(now)],
            |r| r.get(0),
        )?;
        Ok::<_, rusqlite::Error>(Some((name, lines, helm, first, uncounted)))
    });
    let (name, lines, helm, counting_since, uncounted_replies) = read
        .map_err(|e| format!("The usage records could not be read: {e}"))?
        .ok_or_else(|| match scope {
            CostScope::Chat => "There is no such chat.".to_owned(),
            CostScope::Project => "There is no such project.".to_owned(),
        })?;
    // Chats on local models are reported apart: Claude's chats first, costliest first, then the
    // local ones, heaviest first.
    let (mut lines, local_split): (Vec<Line>, Vec<Line>) = lines.into_iter().partition(|l| !l.5);
    // A chat that used several local servers is one line.
    let mut local_lines: Vec<Line> = Vec::new();
    for l in local_split {
        match local_lines.iter_mut().find(|m| m.0 == l.0) {
            Some(m) => m.3.add(&l.3),
            None => local_lines.push(l),
        }
    }
    lines.sort_by(|a, b| {
        b.3.cost
            .total_cmp(&a.3.cost)
            .then(b.3.total().cmp(&a.3.total()))
    });
    local_lines.sort_by_key(|l| std::cmp::Reverse(l.3.total()));
    let share = |cost: f64| window.map(|(pct, _)| if helm > 0.0 { cost / helm * pct } else { 0.0 });
    let mut sum = Sums::default();
    let mut week_cost = 0.0;
    let mut archived_chats = 0;
    let chats = lines
        .into_iter()
        .map(|(chat_id, title, archived, s, week, _)| {
            sum.add(&s);
            week_cost += week;
            archived_chats += u32::from(archived);
            ChatCost {
                chat_id,
                title,
                archived,
                totals: totals(&s),
                week_cost_usd: week,
                weekly_pct: share(week),
                local: None,
            }
        })
        .collect::<Vec<ChatCost>>();
    let mut local_sum = Sums::default();
    let mut chats = chats;
    for (chat_id, title, archived, s, _, _) in local_lines {
        local_sum.add(&s);
        // A chat that ran on Claude and on a local model is one line holding both.
        if let Some(c) = chats.iter_mut().find(|c| c.chat_id == chat_id) {
            c.local = Some(track(&s, &p));
            continue;
        }
        archived_chats += u32::from(archived);
        chats.push(ChatCost {
            chat_id,
            title,
            archived,
            totals: CostTotals::default(),
            week_cost_usd: 0.0,
            weekly_pct: None,
            local: Some(track(&s, &p)),
        });
    }
    Ok(CostReport {
        scope,
        name,
        totals: totals(&sum),
        weekly: window.map(|(week_pct, resets_at)| WeeklyShare {
            week_pct,
            window_start: resets_at - WEEK_MS,
            resets_at,
            helm_cost_usd: helm,
            cost_usd: week_cost,
            share_pct: share(week_cost).unwrap_or(0.0),
        }),
        chats,
        local: (local_sum.turns > 0).then(|| track(&local_sum, &p)),
        archived_chats,
        uncounted_replies,
        counting_since,
    })
}

/// The offset of this machine's clock from UTC, in milliseconds (what periods and day buckets use).
fn local_offset_ms() -> i64 {
    chrono::Local::now().offset().local_minus_utc() as i64 * 1000
}

/// Where to look up a local model's context window by its one-string id: the chat runtime's
/// configured servers, their last-loaded catalogues (never a request at tool or op time).
pub(crate) fn local_windows(services: &helm_ipc::Services) -> impl Fn(&str) -> Option<u64> {
    let rt = services.get::<helm_chats::ChatRuntime>();
    move |model_id| {
        helm_chats::local::split(model_id)?;
        rt.as_ref()?
            .local_models()?
            .catalogue(false)
            .into_iter()
            .find(|m| m.id == model_id)
            .and_then(|m| m.context_tokens)
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_usage_summary",
            "A digest of this machine's own usage records: tokens, turns (Claude Code subagents included) and the cost the binary reported, split by model, day and chat (the ten heaviest). Read-only and local. Period: range (today, morning, 24h, 7d, 30d, all; default 7d; today and morning follow this machine's clock), lastHours (a whole number of hours back, 1 to 744) or since/until (YYYY-MM-DD or an RFC 3339 time). chatId (or session, a Claude Code session id as chats report it) narrows the figures to one chat and adds chat: its token counts, the cost it reported, its estimated share of the plan's weekly limit as a percent, and its current context size in tokens and as a share of its model's window. tree: true widens a chat's numbers to its whole tree (its child chats, forks and chats sharing its session; Claude Code's own Agent-tool subagents are already inside each chat's records). plan: true adds the usage meter's 5-hour and weekly windows: the percent of each used and left, when each resets, whether the plan is at its limit now, and each reading's age and where it came from. fresh: true first asks Claude Code for a reading now (the meter reads at most once a minute), so the plan answer is not stale. Chats on local models are not in Claude's figures: they have their own local block (tokens, which cost nothing, and what the same tokens would have cost on a Claude model, as an estimate). The longest lists are trimmed to keep answers small.",
            json!({
                "type": "object",
                "properties": {
                    "range": {"type": "string", "enum": ["today", "morning", "24h", "7d", "30d", "all"]},
                    "lastHours": {"type": "integer", "minimum": 1, "maximum": 744},
                    "since": {"type": ["string", "integer"]},
                    "until": {"type": ["string", "integer"]},
                    "chatId": {"type": "string", "minLength": 36, "maxLength": 36},
                    "session": {"type": "string", "minLength": 1, "maxLength": 200},
                    "tree": {"type": "boolean"},
                    "plan": {"type": "boolean"},
                    "fresh": {"type": "boolean"}
                },
                "additionalProperties": false
            }),
        )
        .children(),
        |ctx: &ToolCtx, a| {
            let db = ctx
                .services
                .get::<Database>()
                .ok_or_else(|| refuse("The usage records are not available."))?;
            let compare = crate::prices::compare_key(&ctx.services);
            let now = helm_core::now_ms();
            let svc = ctx.services.get::<crate::service::UsageService>();
            let fresh = a.get("fresh").and_then(Value::as_bool).unwrap_or(false);
            // The reading comes first, a fresh one when asked for, so the weekly share below is
            // worked from the same numbers the plan block shows.
            let read = svc.as_ref().map(|s| {
                if fresh {
                    let (snap, outcome) =
                        s.fresh_read(now, std::time::Duration::from_secs(FRESH_BUDGET_SECS));
                    (snap, Some(outcome))
                } else {
                    (s.snapshot(now), None)
                }
            });
            let weekly = read.as_ref().and_then(|(snap, _)| {
                let w = snap.reading.as_ref()?.weekly.as_ref()?;
                Some((w.pct?, w.resets_at?))
            });
            let lw = local_windows(&ctx.services);
            let env = DigestEnv {
                now,
                local_offset_ms: local_offset_ms(),
                weekly,
                local_window: &lw,
            };
            let mut out = digest(&db, &a, &compare, &env)?;
            if fresh || a.get("plan").and_then(Value::as_bool).unwrap_or(false) {
                out["plan"] = match read {
                    None => json!({
                        "meterState": "inactive",
                        "meterReason": "The plan-usage meter is not running here.",
                    }),
                    Some((snap, outcome)) => plan_block(&snap, now, outcome),
                };
                trim(&mut out);
            }
            Ok(out)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;

    fn no_window(_id: &str) -> Option<u64> {
        None
    }

    /// A local stub answering for the fixture's one local chat, and nothing for other ids.
    fn stub_window(id: &str) -> Option<u64> {
        (id == "local/ollama/qwen3:8b").then_some(40_960)
    }

    /// The chat context follows the chat's last measured call (not an older, bigger one, and not
    /// a running total) over that model's real window (not the Claude figure): a local chat at
    /// 230k of its server's 262k window reads 87.74% full; over the window says so past 100;
    /// a server that has not stated a window says no percent; a chat with no turns says nothing.
    #[test]
    fn the_chat_context_follows_the_last_measured_call_and_the_real_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        // A turn as the chats crate records it; the context is input + cache reads + cache writes.
        let turn = |tx: &rusqlite::Transaction<'_>,
                    chat: &str,
                    key: &str,
                    ts: i64,
                    i: i64,
                    r: i64,
                    w: i64| {
            let usage = json!({"inputTokens": i, "outputTokens": 5, "cacheReadTokens": r,
                               "cacheWriteTokens": w, "costUsd": 0.0});
            let provider = if chat == "loc" { "local" } else { "claude" };
            let raw = json!({"type": "usage", "id": key, "timestamp": ts, "nativeSession": "s",
                             "usage": usage, "running": usage, "provider": provider});
            // The context reading comes from the turn's reply row, as the chat shows it.
            tx.execute(
                "INSERT INTO messages(chat_id, source_key, role, timestamp, details)
                 VALUES (?1, ?2, 'assistant', ?3, ?4)",
                rusqlite::params![
                    chat,
                    format!("m:{key}"),
                    ts,
                    json!({"usage": usage}).to_string()
                ],
            )?;
            helm_chats::session_log::project(tx, dir.path(), chat, raw.as_object().unwrap(), 1)
        };
        db.write(|tx| {
            tx.execute(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
                [],
            )?;
            for (id, provider, model) in [
                ("strata", Some("strata"), "qwen"),
                ("big", Some("strata"), "big"),
                ("tiny", Some("strata"), "no-size"),
                ("opus", None, "claude-opus-4-5"),
                ("lonely", None, "opus"),
            ] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, provider, model, created_at, updated_at)
                     VALUES (?1, 'p', ?1, ?2, ?3, 1, 1)",
                    rusqlite::params![id, provider, model],
                )?;
            }
            // The strata chat's older turn measured more context than its newest: the reading
            // must follow the newest measured call, not the biggest one.
            turn(tx, "strata", "s:old", 9 * DAY_MS, 240_000, 100, 50)?;
            turn(tx, "strata", "s:new", 10 * DAY_MS, 229_700, 200, 100)?;
            // Past the window it says so; a server that never stated one says no percent;
            // a Claude chat takes the standard window for its id.
            turn(tx, "big", "s:1", 10 * DAY_MS, 299_000, 900, 100)?;
            turn(tx, "tiny", "s:2", 10 * DAY_MS, 6_000, 1, 1)?;
            turn(tx, "opus", "s:3", 10 * DAY_MS, 150_000, 10, 10)?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let window = |id: &str| match id {
            "local/strata/qwen" | "local/strata/big" => Some(262_144),
            _ => None,
        };
        // (chat, context tokens, window tokens, percent, the model id the reading comes with)
        let cases = [
            (
                "strata",
                Some(230_000),
                Some(262_144),
                Some(87.74),
                "local/strata/qwen",
            ),
            (
                "big",
                Some(300_000),
                Some(262_144),
                Some(114.44),
                "local/strata/big",
            ),
            ("tiny", Some(6_002), None, None, "local/strata/no-size"),
            (
                "opus",
                Some(150_020),
                Some(200_000),
                Some(75.01),
                "claude-opus-4-5",
            ),
            ("lonely", None, None, None, "opus"),
        ];
        for (chat, tokens, win, pct, model_id) in cases {
            let (got_id, _provider, ctx) = chat_context(&db, chat, &window).unwrap();
            assert_eq!(got_id, model_id, "{chat} model id");
            assert_eq!(ctx["contextTokens"].as_u64(), tokens, "{chat} context");
            assert_eq!(ctx["windowTokens"].as_u64(), win, "{chat} window");
            assert_eq!(ctx["windowPct"].as_f64(), pct, "{chat} percent");
        }
        // A chat that does not exist: the op maps this to a plain "no chat with that id" answer.
        assert!(matches!(
            chat_context(&db, "nobody", &window),
            Err(rusqlite::Error::QueryReturnedNoRows)
        ));
    }

    fn reading() -> UsageSnapshot {
        UsageSnapshot {
            reading: Some(crate::types::PlanUsageReading {
                plan: Some("max".into()),
                five_hour: Some(UsageWindow {
                    pct: Some(42.5),
                    resets_at: Some(3_600_000),
                    at: 1_000_000,
                    source: UsageSource::Control,
                }),
                weekly: Some(UsageWindow {
                    pct: Some(100.0),
                    resets_at: Some(60 * 60 * 1000),
                    at: 1_200_000,
                    source: UsageSource::RateLimitEvent,
                }),
                per_model: (0..4)
                    .map(|i| crate::types::ModelUsage {
                        model: format!("m{i}"),
                        pct: Some(10.0),
                        resets_at: Some(2_000_000),
                        at: 1_000_000,
                        source: UsageSource::Control,
                    })
                    .collect(),
                source: UsageSource::RateLimitEvent,
                fetched_at: 1_200_000,
                stale: true,
            }),
            readiness: crate::types::UsageReadiness {
                state: ReadinessState::Ready,
                reason: "Plan usage is up to date.".into(),
                action_label: None,
                action_target: None,
                binary_version: Some("2.1.289".into()),
            },
            refreshing: false,
            next_refresh_at: Some(66_000_000),
        }
    }

    /// The meter's windows as the tool shows them: percent used and left, reset, age, source, and
    /// a window that has reached 100 with its reset ahead reads as at the limit.
    #[test]
    fn the_plan_block_shares_windows_resets_age_and_the_limit() {
        let out = plan_block(&reading(), 2_000_000, None);
        assert_eq!(out["plan"], json!("max"));
        assert_eq!(out["meterState"], "ready");
        assert_eq!(out["stale"], json!(true));
        assert_eq!(out["binaryVersion"], "2.1.289");
        assert_eq!(out["nextReadAtMs"], 66_000_000);
        let five = &out["fiveHour"];
        assert_eq!(
            (
                five["pctUsed"].as_f64(),
                five["pctLeft"].as_f64(),
                five["resetsAtMs"].as_i64(),
                five["ageMs"].as_i64(),
                five["atLimit"].as_bool(),
            ),
            (
                Some(42.5),
                Some(57.5),
                Some(3_600_000),
                Some(1_000_000),
                Some(false)
            ),
        );
        assert_eq!(five["source"], "a control request to Claude Code");
        let week = &out["weekly"];
        assert_eq!(
            (
                week["pctUsed"].as_f64(),
                week["pctLeft"].as_f64(),
                week["atLimit"].as_bool(),
            ),
            (Some(100.0), Some(0.0), Some(true)),
            "100 percent with the reset still ahead: at the limit now",
        );
        assert_eq!(week["source"], "a rate-limit line a chat saw");
        assert_eq!(out["atLimit"], json!(true));
        assert_eq!(out["perModel"].as_array().unwrap().len(), MAX_PER_MODEL);
        assert_eq!(out["perModelCount"], 4);
        // What a fresh ask came down to is reported as asked.
        let fresh = plan_block(&reading(), 2_000_000, Some(FreshOutcome::TooSoon));
        assert_eq!(fresh["fresh"]["outcome"], "too soon");
        assert!(fresh["fresh"]["note"]
            .as_str()
            .unwrap()
            .contains("once a minute"));
        // No reading: the meter's own words say why, and no percentages are claimed.
        let empty = UsageSnapshot {
            reading: None,
            readiness: crate::types::UsageReadiness {
                state: ReadinessState::Inactive,
                reason: "Reading plan usage from Claude Code…".into(),
                action_label: None,
                action_target: None,
                binary_version: None,
            },
            refreshing: false,
            next_refresh_at: None,
        };
        let none = plan_block(&empty, 1, Some(FreshOutcome::Failed));
        assert!(none["reading"].is_null());
        assert_eq!(none["meterState"], "inactive");
        assert!(none["meterReason"]
            .as_str()
            .unwrap()
            .contains("Reading plan usage"));
        assert_eq!(none["fresh"]["outcome"], "failed");
    }

    #[test]
    fn the_digest_sums_by_model_day_and_chat_within_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        let now = 10 * DAY_MS + 5_000;
        // A turn as the chats crate records it: a `usage` entry in the session log, projected.
        let turn = |tx: &rusqlite::Transaction<'_>,
                    chat: &str,
                    sess: &str,
                    key: &str,
                    ts: i64,
                    i: i64,
                    o: i64,
                    c: f64| {
            let usage = json!({"inputTokens": i, "outputTokens": o, "cacheReadTokens": 1,
                               "cacheWriteTokens": 2, "costUsd": c});
            // The provider is the one the turn ran on, which a fixture's local chat is.
            let provider = if chat == "loc" { "local" } else { "claude" };
            let raw = json!({"type": "usage", "id": key, "timestamp": ts, "nativeSession": sess,
                             "usage": usage, "running": usage, "provider": provider});
            helm_chats::session_log::project(tx, dir.path(), chat, raw.as_object().unwrap(), 1)
        };
        db.write(|tx| {
            // Project q is nested in p; its archived chat f is a fork made at day 9.5, so its day-9
            // turn is inherited history.
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1), ('q', 'Q', 1);
                 CREATE TABLE project_parents(project_id TEXT PRIMARY KEY, parent_id TEXT NOT NULL);
                 INSERT INTO project_parents VALUES ('q', 'p');",
            )?;
            for (id, project, model, created, archived) in [
                ("a", "p", Some("opus"), 1, false),
                ("b", "p", None, 1, false),
                ("f", "q", None, 9 * DAY_MS + DAY_MS / 2, true),
            ] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, model, created_at, updated_at, archived)
                     VALUES (?1, ?2, ?1, ?3, ?4, 1, ?5)",
                    rusqlite::params![id, project, model, created, archived],
                )?;
            }
            // A chat on a local model in project p: a million tokens in, a hundred thousand out,
            // and a cost Claude Code made up for a model it does not know (which is not recorded).
            tx.execute(
                "INSERT INTO chats(id, project_id, title, provider, model, created_at, updated_at)
                 VALUES ('loc', 'p', 'Local one', 'ollama', 'qwen3:8b', 1, 1)",
                [],
            )?;
            turn(tx, "loc", "s", "s:9", 10 * DAY_MS, 1_000_000, 100_000, 0.0)?;
            turn(tx, "a", "s", "s:1", 10 * DAY_MS, 100, 10, 0.5)?;
            turn(tx, "a", "s", "s:2", 9 * DAY_MS, 50, 5, 0.25)?;
            turn(tx, "b", "s", "s:3", 9 * DAY_MS, 7, 1, 0.01)?;
            turn(tx, "a", "s", "s:6", 2 * DAY_MS, 1000, 1000, 1.0)?;
            turn(tx, "f", "s", "s:7", 9 * DAY_MS, 50, 5, 0.25)?;
            turn(tx, "f", "s", "s:8", 10 * DAY_MS, 10, 1, 0.24)?;
            // Projecting the same entry again records nothing new.
            turn(tx, "f", "s", "s:8", 10 * DAY_MS, 10, 1, 0.24)?;
            // Replies are not what is counted, whatever usage they carry: chat a has one from
            // before Helm's first recorded turn (day 2), chat b none.
            for (chat, key, ts) in [("a", "m:old", DAY_MS), ("b", "m:new", 9 * DAY_MS)] {
                tx.execute(
                    "INSERT INTO messages(chat_id, source_key, role, timestamp, details)
                     VALUES (?1, ?2, 'assistant', ?3, ?4)",
                    rusqlite::params![chat, key, ts, json!({"usage": {"inputTokens": 999, "costUsd": 9.0}}).to_string()],
                )?;
            }
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();

        let env = DigestEnv {
            now,
            local_offset_ms: 0,
            weekly: None,
            local_window: &no_window,
        };
        let all = digest(&db, &json!({"range": "7d"}), "sonnet", &env).unwrap();
        // s:6 is outside seven days; the fork's inherited s:7 is its source's.
        assert_eq!(all["total"]["turns"], 4);
        assert_eq!(all["total"]["inputTokens"], 167);
        assert_eq!(all["total"]["costUsd"], 1.0);
        assert_eq!(all["byModel"][0]["model"], "opus");
        assert_eq!(all["byModel"][1]["model"], "default");
        assert_eq!(all["byDay"].as_array().unwrap().len(), 2);
        assert_eq!(all["byDay"][0]["day"], "1970-01-10", "oldest first");
        assert_eq!(all["byDay"][1]["day"], "1970-01-11");
        assert_eq!(
            all["chatsWithUsage"], 3,
            "a local chat is not among Claude's"
        );
        // The local chat is its own block, priced against the comparison model.
        let l = &all["local"];
        assert_eq!(
            (l["turns"].as_i64(), l["totalTokens"].as_i64()),
            (Some(1), Some(1_100_003))
        );
        assert_eq!(
            (l["costUsd"].as_i64(), l["comparedWith"].as_str()),
            (Some(0), Some("sonnet"))
        );
        assert!(
            (l["wouldCostUsd"].as_f64().unwrap() - 3.000_005_2).abs() < 1e-9,
            "{l}"
        );
        assert_eq!(l["byModel"][0]["model"], "qwen3:8b");
        assert_eq!(l["chats"][0]["title"], "Local one");
        let haiku = digest(&db, &json!({"range": "7d"}), "haiku", &env).unwrap();
        assert_eq!(haiku["local"]["comparedName"], "Haiku");
        assert!((haiku["local"]["wouldCostUsd"].as_f64().unwrap() - 1.500_002_6).abs() < 1e-9);
        assert!(one_has_no_local(&db, now));
        assert_eq!(all["topChats"][0]["chatId"], "a");
        let one = digest(&db, &json!({"range": "all", "chatId": "b"}), "sonnet", &env).unwrap();
        assert_eq!(one["total"]["turns"], 1);
        let custom = digest(
            &db,
            &json!({"since": "1970-01-10", "until": 10 * DAY_MS}),
            "sonnet",
            &env,
        )
        .unwrap();
        assert_eq!(custom["total"]["turns"], 2);

        // Cost reports. The weekly window began at day 5: a, b and f cost 1.00 in it, and the
        // plan's 40% is shared out in proportion.
        let close = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} != {b}");
        let week = Some((40.0, now + 2 * DAY_MS));
        let a = cost_report(&db, CostScope::Chat, "a", week, "sonnet", now).unwrap();
        assert_eq!(a.name, "a");
        assert_eq!(a.totals.input_tokens, 1150);
        assert_eq!(a.totals.total_tokens, 1150 + 1015 + 3 + 6);
        close(a.totals.cost_usd, 1.75);
        let w = a.weekly.as_ref().unwrap();
        assert_eq!(w.window_start, 5 * DAY_MS + 5_000);
        close(w.helm_cost_usd, 1.0);
        close(w.cost_usd, 0.75);
        close(w.share_pct, 30.0);
        assert_eq!(
            (a.uncounted_replies, a.counting_since),
            (true, Some(2 * DAY_MS))
        );
        assert!(a.local.is_none(), "a Claude chat has no local figures");
        let loc = cost_report(&db, CostScope::Chat, "loc", week, "opus", now).unwrap();
        assert_eq!((loc.totals.total_tokens, loc.totals.cost_usd), (0, 0.0));
        let w = loc.weekly.as_ref().unwrap();
        assert_eq!(
            (w.cost_usd, w.share_pct),
            (0.0, 0.0),
            "no share of the plan"
        );
        assert_eq!(loc.local.as_ref().unwrap().compared_with, "opus");
        assert!((loc.local.unwrap().would_cost_usd - 6.000_010_2).abs() < 1e-9);
        let b = cost_report(&db, CostScope::Chat, "b", week, "sonnet", now).unwrap();
        assert!(!b.uncounted_replies);
        let f = cost_report(&db, CostScope::Chat, "f", week, "sonnet", now).unwrap();
        assert_eq!((f.totals.input_tokens, f.archived_chats), (10, 1));
        // A project counts its sub-projects and archived chats, costliest chat first.
        let p = cost_report(&db, CostScope::Project, "p", week, "sonnet", now).unwrap();
        let ids: Vec<&str> = p.chats.iter().map(|c| c.chat_id.as_str()).collect();
        assert_eq!(
            ids,
            ["a", "f", "b", "loc"],
            "local chats come after Claude's"
        );
        assert_eq!(
            p.chats[3].totals,
            CostTotals::default(),
            "nothing of it is Claude's"
        );
        let track = p.chats[3].local.as_ref().unwrap();
        assert_eq!(
            (track.total_tokens, track.compared_name.as_str()),
            (1_100_003, "Sonnet")
        );
        assert_eq!(p.local.as_ref().unwrap().total_tokens, 1_100_003);
        assert_eq!(
            p.local.as_ref().unwrap().prices_read,
            crate::prices::PRICES_READ
        );
        assert!(p.chats[..3].iter().all(|c| c.local.is_none()));
        assert_eq!(p.archived_chats, 1);
        close(p.totals.cost_usd, 2.0);
        close(p.weekly.as_ref().unwrap().share_pct, 40.0);
        close(p.chats[1].weekly_pct.unwrap(), 9.6);
        let q = cost_report(&db, CostScope::Project, "q", week, "sonnet", now).unwrap();
        assert_eq!(q.chats.len(), 1);
        // A weekly window that has already reset gives no share; unknown ids are refused.
        let old = cost_report(
            &db,
            CostScope::Chat,
            "a",
            Some((40.0, now - 1)),
            "sonnet",
            now,
        )
        .unwrap();
        assert!(old.weekly.is_none() && old.chats[0].weekly_pct.is_none());
        assert!(cost_report(&db, CostScope::Chat, "zz", week, "sonnet", now).is_err());
        assert!(cost_report(&db, CostScope::Project, "zz", None, "sonnet", now).is_err());
        // A chat that ran on Claude and later on a local model is one line holding both.
        db.write(|tx| {
            let usage = json!({"inputTokens": 40, "outputTokens": 4, "cacheReadTokens": 0,
                               "cacheWriteTokens": 0, "costUsd": 0.0});
            let raw = json!({"type": "usage", "id": "b:local", "timestamp": 10 * DAY_MS,
                             "nativeSession": "b-local", "usage": usage, "running": usage,
                             "provider": "local"});
            helm_chats::session_log::project(tx, dir.path(), "b", raw.as_object().unwrap(), 1)
        })
        .unwrap();
        let p = cost_report(&db, CostScope::Project, "p", week, "sonnet", now).unwrap();
        let b: Vec<&ChatCost> = p.chats.iter().filter(|c| c.chat_id == "b").collect();
        assert_eq!(b.len(), 1, "one line for the chat");
        assert!(b[0].totals.total_tokens > 0 && b[0].local.as_ref().unwrap().total_tokens == 44);
        assert_eq!(p.archived_chats, 1);
        // Bad input is refused with nothing read.
        for bad in [
            json!({"range": "year"}),
            json!({"range": "7d", "since": 1}),
            json!({"range": "7d", "lastHours": 3}),
            json!({"lastHours": 0}),
            json!({"lastHours": 745}),
            json!({"lastHours": 1.5}),
            json!({"since": 5, "until": 5}),
            json!({"since": "yesterday"}),
            json!({"session": "s", "tree": true, "chatId": "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"}),
        ] {
            assert!(digest(&db, &bad, "sonnet", &env).is_err(), "{bad}");
        }
        assert!(digest(&db, &json!({"tree": true}), "sonnet", &env).is_err());
        assert!(digest(&db, &json!({"session": "nope"}), "sonnet", &env).is_err());
        // `morning` cuts the day at six o'clock where `today` cuts it at midnight, and
        // `lastHours` reaches back an exact number of hours; a period with nothing in it answers
        // with nothing rather than an error.
        let early = DigestEnv {
            now: 10 * DAY_MS + 8 * HOUR_MS,
            local_offset_ms: 0,
            weekly: None,
            local_window: &no_window,
        };
        for (arg, turns) in [
            (json!({"range": "today"}), 2),
            (json!({"range": "morning"}), 0),
            (json!({"lastHours": 8}), 2),
            (json!({"lastHours": 34}), 4),
        ] {
            let m = digest(&db, &arg, "sonnet", &early).unwrap();
            assert_eq!(m["total"]["turns"], turns, "{arg}");
        }
    }

    /// Periods and day buckets follow this machine's clock, a chat's numbers carry its context
    /// size over its model's window and its share of the week, and a tree takes in child chats,
    /// forks and session siblings at once.
    #[test]
    fn periods_and_chat_scopes_follow_the_machines_clock() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        let turn = |tx: &rusqlite::Transaction<'_>,
                    chat: &str,
                    sess: &str,
                    key: &str,
                    ts: i64,
                    i: i64,
                    o: i64,
                    c: f64| {
            let usage = json!({"inputTokens": i, "outputTokens": o, "cacheReadTokens": 1,
                               "cacheWriteTokens": 2, "costUsd": c});
            // The provider is the one the turn ran on, which a fixture's local chat is.
            let provider = if chat == "loc" { "local" } else { "claude" };
            let raw = json!({"type": "usage", "id": key, "timestamp": ts, "nativeSession": sess,
                             "usage": usage, "running": usage, "provider": provider});
            // The context reading comes from the turn's reply row, as the chat shows it.
            tx.execute(
                "INSERT INTO messages(chat_id, source_key, role, timestamp, details)
                 VALUES (?1, ?2, 'assistant', ?3, ?4)",
                rusqlite::params![
                    chat,
                    format!("m:{key}"),
                    ts,
                    json!({"usage": usage}).to_string()
                ],
            )?;
            helm_chats::session_log::project(tx, dir.path(), chat, raw.as_object().unwrap(), 1)
        };
        db.write(|tx| {
            tx.execute_batch("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1);")?;
            for id in ["a", "b", "kid", "frk", "ses"] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, created_at, updated_at)
                     VALUES (?1, 'p', ?1, 1, 1)",
                    [id],
                )?;
            }
            tx.execute("UPDATE chats SET model = 'claude-opus' WHERE id = 'a'", [])?;
            tx.execute("UPDATE chats SET parent_chat_id = 'a' WHERE id = 'kid'", [])?;
            tx.execute(
                "INSERT INTO chat_forks(fork_id, source_chat_id, chat_id, created_at)
                 VALUES ('f-a', 'a', 'frk', 1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO chats(id, project_id, title, provider, model, created_at, updated_at)
                 VALUES ('loc', 'p', 'Local one', 'ollama', 'qwen3:8b', 1, 1)",
                [],
            )?;
            turn(tx, "a", "sa", "a:1", 10 * DAY_MS, 100, 10, 0.5)?;
            turn(tx, "a", "sa", "a:2", 9 * DAY_MS, 50, 5, 0.25)?;
            turn(tx, "b", "sb", "b:1", 9 * DAY_MS, 7, 1, 0.01)?;
            turn(tx, "kid", "sk", "k:1", 10 * DAY_MS, 3, 1, 0.1)?;
            turn(tx, "frk", "sx", "x:1", 10 * DAY_MS, 4, 1, 0.2)?;
            // A sibling carrying chat a's Claude Code session id. Its e:1 turn is the newest of
            // that session; its e:2 turn sits at 01:30 local on Jan 11 for a machine two hours
            // east — one an old UTC-midnight "today" window would miss.
            turn(tx, "ses", "sa", "e:1", 10 * DAY_MS + 1, 6, 1, 0.3)?;
            turn(
                tx,
                "ses",
                "sa",
                "e:2",
                9 * DAY_MS + 23 * HOUR_MS + 1_800_000,
                8,
                1,
                0.2,
            )?;
            turn(tx, "loc", "sl", "l:1", 10 * DAY_MS, 1_000, 100, 0.0)?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();

        // Jan 11, 02:01 local on a machine two hours east: its local day began at 22:00 UTC the
        // previous evening, so the window took in e:2 (used at 01:30 local), which a window cut
        // at UTC midnight would have left out; the Jan 10 turns stay outside it.
        let clock = 10 * DAY_MS + 60_000;
        let east = DigestEnv {
            now: clock,
            local_offset_ms: 2 * HOUR_MS,
            weekly: None,
            local_window: &no_window,
        };
        let today = digest(&db, &json!({"range": "today"}), "sonnet", &east).unwrap();
        assert_eq!(today["total"]["turns"], 5, "a, kid, frk and both ses turns");
        assert_eq!(today["byDay"].as_array().unwrap().len(), 1);
        assert_eq!(today["byDay"][0]["day"], "1970-01-11");
        // Before six o'clock local, the morning is the whole young day; after six it starts at
        // six o'clock and the young day's turns drop out.
        let morning = digest(&db, &json!({"range": "morning"}), "sonnet", &east).unwrap();
        assert_eq!(morning["total"]["turns"], 5);
        let later = DigestEnv {
            now: 10 * DAY_MS + 8 * HOUR_MS,
            ..east
        };
        let m2 = digest(&db, &json!({"range": "morning"}), "sonnet", &later).unwrap();
        assert_eq!(m2["total"]["turns"], 0, "six o'clock local has passed them");
        let h24 = digest(&db, &json!({"lastHours": 24}), "sonnet", &east).unwrap();
        assert_eq!(h24["total"]["turns"], 5);
        assert_eq!(h24["range"], "24h");
        // The machine's clock buckets days too: two hours west, chat a's two turns fall on the
        // 9th and the 10th.
        let west = DigestEnv {
            now: 10 * DAY_MS + 3_600_000,
            local_offset_ms: -2 * HOUR_MS,
            weekly: None,
            local_window: &no_window,
        };
        let west_a = digest(
            &db,
            &json!({"range": "30d", "chatId": "a"}),
            "sonnet",
            &west,
        )
        .unwrap();
        let days: Vec<&str> = west_a["byDay"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["day"].as_str().unwrap())
            .collect();
        assert_eq!(days, ["1970-01-09", "1970-01-10"]);

        // One chat: its own numbers, context over its model's window, and an estimated weekly
        // share (the plan's 40% split by cost: 0.75 of the 1.00 spent in the window).
        let env = DigestEnv {
            now: clock,
            local_offset_ms: 0,
            weekly: Some((40.0, clock + 2 * DAY_MS)),
            local_window: &stub_window,
        };
        let one = digest(&db, &json!({"range": "all", "chatId": "a"}), "sonnet", &env).unwrap();
        assert_eq!(one["forChat"], "a");
        assert_eq!(one["total"]["turns"], 2);
        let c = &one["chat"];
        assert_eq!(c["title"], "a");
        assert_eq!(c["model"], "claude-opus");
        assert_eq!(c["tokens"]["turns"], 2);
        assert_eq!(c["tokens"]["totalTokens"], 171);
        assert!((c["tokens"]["costUsd"].as_f64().unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(c["context"]["contextTokens"], 103);
        assert_eq!(c["context"]["measuredAtMs"], 10 * DAY_MS);
        assert_eq!(c["context"]["windowTokens"], 200_000);
        assert!((c["context"]["windowPct"].as_f64().unwrap() - 0.05).abs() < 1e-9);
        // 0.75 of the 1.56 every Helm chat cost in the window.
        assert!((c["weeklySharePct"].as_f64().unwrap() - 19.231).abs() < 1e-3);
        // A session id names the chats carrying it; the newest turn's chat is the block's.
        let s = digest(&db, &json!({"session": "sa"}), "sonnet", &env).unwrap();
        assert_eq!(s["forChat"], "ses");
        assert_eq!(s["total"]["turns"], 2);
        assert!(s["chat"]["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("names 2 chats")));
        // The whole tree at once: the chat, its child, its fork and its session sibling.
        let t = digest(&db, &json!({"session": "sa", "tree": true}), "sonnet", &env).unwrap();
        assert_eq!(t["scopeChats"], 4);
        assert_eq!(t["total"]["turns"], 6);
        assert_eq!(t["total"]["costUsd"], 1.55);
        assert_eq!(t["chat"]["context"]["contextTokens"], 9);
        assert_eq!(t["chat"]["title"], "ses");
        // A local chat reports its context over the window its server stated, and takes no part
        // of the plan.
        let loc = digest(
            &db,
            &json!({"range": "all", "chatId": "loc"}),
            "sonnet",
            &env,
        )
        .unwrap();
        assert_eq!(loc["local"]["totalTokens"], 1_103);
        assert_eq!(loc["chat"]["model"], "local/ollama/qwen3:8b");
        assert_eq!(loc["chat"]["context"]["contextTokens"], 1_003);
        assert_eq!(loc["chat"]["context"]["windowTokens"], 40_960);
        assert!(loc["chat"].get("weeklySharePct").is_none());
    }

    fn one_has_no_local(db: &Database, now: i64) -> bool {
        let env = DigestEnv {
            now,
            local_offset_ms: 0,
            weekly: None,
            local_window: &no_window,
        };
        let one = digest(db, &json!({"range": "all", "chatId": "b"}), "sonnet", &env).unwrap();
        one.get("local").is_none()
    }
}
