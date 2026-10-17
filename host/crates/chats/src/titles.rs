//! Automatic titles and renames.
//!
//! - The fallback title (first non-blank line, at most 72 characters) is written by `chat_send`.
//! - A title job starts with the first submission of a fresh chat (still "New chat", no messages)
//!   or of a fork. At most [`MAX_CONCURRENT`] helper runs at once and [`MAX_JOBS`] jobs in all; a
//!   job past that fails at once and the fallback stays. The helper (`helpers/titles`) has its own
//!   45 s timeout and one stricter retry; the host also gives up after [`HOST_TIMEOUT`].
//! - Success writes the title as `generated` unless the user renamed the chat meanwhile: a manual
//!   rename always wins. Failure marks the job `failed`; the fallback stays.
//! - `chat_rename {chatId, title}` (1–512 bytes) sets the title and marks it manual.
//!   `title_suggest {chatId}` runs the helper and answers a suggestion without writing it.

use crate::runtime::ChatRuntime;
use crate::store;
use crate::types::*;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub const MAX_CONCURRENT: usize = 2;
pub const MAX_JOBS: usize = 8;
pub const MAX_TITLE_BYTES: usize = 512;
/// Two helper attempts of 45 s each, plus start-up.
pub const HOST_TIMEOUT: Duration = Duration::from_secs(100);

/// What a title job sends the helper (its `TitleJob`, minus the command the spawner adds).
pub fn job(first_request: &str, latest: Option<&str>, forked: bool) -> Value {
    let mut j = json!({ "firstRequest": first_request, "forked": forked });
    if let Some(l) = latest {
        j["latest"] = json!(l);
    }
    j
}

fn set_job(rt: &ChatRuntime, chat_id: &str, job: Option<&str>) {
    let _ = rt.db().write(|tx| {
        tx.execute(
            "INSERT INTO chat_titles(chat_id, state, job, updated_at) VALUES (?1, 'fallback', ?2, ?3)
             ON CONFLICT(chat_id) DO UPDATE SET job = excluded.job, updated_at = excluded.updated_at",
            params![chat_id, job, helm_core::now_ms()],
        )
    });
}

/// Start a title job for `chat_id` in the background.
pub fn start(rt: &ChatRuntime, chat_id: &str, job_body: Value) {
    let q = &rt.sh.titles;
    if q.jobs.fetch_add(1, Ordering::SeqCst) >= MAX_JOBS {
        q.jobs.fetch_sub(1, Ordering::SeqCst);
        set_job(rt, chat_id, Some("failed"));
        return;
    }
    set_job(rt, chat_id, Some("generating"));
    let rt = rt.clone();
    let chat = chat_id.to_owned();
    rt.handle().clone().spawn(async move {
        let permit = rt.sh.titles.slots.acquire().await;
        let spawner = rt.sh.spawner.clone();
        let out = tokio::time::timeout(
            HOST_TIMEOUT,
            tokio::task::spawn_blocking(move || spawner.run_title(&job_body)),
        )
        .await;
        drop(permit);
        rt.sh.titles.jobs.fetch_sub(1, Ordering::SeqCst);
        match out {
            Ok(Ok(Ok(title))) => finish(&rt, &chat, &title),
            other => {
                let why = match other {
                    Ok(Ok(Err(e))) => e,
                    Err(_) => "timed out".into(),
                    _ => "the helper did not run".into(),
                };
                log::warn!("chat {chat}: no automatic title ({why}); the fallback stays");
                set_job(&rt, &chat, Some("failed"));
            }
        }
    });
}

