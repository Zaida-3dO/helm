//! Automatic titles and renames.
//!
//! - The fallback title (first non-blank line, at most 72 characters) is written by `chat_send` and
//!   by a host send (another chat's message, a schedule, the phone); the lines Helm puts in front of
//!   such a message — another chat's frame, the PC it was typed on, the Telegram and voice notes —
//!   are skipped, so a title comes from the person's own words.
//! - A chat on a local model server is named by its own local background model when one is chosen,
//!   else by Claude's small model from the one message the title is made from (the first, or the
//!   first after a greeting) alone, unless the person turned that off (the `titlesLocalViaClaude`
//!   preference, on by default); with neither, the fallback stays.
//! - One state decides everything: a chat either *has a real title* (`generated`, or `manual`
//!   after a rename by hand — both end the automatic naming for good) or *needs a title*, where
//!   the first-line fallback is only a stand-in and a naming job waits parked (`greeting` or
//!   `failed`). A chat that needs a title retries on each of its sends until a real title lands,
//!   at most [`MAX_TITLE_ATTEMPTS`] helper runs in all; past the cap the stand-in stays and
//!   Helm stops asking. A manual rename always means has-a-title and ends every retry. The
//!   parked job is claimed to `claimed` inside the retrying send's own transaction, so two sends
//!   at once never both run it; a claimed or generating job starts no second one, and a claim
//!   or run the app quitting caught is parked `failed` again at start-up ([`crate::schema`]).
//!   Profiles that once saved a label ("Title", "Conversation title") as a generated title are
//!   repaired once at start-up, back to needing a title with the stand-in restored.
//! - A title job starts with the first submission of a fresh chat (still "New chat", no messages)
//!   or of a fork, from either path, and on every later send while the chat still needs a title:
//!   after a greeting that was parked, after a run that failed or timed out, and after a run
//!   whose answer was a label rather than a name. A retry reads the conversation so far, not one
//!   message: the first request that is more than a greeting, the latest one, and short excerpts
//!   of the requests in between ([`retry_job`]). At most [`MAX_CONCURRENT`] helper runs at once
//!   and [`MAX_JOBS`] jobs in all; a job past that fails at once and the fallback stays. The
//!   helper (`helpers/titles`) has its own 45 s timeout and one stricter retry; the host also
//!   gives up after [`HOST_TIMEOUT`]. A run the app quitting caught mid-flight is parked
//!   `failed` again at start-up (see [`crate::schema`]), so the chat's next send retries it.
//! - Success writes the title as `generated` unless the user renamed the chat meanwhile: a manual
//!   rename always wins. An answer that is only a label ("Title", "Conversation title") names
//!   nothing: the helper rejects its own and the host checks again ([`is_placeholder_label`]), so
//!   a label never reaches the database. Failure marks the job `failed` and the fallback stays;
//!   a failed job is parked just like a greeting one, so the chat's next send starts a fresh
//!   job — one bad helper run never leaves a chat unnamed for good, until the cap.
//! - the job waits until the chat's turn has settled (at most
//!   [`crate::runtime::Limits::title_wait`]), so the helper's claude never competes with the
//!   chat's own cold start. A greeting ("Hello", "hi there") is never titled: the job is parked
//!   as `greeting` and the chat's next send starts a fresh one from the conversation as it then
//!   stands — the same wait-and-retry path as a failed run, not a special case.
//! - `chat_rename {chatId, title}` (1–512 bytes) sets the title and marks it manual.
//!   `title_suggest {chatId}` runs the helper and answers up to three suggestions without writing
//!   any; the helper also sees the titles (only) of the chat's siblings and of its profile's recent
//!   chats, so the options follow the person's naming habits. When the helper cannot answer, the
//!   answer is one name cut from the first message, with a plain note saying why.

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
/// How many helper runs one chat may spend on a name in all. A label answer, a helper failure
/// and a timeout each cost one; a greeting that waits for a real request costs nothing. Five
/// looks, at five points of the conversation, are enough to know a name is not coming: past the
/// cap the first-line stand-in stays and no further run starts.
pub const MAX_TITLE_ATTEMPTS: i64 = 5;
/// How many of the chat's later requests a retry shows the helper, and how long each excerpt is.
pub const RECENT_EXCERPTS: usize = 4;
const RECENT_EXCERPT_CHARS: usize = 800;

