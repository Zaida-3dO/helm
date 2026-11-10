//! Composer autocomplete.
//!
//! - **Off by default.** The `autocomplete` preference (`{enabled}`) turns it on; while off, a
//!   request answers an empty completion and starts nothing.
//! - **Local chats** are completed only by the model chosen for them under `backgroundModel`
//!   (`local`), on the chat's own server. With none chosen, or when that server cannot be used, a
//!   local chat's request answers an empty completion and starts nothing: its draft is never sent
//!   to the Claude helper.
//! - `autocomplete_suggest {chatId, requestId, draft}` answers `{completion, retryAfterMs?}`. The
//!   draft is 4–8,000 characters after trimming; the request id is a UUID.
//! - **Budget per chat**, persisted under `autocompleteBudget:<chat>` and updated in one
//!   transaction: at least 2.5 s since the previous attempt and fewer than 10 in the rolling 60 s.
//!   A slot is taken before the model call and never refunded. Over budget, the answer is empty
//!   with a retry time.
//! - **App-wide**, one request runs at a time (a busy request answers empty, retry in 500 ms), and
//!   any helper failure starts a 30 s cooldown for everyone. The whole request has 5 s.
//! - **Context:** the 3 latest user and 2 latest assistant messages with text, active branch only,
//!   each cut to 1,200 characters, oldest first, plus the draft. Never thinking or tool output.
//!
//! The UI asks 350 ms after typing pauses (the 10-per-minute budget is the host's).

use crate::runtime::ChatRuntime;
use crate::types::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const MIN_GAP_MS: i64 = 2_500;
pub const MAX_PER_MINUTE: usize = 10;
pub const DEADLINE: Duration = Duration::from_secs(5);
pub const COOLDOWN: Duration = Duration::from_secs(30);
pub const BUSY_RETRY_MS: i64 = 500;
pub const CONTEXT_CHARS: usize = 1_200;

/// The app-wide state: one request at a time, and the failure cooldown.
#[derive(Default)]
pub(crate) struct AutocompleteGate {
    pub busy: bool,
    pub cooldown_until: Option<Instant>,
}

/// Whether the user turned autocomplete on (missing or unreadable means off).
pub fn enabled(tx: &rusqlite::Transaction<'_>) -> bool {
    match helm_core::prefs::read(tx, "autocomplete") {
        Some(Value::Bool(b)) => b,
        Some(Value::Object(m)) => m.get("enabled") == Some(&Value::Bool(true)),
        _ => false,
    }
}

/// Take one slot of the chat's budget, or answer how long to wait.
pub fn take_slot(
    tx: &rusqlite::Transaction<'_>,
    chat_id: &str,
    now: i64,
) -> rusqlite::Result<Result<(), i64>> {
    let key = format!("autocompleteBudget:{chat_id}");
    let mut stamps: Vec<i64> = helm_core::prefs::read(tx, &key)
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    stamps.retain(|t| now - t < 60_000 && *t <= now);
    if let Some(last) = stamps.iter().max() {
        if now - last < MIN_GAP_MS {
            return Ok(Err(MIN_GAP_MS - (now - last)));
        }
    }
    if stamps.len() >= MAX_PER_MINUTE {
        let oldest = stamps.iter().min().copied().unwrap_or(now);
        return Ok(Err(60_000 - (now - oldest)));
    }
    stamps.push(now);
    helm_core::prefs::set_raw(tx, &key, &json!(stamps))
        .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;
    Ok(Ok(()))
}

/// Whether this chat's draft may be completed, and whether the chat runs on a local model server
/// (it keeps the server's id in `provider`): `None` when the chat is missing or put away.
pub fn suggestable(
    tx: &rusqlite::Transaction<'_>,
    chat_id: &str,
) -> rusqlite::Result<Option<bool>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT provider IS NOT NULL AND provider != '' FROM chats WHERE id = ?1 AND archived = 0",
        [chat_id],
        |r| r.get(0),
    )
    .optional()
}

