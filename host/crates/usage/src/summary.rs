//! `app_usage_summary`: a bounded, read-only digest of this machine's own usage records.
//!
//! The records are the per-turn token counts and reported cost the chats crate keeps in
//! `chat_turn_usage` (subagents included), so this reads Helm's database and nothing else: no plan
//! percentage, no reset time (the meter owns those) and no network. Cost is what the binary
//! reported, never an estimate. A turn's model is its chat's current model, since the rows
//! themselves do not record one. A fork's copied history counts for the chat it came from only
//! (see [`OWN_USAGE`]). Turns from before Helm recorded them are not there at all; a cost report
//! says so ([`CostReport::uncounted_replies`]).
//!
//! `usage_cost` ([`cost_report`]) reads the same records for one chat or project, and adds an
//! estimated share of the plan's weekly window from the meter's last reading.
//!
//! **Chats on local models are kept apart.** Their turns cost nothing and never count toward the
//! plan, so the totals, models, days and weekly shares here are Claude's alone. What the local
//! chats used is a separate `local` block, with what the same tokens would have cost on a Claude
//! model ([`crate::prices`]).

use crate::prices::{price, would_cost, Price, PRICES_READ};
use crate::types::{ChatCost, CostReport, CostScope, CostTotals, LocalTrack, WeeklyShare};
use helm_core::db::Database;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Chats listed in a digest.
const TOP_CHATS: usize = 10;
/// Days listed in a digest (the newest).
const MAX_DAYS: usize = 31;
/// The answer's size bound.
const MAX_BYTES: usize = 8 * 1024;
const DAY_MS: i64 = 86_400_000;

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