/// What a title job sends the helper (its `TitleJob`, minus the command the spawner adds).
pub fn job(first_request: &str, latest: Option<&str>, forked: bool) -> Value {
    let mut j = json!({ "firstRequest": first_request, "forked": forked });
    if let Some(l) = latest {
        j["latest"] = json!(l);
    }
    j
}

/// What a message is about: whatever Helm puts in front of it is not the chat's subject, so a
/// title is made from the person's own words that follow. Three kinds of front matter exist and
/// one message can carry several (a note relayed onward still keeps its own): the frame of a
/// message from another chat or another household member (a header line, the host's fixed
/// paragraph, a blank line, then the body); one bracketed line naming where or how the message
/// arrived — the PC it was typed on, the Telegram notes, the voice mode and unheard-reply lines;
/// and the age line of a message queued while Helm was down. The voice primer older builds put
/// in the first message is in the system prompt now and never heads a new one, so it needs no
/// strip here.
pub fn unframed(text: &str) -> &str {
    let mut t = text;
    loop {
        match strip_front_matter(t) {
            Some(n) if n != t => t = n,
            _ => return t,
        }
    }
}

/// One piece of front matter; answers the text after it, or None when the text already starts
/// with the message's own words.
fn strip_front_matter(t: &str) -> Option<&str> {
    // A message from another chat: header line, the host's paragraph, a blank line, the body.
    if let Some(rest) = t.strip_prefix("[Message from another chat:") {
        let body = rest.split_once("\n\n")?.1;
        return (!body.trim().is_empty()).then_some(body);
    }
    // A household member's message: the same shape, the header naming the person and their PC.
    if let Some(rest) = t.strip_prefix("[Message from ") {
        let (head, body) = rest.split_once("\n\n")?;
        let header_ends = head.lines().next().unwrap_or_default().ends_with(']');
        if header_ends && head.contains("(household)") && !body.trim().is_empty() {
            return Some(body);
        }
        return None;
    }
    // The PC the message was typed on: one bracketed line, the name 1 to 40 characters without
    // brackets, control characters or a line break — exactly as the transcript reads it.
    if let Some(rest) = t.strip_prefix("[Typed on ") {
        let (inner, tail) = bracket_line(rest)?;
        let ok = (1..=40).contains(&inner.chars().count())
            && !inner
                .chars()
                .any(|c| c == '[' || (c as u32) < 0x20 || c == '\u{7f}');
        return ok.then_some(tail);
    }
    // The Telegram notes and the voice lines: one bracketed line each, closed on the same line.
    for p in [
        "[Sent from Telegram:",
        "[Sent to the user on Telegram by other chats",
        "[Voice · ",
        "[Not heard by the user: ",
    ] {
        if let Some(rest) = t.strip_prefix(p) {
            let (_, tail) = bracket_line(rest)?;
            return Some(tail);
        }
    }
    // The age line of a message queued while Helm was down.
    const AGE_TAIL: &str = " min ago, queued while Helm was offline)\n";
    if let Some(rest) = t.strip_prefix("(sent ") {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && rest[digits..].starts_with(AGE_TAIL) {
            return Some(&rest[digits + AGE_TAIL.len()..]);
        }
    }
    None
}

/// `rest` followed a `[` prefix: the bracketed line ends at the first `]\n` on that line; the
/// inside holds no bracket and no line break. Answers the inside and the text after the break.
fn bracket_line(rest: &str) -> Option<(&str, &str)> {
    let end = rest.find("]\n")?;
    let inner = &rest[..end];
    if inner.contains('\n') || inner.contains(']') {
        return None;
    }
    Some((inner, &rest[end + 2..]))
}

/// The fallback title for a first message: its first non-blank line, at most
/// [`store::FALLBACK_TITLE_CHARS`] characters.
pub fn fallback_title(text: &str) -> Option<String> {
    unframed(text)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(store::FALLBACK_TITLE_CHARS).collect())
}

