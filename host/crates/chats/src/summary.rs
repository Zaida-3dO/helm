//! The hover card's summary: two plain sentences — what this chat is about, and where it
//! stands now.
//!
//! - The card asks once a chat row has been hovered about a second (`chat_summarize {chatId}`).
//!   The call answers at once with what is stored and whether an answer is on its way; it never
//!   waits on a model. The card shows the last reply the whole time; when a summary arrives it
//!   takes the last reply's place in the same fixed-height box, so nothing jumps or spins.
//! - The cache is `chat_summary`: the text, when it was made (`summary_at`) and the
//!   last-activity it was made for (`for_activity`). A summary is fresh while
//!   `summary_at >= last_activity_at`: once the chat moves the card asks again, and the stamp
//!   means one ask per activity burst — an attempt that found nothing to say stamps too, so a
//!   quiet chat the model could not answer is not asked again until it moves.
//! - The model is always a local one, so hovers never spend the plan. The pick comes from the
//!   catalogue Helm already holds: a model on a Strata server that this PC can reach, preferring
//!   one already loaded, then the smallest. [`pick_summary_model`] is the seam: when a 4B
//!   variant becomes the everyday summary model, the preference moves there and nowhere else.
//! - Guards: one generation per chat at a time ([`crate::runtime::Shared::summaries`], this
//!   PC), and none at all while any chat is answering a turn on a local server — the small call
//!   never cuts in front of a turn and never queues behind one visibly: the card simply keeps
//!   its fallback and the next hover asks again. With no local server, a chat with no words, or
//!   a helper that cannot answer, the card keeps the last-message fallback; nothing retries on
//!   its own.
//! - The job rides `helpers/summarize` exactly like a title job: the loopback address on the
//!   job, the server's key as the one environment variable the helper reads once (never in the
//!   job), thinking off, below-normal priority. A configured variant that is not loaded may
//!   answer slowly or not at all; the job's own timeout covers that, like the background jobs
//!   in [`crate::background`].
//! - The summary rides the chat record, so a chat watched from another PC shows the one made
//!   where it runs; it is only ever made there.

use crate::runtime::ChatRuntime;
use crate::types::{ChatSummary, LocalModelInfo, LocalServerKind};
use helm_ipc::ApiError;
use rusqlite::{params, OptionalExtension};
use serde_json::json;
use std::time::Duration;

/// Messages the summary job reads: the newest of the chat's own words.
pub const SUMMARY_MESSAGES: usize = 30;
/// Characters of each of those the job carries.
pub const MESSAGE_CHARS: usize = 700;
/// The longest summary the host stores.
pub const SUMMARY_CHARS: usize = 280;
/// The helper's own timeout for one attempt (it makes at most two; see `helpers/summarize`).
pub const HELPER_TIMEOUT_MS: u64 = 9_000;
/// What the host gives one helper run end to end.
pub const HOST_TIMEOUT: Duration = Duration::from_secs(22);

/// Whether a stored summary still covers the chat: made at or after its last activity.
pub fn fresh(summary_at: Option<i64>, last_activity_at: i64) -> bool {
    summary_at.is_some_and(|t| t >= last_activity_at)
}

/// What a summary ask does, from what the chat looks like. Pure, so the rules read as one
/// table; [`request`] turns it into the reading, stamping and running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// A stored summary still covers the chat: answer it, no model.
    Cached,
    /// A generation for this chat is already running on this PC.
    Running,
    /// A chat busy with a turn, a local server already carrying someone's turn, or an
    /// attempt already made for this activity: the card keeps its fallback.
    Later,
    /// Nothing covers the chat's newest activity and the way is clear: one generation may start.
    Go,
}

pub fn decide(
    chat_busy: bool,
    local_server_busy: bool,
    in_flight: bool,
    is_fresh: bool,
    asked_for_activity: bool,
) -> Ask {
    if is_fresh {
        return Ask::Cached;
    }
    if in_flight {
        return Ask::Running;
    }
    if chat_busy || local_server_busy || asked_for_activity {
        return Ask::Later;
    }
    Ask::Go
}

