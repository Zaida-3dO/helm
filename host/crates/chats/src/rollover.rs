//! Roll over: move a long chat's work into a fresh one.
//!
//! `chat_roll_over {chatId}` asks the chat's own agent for a hand-off (what is done, what is left,
//! the files and branches that matter, open problems), waits for that turn to end, starts a
//! successor chat in the same folder, on the same board, with the same model and effort — or the model and
//! effort asked for, which is how the app offers a roll over when a conversation has outgrown a
//! model the chat wants to move to — sends the successor the hand-off as its first message (with
//! the old chat's title and id so the two can be matched), and archives the old chat, stopping it
//! first if it is somehow working again.
//!
//! The op answers at once with `{started: true}`; the rest runs in the background. When the
//! successor exists the app opens it (`shell_open_chat`). A failure before the successor is
//! started changes nothing: the old chat stays as it was and a `chat_roll_over_failed` event
//! carries a plain message for the UI to show.
//!
//! Refused up front: an archived chat, a chat that is working or has messages waiting, and a chat
//! that has never had an assistant reply.
//!
//! The same flow runs itself when a chat's turn ends and its context has grown past the
//! roll-over setting ([`on_turn_end`]): the chat is idle at that moment, so every check the op
//! makes holds, and the only difference is who asks.

use crate::actor::Cmd;
use crate::runtime::{ChatRuntime, Shared};
use crate::submit::HostSend;
use crate::types::*;
use helm_ipc::{ApiError, AppEvent, OpCtx, OpRegistry};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
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

/// Context size at which a quiet chat is asked to roll over, when the setting is unset
/// (`0` in the setting switches the asking off).
pub const DEFAULT_ROLLOVER_TOKENS: i64 = 180_000;

/// Chats the automatic trigger has already fired for. Once per process run: after a restart a
/// chat still over the threshold is asked again, which is bounded (a successor starts well under
/// it, and a failed attempt leaves the chat working or idle-but-asked) and harmless.
static AUTO_DONE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Chats with a roll over running, whichever way it started.
static IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// The successor's model and effort a roll-over request asked for.
type Target = (Option<String>, Option<String>);

/// Roll overs the chat itself asked for with `app_roll_over`: chat id to the successor's model
/// and effort, and the newest message the chat had when it asked (the hand-off is a reply after
/// it). A chat that calls a tool is in mid-turn, and a roll over refuses a working chat,
/// so the tool can only leave a note; [`on_turn_end`] runs it.
static PENDING: LazyLock<Mutex<HashMap<String, (Target, i64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Note that this chat asked to roll itself over when its turn ends; the model and effort must
/// already be validated.
pub(crate) fn tool_request(
    rt: &ChatRuntime,
    chat_id: &str,
    model: Option<String>,
    effort: Option<String>,
) {
    let mark = last_message_id(rt, chat_id).unwrap_or(0);
    PENDING
        .lock()
        .unwrap()
        .insert(chat_id.to_owned(), ((model, effort), mark));
}

/// The chat's turn was stopped on purpose: a roll over it asked for with `app_roll_over` is
/// dropped, and nothing is started at this turn end.
pub(crate) fn turn_stopped(chat_id: &str) {
    PENDING.lock().unwrap().remove(chat_id);
}

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

/// `chat_roll_over {chatId, model?, effort?}:` the successor runs on the chat's own model and
/// effort unless the caller names a model (a Claude model id or a `local/<server>/<model>` id),
/// which is how a chat moves to a model its conversation would not otherwise fit.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RollOverArgs {
    chat_id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}

/// Validate the successor's model and effort: a named model must at least name a server that
/// is set up (the successor's own creation checks the rest), and effort must be one of the
/// five words. Shared by the op and the agent tool.
pub(crate) fn check_target(
    rt: &ChatRuntime,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<(Option<String>, Option<String>), ApiError> {
    let target_model = match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => {
            let (server, _) = crate::local::columns(Some(m)).map_err(ApiError::bad_args)?;
            if let Some(server) = &server {
                if rt.local_models().is_some_and(|l| !l.knows(server)) {
                    return Err(ApiError::bad_args(
                        "That local model server is not set up. Add it in Settings > Local models.",
                    ));
                }
            }
            Some(m.to_owned())
        }
        None => None,
    };
    let target_effort = match effort.map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) if crate::catalogue::is_effort(e) => Some(e.to_owned()),
        Some(_) => {
            return Err(ApiError::bad_args(
                "Effort must be low, medium, high, xhigh or max.",
            ))
        }
        None => None,
    };
    Ok((target_model, target_effort))
}