/// Write the fallback title in place of "New chat", never over a manual title.
pub(crate) fn write_fallback(
    tx: &rusqlite::Transaction<'_>,
    chat_id: &str,
    text: &str,
) -> rusqlite::Result<()> {
    if let Some(fallback) = fallback_title(text) {
        tx.execute(
            "UPDATE chats SET title = ?1 WHERE id = ?2 AND title = ?3
               AND NOT EXISTS(SELECT 1 FROM chat_titles t WHERE t.chat_id = ?2 AND t.state = 'manual')",
            params![fallback, chat_id, store::DEFAULT_TITLE],
        )?;
    }
    Ok(())
}

/// Record what this send left parked or running. Only starting a helper run (`generating`) costs
/// the chat one of its [`MAX_TITLE_ATTEMPTS`]; parking a greeting or a failure costs nothing.
fn set_job(rt: &ChatRuntime, chat_id: &str, job: Option<&str>) {
    let run = i64::from(job == Some("generating"));
    let _ = rt.db().write(|tx| {
        tx.execute(
            "INSERT INTO chat_titles(chat_id, state, job, attempts, updated_at)
             VALUES (?1, 'fallback', ?2, ?4, ?3)
             ON CONFLICT(chat_id) DO UPDATE SET
                 job = excluded.job,
                 attempts = attempts + ?4,
                 updated_at = excluded.updated_at",
            params![chat_id, job, helm_core::now_ms(), run],
        )
    });
}

/// Words a message made only of is a greeting or a check-in, not something to name a chat after.
const GREETING_WORDS: &[&str] = &[
    "hi",
    "how's",
    "hows",
    "hello",
    "hey",
    "heya",
    "hiya",
    "howdy",
    "heyo",
    "hai",
    "helo",
    "hii",
    "hej",
    "yo",
    "sup",
    "hola",
    "greetings",
    "morning",
    "afternoon",
    "evening",
    "good",
    "gm",
    "there",
    "all",
    "everyone",
    "claude",
    "thanks",
    "thank",
    "you",
    "ty",
    "ok",
    "okay",
    "test",
    "testing",
    "ping",
    "again",
    "how",
    "are",
    "is",
    "it",
    "going",
    "what's",
    "whats",
    "up",
    "u",
    "ya",
    "doing",
];

/// A message that is only a greeting or a check-in (at most six such words, or no words at all).
pub fn is_greeting(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    words.len() <= 6 && words.iter().all(|w| GREETING_WORDS.contains(&w.as_str()))
}

/// Wait until the chat has no turn in flight: nothing saved, being sent, accepted or delivered,
/// and a settled status. Gives up after `limit` (the job then runs anyway).
async fn wait_for_turn(rt: &ChatRuntime, chat_id: &str, limit: Duration) {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        let settled = rt
            .db()
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT status NOT IN ('starting', 'running', 'stopping')
                            AND NOT EXISTS(SELECT 1 FROM submissions s WHERE s.chat_id = c.id
                                AND s.state IN ('saved', 'dispatching', 'accepted', 'delivered'))
                     FROM chats c WHERE c.id = ?1",
                    [chat_id],
                    |r| r.get::<_, bool>(0),
                )
                .optional()
            })
            .ok()
            .flatten()
            .unwrap_or(true);
        if settled {
            return;
        }
        tokio::time::sleep(TURN_POLL).await;
    }
}

const TURN_POLL: Duration = Duration::from_millis(200);

