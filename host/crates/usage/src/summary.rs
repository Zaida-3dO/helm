//! `app_usage_summary`: a bounded, read-only digest of this machine's own usage records.
//!
//! The records are the token counts and reported cost already stored on each assistant message,
//! so this reads Helm's database and nothing else: no plan percentage, no reset time (the meter
//! owns those) and no network. Cost is what the binary reported, never an estimate. A message's
//! model is its chat's current model, since the rows themselves do not record one.

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
    requests: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
}

impl Sums {
    fn add(&mut self, o: &Sums) {
        self.requests += o.requests;
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cost += o.cost;
    }

    fn json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("requests".into(), json!(self.requests));
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

type Row = (String, String, String, i64, Sums);

fn read_rows(
    db: &Database,
    since: i64,
    until: i64,
    chat: Option<&str>,
) -> Result<Vec<Row>, ToolError> {
    db.snapshot(|tx| {
        let mut st = tx.prepare(
            "SELECT c.id, c.title, COALESCE(c.model, 'default'), m.timestamp / 86400000,
                    COUNT(*),
                    SUM(COALESCE(json_extract(m.details, '$.usage.inputTokens'), 0)),
                    SUM(COALESCE(json_extract(m.details, '$.usage.outputTokens'), 0)),
                    SUM(COALESCE(json_extract(m.details, '$.usage.cacheReadTokens'), 0)),
                    SUM(COALESCE(json_extract(m.details, '$.usage.cacheWriteTokens'), 0)),
                    SUM(COALESCE(json_extract(m.details, '$.usage.costUsd'), 0.0))
             FROM messages m JOIN chats c ON c.id = m.chat_id
             WHERE m.role = 'assistant' AND m.details IS NOT NULL AND json_valid(m.details)
               AND json_extract(m.details, '$.usage') IS NOT NULL
               AND m.timestamp >= ?1 AND m.timestamp < ?2
               AND (?3 IS NULL OR c.id = ?3)
             GROUP BY c.id, 3, 4",
        )?;
        let it = st.query_map(rusqlite::params![since, until, chat], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                Sums {
                    requests: r.get(4)?,
                    input: r.get(5)?,
                    output: r.get(6)?,
                    cache_read: r.get(7)?,
                    cache_write: r.get(8)?,
                    cost: r.get(9)?,
                },
            ))
        })?;
        it.collect::<rusqlite::Result<Vec<Row>>>()
    })
    .map_err(|e| refuse(format!("The usage records could not be read: {e}")))
}

/// Build the digest. `now` is a parameter so a test can pin the window.
pub fn digest(db: &Database, a: &Value, now: i64) -> ToolResult {
    let (since, until, label) = window(a, now)?;
    let rows = read_rows(db, since, until, a.get("chatId").and_then(Value::as_str))?;

    let mut total = Sums::default();
    let mut by_model: BTreeMap<String, Sums> = BTreeMap::new();
    let mut by_day: BTreeMap<i64, Sums> = BTreeMap::new();
    let mut by_chat: BTreeMap<String, (String, Sums)> = BTreeMap::new();
    for (id, title, model, day, s) in &rows {
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
        "notes": "Local records only: token counts and the cost the binary reported. A model is the chat's current model. No plan percentage or reset time.",
    });
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

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::read(
            "app_usage_summary",
            "A digest of this machine's own usage records: tokens, requests and the cost the binary reported, totalled and split by model, day and chat (the ten heaviest). Read-only and local; it has no plan percentage or reset time. Use range (today, 24h, 7d, 30d, all; default 7d) or since/until (YYYY-MM-DD or an RFC 3339 time), and chatId to look at one chat. Answers are at most 8 KiB.",
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
            digest(&db, &a, helm_core::now_ms())
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
        let db = Database::open(&dir.path().join("t.sqlite3"), &MigrationRegistry::new()).unwrap();
        let now = 10 * DAY_MS + 5_000;
        let usage = |i: i64, o: i64, c: f64| {
            json!({"usage": {"inputTokens": i, "outputTokens": o, "cacheReadTokens": 1,
                             "cacheWriteTokens": 2, "costUsd": c}})
            .to_string()
        };
        db.write(|tx| {
            tx.execute(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
                [],
            )?;
            for (id, model) in [("a", Some("opus")), ("b", None)] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, model, created_at, updated_at)
                     VALUES (?1, 'p', ?1, ?2, 1, 1)",
                    rusqlite::params![id, model],
                )?;
            }
            // (chat, key, role, timestamp, details)
            let rows = [
                (
                    "a",
                    "m1",
                    "assistant",
                    10 * DAY_MS,
                    Some(usage(100, 10, 0.5)),
                ),
                ("a", "m2", "assistant", 9 * DAY_MS, Some(usage(50, 5, 0.25))),
                ("b", "m3", "assistant", 9 * DAY_MS, Some(usage(7, 1, 0.01))),
                ("a", "m4", "user", 10 * DAY_MS, Some(usage(999, 999, 9.0))),
                ("a", "m5", "assistant", 10 * DAY_MS, None),
                (
                    "a",
                    "m6",
                    "assistant",
                    2 * DAY_MS,
                    Some(usage(1000, 1000, 1.0)),
                ),
            ];
            for (chat, key, role, ts, details) in rows {
                tx.execute(
                    "INSERT INTO messages(chat_id, source_key, role, timestamp, details)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![chat, key, role, ts, details],
                )?;
            }
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();

        let all = digest(&db, &json!({"range": "7d"}), now).unwrap();
        // m6 is outside seven days; the user row and the row with no usage are not requests.
        assert_eq!(all["total"]["requests"], 3);
        assert_eq!(all["total"]["inputTokens"], 157);
        assert_eq!(all["total"]["costUsd"], 0.76);
        assert_eq!(all["byModel"][0]["model"], "opus");
        assert_eq!(all["byModel"][1]["model"], "default");
        assert_eq!(all["byDay"].as_array().unwrap().len(), 2);
        assert_eq!(all["topChats"][0]["chatId"], "a");
        let one = digest(&db, &json!({"range": "all", "chatId": "b"}), now).unwrap();
        assert_eq!(one["total"]["requests"], 1);
        let custom = digest(
            &db,
            &json!({"since": "1970-01-10", "until": 10 * DAY_MS}),
            now,
        )
        .unwrap();
        assert_eq!(custom["total"]["requests"], 2);
        // Bad input is refused with nothing read.
        for bad in [
            json!({"range": "year"}),
            json!({"range": "7d", "since": 1}),
            json!({"since": 5, "until": 5}),
            json!({"since": "yesterday"}),
        ] {
            assert!(digest(&db, &bad, now).is_err(), "{bad}");
        }
    }
}