async fn chat_roll_over(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: RollOverArgs = crate::ops::parse_args(args)?;
    let rt = ctx.services.need::<ChatRuntime>()?;
    let (target_model, target_effort) = check_target(&rt, a.model.as_deref(), a.effort.as_deref())?;
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
    start(&rt, &ctx, &chat, target_model, target_effort)?;
    Ok(json!({"started": true}))
}

/// Run the roll-over flow for `chat`: ask for a hand-off, then finish in the background. Shared
/// by the op, the automatic trigger and the agent tool. A chat with a roll over already running
/// is refused; `model` and `effort` must already be validated.
fn start(
    rt: &Arc<ChatRuntime>,
    ctx: &OpCtx,
    chat: &ChatRecord,
    model: Option<String>,
    effort: Option<String>,
) -> Result<(), ApiError> {
    if !IN_FLIGHT.lock().unwrap().insert(chat.id.clone()) {
        return Err(refuse("A roll over for this chat is already running."));
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let mark = match last_message_id(rt, &chat.id) {
        Ok(m) => m,
        Err(e) => {
            IN_FLIGHT.lock().unwrap().remove(&chat.id);
            return Err(crate::ops::sql(e));
        }
    };
    if let Err(e) = crate::submit::host_send(
        rt,
        HostSend {
            chat_id: &chat.id,
            submission_id: &request_id,
            text: REQUEST,
            origin: SubmissionOrigin::App,
        },
    ) {
        IN_FLIGHT.lock().unwrap().remove(&chat.id);
        return Err(e);
    }
    let handle = rt.handle().clone();
    let rt = Arc::clone(rt);
    let ctx = ctx.clone();
    let chat = chat.clone();
    handle.spawn(async move {
        if let Err(why) = finish(&ctx, &rt, &chat, mark, &request_id, model, effort).await {
            log::warn!("roll over of {}: {why}", chat.id);
            ctx.events.emit(
                AppEvent::new("chat_roll_over_failed")
                    .with("chatId", chat.id.clone())
                    .with("message", why),
            );
        }
        IN_FLIGHT.lock().unwrap().remove(&chat.id);
    });
    Ok(())
}

/// A turn has just ended and the chat is quiet. Two things may be waiting here: a roll over
/// the chat itself asked for with `app_roll_over` (its last reply's text is the hand-off, so
/// the flow runs without the request turn), or — when the chat's context has grown past the
/// roll-over setting — the automatic ask, the same flow the button runs. The chat is idle at
/// this moment, so every guard the op makes holds. Every check is a cheap read; side chats,
/// archived chats, chats without a context reading, and chats already asked once are alone.
pub(crate) fn on_turn_end(sh: &Arc<Shared>, chat_id: &str) {
    // The flow runs on the app's services; until the host seam is attached there is nothing to
    // run it with.
    let Some(seam) = sh.host.get().cloned() else {
        return;
    };
    if IN_FLIGHT.lock().unwrap().contains(chat_id) {
        return;
    }
    let asked = PENDING.lock().unwrap().remove(chat_id);
    if asked.is_none() {
        let over = sh
            .db
            .snapshot(|tx| {
                let threshold = helm_core::prefs::get_or(
                    tx,
                    helm_core::prefs::ROLLOVER_TOKENS,
                    DEFAULT_ROLLOVER_TOKENS,
                );
                if threshold <= 0 {
                    return Ok::<_, rusqlite::Error>(false);
                }
                let idle_root: bool = tx
                    .query_row(
                        "SELECT archived = 0 AND side_source_id IS NULL AND status = 'idle'
                     FROM chats WHERE id = ?1",
                        [chat_id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(false);
                if !idle_root {
                    return Ok(false);
                }
                Ok(crate::turn_usage::context_reported_at(tx, chat_id)
                    .is_some_and(|(n, _)| n >= threshold as u64))
            })
            .unwrap_or(false);
        if !over || !AUTO_DONE.lock().unwrap().insert(chat_id.to_owned()) {
            return;
        }
    }
    let rt = Arc::new(ChatRuntime { sh: sh.clone() });
    let chat = match rt
        .db()
        .snapshot(|tx| crate::store::get_chat(tx, chat_id, 0))
    {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            log::warn!("roll over check for {chat_id}: {e}");
            return;
        }
    };
    if chat.archived || chat.side_source_id.is_some() {
        return;
    }
    if let Some(((model, effort), mark)) = asked {
        // The hand-off is the chat's own reply, written in the turn that called the tool: one
        // after the tool call, never an older reply from before it.
        let handoff = handoff_after(&rt, chat_id, mark).ok().flatten();
        match handoff {
            Some(h) => start_with_handoff(&rt, &seam.ctx, &chat, &h, model, effort),
            // The agent ended the turn without writing text: fall back to the normal ask.
            None => {
                if let Err(e) = start(&rt, &seam.ctx, &chat, model, effort) {
                    log::warn!("roll over of {chat_id}: {}", e.message);
                }
            }
        }
        return;
    }
    if let Err(e) = start(&rt, &seam.ctx, &chat, None, None) {
        log::warn!("auto roll over of {chat_id}: {}", e.message);
    }
}

/// The roll-over flow when the hand-off is already written: no request turn — the successor
/// starts from the text at once. Used when the chat's own turn ended with `app_roll_over`
/// pending and its last reply is the hand-off.
fn start_with_handoff(
    rt: &Arc<ChatRuntime>,
    ctx: &OpCtx,
    chat: &ChatRecord,
    handoff: &str,
    model: Option<String>,
    effort: Option<String>,
) {
    if !IN_FLIGHT.lock().unwrap().insert(chat.id.clone()) {
        return;
    }
    let handle = rt.handle().clone();
    let rt = Arc::clone(rt);
    let ctx = ctx.clone();
    let chat = chat.clone();
    let handoff = handoff.to_owned();
    handle.spawn(async move {
        if let Err(why) = carry_on(&ctx, &rt, &chat, &handoff, model, effort).await {
            log::warn!("roll over of {}: {why}", chat.id);
            ctx.events.emit(
                AppEvent::new("chat_roll_over_failed")
                    .with("chatId", chat.id.clone())
                    .with("message", why),
            );
        }
        IN_FLIGHT.lock().unwrap().remove(&chat.id);
    });
}

async fn finish(
    ctx: &OpCtx,
    rt: &ChatRuntime,
    chat: &ChatRecord,
    mark: i64,
    request_id: &str,
    model: Option<String>,
    effort: Option<String>,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + HANDOFF_WAIT;
    let handoff = loop {
        if tokio::time::Instant::now() >= deadline {
            withdraw_request(rt, &chat.id, request_id);
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
    carry_on(ctx, rt, chat, &handoff, model, effort).await
}

/// The hand-off request is still waiting in the queue after the wait ran out: take it back, so it
/// does not run later, ahead of the user's next message. A request already on its way is left.
/// The whole waiting start goes back, so a message of the user's that waits behind it returns to
/// their draft with it; nothing is lost.
fn withdraw_request(rt: &ChatRuntime, chat: &str, request_id: &str) {
    let waiting = rt
        .db()
        .snapshot(|tx| {
            tx.query_row(
                "SELECT state = 'saved' FROM submissions WHERE id = ?1",
                [request_id],
                |r| r.get::<_, bool>(0),
            )
            .optional()
        })
        .ok()
        .flatten()
        .unwrap_or(false);
    if waiting {
        // A chat the user parked by hand stays parked: only the request goes back.
        crate::queue::cancel_waiting_keeping_park(rt, chat);
    }
}

/// The hand-off exists: start the successor, hand it the brief, take the old chat's place and
/// archive the old chat.
async fn carry_on(
    ctx: &OpCtx,
    rt: &ChatRuntime,
    chat: &ChatRecord,
    handoff: &str,
    model: Option<String>,
    effort: Option<String>,
) -> Result<(), String> {
    let message = successor_message(&chat.title, &chat.id, &cut(handoff, MAX_HANDOFF_BYTES));
    let mut create = json!({"cwd": chat.cwd, "projectId": chat.project_id});
    // The successor stays on the old chat's board while that board still takes chats; otherwise
    // the host picks its home as for any new chat.
    let boards = crate::store::Boards::of(&ctx.services);
    let board = rt
        .db()
        .snapshot(|tx| {
            Ok::<_, rusqlite::Error>(match boards.chat_board(tx, &chat.id)? {
                Some(b) => boards
                    .board(tx, &b)?
                    .filter(|s| !s.archived && !s.stale)
                    .map(|s| s.id),
                None => None,
            })
        })
        .map_err(|e| e.to_string())?;
    if let Some(b) = board {
        create["boardId"] = json!(b);
    }
    // The asked-for model and effort win; otherwise the successor keeps the old chat's.
    if let Some(m) = model.as_ref().or(chat.model.as_ref()) {
        create["model"] = json!(m);
    }
    if let Some(e) = effort.as_ref().or(chat.effort.as_ref()) {
        create["effort"] = json!(e);
    }
    let ctx2 = ctx.clone();
    let made = tokio::task::spawn_blocking(move || crate::ops::chat_create(&ctx2, create))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.message)?;
    let new_id = made["id"].as_str().unwrap_or_default().to_owned();
    // The successor belongs to the old chat's person before anything runs in it: its first turn
    // reads that person's rules, memories and settings when its worker launches.
    if let Err(e) = rt.db().write(|tx| carry_person(tx, &chat.id, &new_id)) {
        log::warn!("roll over of {}: the successor kept no owner: {e}", chat.id);
    }
    if let Some(j) = rt.sh.stop_judge.get() {
        j.agent_mode_carried(&chat.id, &new_id);
    }
    // The successor carries on in the mode the old chat was in.
    if let Some(m) = crate::submit::picked_mode(&chat.id) {
        crate::submit::seed_mode(&new_id, &m);
    }
    // Muted (and made by the same chat) before its brief, so nothing it does is announced
    // when the old chat was not.
    if let Err(e) = rt
        .db()
        .write(|tx| crate::mute::inherit(tx, &chat.id, &new_id, helm_core::now_ms()))
    {
        log::warn!(
            "roll over of {}: the successor's mute was not set: {e}",
            chat.id
        );
    }
    // Told before its first message goes out, so the project's startup carries on from this chat's.
    if let Err(e) = rt.db().write(|tx| {
        tx.execute(
            "INSERT OR REPLACE INTO chat_succession(chat_id, predecessor_id) VALUES (?1, ?2)",
            [&new_id, &chat.id],
        )
    }) {
        log::warn!(
            "roll over of {}: the successor's predecessor was not recorded: {e}",
            chat.id
        );
    }
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
    // The successor stands where the old chat stood, and keeps what it was (muted or not, and
    // who made it): its place in the sidebar, and the chats started under the old one.
    match rt
        .db()
        .write(|tx| take_place(tx, &chat.id, &new_id, helm_core::now_ms()))
    {
        Ok(changed) => ctx
            .events
            .emit(AppEvent::new("sidebar").with("chatIds", json!(changed))),
        Err(e) => log::warn!("roll over of {}: the successor kept no place: {e}", chat.id),
    }
    // Stop first: the old chat should be idle, but never archive over a working one.
    if !settled(rt, &chat.id) {
        let _ = rt
            .ask(
                &chat.id,
                |reply| Cmd::StopNoResend { reply },
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

/// Make `new` belong to the person `old` belongs to, and show it to the same profiles.
fn carry_person(tx: &rusqlite::Transaction<'_>, old: &str, new: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE chats SET profile_id = (SELECT profile_id FROM chats WHERE id = ?1) WHERE id = ?2",
        [old, new],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO chat_shares(chat_id, profile_id)
         SELECT ?2, profile_id FROM chat_shares WHERE chat_id = ?1",
        [old, new],
    )?;
    Ok(())
}

/// Give `new` the old chat's parent, position and pin, move the old chat's children under it, and
/// copy its mute and creator. Returns the chats whose sidebar rows changed.
fn take_place(
    tx: &rusqlite::Transaction<'_>,
    old: &str,
    new: &str,
    now: i64,
) -> rusqlite::Result<Vec<String>> {
    tx.execute(
        "UPDATE chats SET (parent_chat_id, position, pinned) =
             (SELECT parent_chat_id, position, pinned FROM chats WHERE id = ?1)
         WHERE id = ?2",
        [old, new],
    )?;
    carry_person(tx, old, new)?;
    let children: Vec<String> = {
        let mut st = tx.prepare("SELECT id FROM chats WHERE parent_chat_id = ?1 AND id <> ?2")?;
        let rows = st.query_map([old, new], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    tx.execute(
        "UPDATE chats SET parent_chat_id = ?2 WHERE parent_chat_id = ?1 AND id <> ?2",
        [old, new],
    )?;
    crate::mute::inherit(tx, old, new, now)?;
    let mut changed = vec![new.to_owned(), old.to_owned()];
    changed.extend(children);
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hand_off_request_taken_back_leaves_the_users_park_in_place() {
        use crate::submit::{host_send, HostSend};
        let h = crate::runtime_tests::harness();
        let chat = h.create().await;
        crate::queue::manual_hold(&h.db, &chat, "normal", helm_core::now_ms());
        host_send(
            &h.rt,
            HostSend {
                chat_id: &chat,
                submission_id: "ro-1",
                text: "write the hand-off",
                origin: SubmissionOrigin::Schedule,
            },
        )
        .unwrap();
        h.until("the request waits behind the park", |h| {
            h.state("ro-1") == "saved"
        })
        .await;
        withdraw_request(&h.rt, &chat, "ro-1");
        h.until("the request is given back", |h| h.state("ro-1") != "saved")
            .await;
        assert!(
            crate::queue::is_manual(&h.db, &chat),
            "the user's park is still in place"
        );
    }

    #[test]
    fn the_successor_message_names_the_old_chat_and_the_cut_respects_characters() {
        let m = successor_message("Fix login", "abc", "Goal: x");
        assert!(m.contains("\"Fix login\" (chat id abc)") && m.ends_with("Goal: x"));
        let s = "é".repeat(10);
        let c = cut(&s, 5);
        assert!(c.starts_with("éé") && c.contains("cut here"));
        assert_eq!(cut("short", 100), "short");
    }

    #[test]
    fn the_successor_takes_the_old_chats_place_children_mute_and_creator() {
        let (_d, db) = crate::schema::tests::temp_db();
        let mk = || {
            db.write(|tx| crate::store::create_chat(tx, "C:/w/proj", None, None, 1))
                .unwrap()
        };
        let (parent, old, new, kid, maker) = (mk(), mk(), mk(), mk(), mk());
        db.write(|tx| {
            tx.execute(
                "UPDATE chats SET parent_chat_id = ?1, position = 3, pinned = 1 WHERE id = ?2",
                [&parent, &old],
            )?;
            tx.execute(
                "UPDATE chats SET parent_chat_id = ?1 WHERE id = ?2",
                [&old, &kid],
            )?;
            tx.execute(
                "UPDATE chats SET profile_id = 'p-sam' WHERE id = ?1",
                [&old],
            )?;
            tx.execute(
                "INSERT INTO chat_shares(chat_id, profile_id) VALUES (?1, 'p-ann')",
                [&old],
            )?;
            crate::mute::set_muted(tx, &old, true, 5)?;
            crate::mute::set_created_by(tx, &old, &maker)
        })
        .unwrap();
        let changed = db.write(|tx| take_place(tx, &old, &new, 9)).unwrap();
        assert_eq!(changed, [new.clone(), old.clone(), kid.clone()]);
        let row = |id: &str| -> (Option<String>, Option<i64>, bool, bool, Option<String>) {
            db.snapshot(|tx| {
                tx.query_row(
                    "SELECT parent_chat_id, position, pinned, EXISTS(SELECT 1 FROM chat_mutes WHERE chat_id = chats.id), created_by_chat
                     FROM chats WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
            })
            .unwrap()
        };
        assert_eq!(row(&new), (Some(parent), Some(3), true, true, Some(maker)));
        let owner_and_shares: (String, String) = db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT profile_id, (SELECT group_concat(profile_id) FROM chat_shares
                                         WHERE chat_id = ?1) FROM chats WHERE id = ?1",
                    [&new],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!(owner_and_shares, ("p-sam".to_owned(), "p-ann".to_owned()));
        assert_eq!(
            row(&kid).0,
            Some(new),
            "the old chat's children move under the successor"
        );
    }
}