/// Start a title job for `chat_id` in the background (after its turn settles; a greeting is
/// parked instead, see the module notes).
pub fn start(rt: &ChatRuntime, chat_id: &str, job_body: Value) {
    let first = job_body["firstRequest"].as_str().unwrap_or_default();
    if job_body.get("latest").is_none() && is_greeting(first) {
        set_job(rt, chat_id, Some("greeting"));
        return;
    }
    let mut job_body = job_body;
    // A local chat is named by its own local background model, else by Claude's small model from
    // the one message the title is made from, the first or the first after a greeting (the person can
    // switch that off); with neither the fallback stays.
    if let Err(why) = crate::background::decorate_title(&rt.sh, chat_id, &mut job_body) {
        // Nothing asked for this chat can name it (a local chat with no background model while
        // the Claude route is switched off): leave no job parked at all. Parking `failed` would
        // warn again on every send forever and charge the chat for something no retry can fix
        // while the setting stands.
        log::warn!("chat {chat_id}: no automatic title ({why}); the fallback stays");
        set_job(rt, chat_id, None);
        return;
    }
    set_job(rt, chat_id, Some("generating"));
    let rt = rt.clone();
    let chat = chat_id.to_owned();
    rt.handle().clone().spawn(async move {
        let wait = rt.limits().title_wait;
        wait_for_turn(&rt, &chat, wait).await;
        // A job takes one of the MAX_JOBS places only once it is ready to run, so jobs waiting
        // for their turn never crowd out another chat's title.
        let q = &rt.sh.titles;
        if q.jobs.fetch_add(1, Ordering::SeqCst) >= MAX_JOBS {
            q.jobs.fetch_sub(1, Ordering::SeqCst);
            set_job(&rt, &chat, Some("failed"));
            return;
        }
        let permit = rt.sh.titles.slots.acquire().await;
        let spawner = rt.sh.spawner.clone();
        let route = route_note(&job_body);
        let started = std::time::Instant::now();
        let out = tokio::time::timeout(
            HOST_TIMEOUT,
            tokio::task::spawn_blocking(move || spawner.run_title(&job_body)),
        )
        .await;
        drop(permit);
        rt.sh.titles.jobs.fetch_sub(1, Ordering::SeqCst);
        match out {
            Ok(Ok(Ok(answer))) => {
                let title = screened(&answer);
                if title.is_empty() || is_placeholder_label(&title) {
                    // An answer that is only a label ("Title", "Conversation title") names
                    // nothing: park the run as failed like any other miss and let the next
                    // send try again. The helper rejects its own labels; this also covers an
                    // older helper whose answer slipped through.
                    log::warn!(
                        "chat {chat}: no automatic title (the answer was a label, not a name, after {} ms, {route}); the fallback stays",
                        started.elapsed().as_millis()
                    );
                    set_job(&rt, &chat, Some("failed"));
                    return;
                }
                log::info!(
                    "chat {chat}: title made in {} ms ({route})",
                    started.elapsed().as_millis()
                );
                finish(&rt, &chat, &title)
            }
            other => {
                let why = match other {
                    Ok(Ok(Err(e))) => e,
                    Err(_) => "timed out".into(),
                    _ => "the helper did not run".into(),
                };
                log::warn!(
                    "chat {chat}: no automatic title ({why}, after {} ms, {route}); the fallback stays",
                    started.elapsed().as_millis()
                );
                set_job(&rt, &chat, Some("failed"));
            }
        }
    });
}

/// Which model a background job asks, for the log: the route and the model name (the helper's own
/// small default, haiku, when none is named).
fn route_note(job: &Value) -> String {
    let model = job["model"].as_str().unwrap_or("haiku");
    if job.get("localBaseUrl").is_some() {
        format!("local model {model}")
    } else {
        format!("Claude {model}")
    }
}

/// A title that is only the word for "title", with maybe an article or a prefix in front of it
/// ("Title", "The conversation", "Untitled chat"), is not a name. The titles helper rejects
/// these itself; the host checks the answer again so a label can never reach the database, even
/// from a helper build that predates its own check or an answer that slipped through it.
pub(crate) fn is_placeholder_label(title: &str) -> bool {
    let t = title.trim().to_lowercase();
    let t = t.trim_end_matches([':', '.', '!', '?']).trim();
    let core = ["a ", "the ", "new ", "conversation ", "chat ", "untitled "]
        .into_iter()
        .find_map(|p| t.strip_prefix(p))
        .unwrap_or(t);
    matches!(
        core.trim_end_matches([':', '.', '!', '?']),
        "title"
            | "chat"
            | "conversation"
            | "name"
            | "summary"
            | "untitled"
            | "no title"
            | "untitled chat"
            | "untitled conversation"
    )
}