/// The model a summary asks: always a local one, so no plan minutes pay for a hover. Out of
/// the servers' last-seen catalogue: any model on a Strata server this PC can reach,
/// preferring one already loaded, then the smallest. **The seam:** when a 4B variant becomes
/// the everyday summary model, the preference moves here and nowhere else.
pub fn pick_summary_model(models: &[LocalModelInfo]) -> Option<&LocalModelInfo> {
    let mut candidates: Vec<&LocalModelInfo> = models
        .iter()
        .filter(|m| {
            m.kind == LocalServerKind::Strata
                && (m.loaded == Some(true) || m.reachable || m.startable)
        })
        .collect();
    candidates.sort_by_key(|m| (m.loaded != Some(true), m.size_bytes.unwrap_or(u64::MAX)));
    candidates.into_iter().next()
}

/// The head and tail of a long message, so the job never carries a giant one.
fn clip(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_owned();
    }
    let head = max * 2 / 3;
    let tail = max - head;
    let head: String = chars[..head].iter().collect();
    let tail: String = chars[chars.len() - tail..].iter().collect();
    format!("{}\n…\n{}", head.trim_end(), tail.trim_start())
}

/// The answer as stored: hidden characters and control characters out, whitespace folded, and
/// a too-long answer cut at the last whole word that fits. Only an answer with nothing left in
/// it is no answer.
pub fn clean(raw: &str) -> String {
    let folded: String = raw
        .chars()
        .filter(|c| {
            !helm_core::hidden::is_hidden_char(*c) && (!c.is_control() || c.is_whitespace())
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out: String = folded.chars().take(SUMMARY_CHARS).collect();
    if folded.chars().count() > SUMMARY_CHARS {
        match out.rfind(' ') {
            Some(i) if i > SUMMARY_CHARS / 2 => out.truncate(i),
            _ => {}
        }
    }
    out.trim_end().to_owned()
}

/// One try at a time per activity burst: take the `chat_summary` row for this activity, and
/// nothing else may take it too. Answers 1 when this caller got it, 0 when the activity is
/// already covered or already asked for.
pub(crate) fn stamp_attempt(
    tx: &rusqlite::Transaction<'_>,
    chat_id: &str,
    for_activity: i64,
    now: i64,
) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO chat_summary(chat_id, checked_at, for_activity) VALUES (?1, ?3, ?2)
         ON CONFLICT(chat_id) DO UPDATE SET checked_at = excluded.checked_at, for_activity = excluded.for_activity
         WHERE chat_summary.for_activity < excluded.for_activity",
        params![chat_id, for_activity, now],
    )
}

/// What one chat looks like to a hover request: its run state, its newest activity, and what
/// the `chat_summary` row holds (nothing when there is no row yet).
struct Seen {
    status: String,
    last_activity: i64,
    summary: Option<String>,
    summary_at: Option<i64>,
    for_activity: Option<i64>,
}

