//! Roll over: move a long chat's work into a fresh one.
//!
//! `chat_roll_over {chatId}` asks the chat's own agent for a hand-off (what is done, what is left,
//! the files and branches that matter, open problems), waits for that turn to end, starts a
//! successor chat in the same project and folder with the same model and effort, sends it the
//! hand-off as its first message (with the old chat's title and id so the two can be matched), and
//! archives the old chat, stopping it first if it is somehow working again.
//!
//! The op answers at once with `{started: true}`; the rest runs in the background. When the
//! successor exists the app opens it (`shell_open_chat`). A failure before the successor is
//! started changes nothing: the old chat stays as it was and a `chat_roll_over_failed` event
//! carries a plain message for the UI to show.
//!
//! Refused up front: an archived chat, a chat that is working or has messages waiting, and a chat
//! that has never had an assistant reply.

use crate::actor::Cmd;
use crate::runtime::ChatRuntime;
use crate::submit::{HostSend, HostSendMode};
use crate::types::*;
use helm_ipc::{ApiError, AppEvent, OpCtx, OpRegistry};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::time::Duration;

/// How long the agent gets to write the hand-off.
pub const HANDOFF_WAIT: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(250);
/// The hand-off is cut here, on a character boundary, before it is sent on.
const MAX_HANDOFF_BYTES: usize = 60 * 1024;

pub fn register(reg: &mut OpRegistry) {
    reg.async_op("chat_roll_over", chat_roll_over);
}

/// What the old chat's agent is asked. Plain text, because it appears in that chat's transcript.
pub const REQUEST: &str = "This chat is being rolled over into a fresh one because its context has grown large. Reply with a hand-off for the agent that takes over, and nothing else: no tool calls. Use these headings: Goal, Done, Left to do, Key files and branches, Open problems and decisions. Be specific (paths, branch names, commands, ids) and keep it under 600 words. The new agent starts with no memory of this chat.";

/// The first message of the successor chat.
pub fn successor_message(old_title: &str, old_id: &str, handoff: &str) -> String {
    format!(
        "This chat continues \"{old_title}\" (chat id {old_id}), which was rolled over because its context had grown large. Below is the hand-off its agent wrote. Check the actual state of the repository and any running work yourself, then carry on from \"Left to do\".\n\n---\n\n{handoff}"
    )
}

fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n[hand-off cut here: it was too long]", &s[..end])
}

fn refuse(m: &str) -> ApiError {
    ApiError::new("refused", m)
}

/// The newest message id of the chat (0 when none).
fn last_message_id(rt: &ChatRuntime, chat: &str) -> rusqlite::Result<i64> {
    rt.db().snapshot(|tx| {
        tx.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM messages WHERE chat_id = ?1",
            [chat],
            |r| r.get(0),
        )
    })
}

/// No turn in flight: a settled status and no submission on its way.
fn settled(rt: &ChatRuntime, chat: &str) -> bool {
    rt.db()
        .snapshot(|tx| {
            tx.query_row(
                "SELECT status NOT IN ('starting', 'running', 'stopping')
                        AND NOT EXISTS(SELECT 1 FROM submissions s WHERE s.chat_id = c.id
                            AND s.state IN ('saved', 'dispatching', 'accepted', 'delivered'))
                 FROM chats c WHERE c.id = ?1",
                [chat],
                |r| r.get::<_, bool>(0),
            )
            .optional()
        })
        .ok()
        .flatten()
        .unwrap_or(true)
}

/// The text of the newest assistant message after `after`.
fn handoff_after(rt: &ChatRuntime, chat: &str, after: i64) -> rusqlite::Result<Option<String>> {
    rt.db().snapshot(|tx| {
        tx.query_row(
            "SELECT text FROM messages WHERE chat_id = ?1 AND role = 'assistant' AND active = 1
             AND id > ?2 AND trim(text) != '' ORDER BY id DESC LIMIT 1",
            rusqlite::params![chat, after],
            |r| r.get::<_, String>(0),
        )
        .optional()
    })
}