/// A title with the characters that change how it reads without showing (direction overrides,
/// zero-width and filler characters) taken out, as names are elsewhere, and trimmed.
pub(crate) fn screened(title: &str) -> String {
    title
        .chars()
        .filter(|c| !helm_core::hidden::is_hidden_char(*c))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Write a generated title unless a manual rename came first.
fn finish(rt: &ChatRuntime, chat_id: &str, title: &str) {
    let title = screened(title);
    if title.is_empty() {
        return;
    }
    let title = title.as_str();
    let wrote = rt
        .db()
        .write(|tx| {
            // A manual title always wins; and a chat that already carries a generated title
            // (a second run that raced in) is not overwritten by another.
            let done: bool = tx
                .query_row(
                    "SELECT state IN ('manual', 'generated') FROM chat_titles WHERE chat_id = ?1",
                    [chat_id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if done {
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
    let clean = screened(title);
    let t = clean.as_str();
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

/// How many other titles a suggestion job may see: the chat's siblings in the same board or
/// folder (archived included), and the same profile's most recent titles elsewhere. Titles only.
pub const MAX_SIBLING_TITLES: usize = 30;
pub const MAX_PROFILE_TITLES: usize = 20;
pub const MAX_SUGGESTIONS: usize = 3;
/// A suggestion is a quick look, not a title job: one attempt, no retry.
pub const SUGGEST_TIMEOUT: Duration = Duration::from_secs(25);

/// The naming habits a suggestion should match: `(siblings, profile)`. Both are only this chat's
/// own profile's chats (side chats and untitled ones left out, newest first), and the second list
/// leaves out the first's folder so nothing repeats.
fn naming_context(
    rt: &ChatRuntime,
    chat_id: &str,
) -> rusqlite::Result<(String, Vec<String>, Vec<String>)> {
    rt.db().snapshot(|tx| {
        let (project, profile, title): (String, String, String) = tx.query_row(
            "SELECT project_id, profile_id, title FROM chats WHERE id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let titles = |sql: &str, limit: usize| -> rusqlite::Result<Vec<String>> {
            let mut st = tx.prepare(sql)?;
            let rows = st.query_map(
                params![
                    project,
                    profile,
                    chat_id,
                    store::DEFAULT_TITLE,
                    limit as i64
                ],
                |r| r.get::<_, String>(0),
            )?;
            rows.collect()
        };
        let siblings = titles(
            "SELECT title FROM chats WHERE project_id = ?1 AND profile_id = ?2 AND id != ?3
               AND side_source_id IS NULL AND trim(title) != '' AND title != ?4
             ORDER BY updated_at DESC, id LIMIT ?5",
            MAX_SIBLING_TITLES,
        )?;
        let profile_titles = titles(
            "SELECT title FROM chats WHERE project_id != ?1 AND profile_id = ?2 AND id != ?3
               AND side_source_id IS NULL AND trim(title) != '' AND title != ?4
             ORDER BY updated_at DESC, id LIMIT ?5",
            MAX_PROFILE_TITLES,
        )?;
        Ok((title, siblings, profile_titles))
    })
}

/// A helper line as an option: one line, no control characters, at most the title limit.
fn clean_suggestion(raw: &str) -> Option<String> {
    let t = screened(raw);
    (!t.is_empty() && t.len() <= MAX_TITLE_BYTES && !t.chars().any(char::is_control)).then_some(t)
}

/// The options to show: cleaned, no repeats (ignoring case), none equal to the current title, at
/// most [`MAX_SUGGESTIONS`].
fn pick_suggestions(raw: Vec<String>, current: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in raw.iter().filter_map(|t| clean_suggestion(t)) {
        let same = |o: &str| o.to_lowercase() == t.to_lowercase();
        if !same(current.trim()) && !out.iter().any(|o| same(o)) {
            out.push(t);
        }
    }
    out.truncate(MAX_SUGGESTIONS);
    out
}

/// The options `title_suggest` answers, and a plain-words note when they are not the model's.
pub struct Suggestions {
    pub titles: Vec<String>,
    pub note: Option<String>,
}

/// A name taken from the first message, for when the helper cannot answer: its first line cut at
/// a word boundary to the plain title length, and never the title the chat has now.
fn suggestion_from_message(first: &str, current: &str) -> Option<String> {
    let line = fallback_title(first)?;
    let line = line.trim_end_matches(['.', ':', ',', ';']).trim();
    let short = if line.chars().count() <= SUGGESTION_FALLBACK_CHARS {
        line.to_owned()
    } else {
        let cut: String = line.chars().take(SUGGESTION_FALLBACK_CHARS).collect();
        match cut.rsplit_once(' ') {
            Some((head, _)) if head.chars().count() >= SUGGESTION_FALLBACK_CHARS / 2 => {
                head.to_owned()
            }
            _ => cut,
        }
    };
    pick_suggestions(vec![short], current).pop()
}

/// How long a fallback suggestion is at most.
const SUGGESTION_FALLBACK_CHARS: usize = 50;

/// `title_suggest`: run the helper on the chat's first and latest requests, plus the titles of its
/// siblings and of the profile's recent chats so the options follow the person's naming habits;
/// answers up to three options and writes nothing. The context is titles only: no message text
/// from any other chat reaches the model. When the helper cannot answer (it failed, it ran out of
/// time, or the chat is on a local model server with no background model chosen) the answer is a
/// name from the chat's first message instead, with a note saying why, so the person is never
/// left waiting on nothing.
pub async fn suggest(rt: &ChatRuntime, chat_id: &str) -> Result<Suggestions, helm_ipc::ApiError> {
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
    let (current, siblings, profile) = naming_context(rt, chat_id).map_err(crate::ops::sql)?;
    let latest = texts.last().filter(|_| texts.len() > 1);
    let mut body = job(unframed(first), latest.map(|l| unframed(l)), false);
    body["suggest"] = json!(MAX_SUGGESTIONS);
    body["currentTitle"] = json!(current);
    body["siblingTitles"] = json!(siblings);
    body["profileTitles"] = json!(profile);
    body["timeoutMs"] = json!(SUGGEST_TIMEOUT.as_millis() as u64 - 2_000);
    let started = std::time::Instant::now();
    let mut route = String::new();
    let why = match crate::background::decorate_title(&rt.sh, chat_id, &mut body) {
        Err(why) => why,
        Ok(()) => {
            route = route_note(&body);
            let spawner = rt.sh.spawner.clone();
            let r = tokio::time::timeout(
                SUGGEST_TIMEOUT,
                tokio::task::spawn_blocking(move || spawner.run_title_suggestions(&body)),
            )
            .await;
            match r {
                Ok(Ok(Ok(raw))) => match pick_suggestions(raw, &current) {
                    list if list.is_empty() => "the model gave nothing new".to_owned(),
                    titles => {
                        log::info!(
                            "chat {chat_id}: {} suggested names in {} ms ({route})",
                            titles.len(),
                            started.elapsed().as_millis()
                        );
                        return Ok(Suggestions { titles, note: None });
                    }
                },
                Ok(Ok(Err(e))) => e,
                Ok(Err(_)) => "the helper did not run".to_owned(),
                Err(_) => "the model took too long".to_owned(),
            }
        }
    };
    log::warn!(
        "chat {chat_id}: no suggested names from the model ({why}, after {} ms{}); offering one from the first message",
        started.elapsed().as_millis(),
        if route.is_empty() { String::new() } else { format!(", {route}") }
    );
    match suggestion_from_message(first, &current) {
        Some(title) => Ok(Suggestions {
            titles: vec![title],
            note: Some(format!(
                "No ideas from the model ({}). This is the start of your first message.",
                why.trim_end_matches('.')
            )),
        }),
        None => Err(helm_ipc::ApiError::new(
            "unavailable",
            format!("No suggestion: {}.", why.trim_end_matches('.')),
        )),
    }
}

/// Start `node <script>`, write the job to stdin, read one JSON line back. Killed after
/// [`HOST_TIMEOUT`]. `low` runs the helper at below-normal priority.
pub fn run_helper(
    node: &std::path::Path,
    script: &std::path::Path,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    job: &Value,
    timeout: Duration,
    low: bool,
) -> Result<Value, String> {
    run_helper_until(node, script, env, job, timeout, low, None)
}

/// [`run_helper`], ended early (the helper killed) once `cancel` is set: the app is quitting
/// and no helper may outlive it.
pub fn run_helper_until(
    node: &std::path::Path,
    script: &std::path::Path,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    job: &Value,
    timeout: Duration,
    low: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Value, String> {
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
        cmd.creation_flags(helm_workers::priority::creation_flags(low));
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("the helper did not start: {e}"))?;
    helm_workers::priority::lower_after_spawn(Some(child.id()), low);
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
    let deadline = std::time::Instant::now() + timeout;
    let out = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        // Look at the cancel flag a few times a second; without one, wait the whole time.
        let slice = match cancel {
            Some(_) => left.min(Duration::from_millis(200)),
            None => left,
        };
        match rx.recv_timeout(slice) {
            Ok(s) => break s,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                if !left.is_zero()
                    && !cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst)) =>
            {
                continue
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst)) {
                        "Helm is quitting".into()
                    } else {
                        "timed out".into()
                    },
                );
            }
        }
    };
    let _ = child.wait();
    serde_json::from_str(out.trim()).map_err(|_| "the helper's answer was not JSON".to_owned())
}

/// What an earlier send left parked: a greeting that waits for a real request, or a naming run
/// that failed, timed out or answered with a label. `None` when nothing waits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Parked {
    None,
    Greeting,
    Failed,
}