/// `chat_summarize`: answer what the card can show right now, and — when nothing covers the
/// chat's newest activity and the way is clear — start one generation in the background. The
/// call answers at once in every case; a model never sits in front of the UI.
pub(crate) fn request(rt: &ChatRuntime, chat_id: &str) -> Result<ChatSummary, ApiError> {
    let sh = rt.sh.clone();
    let (seen, local_busy) = sh
        .db
        .snapshot(|tx| {
            let row: Option<Seen> = tx
                .query_row(
                    "SELECT c.status,
                            MAX(c.updated_at, COALESCE(a.last_activity_at, 0)),
                            s.summary, s.summary_at, s.for_activity
                     FROM chats c
                     LEFT JOIN chat_activity a ON a.chat_id = c.id
                     LEFT JOIN chat_summary s ON s.chat_id = c.id
                     WHERE c.id = ?1",
                    [chat_id],
                    |r| {
                        Ok(Seen {
                            status: r.get(0)?,
                            last_activity: r.get(1)?,
                            summary: r.get(2)?,
                            summary_at: r.get(3)?,
                            for_activity: r.get(4)?,
                        })
                    },
                )
                .optional()?;
            // The local servers answer one request at a time: while a chat is answering on
            // one, the hover call waits for the next hover rather than cutting in.
            let local_busy = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM chats
                 WHERE provider IS NOT NULL AND status IN ('starting', 'running', 'stopping'))",
                [],
                |r| r.get::<_, bool>(0),
            )?;
            Ok::<_, rusqlite::Error>((row, local_busy))
        })
        .map_err(crate::ops::sql)?;
    let Some(seen) = seen else {
        return Err(crate::ops::not_found());
    };
    let Seen {
        status,
        last_activity,
        summary,
        summary_at,
        for_activity,
    } = seen;
    let busy = matches!(status.as_str(), "starting" | "running" | "stopping");
    let in_flight = sh
        .summaries
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(chat_id);
    let is_fresh = fresh(summary_at, last_activity);
    let asked = for_activity.is_some_and(|f| f >= last_activity);
    let stored = || ChatSummary {
        summary: summary.clone(),
        summary_at,
        generating: false,
    };
    match decide(busy, local_busy, in_flight, is_fresh, asked) {
        Ask::Cached => Ok(ChatSummary {
            generating: in_flight,
            ..stored()
        }),
        Ask::Running => Ok(ChatSummary {
            generating: true,
            ..stored()
        }),
        Ask::Later => Ok(stored()),
        Ask::Go => {
            let now = helm_core::now_ms();
            let mine = sh
                .db
                .write(|tx| stamp_attempt(tx, chat_id, last_activity, now))
                .map(|n| n > 0)
                .map_err(crate::ops::sql)?;
            if !mine {
                // Someone else took this burst; their answer rides the next record news.
                return Ok(ChatSummary {
                    generating: in_flight,
                    ..stored()
                });
            }
            if !sh
                .summaries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(chat_id.to_owned())
            {
                return Ok(ChatSummary {
                    generating: true,
                    ..stored()
                });
            }
            match start(&sh, chat_id) {
                Ok(()) => Ok(ChatSummary {
                    generating: true,
                    ..stored()
                }),
                Err(why) => {
                    log::debug!(
                        "chat {chat_id}: no hover summary ({why}); the card keeps the last reply"
                    );
                    sh.summaries
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(chat_id);
                    Ok(stored())
                }
            }
        }
    }
}

