//! Admission: sends, the queue's actions, model changes and the restart continuation.
//!
//! These run on the blocking pool (most under `maintenance`), write the database, then hand the
//! actor a command. The draft is never touched by a refused send.

use crate::actor::Cmd;
use crate::runtime::ChatRuntime;
use crate::send_behavior;
use crate::session_log::{self, RowWrite};
use crate::store;
use crate::types::*;
use helm_ipc::ApiError;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::json;

/// The fixed restart-continuation prompt, in Helm's own words.
pub const CONTINUATION_TEXT: &str = "Helm was restarted while you were working on this task, so \
your last turn was cut short. Before you carry on, check the current state (files, branches, \
running processes and any command you had started) and do not repeat an action that may already \
have completed. Then continue the task.";

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn chat_status(
    tx: &Transaction<'_>,
    chat_id: &str,
) -> rusqlite::Result<Option<(ChatStatus, bool)>> {
    tx.query_row(
        "SELECT status, archived FROM chats WHERE id = ?1",
        [chat_id],
        |r| {
            Ok((
                ChatStatus::parse(&r.get::<_, String>(0)?),
                r.get::<_, bool>(1)?,
            ))
        },
    )
    .optional()
}

/// How a send is admitted. A `Gated` send to an idle chat may wait for a
/// run slot ([`crate::runslots`]); the class is a required argument of the one internal send path
/// so every caller has to say so (the caller table is checked by a test).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionClass {
    /// Starts at once (the user's own sends, the restart continuation).
    Exempt,
    /// Waits for a run slot (automated sends); carries no attachments.
    Gated,
    /// A gated send being re-admitted.
    Readmit,
}

/// One new submission for [`insert_submission`]: every field is required, the admission class
/// included.
pub(crate) struct NewSubmission<'a> {
    pub id: &'a str,
    pub chat_id: &'a str,
    pub text: &'a str,
    pub mode: SubmissionMode,
    pub state: SubmissionState,
    pub origin: SubmissionOrigin,
    pub priority: Option<&'a str>,
    pub admission: AdmissionClass,
    pub attachments: &'a [String],
    pub now: i64,
}

/// The one internal send path: every submission is written here.
pub(crate) fn insert_submission(
    tx: &Transaction<'_>,
    s: &NewSubmission<'_>,
) -> rusqlite::Result<()> {
    if s.admission == AdmissionClass::Gated && !s.attachments.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "a gated send carries no attachments".into(),
        ));
    }
    let NewSubmission {
        id,
        chat_id,
        text,
        mode,
        state,
        origin,
        priority,
        now,
        ..
    } = *s;
    tx.execute(
        "INSERT INTO submissions(id, chat_id, text, mode, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, chat_id, text, store::mode_str(mode), store::state_str(state), now],
    )?;
    tx.execute(
        "INSERT INTO submission_meta(submission_id, origin, priority) VALUES (?1, ?2, ?3)",
        params![id, origin.as_str(), priority],
    )?;
    // The user writing in the chat answers anything its agent flagged for them.
    if matches!(origin, SubmissionOrigin::Typed | SubmissionOrigin::Answer) {
        crate::flags::clear(tx, chat_id)?;
    }
    if state == SubmissionState::Queued {
        let pos: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM queued_submissions WHERE chat_id = ?1",
            [chat_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO queued_submissions(submission_id, chat_id, position) VALUES (?1, ?2, ?3)",
            params![id, chat_id, pos],
        )?;
    }
    Ok(())
}