/// Whether this send should start a naming run, answering one clear question: does the chat have
/// a real title yet? `generated` or `manual` says it has, and no run ever starts again (a rename
/// by hand ends the automatic naming for good). Otherwise the chat needs a title: a job parked
/// `greeting` or `failed` waits for this send — and is claimed to `claimed` right here, in this
/// transaction, so two sends at once cannot both run it — a fresh chat's first submission (still
/// "New chat", no messages) or a fork's starts one, and the run that started at most
/// [`MAX_TITLE_ATTEMPTS`] times is the last one: past the cap the first-line stand-in stays. A
/// job `claimed` or `generating` starts no second one. It also answers which job is parked, so a
/// retry reads the conversation so far instead of only this send's words ([`retry_job`]). Called
/// inside the send's transaction, before the submission is written; a send that commits but never
/// starts its claimed run hands it back with [`release_claim`].
pub(crate) fn wants_job(
    tx: &rusqlite::Transaction<'_>,
    chat_id: &str,
) -> rusqlite::Result<Option<(bool, Parked)>> {
    let (title, subs, users, forked, titled, parked, attempts): (
        String,
        i64,
        i64,
        bool,
        bool,
        Option<String>,
        i64,
    ) = tx.query_row(
        "SELECT c.title,
                (SELECT count(*) FROM submissions s WHERE s.chat_id = c.id),
                (SELECT count(*) FROM messages m WHERE m.chat_id = c.id AND m.role != 'context'),
                EXISTS(SELECT 1 FROM chat_forks f WHERE f.chat_id = c.id),
                EXISTS(SELECT 1 FROM chat_titles t
                        WHERE t.chat_id = c.id AND t.state IN ('manual', 'generated')),
                (SELECT t.job FROM chat_titles t
                   WHERE t.chat_id = c.id
                     AND t.job IN ('greeting', 'failed', 'generating', 'claimed') LIMIT 1),
                COALESCE((SELECT t.attempts FROM chat_titles t WHERE t.chat_id = c.id), 0)
         FROM chats c WHERE c.id = ?1",
        [chat_id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        },
    )?;
    if titled || attempts >= MAX_TITLE_ATTEMPTS {
        return Ok(None);
    }
    let parked = match parked.as_deref() {
        Some("claimed") | Some("generating") => return Ok(None),
        Some("failed") => Parked::Failed,
        Some("greeting") => Parked::Greeting,
        _ => Parked::None,
    };
    if parked != Parked::None {
        let claimed = tx.execute(
            "UPDATE chat_titles SET job = 'claimed', updated_at = ?2
             WHERE chat_id = ?1 AND job IN ('greeting', 'failed')",
            params![chat_id, helm_core::now_ms()],
        )?;
        return Ok((claimed == 1).then_some((forked, parked)));
    }
    if subs > 0 {
        return Ok(None);
    }
    if forked {
        return Ok(Some((true, Parked::None)));
    }
    Ok((title == store::DEFAULT_TITLE && users == 0).then_some((false, Parked::None)))
}