/// Write a generated title unless a manual rename came first.
fn finish(rt: &ChatRuntime, chat_id: &str, title: &str) {
    let wrote = rt
        .db()
        .write(|tx| {
            let manual: bool = tx
                .query_row(
                    "SELECT state = 'manual' FROM chat_titles WHERE chat_id = ?1",
                    [chat_id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if manual {
                return Ok(false);
            }
            tx.execute(
                "UPDATE chats SET title = ?1 WHERE id = ?2",
                params![title, chat_id],
            )?;
            tx.execute(
                "INSERT INTO chat_titles(chat_id, state, job, updated_at) VALUES (?1, 'generated', NULL, ?2)
                 ON CONFLICT(chat_id) DO UPDATE SET state = 'generated', job = NULL, updated_at = excluded.updated_at",
                params![chat_id, helm_core::now_ms()],
            )?;
            Ok::<_, rusqlite::Error>(true)
        })
        .unwrap_or(false);
    if wrote {
        crate::submit::emit_chat(rt, chat_id);
        rt.publish_attention();
    }
}

/// `chat_rename`: a manual title (1–512 bytes after trimming), which always wins.
pub fn rename(
    rt: &ChatRuntime,
    chat_id: &str,
    title: &str,
) -> Result<ChatRecord, helm_ipc::ApiError> {
    let t = title.trim();
    if t.is_empty() || t.len() > MAX_TITLE_BYTES || t.chars().any(char::is_control) {
        return Err(helm_ipc::ApiError::bad_args(
            "A title is 1 to 512 bytes on one line.",
        ));
    }
    let found = rt
        .db()
        .write(|tx| {
            let side: Option<Option<String>> = tx
                .query_row(
                    "SELECT side_source_id FROM chats WHERE id = ?1",
                    [chat_id],
                    |r| r.get(0),
                )
                .optional()?;
            match side {
                None => return Ok(Err("not_found")),
                Some(Some(_)) => return Ok(Err("side")),
                Some(None) => {}
            }
            tx.execute(
                "UPDATE chats SET title = ?1 WHERE id = ?2",
                params![t, chat_id],
            )?;
            tx.execute(
                "INSERT INTO chat_titles(chat_id, state, job, updated_at) VALUES (?1, 'manual', NULL, ?2)
                 ON CONFLICT(chat_id) DO UPDATE SET state = 'manual', job = NULL, updated_at = excluded.updated_at",
                params![chat_id, helm_core::now_ms()],
            )?;
            Ok::<_, rusqlite::Error>(Ok(()))
        })
        .map_err(crate::ops::sql)?;
    match found {
        Ok(()) => {}
        Err("side") => {
            return Err(helm_ipc::ApiError::new(
                "refused",
                "Promote this side chat before renaming it.",
            ))
        }
        Err(_) => return Err(crate::ops::not_found()),
    }
    rt.publish_attention();
    crate::submit::emit_chat(rt, chat_id).ok_or_else(crate::ops::not_found)
}

/// `title_suggest`: run the helper on the chat's first and latest requests; nothing is written.
pub async fn suggest(rt: &ChatRuntime, chat_id: &str) -> Result<String, helm_ipc::ApiError> {
    let texts: Vec<String> = rt
        .db()
        .snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT text FROM messages WHERE chat_id = ?1 AND role = 'user' AND active = 1
                 AND text != '' ORDER BY timestamp, id",
            )?;
            let rows = st.query_map([chat_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .map_err(crate::ops::sql)?;
    let Some(first) = texts.first() else {
        return Err(helm_ipc::ApiError::new(
            "refused",
            "There is nothing to name yet: send a message first.",
        ));
    };
    let latest = texts.last().filter(|_| texts.len() > 1);
    let body = job(first, latest.map(String::as_str), false);
    let spawner = rt.sh.spawner.clone();
    let r = tokio::time::timeout(
        HOST_TIMEOUT,
        tokio::task::spawn_blocking(move || spawner.run_title(&body)),
    )
    .await;
    match r {
        Ok(Ok(Ok(t))) => Ok(t),
        Ok(Ok(Err(e))) => Err(helm_ipc::ApiError::new(
            "unavailable",
            format!("No suggestion: {e}"),
        )),
        _ => Err(helm_ipc::ApiError::new("timeout", "No suggestion in time.")),
    }
}

/// Start `node <script>`, write the job to stdin, read one JSON line back. Killed after
/// [`HOST_TIMEOUT`].
pub fn run_helper(
    node: &std::path::Path,
    script: &std::path::Path,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    job: &Value,
) -> Result<String, String> {
    use std::io::{Read, Write};
    let mut cmd = std::process::Command::new(node);
    cmd.arg(script)
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("the titles helper did not start: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(job.to_string().as_bytes());
    }
    let mut stdout = child.stdout.take().ok_or("no helper output")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let out = match rx.recv_timeout(HOST_TIMEOUT) {
        Ok(s) => s,
        Err(_) => {
            let _ = child.kill();
            return Err("timed out".into());
        }
    };
    let _ = child.wait();
    let v: Value = serde_json::from_str(out.trim())
        .map_err(|_| "the helper's answer was not JSON".to_owned())?;
    match (
        v.get("title").and_then(Value::as_str),
        v.get("error").and_then(Value::as_str),
    ) {
        (Some(t), _) => Ok(t.to_owned()),
        (_, Some(e)) => Err(e.to_owned()),
        _ => Err("the helper gave no title".into()),
    }
}

/// Whether this send should start a title job: the chat's first submission while it is still
/// "New chat" with no messages, or a fork's first submission. Called inside the send's
/// transaction, before the submission is written.
pub(crate) fn wants_job(
    tx: &rusqlite::Transaction<'_>,
    chat_id: &str,
) -> rusqlite::Result<Option<bool>> {
    let (title, subs, users, forked, manual): (String, i64, i64, bool, bool) = tx.query_row(
        "SELECT c.title,
                (SELECT count(*) FROM submissions s WHERE s.chat_id = c.id),
                (SELECT count(*) FROM messages m WHERE m.chat_id = c.id AND m.role != 'context'),
                EXISTS(SELECT 1 FROM chat_forks f WHERE f.chat_id = c.id),
                EXISTS(SELECT 1 FROM chat_titles t WHERE t.chat_id = c.id AND t.state = 'manual')
         FROM chats c WHERE c.id = ?1",
        [chat_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    if manual || subs > 0 {
        return Ok(None);
    }
    if forked {
        return Ok(Some(true));
    }
    Ok((title == store::DEFAULT_TITLE && users == 0).then_some(false))
}