/// The `[since, until)` window in milliseconds the arguments ask for.
fn window(a: &Value, now: i64) -> Result<(i64, i64, String), ToolError> {
    let explicit = a.get("since").is_some() || a.get("until").is_some();
    if explicit && a.get("range").is_some() {
        return Err(refuse("Give either range or since/until, not both."));
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
    let range = a.get("range").and_then(Value::as_str).unwrap_or("7d");
    let since = match range {
        "today" => now - now.rem_euclid(DAY_MS),
        "24h" => now - DAY_MS,
        "7d" => now - 7 * DAY_MS,
        "30d" => now - 30 * DAY_MS,
        "all" => 0,
        _ => return Err(refuse("range is today, 24h, 7d, 30d or all.")),
    };
    Ok((since, now + 1, range.into()))
}

/// A chat, its title, model, day and sums, and whether the chat runs on a local model.
type Row = (String, String, String, i64, Sums, bool);

fn read_rows(
    db: &Database,
    since: i64,
    until: i64,
    chat: Option<&str>,
) -> Result<Vec<Row>, ToolError> {
    db.snapshot(|tx| {
        let mut st = tx.prepare(&format!(
            "SELECT c.id, c.title, COALESCE(c.model, 'default'), u.timestamp / 86400000,
                        COUNT(*), {SUMS}, c.provider IS NOT NULL
                 FROM chat_turn_usage u JOIN chats c ON c.id = u.chat_id
                 WHERE {OWN_USAGE} AND u.timestamp >= ?1 AND u.timestamp < ?2
                   AND (?3 IS NULL OR c.id = ?3)
                 GROUP BY c.id, 3, 4"
        ))?;
        let it = st.query_map(rusqlite::params![since, until, chat], |r| {
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

/// Build the digest. `now` is a parameter so a test can pin the window.
pub fn digest(db: &Database, a: &Value, compare: &str, now: i64) -> ToolResult {
    let (since, until, label) = window(a, now)?;
    let rows = read_rows(db, since, until, a.get("chatId").and_then(Value::as_str))?;
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
        "untilMs": until.min(now + 1),
        "total": Value::Object(total.json()),
        "byModel": models,
        "byDay": days,
        "topChats": chats,
        "chatsWithUsage": chats_total,
        "notes": "Local records only: each turn's token counts (subagents included) and the cost the binary reported. A model is the chat's current model. No plan percentage or reset time. Chats on local models are not in these figures; see local.",
    });
    if local.turns > 0 {
        out["local"] = local_block(&local, &local_models, local_chats, &p);
    }
    // Bounded: drop the tails of the longest lists first if the answer is over the size bound.
    for key in ["topChats", "byDay"] {
        while serde_json::to_vec(&out).map(|v| v.len()).unwrap_or(0) > MAX_BYTES {
            let Some(list) = out[key].as_array_mut().filter(|l| !l.is_empty()) else {
                break;
            };
            list.pop();
            out["truncated"] = json!(true);
        }
    }
    Ok(out)
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

const WEEK_MS: i64 = 7 * DAY_MS;

/// A turn counts for its chat only if it is newer than the chat. A fork's log starts with copies
/// of its source's entries (turn usage included) under their original times, so this leaves the
/// inherited history with the chat it came from and nothing is counted twice.
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
                    c.provider IS NOT NULL
             FROM chat_turn_usage u JOIN chats c ON c.id = u.chat_id
             WHERE {chats} AND {OWN_USAGE}
             GROUP BY c.id"
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
                 WHERE u.timestamp >= ?1 AND {OWN_USAGE} AND c.provider IS NULL"
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
    let (mut lines, mut local_lines): (Vec<Line>, Vec<Line>) =
        lines.into_iter().partition(|l| !l.5);
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

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_usage_summary",
            "A digest of this machine's own usage records: tokens, turns (subagents included) and the cost the binary reported, totalled and split by model, day and chat (the ten heaviest). Read-only and local; it has no plan percentage or reset time. Use range (today, 24h, 7d, 30d, all; default 7d) or since/until (YYYY-MM-DD or an RFC 3339 time), and chatId to look at one chat. Chats on local models are not in those figures: they have their own local block (tokens, which cost nothing, and what the same tokens would have cost on a Claude model, as an estimate). Answers are at most 8 KiB.",
            json!({
                "type": "object",
                "properties": {
                    "range": {"type": "string", "enum": ["today", "24h", "7d", "30d", "all"]},
                    "since": {"type": ["string", "integer"]},
                    "until": {"type": ["string", "integer"]},
                    "chatId": {"type": "string", "minLength": 36, "maxLength": 36}
                },
                "additionalProperties": false
            }),
        ),
        |ctx: &ToolCtx, a| {
            let db = ctx
                .services
                .get::<Database>()
                .ok_or_else(|| refuse("The usage records are not available."))?;
            let compare = crate::prices::compare_key(&ctx.services);
            digest(&db, &a, &compare, helm_core::now_ms())
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;

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
                    key: &str,
                    ts: i64,
                    i: i64,
                    o: i64,
                    c: f64| {
            let usage = json!({"inputTokens": i, "outputTokens": o, "cacheReadTokens": 1,
                               "cacheWriteTokens": 2, "costUsd": c});
            let raw = json!({"type": "usage", "id": key, "timestamp": ts, "nativeSession": "s",
                             "usage": usage, "running": usage});
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
            turn(tx, "loc", "s:9", 10 * DAY_MS, 1_000_000, 100_000, 0.0)?;
            turn(tx, "a", "s:1", 10 * DAY_MS, 100, 10, 0.5)?;
            turn(tx, "a", "s:2", 9 * DAY_MS, 50, 5, 0.25)?;
            turn(tx, "b", "s:3", 9 * DAY_MS, 7, 1, 0.01)?;
            turn(tx, "a", "s:6", 2 * DAY_MS, 1000, 1000, 1.0)?;
            turn(tx, "f", "s:7", 9 * DAY_MS, 50, 5, 0.25)?;
            turn(tx, "f", "s:8", 10 * DAY_MS, 10, 1, 0.24)?;
            // Projecting the same entry again records nothing new.
            turn(tx, "f", "s:8", 10 * DAY_MS, 10, 1, 0.24)?;
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

        let all = digest(&db, &json!({"range": "7d"}), "sonnet", now).unwrap();
        // s:6 is outside seven days; the fork's inherited s:7 is its source's.
        assert_eq!(all["total"]["turns"], 4);
        assert_eq!(all["total"]["inputTokens"], 167);
        assert_eq!(all["total"]["costUsd"], 1.0);
        assert_eq!(all["byModel"][0]["model"], "opus");
        assert_eq!(all["byModel"][1]["model"], "default");
        assert_eq!(all["byDay"].as_array().unwrap().len(), 2);
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
        let haiku = digest(&db, &json!({"range": "7d"}), "haiku", now).unwrap();
        assert_eq!(haiku["local"]["comparedName"], "Haiku");
        assert!((haiku["local"]["wouldCostUsd"].as_f64().unwrap() - 1.500_002_6).abs() < 1e-9);
        assert!(one_has_no_local(&db, now));
        assert_eq!(all["topChats"][0]["chatId"], "a");
        let one = digest(&db, &json!({"range": "all", "chatId": "b"}), "sonnet", now).unwrap();
        assert_eq!(one["total"]["turns"], 1);
        let custom = digest(
            &db,
            &json!({"since": "1970-01-10", "until": 10 * DAY_MS}),
            "sonnet",
            now,
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
        // Bad input is refused with nothing read.
        for bad in [
            json!({"range": "year"}),
            json!({"range": "7d", "since": 1}),
            json!({"since": 5, "until": 5}),
            json!({"since": "yesterday"}),
        ] {
            assert!(digest(&db, &bad, "sonnet", now).is_err(), "{bad}");
        }
    }

    fn one_has_no_local(db: &Database, now: i64) -> bool {
        let one = digest(db, &json!({"range": "all", "chatId": "b"}), "sonnet", now).unwrap();
        one.get("local").is_none()
    }
}