/// Hand back a job this send claimed but never got to run: a send whose transaction committed
/// anyway (a refused attachment, a refused context block) must not leave the chat parked in
/// `claimed` until the next start-up.
pub(crate) fn release_claim(tx: &rusqlite::Transaction<'_>, chat_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE chat_titles SET job = 'failed' WHERE chat_id = ?1 AND job = 'claimed'",
        [chat_id],
    )?;
    Ok(())
}

/// The job for a retry: the naming missed on an earlier turn, so this run reads the conversation
/// as it now stands, not one message — the chat's first request that is more than a greeting, its
/// latest request, and the requests in between as short excerpts, oldest first. Naming from this
/// send's own words alone would let a short follow-up ("ok") name the chat; the first real
/// request stays the heart of the name, and the later turns show what the chat went on to be
/// about. `fallback_text` is this send's message, used only when every stored request is a
/// greeting or empty — then this send's words are all the chat has.
pub(crate) fn retry_job(
    rt: &ChatRuntime,
    chat_id: &str,
    fallback_text: &str,
    forked: bool,
) -> Value {
    let oldest = substantive_texts(rt, chat_id, false);
    let Some(first) = oldest.first() else {
        return job(unframed(fallback_text), None, forked);
    };
    // The newest turns are read from the back of the queue, so a chat longer than one window
    // is still named after its latest request, not after its fortieth.
    let newest = substantive_texts(rt, chat_id, true);
    let latest = newest
        .last()
        .filter(|t| newest.len() > 1 || *t != first)
        .map(String::as_str);
    let mut body = job(first, latest, forked);
    let before = || {
        newest[..newest.len().saturating_sub(1)]
            .iter()
            .filter(|t| **t != *first)
    };
    let recent: Vec<String> = {
        let mut all: Vec<String> = before().map(|t| excerpt(t)).collect();
        if all.len() > RECENT_EXCERPTS {
            all.drain(..all.len() - RECENT_EXCERPTS);
        }
        all
    };
    if !recent.is_empty() {
        body["recent"] = json!(recent);
    }
    body
}