/// Resolve the local route and hand the job to the background. An `Err` is a plain sentence
/// for the log; either way the card keeps its fallback until a summary arrives.
fn start(sh: &std::sync::Arc<crate::runtime::Shared>, chat_id: &str) -> Result<(), String> {
    let (title, messages) = sh
        .db
        .snapshot(|tx| {
            let title: String =
                tx.query_row("SELECT title FROM chats WHERE id = ?1", [chat_id], |r| {
                    r.get(0)
                })?;
            let mut st = tx.prepare(
                "SELECT role, text FROM messages
                 WHERE chat_id = ?1 AND active = 1 AND role IN ('user', 'assistant') AND text != ''
                 ORDER BY timestamp DESC, id DESC LIMIT ?2",
            )?;
            let rows: Vec<(String, String)> = st
                .query_map(params![chat_id, SUMMARY_MESSAGES as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok::<_, rusqlite::Error>((title, rows))
        })
        .map_err(|e| e.to_string())?;
    if messages.is_empty() {
        return Err("the chat has no words to summarise".into());
    }
    let messages: Vec<(String, String)> = messages
        .into_iter()
        .rev()
        .map(|(role, text)| (role, clip(&text, MESSAGE_CHARS)))
        .collect();
    let lm = sh
        .local
        .get()
        .cloned()
        .ok_or("local models are not available")?;
    let catalogue = lm.catalogue(false);
    let model = pick_summary_model(&catalogue).ok_or("no local model this PC can reach")?;
    let (server_id, model_name) =
        crate::local::split(&model.id).ok_or("the catalogue gave a strange model id")?;
    let server = lm.server(server_id, model_name)?;
    let mut job = json!({
        "title": title,
        "messages": messages
            .iter()
            .map(|(role, text)| json!({ "role": role, "text": text }))
            .collect::<Vec<_>>(),
        "timeoutMs": HELPER_TIMEOUT_MS,
        "model": model_name,
        "localBaseUrl": server.base_url,
        "localKey": server.key,
    });
    if let Some(n) = server.context_tokens {
        job["localContextTokens"] = json!(n);
    }
    if let Some(n) = server.max_output_tokens {
        job["localMaxOutputTokens"] = json!(n);
    }
    log::debug!("chat {chat_id}: hover summary on local model {model_name} ({server_id})");
    let chat_id = chat_id.to_owned();
    let rt = crate::runtime::ChatRuntime { sh: sh.clone() };
    sh.handle.spawn(async move {
        let spawner = rt.sh.spawner.clone();
        let out = tokio::time::timeout(
            HOST_TIMEOUT,
            tokio::task::spawn_blocking(move || spawner.run_summarize(&job)),
        )
        .await;
        let text = match out {
            Ok(Ok(Ok(t))) => clean(&t),
            Ok(Ok(Err(e))) => {
                log::warn!("chat {chat_id}: no hover summary ({e}); the card keeps the last reply");
                String::new()
            }
            Ok(Err(_)) => {
                log::warn!("chat {chat_id}: no hover summary (the helper did not run)");
                String::new()
            }
            Err(_) => {
                log::warn!("chat {chat_id}: no hover summary (timed out)");
                String::new()
            }
        };
        if !text.is_empty() {
            let wrote = rt
                .sh
                .db
                .write(|tx| {
                    tx.execute(
                        "UPDATE chat_summary SET summary = ?2, summary_at = ?3, checked_at = ?3 WHERE chat_id = ?1",
                        params![chat_id, text, helm_core::now_ms()],
                    )
                })
                .unwrap_or(0);
            if wrote > 0 {
                // The record carries the summary: this PC's sidebar picks it up, and the copy
                // on every other PC rides the same chat news.
                crate::submit::emit_chat(&rt, &chat_id);
            }
        }
        rt.sh
            .summaries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&chat_id);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;

    fn model(
        id: &str,
        model: &str,
        kind: LocalServerKind,
        loaded: Option<bool>,
        reachable: bool,
        startable: bool,
        size_bytes: Option<u64>,
    ) -> LocalModelInfo {
        LocalModelInfo {
            id: id.to_owned(),
            name: model.to_owned(),
            model: model.to_owned(),
            server: id.split('/').nth(2).unwrap_or("?").to_owned(),
            kind,
            context_tokens: None,
            size_bytes,
            loaded,
            reachable,
            startable,
        }
    }

    #[test]
    fn a_summary_is_fresh_until_the_chat_moves() {
        for (at, last, want) in [
            (None, 0, false),
            (None, 5, false),
            (Some(10), 10, true),
            (Some(10), 9, true),
            (Some(10), 11, false),
        ] {
            assert_eq!(fresh(at, last), want, "{at:?} vs {last}");
        }
    }

    #[test]
    fn one_rule_table_decides_what_an_ask_does() {
        // (chat busy, local server busy, in flight, fresh, asked for this activity) -> answer
        for (busy, server, fly, fr, asked, want) in [
            // A fresh summary answers first, whatever else is true.
            (false, false, false, true, false, Ask::Cached),
            (true, true, true, true, true, Ask::Cached),
            // A generation already running: no second one.
            (false, false, true, false, true, Ask::Running),
            // The way is clear only with no busy local work and no ask for this activity yet.
            (true, false, false, false, false, Ask::Later),
            (false, true, false, false, false, Ask::Later),
            (false, false, false, false, true, Ask::Later),
            (false, false, false, false, false, Ask::Go),
        ] {
            assert_eq!(
                decide(busy, server, fly, fr, asked),
                want,
                "{busy} {server} {fly} {fr} {asked}"
            );
        }
    }

    #[test]
    fn the_summary_model_is_stratas_loaded_model_else_the_smallest_reachable() {
        let big = model(
            "local/strata/qwen-big",
            "qwen-big",
            LocalServerKind::Strata,
            Some(false),
            true,
            false,
            Some(40_000_000_000),
        );
        let loaded = model(
            "local/strata/qwen-mid",
            "qwen-mid",
            LocalServerKind::Strata,
            Some(true),
            true,
            false,
            Some(20_000_000_000),
        );
        let small = model(
            "local/strata/qwen-small",
            "qwen-small",
            LocalServerKind::Strata,
            Some(false),
            true,
            false,
            Some(3_000_000_000),
        );
        let ollama = model(
            "local/ollama/qwen3:8b",
            "qwen3:8b",
            LocalServerKind::Ollama,
            Some(true),
            true,
            false,
            Some(1),
        );
        let down = model(
            "local/strata/qwen-gone",
            "qwen-gone",
            LocalServerKind::Strata,
            Some(false),
            false,
            false,
            Some(1),
        );
        // Loaded first.
        assert_eq!(
            pick_summary_model(&[big.clone(), loaded.clone(), small.clone()])
                .map(|m| m.model.as_str()),
            Some("qwen-mid")
        );
        // Nothing loaded: the smallest reachable.
        assert_eq!(
            pick_summary_model(&[big.clone(), small.clone()]).map(|m| m.model.as_str()),
            Some("qwen-small")
        );
        // Ollama never carries a hover, and an unreachable Strata model is not a candidate.
        assert_eq!(
            pick_summary_model(&[ollama, down]).map(|m| m.model.as_str()),
            None
        );
        // A startable server counts (it answers when asked).
        assert_eq!(
            pick_summary_model(&[model(
                "local/strata/x",
                "x",
                LocalServerKind::Strata,
                None,
                false,
                true,
                None
            )])
            .map(|m| m.model.as_str()),
            Some("x")
        );
        assert_eq!(pick_summary_model(&[]), None);
    }

    #[test]
    fn an_answer_is_plain_words_and_never_longer_than_the_card() {
        assert_eq!(
            clean(" Two sentences.\n\nAbout  this.  "),
            "Two sentences. About this."
        );
        assert_eq!(clean("\u{202e}hidden\u{200b} text\x07!"), "hidden text!");
        let long = "word ".repeat(200);
        let got = clean(&long);
        assert!(got.chars().count() <= SUMMARY_CHARS);
        assert!(
            !got.ends_with(' '),
            "the cut does not leave a dangling space"
        );
        assert!(got.ends_with("word") || got.ends_with('.'));
        assert_eq!(clean("   "), "");
    }

    #[test]
    fn one_try_per_activity_and_the_record_carries_the_summary() {
        let (_d, db) = temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, status, created_at, updated_at) VALUES ('c', 'p', 'idle', 0, 5);
                 INSERT INTO chat_activity(chat_id, last_activity_at) VALUES ('c', 9);",
            )
        })
        .unwrap();
        // The activity stamp: one caller wins it, the next one for the same activity does not.
        let stamp = |db: &helm_core::db::Database, for_activity: i64, now: i64| -> usize {
            db.write(move |tx| stamp_attempt(tx, "c", for_activity, now))
                .unwrap()
        };
        assert_eq!(stamp(&db, 9, 100), 1);
        assert_eq!(stamp(&db, 9, 110), 0, "this activity was already asked for");
        assert_eq!(stamp(&db, 12, 120), 1, "new activity asks again");
        // A written summary rides the chat record.
        db.write(|tx| {
            tx.execute(
                "UPDATE chat_summary SET summary = 'About the plan. It waits on step two.', summary_at = 130, checked_at = 130 WHERE chat_id = 'c'",
                [],
            )
        })
        .unwrap();
        let rec = db
            .snapshot(|tx| crate::store::get_chat(tx, "c", 1))
            .unwrap()
            .unwrap();
        assert_eq!(
            rec.summary.as_deref(),
            Some("About the plan. It waits on step two.")
        );
        assert_eq!(rec.summary_at, Some(130));
        assert!(fresh(rec.summary_at, 12), "fresh against the last activity");
        assert!(!fresh(rec.summary_at, 131), "and stale once the chat moved");
        // Deleting the chat takes its summary with it.
        db.write(|tx| tx.execute("DELETE FROM chats WHERE id = 'c'", []))
            .unwrap();
        let left: i64 = db
            .snapshot(|tx| tx.query_row("SELECT COUNT(*) FROM chat_summary", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(left, 0);
    }
}