/// Put `text` into the chat's draft as a newer revision (`max(now, prev + 1)`), after any draft
/// already there. The composer picks it up from the `chat` event.
pub(crate) fn text_to_draft(
    tx: &Transaction<'_>,
    chat_id: &str,
    text: &str,
) -> rusqlite::Result<i64> {
    let (draft, rev): (String, i64) = tx.query_row(
        "SELECT draft, draft_revision FROM chats WHERE id = ?1",
        [chat_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let next = helm_core::now_ms().max(rev + 1);
    let merged = if draft.trim().is_empty() {
        text.to_owned()
    } else {
        format!("{draft}\n\n{text}")
    };
    tx.execute(
        "UPDATE chats SET draft = ?1, draft_revision = ?2, updated_at = ?3 WHERE id = ?4",
        params![merged, next, helm_core::now_ms(), chat_id],
    )?;
    Ok(next)
}

pub(crate) fn emit_chat(rt: &ChatRuntime, chat_id: &str) -> Option<ChatRecord> {
    let rev = rt.clock().next();
    let chat = rt
        .db()
        .snapshot(|t| store::get_chat(t, chat_id, rev))
        .ok()
        .flatten()?;
    rt.sh
        .events
        .emit(ChatEvent::Chat { chat: chat.clone() }.to_app_event());
    Some(chat)
}

pub(crate) fn emit_queue(rt: &ChatRuntime, chat_id: &str) {
    let rev = rt.clock().next();
    if let Ok(pending) = rt.db().snapshot(|t| store::pending(t, chat_id)) {
        rt.sh.events.emit(
            ChatEvent::Queue {
                chat_id: chat_id.to_owned(),
                revision: rev,
                pending,
            }
            .to_app_event(),
        );
    }
}

/// `chat_send`. `context` is the registered panel-context builder,
/// called only for a composer send that carries a `context` payload.
pub fn chat_send(
    rt: &ChatRuntime,
    a: ChatSendArgs,
    context: Option<&crate::seams::SendContext>,
) -> Result<ChatSendResult, ApiError> {
    let origin = a.origin.unwrap_or_default();
    let attachments: Vec<String> = a.attachments.clone().unwrap_or_default();
    if a.text.trim().is_empty() && attachments.is_empty() {
        return Err(ApiError::bad_args("There is nothing to send."));
    }
    if a.text.len() > store::MAX_SUBMISSION_BYTES {
        return Err(ApiError::new(
            "too_large",
            "The message is larger than 2 MB; it is kept in the draft.",
        ));
    }
    let sid = uuid::Uuid::parse_str(&a.submission_id)
        .map_err(|_| ApiError::bad_args("submissionId must be a UUID."))?
        .to_string();
    let limits = rt.limits().clone();
    let sb = rt
        .db()
        .snapshot(|t| {
            Ok::<_, rusqlite::Error>(send_behavior::resolve(
                helm_core::prefs::read(t, "sendBehavior").as_ref(),
            ))
        })
        .map_err(sql)?;
    let now = helm_core::now_ms();
    let gen = rt.context_gen(&a.chat_id);
    let payload = a
        .context
        .as_ref()
        .filter(|_| origin == SendOrigin::Composer);
    if payload.is_some() && context.is_none() {
        log::warn!("chat {}: a context payload arrived but no context builder is registered; sent without it", a.chat_id);
    }
    enum Out {
        Repeat(Submission),
        New(Submission, bool, Option<bool>),
    }
    let out = rt
        .db()
        .write(|tx| {
            if let Some(existing) = store::get_submission(tx, &sid)? {
                return Ok(Ok(Out::Repeat(existing)));
            }
            let Some((status, archived)) = chat_status(tx, &a.chat_id)? else {
                return Ok(Err(crate::ops::not_found()));
            };
            if archived {
                return Ok(Err(ApiError::new("refused", "This chat is archived; your message is kept in the draft.")));
            }
            if status == ChatStatus::Stopping {
                return Ok(Err(ApiError::new("refused", "This chat is stopping; your message is kept, send it again in a moment.")));
            }
            let busy = status.is_busy();
            if !busy {
                let active: i64 = tx.query_row(
                    "SELECT count(*) FROM chats WHERE status IN ('starting', 'running', 'stopping')",
                    [],
                    |r| r.get(0),
                )?;
                if active as usize >= limits.max_active {
                    return Ok(Err(ApiError::new(
                        "capacity",
                        format!("{} chats are already working, which is the most Helm runs at once. Your message is kept in the draft.", limits.max_active),
                    )));
                }
            }
            // What Send means: idle, always a prompt; busy, the action or the mode.
            let (mode, state, priority) = if !busy {
                (SubmissionMode::Prompt, SubmissionState::Saved, None)
            } else {
                let action = match a.action {
                    SendAction::Default => match sb.mode {
                        SendMode::Asap => SendAction::Steer,
                        SendMode::Interrupt => SendAction::Now,
                    },
                    other => other,
                };
                match action {
                    SendAction::Now if status != ChatStatus::Starting => {
                        (SubmissionMode::Prompt, SubmissionState::Saved, Some("now"))
                    }
                    // A turn still starting is never interrupted: the message steers into it.
                    _ => (SubmissionMode::Steer, SubmissionState::Saved, None),
                }
            };
            let title_job = crate::titles::wants_job(tx, &a.chat_id)?;
            let block = match (payload, context) {
                (Some(p), Some(b)) => match b.0.build(tx, &a.chat_id, p, gen) {
                    Ok(block) => Some(block),
                    Err(r) => return Ok(Err(ApiError::from(r))),
                },
                _ => None,
            };
            insert_submission(
                tx,
                &NewSubmission {
                    id: &sid,
                    chat_id: &a.chat_id,
                    text: &a.text,
                    mode,
                    state,
                    origin: origin.into(),
                    priority,
                    admission: AdmissionClass::Exempt,
                    attachments: &attachments,
                    now,
                },
            )?;
            if !attachments.is_empty() {
                if let Err(r) = crate::attachments::link(tx, &a.chat_id, &sid, &attachments)? {
                    // Undo the submission: nothing of this send is kept but the draft.
                    tx.execute("DELETE FROM submissions WHERE id = ?1", [&sid])?;
                    return Ok(Err(ApiError::from(r)));
                }
            }
            // The panel-context block, then the section naming the files, folders and
            // chats attached by reference; both ride the prompt as context, not as the user's text.
            let refs = if attachments.is_empty() {
                Vec::new()
            } else {
                crate::attachments::refs_for(tx, &sid)?
            };
            let context_text: Vec<String> = [
                block.map(|b| b.text),
                crate::attachments::refs_section(&refs),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !context_text.is_empty() {
                tx.execute(
                    "INSERT INTO submission_context(submission_id, text) VALUES (?1, ?2)",
                    params![sid, context_text.join("\n\n")],
                )?;
            }
            // The fallback title: the first non-blank line, at most 72
            // characters, replacing only "New chat" and never a manual title.
            if let Some(line) = a.text.lines().map(str::trim).find(|l| !l.is_empty()) {
                let fallback: String = line.chars().take(store::FALLBACK_TITLE_CHARS).collect();
                tx.execute(
                    "UPDATE chats SET title = ?1 WHERE id = ?2 AND title = ?3
                       AND NOT EXISTS(SELECT 1 FROM chat_titles t WHERE t.chat_id = ?2 AND t.state = 'manual')",
                    params![fallback, a.chat_id, store::DEFAULT_TITLE],
                )?;
            }
            // Clear the draft unless the composer has saved a newer one meanwhile. A send from a
            // button, a notification or a quick reply never touches the draft.
            if origin == SendOrigin::Composer {
                tx.execute(
                    "UPDATE chats SET draft = '', draft_revision = max(?1, draft_revision + 1), updated_at = ?1
                     WHERE id = ?2 AND draft_revision <= ?3",
                    params![now, a.chat_id, a.draft_revision],
                )?;
            }
            let sub = store::get_submission(tx, &sid)?.expect("just inserted");
            Ok(Ok(Out::New(sub, true, title_job)))
        })
        .map_err(sql)??;
    let submission = match out {
        Out::Repeat(s) => s,
        Out::New(s, dispatch, title_job) => {
            if dispatch {
                // Could not hand it over (refused now, or a send soon that never fit): undo,
                // keep the text for the user.
                let undo = {
                    let (rt, chat, id, text) =
                        (rt.clone(), a.chat_id.clone(), s.id.clone(), s.text.clone());
                    move || {
                        let _ = rt.db().write(|tx| {
                            // Only while it is still waiting: never undo a message the chat took.
                            let n = tx.execute(
                                "UPDATE submissions SET state = 'rejected' WHERE id = ?1                                  AND state = 'saved'",
                                [&id],
                            )?;
                            if n == 1 {
                                text_to_draft(tx, &chat, &text)?;
                            }
                            Ok::<_, rusqlite::Error>(())
                        });
                        emit_chat(&rt, &chat);
                        emit_queue(&rt, &chat);
                    }
                };
                let later = undo.clone();
                if let Err(e) =
                    rt.command_soon(&a.chat_id, Cmd::Dispatch(s.id.clone()), move |_| later())
                {
                    undo();
                    return Err(e);
                }
            }
            emit_queue(rt, &a.chat_id);
            if let Some(forked) = title_job {
                crate::titles::start(rt, &a.chat_id, crate::titles::job(&s.text, None, forked));
            }
            s
        }
    };
    let chat = emit_chat(rt, &a.chat_id).ok_or_else(crate::ops::not_found)?;
    Ok(ChatSendResult { submission, chat })
}

/// How a host send lands on a busy chat (cross-chat messages). Both go to Claude at once and are
/// read at the next tool boundary of the running turn (never cancelling running tools); nothing
/// waits in Helm for the turn to end. `Queue` is the older name, kept so existing callers and
/// stored settings keep working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostSendMode {
    Steer,
    Queue,
}

/// A send made by the host for something other than the user's own typing: a cross-chat message
/// (origin `other_chat`, admission Gated) or an app note (origin `app`). Never counts as consent
/// and never follows the user's send behaviour. Idempotent per `submission_id`.
pub struct HostSend<'a> {
    pub chat_id: &'a str,
    pub submission_id: &'a str,
    pub text: &'a str,
    pub mode: HostSendMode,
    pub origin: SubmissionOrigin,
    /// Gated (waits for a run slot, no attachments) for automated sends such as cross-chat
    /// messages; otherwise Exempt (app notes the agent is owed).
    pub gated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSent {
    /// Saved, and handed to the chat now (idle: a prompt; busy: a steer).
    pub delivered: bool,
    /// Always false: a message no longer waits in Helm behind the running turn. Kept so the
    /// shape of the tool replies does not change.
    pub queued: bool,
    /// Saved, waiting for a run slot: its 1-based place in the queue ([`crate::runslots`]).
    pub waiting: Option<usize>,
}

/// One picture sent with a host send (a photo from the phone): checked and stored exactly like an
/// image attached in the composer.
pub struct HostImage<'a> {
    pub name: &'a str,
    pub mime: &'a str,
    pub bytes: &'a [u8],
}

/// Code of the error a host send answers when a picture is refused (its text says why), so the
/// caller can send the message again without it.
pub const IMAGE_REFUSED: &str = "image_refused";

/// One file sent with a host send (a document or recording from the phone): saved where the chat's
/// agent can read it, and attached by reference like a file the user attaches in the composer.
pub struct HostFile<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

/// Code of the error a host send answers when a file cannot be saved (its text says why), so the
/// caller can send the message again without its files.
pub const FILE_REFUSED: &str = "file_refused";

/// Forget pictures and files stored for a send that is not made after all.
fn drop_stored(
    tx: &Transaction<'_>,
    dir: &std::path::Path,
    ids: &[String],
) -> rusqlite::Result<()> {
    for id in ids {
        tx.execute("DELETE FROM chat_attachments WHERE id = ?1", [id])?;
        tx.execute("DELETE FROM chat_attachment_refs WHERE id = ?1", [id])?;
        let path = dir.join(id);
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
    }
    Ok(())
}

pub fn host_send(rt: &ChatRuntime, s: HostSend<'_>) -> Result<HostSent, ApiError> {
    host_send_with_images(rt, s, &[])
}

/// [`host_send`] with pictures attached. Only an ungated send carries them.
pub fn host_send_with_images(
    rt: &ChatRuntime,
    s: HostSend<'_>,
    images: &[HostImage<'_>],
) -> Result<HostSent, ApiError> {
    host_send_with_attachments(rt, s, images, &[])
}

/// [`host_send`] with pictures and files attached. Only an ungated send carries them. The files
/// are listed for the agent as the composer's file references are.
pub fn host_send_with_attachments(
    rt: &ChatRuntime,
    s: HostSend<'_>,
    images: &[HostImage<'_>],
    files: &[HostFile<'_>],
) -> Result<HostSent, ApiError> {
    if s.gated && !(images.is_empty() && files.is_empty()) {
        return Err(ApiError::bad_args("A gated send carries no attachments."));
    }
    if s.text.trim().is_empty() {
        return Err(ApiError::bad_args("The message is empty."));
    }
    if s.text.len() > store::MAX_SUBMISSION_BYTES {
        return Err(ApiError::new("too_large", "The message is too large."));
    }
    let now = helm_core::now_ms();
    let limits = rt.limits().clone();
    // Only a gated send asks the hold (the usage governor): nothing else can wait.
    let held = s.gated && rt.slot_hold(now).is_some();
    let reserved = if s.gated {
        rt.sh.run_slots.reserved()
    } else {
        vec![]
    };
    let out = rt
        .db()
        .write(|tx| {
            if let Some(existing) = store::get_submission(tx, s.submission_id)? {
                let waiting = crate::runslots::position(tx, s.submission_id)?;
                return Ok(Ok((
                    existing.state != SubmissionState::Queued && waiting.is_none(),
                    false,
                    waiting,
                )));
            }
            let Some((status, archived)) = chat_status(tx, s.chat_id)? else {
                return Ok(Err(crate::ops::not_found()));
            };
            if archived {
                return Ok(Err(ApiError::new("refused", "That chat is archived.")));
            }
            if status == ChatStatus::Stopping {
                return Ok(Err(ApiError::new("busy", "That chat is stopping; try again in a moment.")));
            }
            let busy = status.is_busy();
            // A gated start of an idle chat may wait for a run slot (a running chat's message
            // joins its turn as before).
            let wait_head = if s.gated && !busy {
                let head = crate::runslots::is_head(tx, s.chat_id, s.text)?;
                crate::runslots::must_wait(tx, s.chat_id, head, held, &reserved)?.then_some(head)
            } else {
                None
            };
            if !busy && wait_head.is_none() {
                let active: i64 = tx.query_row(
                    "SELECT count(*) FROM chats WHERE status IN ('starting', 'running', 'stopping')",
                    [],
                    |r| r.get(0),
                )?;
                if active as usize >= limits.max_active {
                    return Ok(Err(ApiError::new("capacity", "Too many chats are already working.")));
                }
            }
            let (mode, state) = match (busy, s.mode) {
                (false, _) => (SubmissionMode::Prompt, SubmissionState::Saved),
                (true, HostSendMode::Steer | HostSendMode::Queue) => {
                    (SubmissionMode::Steer, SubmissionState::Saved)
                }
            };
            let dir = rt.sh.cfg.profile_dir.join("attachments");
            let mut ids = Vec::new();
            for img in images {
                match crate::attachments::put(tx, &dir, s.chat_id, img.name, img.mime, img.bytes, now)? {
                    Ok(stored) => ids.push(stored.id),
                    Err(r) => {
                        drop_stored(tx, &dir, &ids)?;
                        return Ok(Err(ApiError::new(IMAGE_REFUSED, r.message)));
                    }
                }
            }
            let mut refs = Vec::new();
            for f in files {
                match crate::attachments::put_file(tx, &dir, s.chat_id, f.name, f.bytes, now)? {
                    Ok(r) => {
                        ids.push(r.id.clone());
                        refs.push(r);
                    }
                    Err(r) => {
                        drop_stored(tx, &dir, &ids)?;
                        return Ok(Err(ApiError::new(FILE_REFUSED, r.message)));
                    }
                }
            }
            insert_submission(
                tx,
                &NewSubmission {
                    id: s.submission_id,
                    chat_id: s.chat_id,
                    text: s.text,
                    mode,
                    state,
                    origin: s.origin,
                    priority: None,
                    admission: if s.gated { AdmissionClass::Gated } else { AdmissionClass::Exempt },
                    attachments: &ids,
                    now,
                },
            )?;
            if !ids.is_empty() {
                if let Err(r) = crate::attachments::link(tx, s.chat_id, s.submission_id, &ids)? {
                    tx.execute("DELETE FROM submissions WHERE id = ?1", [s.submission_id])?;
                    drop_stored(tx, &dir, &ids)?;
                    let code = if refs.is_empty() { IMAGE_REFUSED } else { FILE_REFUSED };
                    return Ok(Err(ApiError::new(code, r.message)));
                }
            }
            // The files are listed for the agent beside the prompt, as data.
            if let Some(section) = crate::attachments::refs_section(&refs) {
                tx.execute(
                    "INSERT INTO submission_context(submission_id, text) VALUES (?1, ?2)",
                    params![s.submission_id, section],
                )?;
            }
            tx.execute("UPDATE chats SET updated_at = ?1 WHERE id = ?2", params![now, s.chat_id])?;
            if let Some(head) = wait_head {
                crate::runslots::insert_wait(tx, s.submission_id, s.chat_id, now, head)?;
                let pos = crate::runslots::position(tx, s.submission_id)?;
                return Ok(Ok((false, true, pos)));
            }
            Ok(Ok((state != SubmissionState::Queued, true, None)))
        })
        .map_err(sql)??;
    let (dispatch, fresh, waiting) = out;
    if fresh && dispatch {
        rt.command(s.chat_id, Cmd::Dispatch(s.submission_id.to_owned()))?;
    }
    if fresh {
        emit_queue(rt, s.chat_id);
        emit_chat(rt, s.chat_id);
        if waiting.is_some() {
            rt.wake_slots();
        }
    }
    Ok(HostSent {
        delivered: dispatch,
        queued: !dispatch && waiting.is_none(),
        waiting,
    })
}

/// Send the restart continuation (origin `harness`, a plain prompt: it never interrupts and never
/// follows the user's send mode) and drop the row, so it is sent once.
pub fn continuation(rt: &ChatRuntime, chat_id: &str) -> Result<(), ApiError> {
    let sid = uuid::Uuid::new_v4().to_string();
    let now = helm_core::now_ms();
    let dispatch = rt
        .db()
        .write(|tx| {
            let n = tx.execute("DELETE FROM restart_continuations WHERE chat_id = ?1", [chat_id])?;
            if n == 0 {
                return Ok(None);
            }
            let Some((status, _)) = chat_status(tx, chat_id)? else {
                return Ok(None);
            };
            let busy = status.is_busy();
            // A chat already working is not made to wait: the prompt is read at its next boundary.
            let state = SubmissionState::Saved;
            let mode = if busy { SubmissionMode::Steer } else { SubmissionMode::Prompt };
            insert_submission(
                tx,
                &NewSubmission {
                    id: &sid,
                    chat_id,
                    text: CONTINUATION_TEXT,
                    mode,
                    state,
                    origin: SubmissionOrigin::App,
                    priority: None,
                    admission: AdmissionClass::Exempt,
                    attachments: &[],
                    now,
                },
            )?;
            let artifacts = rt.sh.cfg.artifacts();
            let row = RowWrite {
                source_key: format!("c:restart:{sid}"),
                role: "context",
                text: "Helm restarted; this chat was working, so it was asked to check its state and continue.".into(),
                thinking: String::new(),
                tool_name: None,
                tool_call_id: None,
                status: "complete",
                timestamp: now,
                details: json!({"contextKind": "restart"}),
                entry_id: None,
                parent_id: None,
            };
            session_log::upsert_row(tx, &artifacts, chat_id, &row, rt.clock().next())?;
            Ok(Some(()))
        })
        .map_err(sql)?;
    match dispatch {
        Some(()) => rt.command(chat_id, Cmd::Dispatch(sid)),
        None => Ok(()),
    }
}

/// `submission_action`.
pub fn submission_action(
    rt: &ChatRuntime,
    a: SubmissionActionArgs,
) -> Result<SubmissionActionResult, ApiError> {
    let sub = rt
        .db()
        .snapshot(|t| store::get_submission(t, &a.submission_id))
        .map_err(sql)?
        .ok_or_else(|| ApiError::new("not_found", "That message is no longer waiting."))?;
    let chat = sub.chat_id.clone();
    let caps = rt
        .live_thread(&chat)
        .map(|t| t.capabilities)
        .unwrap_or_default();
    let take_to_draft = |rt: &ChatRuntime| -> Result<(), ApiError> {
        rt.db()
            .write(|tx| {
                tx.execute(
                    "UPDATE submissions SET state = 'withdrawn' WHERE id = ?1",
                    [&sub.id],
                )?;
                tx.execute(
                    "DELETE FROM queued_submissions WHERE submission_id = ?1",
                    [&sub.id],
                )?;
                text_to_draft(tx, &chat, &sub.text).map(|_| ())
            })
            .map_err(sql)?;
        emit_chat(rt, &chat);
        Ok(())
    };
    match (a.action, sub.state) {
        (SubmissionActionKind::Withdraw, SubmissionState::Queued) => take_to_draft(rt)?,
        (SubmissionActionKind::Withdraw, SubmissionState::Accepted) => {
            if !caps.withdraw {
                return Err(ApiError::new(
                    "unsupported",
                    "This chat cannot take back a waiting message.",
                ));
            }
            let withdrawn = rt.handle().block_on(rt.ask(
                &chat,
                |reply| Cmd::Withdraw {
                    submission_id: sub.id.clone(),
                    reply,
                },
                std::time::Duration::from_secs(15),
            ))?;
            match withdrawn {
                Ok(true) => take_to_draft(rt)?,
                Ok(false) => {
                    return Err(ApiError::new(
                        "conflict",
                        "Claude has already read that message.",
                    ))
                }
                Err(e) => return Err(ApiError::new("refused", e)),
            }
        }
        (
            SubmissionActionKind::Recover,
            SubmissionState::Uncertain | SubmissionState::Rejected | SubmissionState::Queued,
        ) => take_to_draft(rt)?,
        (
            SubmissionActionKind::SteerNow | SubmissionActionKind::SendNow,
            SubmissionState::Queued,
        ) => {
            let busy = rt
                .db()
                .snapshot(|t| chat_status(t, &chat))
                .map_err(sql)?
                .is_some_and(|(s, _)| s.is_busy());
            let now_send = a.action == SubmissionActionKind::SendNow;
            rt.db()
                .write(|tx| {
                    tx.execute(
                        "DELETE FROM queued_submissions WHERE submission_id = ?1",
                        [&sub.id],
                    )?;
                    tx.execute(
                        "UPDATE submissions SET state = 'saved', mode = ?1 WHERE id = ?2",
                        params![if now_send || !busy { "prompt" } else { "steer" }, sub.id],
                    )?;
                    tx.execute(
                        "UPDATE submission_meta SET priority = ?1 WHERE submission_id = ?2",
                        params![if now_send && busy { Some("now") } else { None }, sub.id],
                    )
                })
                .map_err(sql)?;
            rt.command(&chat, Cmd::Dispatch(sub.id.clone()))?;
        }
        (SubmissionActionKind::SendNow, SubmissionState::Accepted) => {
            if !caps.deliver_now {
                return Err(ApiError::new(
                    "unsupported",
                    "This chat cannot deliver a waiting message early.",
                ));
            }
            rt.command(
                &chat,
                Cmd::DeliverNow {
                    submission_id: sub.id.clone(),
                    reason: "send_now",
                },
            )?;
        }
        (SubmissionActionKind::Move, SubmissionState::Queued) => {
            let to = a
                .position
                .ok_or_else(|| ApiError::bad_args("move needs a position"))?;
            rt.db()
                .write(|tx| {
                    let mut ids: Vec<String> = {
                        let mut st = tx.prepare(
                            "SELECT submission_id FROM queued_submissions WHERE chat_id = ?1 ORDER BY position",
                        )?;
                        let rows = st.query_map([&chat], |r| r.get(0))?;
                        rows.collect::<rusqlite::Result<_>>()?
                    };
                    ids.retain(|i| i != &sub.id);
                    let at = (to.max(0) as usize).min(ids.len());
                    ids.insert(at, sub.id.clone());
                    for (i, id) in ids.iter().enumerate() {
                        tx.execute(
                            "UPDATE queued_submissions SET position = ?1 WHERE submission_id = ?2",
                            params![i as i64, id],
                        )?;
                    }
                    Ok(())
                })
                .map_err(sql)?;
        }
        _ => {
            return Err(ApiError::new(
                "conflict",
                "That action does not apply to this message now.",
            ))
        }
    }
    emit_queue(rt, &chat);
    let submission = rt
        .db()
        .snapshot(|t| store::get_submission(t, &a.submission_id))
        .map_err(sql)?
        .ok_or_else(|| ApiError::internal("submission vanished"))?;
    Ok(SubmissionActionResult { submission })
}

/// `chat_set_model`: store the choice, note it in the transcript, drop any pending restart
/// continuation (the chat changed), and apply it from the next turn.
pub fn chat_set_model(rt: &ChatRuntime, a: ChatSetModelArgs) -> Result<ChatRecord, ApiError> {
    if let Some(e) = a.effort.as_deref() {
        if !crate::catalogue::is_effort(e) {
            return Err(ApiError::bad_args(
                "Effort must be low, medium, high, xhigh or max.",
            ));
        }
    }
    let before = rt
        .db()
        .snapshot(|t| store::get_chat(t, &a.chat_id, 0))
        .map_err(sql)?
        .ok_or_else(crate::ops::not_found)?;
    let model = a.model.clone().filter(|m| Some(m) != before.model.as_ref());
    let effort = a
        .effort
        .clone()
        .filter(|e| Some(e) != before.effort.as_ref());
    if model.is_none() && effort.is_none() {
        return Ok(before);
    }
    let now = helm_core::now_ms();
    let artifacts = rt.sh.cfg.artifacts();
    rt.db()
        .write(|tx| {
            if let Some(m) = &model {
                tx.execute(
                    "UPDATE chats SET model = ?1, updated_at = ?2 WHERE id = ?3",
                    params![m, now, a.chat_id],
                )?;
            }
            if let Some(e) = &effort {
                tx.execute(
                    "UPDATE chats SET thinking = ?1, updated_at = ?2 WHERE id = ?3",
                    params![e, now, a.chat_id],
                )?;
            }
            tx.execute(
                "DELETE FROM restart_continuations WHERE chat_id = ?1",
                [&a.chat_id],
            )?;
            for (kind, text) in [
                (
                    model.as_ref().map(|_| ContextKind::ModelChange),
                    model.as_ref().map(|m| format!("Model changed to {m}")),
                ),
                (
                    effort.as_ref().map(|_| ContextKind::EffortChange),
                    effort.as_ref().map(|e| format!("Effort changed to {e}")),
                ),
            ] {
                if let (Some(kind), Some(text)) = (kind, text) {
                    let row = RowWrite {
                        source_key: format!("c:{}", uuid::Uuid::new_v4()),
                        role: "context",
                        text,
                        thinking: String::new(),
                        tool_name: None,
                        tool_call_id: None,
                        status: "complete",
                        timestamp: now,
                        details: json!({"contextKind": kind}),
                        entry_id: None,
                        parent_id: None,
                    };
                    session_log::upsert_row(tx, &artifacts, &a.chat_id, &row, rt.clock().next())?;
                }
            }
            Ok(())
        })
        .map_err(sql)?;
    if rt.sh.actors.lock().unwrap().contains_key(&a.chat_id) {
        rt.command(&a.chat_id, Cmd::ApplyModel { model, effort })?;
    }
    rt.sh.events.emit(
        ChatEvent::Refresh {
            chat_id: a.chat_id.clone(),
        }
        .to_app_event(),
    );
    emit_chat(rt, &a.chat_id).ok_or_else(crate::ops::not_found)
}

/// Permission modes the composer's mode picker offers: the chip picks the mode for the next turn.
pub const PERMISSION_MODES: &[&str] = &["bypassPermissions", "default", "acceptEdits", "plan"];

/// A chat's pick, while Helm runs: the mode, and whether a worker has been given it yet.
#[derive(Clone, Debug)]
struct Pick {
    mode: String,
    delivered: bool,
}

/// The mode each chat's user picked. A worker that starts later (the first send, or after an idle
/// retire) gets a start line; without a pick a worker keeps its launch mode.
fn picks() -> &'static std::sync::Mutex<std::collections::HashMap<String, Pick>> {
    static PICKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Pick>>> =
        std::sync::OnceLock::new();
    PICKS.get_or_init(Default::default)
}

/// The worker line that sets `mode`; the worker applies it before its next prompt.
pub fn permission_mode_line(mode: &str) -> serde_json::Value {
    json!({"type": "set_permission_mode", "mode": mode})
}

/// The start line for a chat whose user picked a mode: the mode to start its next worker in.
pub fn permission_start_line(chat_id: &str) -> Option<serde_json::Value> {
    let mut map = picks().lock().unwrap();
    let pick = map.get_mut(chat_id)?;
    pick.delivered = true;
    Some(permission_mode_line(&pick.mode))
}

/// A worker's `permission_mode_kept` line: after a pick, the mode the chat is really in (Claude Code
/// left plan mode, or refused the pick and stayed where it was); `None` drops a pick that never took.
/// The next worker starts there rather than re-entering the old pick. A pick the user made since,
/// not yet given to any worker, wins.
pub fn note_kept_mode(chat_id: &str, mode: Option<&str>) {
    let mut map = picks().lock().unwrap();
    let Some(pick) = map.get_mut(chat_id) else {
        return;
    };
    if !pick.delivered {
        return;
    }
    match mode.filter(|m| PERMISSION_MODES.contains(m)) {
        Some(m) => pick.mode = m.to_owned(),
        None => {
            map.remove(chat_id);
        }
    }
}

/// Registers the start line that carries a picked permission mode to every new worker, and the
/// handler for the worker's `permission_mode_kept` line.
pub fn register_worker_lines(l: &mut helm_hosttools::WorkerLines) {
    l.register_start_line(|chat_id, _| permission_start_line(chat_id));
    l.register_worker_line("permission_mode_kept", |ctx, v| {
        note_kept_mode(ctx.chat_id, v.get("mode").and_then(|m| m.as_str()));
    });
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSetPermissionModeArgs {
    pub chat_id: String,
    pub mode: String,
}

/// `chat_set_permission_mode`: remember the pick and tell a live worker, which applies it before
/// its next prompt (Claude Code's `set_permission_mode` control request). Answers `{mode}`.
pub fn chat_set_permission_mode(
    rt: &ChatRuntime,
    a: ChatSetPermissionModeArgs,
) -> Result<serde_json::Value, ApiError> {
    if !PERMISSION_MODES.contains(&a.mode.as_str()) {
        return Err(ApiError::bad_args(
            "Permission mode must be bypassPermissions, default, acceptEdits or plan.",
        ));
    }
    rt.db()
        .snapshot(|t| store::get_chat(t, &a.chat_id, 0))
        .map_err(sql)?
        .ok_or_else(crate::ops::not_found)?;
    // A chat with a live worker hears it now; otherwise its next worker gets it at start.
    let live_worker = rt
        .sh
        .actors
        .lock()
        .unwrap()
        .get(&a.chat_id)
        .is_some_and(|h| h.live.lock().unwrap().has_worker);
    picks().lock().unwrap().insert(
        a.chat_id.clone(),
        Pick {
            mode: a.mode.clone(),
            delivered: live_worker,
        },
    );
    if live_worker {
        rt.command(&a.chat_id, Cmd::WorkerLine(permission_mode_line(&a.mode)))?;
    }
    Ok(json!({"mode": a.mode}))
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    /// The caller table: every call of the one internal send path, and the class it passes. A new
    /// caller fails this test until it is added here with its class.
    const CALLERS: &[(&str, &str)] = &[
        ("chat_send", "AdmissionClass::Exempt"),
        (
            "host_send",
            "if s.gated { AdmissionClass::Gated } else { AdmissionClass::Exempt }",
        ),
        ("continuation", "AdmissionClass::Exempt"),
    ];

    #[test]
    fn every_caller_of_the_send_path_states_its_admission_class() {
        let src = include_str!("submit.rs");
        let body = src.split("mod admission_tests").next().unwrap();
        // Every `insert_submission(` that is not its own definition.
        let calls: Vec<&str> = body
            .match_indices("insert_submission(")
            .filter(|(i, _)| !body[..*i].ends_with("fn "))
            .map(|(i, _)| &body[i..])
            .collect();
        assert_eq!(
            calls.len(),
            CALLERS.len(),
            "a caller is missing from the table"
        );
        for (call, (name, class)) in calls.iter().zip(CALLERS) {
            let args = call.split(")?;").next().unwrap();
            assert!(
                args.contains(&format!("admission: {class}")),
                "{name} must pass {class}"
            );
        }
        for (name, _) in CALLERS {
            assert!(
                body.contains(&format!("pub fn {name}(")),
                "{name} is a send path caller"
            );
        }
    }

    #[test]
    fn a_gated_send_refuses_attachments() {
        let (_d, db) = crate::schema::tests::temp_db();
        let chat = db
            .write(|tx| store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap();
        let att = vec!["a1".to_owned()];
        let r = db.write(|tx| {
            insert_submission(
                tx,
                &NewSubmission {
                    id: "00000000-0000-4000-8000-000000000001",
                    chat_id: &chat,
                    text: "x",
                    mode: SubmissionMode::Prompt,
                    state: SubmissionState::Saved,
                    origin: SubmissionOrigin::Schedule,
                    priority: None,
                    admission: AdmissionClass::Gated,
                    attachments: &att,
                    now: 1,
                },
            )
        });
        assert!(r.is_err());
    }
}