/// The chat's requests that are more than greetings, in date order: `newest_first` reads from
/// the back of the queue (and answers oldest-first all the same). Withdrawn sends never name
/// anything.
fn substantive_texts(rt: &ChatRuntime, chat_id: &str, newest_first: bool) -> Vec<String> {
    let order = if newest_first {
        "created_at DESC, id DESC"
    } else {
        "created_at, id"
    };
    let mut out: Vec<String> = rt
        .db()
        .snapshot(|tx| {
            let mut st = tx.prepare(&format!(
                "SELECT text FROM submissions WHERE chat_id = ?1 AND state <> 'rejected' ORDER BY {order} LIMIT 40",
            ))?;
            let rows = st.query_map([chat_id], |r| r.get::<_, String>(0))?;
            Ok::<_, rusqlite::Error>(
                rows.filter_map(|r| r.ok())
                    .map(|t| unframed(&t).trim().to_owned())
                    .filter(|t| !t.is_empty() && !is_greeting(t))
                    .collect(),
            )
        })
        .unwrap_or_default();
    if newest_first {
        out.reverse();
    }
    out
}

/// One excerpt of a request for the helper's `recent` list: the helper joins these as they
/// arrive, so the host clips each one here.
fn excerpt(text: &str) -> String {
    let cut: String = text.chars().take(RECENT_EXCERPT_CHARS).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}