async fn chat_roll_over(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatIdArgs = crate::ops::parse_args(args)?;
    let rt = (*ctx.services.need::<ChatRuntime>()?).clone();
    let chat = rt
        .db()
        .snapshot(|tx| crate::store::get_chat(tx, &a.chat_id, 0))
        .map_err(crate::ops::sql)?
        .ok_or_else(crate::ops::not_found)?;
    if chat.archived {
        return Err(refuse("That chat is archived."));
    }
    if chat.side_source_id.is_some() {
        return Err(refuse("Promote this side chat before rolling it over."));
    }
    if !settled(&rt, &chat.id) {
        return Err(refuse(
            "This chat is working or has messages waiting. Roll it over when it is idle.",
        ));
    }
    if handoff_after(&rt, &chat.id, 0)
        .map_err(crate::ops::sql)?
        .is_none()
    {
        return Err(refuse(
            "There is nothing to hand over yet: this chat has no replies.",
        ));
    }
    let mark = last_message_id(&rt, &chat.id).map_err(crate::ops::sql)?;
    crate::submit::host_send(
        &rt,
        HostSend {
            chat_id: &chat.id,
            submission_id: &uuid::Uuid::new_v4().to_string(),
            text: REQUEST,
            mode: HostSendMode::Queue,
            origin: SubmissionOrigin::App,
            gated: false,
        },
    )?;
    let handle = rt.handle().clone();
    handle.spawn(async move {
        if let Err(why) = finish(&ctx, &rt, &chat, mark).await {
            log::warn!("roll over of {}: {why}", chat.id);
            ctx.events.emit(
                AppEvent::new("chat_roll_over_failed")
                    .with("chatId", chat.id.clone())
                    .with("message", why),
            );
        }
    });
    Ok(json!({"started": true}))
}

async fn finish(ctx: &OpCtx, rt: &ChatRuntime, chat: &ChatRecord, mark: i64) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + HANDOFF_WAIT;
    let handoff = loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("The agent did not write a hand-off in time. Nothing was changed.".into());
        }
        tokio::time::sleep(POLL).await;
        if !settled(rt, &chat.id) {
            continue;
        }
        break handoff_after(rt, &chat.id, mark)
            .map_err(|e| e.to_string())?
            .ok_or("The agent ended its turn without a hand-off. Nothing was changed.")?;
    };
    let message = successor_message(&chat.title, &chat.id, &cut(&handoff, MAX_HANDOFF_BYTES));
    let mut create = json!({"cwd": chat.cwd, "projectId": chat.project_id});
    if let Some(m) = &chat.model {
        create["model"] = json!(m);
    }
    if let Some(e) = &chat.effort {
        create["effort"] = json!(e);
    }
    let ctx2 = ctx.clone();
    let made = tokio::task::spawn_blocking(move || crate::ops::chat_create(&ctx2, create))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.message)?;
    let new_id = made["id"].as_str().unwrap_or_default().to_owned();
    let title: String = chat.title.chars().take(200).collect();
    let _ = crate::titles::rename(rt, &new_id, &format!("{title} (continued)"));
    let sent = crate::submit::chat_send(
        rt,
        ChatSendArgs {
            chat_id: new_id.clone(),
            submission_id: uuid::Uuid::new_v4().to_string(),
            text: message,
            action: SendAction::Default,
            draft_revision: 0,
            origin: Some(SendOrigin::Action),
            attachments: None,
            context: None,
        },
        None,
    );
    if let Err(e) = sent {
        // The successor never got its brief: remove it and leave the old chat as it was.
        // The normal archive and delete ops, so the sidebar hears about it.
        let (ctx4, id) = (ctx.clone(), new_id.clone());
        let _ = tokio::task::spawn_blocking(move || {
            let args = json!({"chatId": id});
            crate::manage::chat_archive(&ctx4, args.clone())
                .and_then(|_| crate::manage::chat_delete(&ctx4, args))
        })
        .await;
        return Err(format!(
            "The new chat could not be started ({}). Nothing was changed.",
            e.message
        ));
    }
    // Stop first: the old chat should be idle, but never archive over a working one.
    if !settled(rt, &chat.id) {
        let _ = rt
            .ask(
                &chat.id,
                |reply| Cmd::Stop {
                    continue_queue: false,
                    reply,
                },
                rt.limits().stop + Duration::from_secs(5),
            )
            .await;
    }
    let ctx3 = ctx.clone();
    let old = chat.id.clone();
    tokio::task::spawn_blocking(move || crate::manage::chat_archive(&ctx3, json!({"chatId": old})))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| {
            format!(
                "The new chat is started but the old one was not archived: {}",
                e.message
            )
        })?;
    ctx.events
        .emit(AppEvent::new("shell_open_chat").with("chatId", new_id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_successor_message_names_the_old_chat_and_the_cut_respects_characters() {
        let m = successor_message("Fix login", "abc", "Goal: x");
        assert!(m.contains("\"Fix login\" (chat id abc)") && m.ends_with("Goal: x"));
        let s = "é".repeat(10);
        let c = cut(&s, 5);
        assert!(c.starts_with("éé") && c.contains("cut here"));
        assert_eq!(cut("short", 100), "short");
    }
}