/// The recent turns sent with the draft.
pub fn context(tx: &rusqlite::Transaction<'_>, chat_id: &str) -> rusqlite::Result<Vec<Value>> {
    let pick = |role: &str, n: i64| -> rusqlite::Result<Vec<(i64, i64, String)>> {
        let mut st = tx.prepare(
            "SELECT timestamp, id, text FROM messages WHERE chat_id = ?1 AND role = ?2 AND active = 1
             AND text != '' ORDER BY timestamp DESC, id DESC LIMIT ?3",
        )?;
        let rows = st.query_map(rusqlite::params![chat_id, role, n], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect()
    };
    let mut all: Vec<(i64, i64, &str, String)> = Vec::new();
    for (t, i, s) in pick("user", 3)? {
        all.push((t, i, "user", s));
    }
    for (t, i, s) in pick("assistant", 2)? {
        all.push((t, i, "assistant", s));
    }
    all.sort_by_key(|(t, i, _, _)| (*t, *i));
    Ok(all
        .into_iter()
        .map(|(_, _, role, text)| json!({"role": role, "text": text.chars().take(CONTEXT_CHARS).collect::<String>()}))
        .collect())
}

fn empty(retry: Option<i64>) -> AutocompleteResult {
    AutocompleteResult {
        completion: String::new(),
        retry_after_ms: retry,
    }
}

/// `autocomplete_suggest`.
pub async fn suggest(
    rt: &ChatRuntime,
    a: AutocompleteArgs,
) -> Result<AutocompleteResult, helm_ipc::ApiError> {
    if uuid::Uuid::parse_str(&a.request_id).is_err() {
        return Err(helm_ipc::ApiError::bad_args("requestId must be a UUID."));
    }
    let n = a.draft.trim().chars().count();
    if !(4..=8_000).contains(&n) {
        return Ok(empty(None));
    }
    let now = helm_core::now_ms();
    let prep = rt
        .db()
        .write(|tx| {
            let Some(local) = suggestable(tx, &a.chat_id)? else {
                return Ok(Err(None));
            };
            if !enabled(tx) {
                return Ok(Err(None));
            }
            // A local chat's draft goes only to the model chosen for local chats; with none
            // chosen it goes nowhere.
            if local && !crate::background::local_name_chosen(tx) {
                return Ok(Err(None));
            }
            if let Err(wait) = take_slot(tx, &a.chat_id, now)? {
                return Ok(Err(Some(wait)));
            }
            Ok(Ok((context(tx, &a.chat_id)?, local)))
        })
        .map_err(crate::ops::sql)?;
    let (ctx, local) = match prep {
        Ok(c) => c,
        Err(retry) => return Ok(empty(retry)),
    };
    let mut job = json!({"draft": a.draft, "context": ctx});
    if let Err(why) = crate::background::decorate_checked(&rt.sh, &a.chat_id, &mut job) {
        if local {
            // The chosen server cannot be used: say so, and do not hand the draft to Claude.
            log::warn!("chat {}: autocomplete not run ({why})", a.chat_id);
            return Ok(empty(Some(COOLDOWN.as_millis() as i64)));
        }
        log::warn!("chat {}: the background job stays on the default model ({why})", a.chat_id);
    }
    {
        let mut g = rt.sh.autocomplete.lock().unwrap();
        if g.cooldown_until.is_some_and(|t| t > Instant::now()) {
            let left = g
                .cooldown_until
                .unwrap()
                .saturating_duration_since(Instant::now());
            return Ok(empty(Some(left.as_millis() as i64)));
        }
        if g.busy {
            return Ok(empty(Some(BUSY_RETRY_MS)));
        }
        g.busy = true;
    }
    // Released however this request ends, a cancelled one included.
    struct Release<'a>(&'a std::sync::Mutex<AutocompleteGate>);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0.lock().unwrap().busy = false;
        }
    }
    let release = Release(&rt.sh.autocomplete);
    let spawner = rt.sh.spawner.clone();
    let out = tokio::time::timeout(
        DEADLINE,
        tokio::task::spawn_blocking(move || spawner.run_autocomplete(&job)),
    )
    .await;
    drop(release);
    let mut g = rt.sh.autocomplete.lock().unwrap();
    match out {
        Ok(Ok(Ok(c))) => Ok(AutocompleteResult {
            completion: c,
            retry_after_ms: None,
        }),
        _ => {
            g.cooldown_until = Some(Instant::now() + COOLDOWN);
            Ok(empty(Some(COOLDOWN.as_millis() as i64)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;

    #[test]
    fn the_budget_needs_a_gap_and_caps_ten_a_minute() {
        let (_d, db) = temp_db();
        let take = |t: i64| db.write(|tx| take_slot(tx, "c1", t)).unwrap();
        assert_eq!(take(100_000), Ok(()));
        assert_eq!(
            take(101_000),
            Err(1_500),
            "2.5 s since the previous attempt"
        );
        for i in 1..10 {
            assert_eq!(take(100_000 + i * 2_500), Ok(()), "attempt {i}");
        }
        assert!(
            take(100_000 + 10 * 2_500).is_err(),
            "the eleventh in a minute is refused"
        );
        assert_eq!(take(161_000), Ok(()), "the oldest has aged out");
        // Another chat has its own budget.
        assert_eq!(db.write(|tx| take_slot(tx, "c2", 101_000)).unwrap(), Ok(()));
    }

    #[test]
    fn an_open_chat_is_suggestable_and_says_whether_it_is_local() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('claude', 'p', 0, 0), ('put_away', 'p', 0, 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at, provider, model) VALUES ('local', 'p', 0, 0, 'ollama', 'qwen3:8b')",
                [],
            )?;
            tx.execute("UPDATE chats SET archived = 1 WHERE id = 'put_away'", [])?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        // A chat on a local server is suggestable too (the request then needs a model chosen for
        // local chats); a put-away or missing chat is not.
        for (id, want) in [
            ("claude", Some(false)),
            ("local", Some(true)),
            ("put_away", None),
            ("missing", None),
        ] {
            assert_eq!(db.snapshot(|tx| suggestable(tx, id)).unwrap(), want, "{id}");
        }
        // Only a name under `local` counts as a model chosen for local chats.
        for (pref, want) in [
            (None, false),
            (Some(json!({"claude": "haiku"})), false),
            (Some(json!({"local": "qwen-helm-4k"})), true),
        ] {
            db.write(|tx| match &pref {
                Some(v) => helm_core::prefs::set_raw(tx, crate::background::PREF_KEY, v),
                None => Ok(()),
            })
            .unwrap();
            let chosen = db
                .snapshot(|tx| Ok::<_, rusqlite::Error>(crate::background::local_name_chosen(tx)))
                .unwrap();
            assert_eq!(chosen, want, "{pref:?}");
        }
    }

    #[test]
    fn off_by_default() {
        let (_d, db) = temp_db();
        assert!(!db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(enabled(tx)))
            .unwrap());
        db.write(|tx| helm_core::prefs::set_raw(tx, "autocomplete", &json!({"enabled": true})))
            .unwrap();
        assert!(db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(enabled(tx)))
            .unwrap());
    }
}
