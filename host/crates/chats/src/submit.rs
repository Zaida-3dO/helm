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

#[allow(clippy::too_many_arguments)]
fn insert_submission(
    tx: &Transaction<'_>,
    id: &str,
    chat_id: &str,
    text: &str,
    mode: SubmissionMode,
    state: SubmissionState,
    origin: SubmissionOrigin,
    priority: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO submissions(id, chat_id, text, mode, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, chat_id, text, store::mode_str(mode), store::state_str(state), now],
    )?;
    tx.execute(
        "INSERT INTO submission_meta(submission_id, origin, priority) VALUES (?1, ?2, ?3)",
        params![
            id,
            if origin == SubmissionOrigin::Harness {
                "harness"
            } else {
                "user"
            },
            priority
        ],
    )?;
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

/// `chat_send`.
pub fn chat_send(rt: &ChatRuntime, a: ChatSendArgs) -> Result<ChatSendResult, ApiError> {
    if a.text.trim().is_empty() {
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
    enum Out {
        Repeat(Submission),
        New(Submission, bool),
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
                        SendMode::Queue => SendAction::Queue,
                        SendMode::Asap => SendAction::Steer,
                        SendMode::Interrupt => SendAction::Now,
                    },
                    other => other,
                };
                match action {
                    SendAction::Queue => (SubmissionMode::FollowUp, SubmissionState::Queued, None),
                    SendAction::Now if status != ChatStatus::Starting => {
                        (SubmissionMode::Prompt, SubmissionState::Saved, Some("now"))
                    }
                    // A turn still starting is never interrupted: the message steers into it.
                    _ => (SubmissionMode::Steer, SubmissionState::Saved, None),
                }
            };
            insert_submission(tx, &sid, &a.chat_id, &a.text, mode, state, SubmissionOrigin::User, priority, now)?;
            // Clear the draft unless the composer has saved a newer one meanwhile.
            tx.execute(
                "UPDATE chats SET draft = '', draft_revision = max(?1, draft_revision + 1), updated_at = ?1
                 WHERE id = ?2 AND draft_revision <= ?3",
                params![now, a.chat_id, a.draft_revision],
            )?;
            tx.execute("DELETE FROM chat_queue_holds WHERE chat_id = ?1", [&a.chat_id])?;
            if mode != SubmissionMode::Prompt || priority.is_some() {
                // A busy send: the user row appears when the harness echoes it.
            }
            let sub = store::get_submission(tx, &sid)?.expect("just inserted");
            Ok(Ok(Out::New(sub, state != SubmissionState::Queued)))
        })
        .map_err(sql)??;
    let submission = match out {
        Out::Repeat(s) => s,
        Out::New(s, dispatch) => {
            if let Some(live) = rt.sh.actors.lock().unwrap().get(&a.chat_id) {
                live.live.lock().unwrap().thread.queue_held = false;
            }
            if dispatch {
                if let Err(e) = rt.command(&a.chat_id, Cmd::Dispatch(s.id.clone())) {
                    // Could not hand it over: undo, keep the text for the user.
                    let _ = rt.db().write(|tx| {
                        tx.execute(
                            "UPDATE submissions SET state = 'rejected' WHERE id = ?1",
                            [&s.id],
                        )?;
                        text_to_draft(tx, &a.chat_id, &s.text)
                    });
                    emit_chat(rt, &a.chat_id);
                    return Err(e);
                }
            }
            emit_queue(rt, &a.chat_id);
            s
        }
    };
    let chat = emit_chat(rt, &a.chat_id).ok_or_else(crate::ops::not_found)?;
    Ok(ChatSendResult { submission, chat })
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
            let state = if busy { SubmissionState::Queued } else { SubmissionState::Saved };
            let mode = if busy { SubmissionMode::FollowUp } else { SubmissionMode::Prompt };
            insert_submission(tx, &sid, chat_id, CONTINUATION_TEXT, mode, state, SubmissionOrigin::Harness, None, now)?;
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
            Ok(Some(!busy))
        })
        .map_err(sql)?;
    match dispatch {
        Some(true) => rt.command(chat_id, Cmd::Dispatch(sid)),
        Some(false) => Ok(()),
        None => Ok(()),
    }
}

/// `queue_resume`: release a held queue and let it advance.
pub fn queue_resume(rt: &ChatRuntime, chat_id: &str) -> Result<(), ApiError> {
    rt.db()
        .write(|tx| tx.execute("DELETE FROM chat_queue_holds WHERE chat_id = ?1", [chat_id]))
        .map_err(sql)?;
    if let Some(live) = rt.sh.actors.lock().unwrap().get(chat_id) {
        live.live.lock().unwrap().thread.queue_held = false;
    }
    rt.command(chat_id, Cmd::Kick)
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
