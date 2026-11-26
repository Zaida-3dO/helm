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
//! The roll over is meant to be seamless, so the old chat's composer goes with the work: its
//! unsent draft, the pictures and references staged beside it, and its open-item state move to
//! the successor ([`carry_composer`]).
//!
//! The op answers at once with `{started: true}`; the rest runs in the background. When the
//! successor exists the app sends `shell_open_chat`, naming the old chat under `ifViewing`: the
//! window follows to the successor only while the user is still looking at the old chat;
//! otherwise the user gets a notice with an Open button. A failure before the successor is
//! started changes nothing: the old chat stays as it was and a `chat_roll_over_failed` event
//! carries a plain message for the UI to show.
//!
//! Refused up front: an archived chat, a chat that is working or has messages waiting, and a chat
//! that has never had an assistant reply. The cascade therefore starts only from an idle parent:
//! an idle chat with working children still rolls over, because the children are what the
//! cascade is for.
//!
//! A roll over of a chat with children cascades one level first: every child that is working or
//! has messages waiting is asked to stop at the next reasonable point and write its **own**
//! roll-over document — a Markdown file at `<profile>/handoffs/<stack>/<child id>.md` that the
//! child writes itself with its file tools (its task, state, branch and worktree, what it was
//! doing, next steps). The parent waits for the children to go quiet, bounded by
//! [`CASCADE_WAIT`]; then it is asked for its own hand-off, with a "Running subagents" section
//! taught in. Whatever the children did, the HOST appends a machine-written roster after the
//! hand-off — each cascaded child, its state at roll-over, and whether its document is really
//! there — so the successor is told, in the successor's own first message, to start one new
//! subagent per document. After the cap the roll over carries on regardless: a straggler that
//! ignored the notice is named honestly in the roster, never a reason to lose the roll over.
//!
//! The same flow runs itself when a chat's turn ends and its context has grown past the
//! roll-over setting ([`on_turn_end`]): the chat is idle at that moment, so every check the op
//! makes holds, and the only difference is who asks.

use crate::actor::Cmd;
use crate::clock::Clock;
use crate::runtime::{ChatRuntime, Shared};
use crate::submit::HostSend;
use crate::types::*;
use helm_ipc::{ApiError, AppEvent, OpCtx, OpRegistry};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// How long the agent gets to write the hand-off.
pub const HANDOFF_WAIT: Duration = Duration::from_secs(300);
/// How long a cascading roll over waits for its children to go quiet before it carries on
/// without them. Ten minutes covers a child finishing a step and writing its document; past
/// that the roster marks the straggler and the roll over proceeds.
pub const CASCADE_WAIT: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_millis(250);
/// The hand-off is cut here, on a character boundary, before it is sent on.
const MAX_HANDOFF_BYTES: usize = 60 * 1024;
/// A child's roll-over document counts as written when it is at least this many bytes: long
/// enough that a stray word or an empty file does not count.
const MIN_DOC_BYTES: u64 = 80;

pub fn register(reg: &mut OpRegistry) {
    reg.async_op("chat_roll_over", chat_roll_over);
}

/// The core of what the old chat's agent is asked. Plain text, because it appears in that
/// chat's transcript.
const REQUEST_CORE: &str = "This chat is being rolled over into a fresh one because its context has grown large. Reply with a hand-off for the agent that takes over, and nothing else: no tool calls except writing packet files in the folder named below. Use these headings: Goal, Done, Left to do, Key files and branches, Open problems and decisions. Be specific (paths, branch names, commands, ids) and keep it under 600 words. The new agent starts with no memory of this chat.";

/// The hand-off request as sent. The packet teaching is in every ask: detail that does not fit
/// 600 words goes into a packet file beside the hand-off documents, linked by path — critical
/// context only, because the successor can search the old sessions itself. `with_subagents`
/// adds the "Running subagents" section a cascading roll over needs.
fn handoff_request(dir: &Path, with_subagents: bool) -> String {
    let mut s = format!(
        "{REQUEST_CORE} Anything too detailed for 600 words goes into a packet file inside {} (any clear name; make the folder first if it is missing), linked by its full path from the hand-off. A packet holds only critical context — the successor can search this chat's older sessions, so no transcripts.",
        plain_path(dir)
    );
    if with_subagents {
        s.push_str(" This chat has subagents being rolled over with it: end with a heading \"Running subagents\", one line per child — what it was doing, the path of its roll-over document if you know it, and that the successor's first job is to start one new subagent per document, each picking up where that child stopped.");
    }
    s
}

/// What a working or waiting child is asked first: stop soon and write its own roll-over
/// document where the successor will look for it.
fn cascade_request(doc: &Path) -> String {
    format!(
        "Your parent chat is being rolled over into a fresh one, and the roll over cascades to you. Stop at the next reasonable point: finish what is already in flight, start no new work. Then write your own roll-over document to this exact path (make the folders if needed — your file tools can):\n{}\nCover: your task, its current state, the branch and worktree you work in, what you were doing when this arrived, and your next steps. Write the file, then answer with one short line saying you wrote it (or why you could not), and end your turn.",
        plain_path(doc)
    )
}

/// The folder one stack's hand-off documents and packets live in: `<profile>/handoffs/<stack>`.
fn handoff_dir(rt: &ChatRuntime, stack: &str) -> PathBuf {
    rt.sh.cfg.profile_dir.join("handoffs").join(stack)
}

/// A path as an agent should read it: Windows' verbatim (`\\?\`) prefix would only confuse it.
fn plain_path(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    match s.strip_prefix(r"\\?\") {
        Some(s) => s.to_owned(),
        None => s,
    }
}

/// A child chat the roll over cascades to, and where its roll-over document must go.
struct CascadeChild {
    id: String,
    title: String,
    doc: PathBuf,
}

/// The direct children of `chat` that are working or have messages waiting: each gets the
/// cascade notice and writes its own roll-over document. Archived chats, side chats and
/// children with a roll over of their own running are left alone; idle children need no
/// document — they carry on under the successor untouched.
fn cascade_targets(rt: &ChatRuntime, chat: &ChatRecord) -> Vec<CascadeChild> {
    let stack = chat.stack_id.clone().unwrap_or_else(|| chat.id.clone());
    let children = rt
        .db()
        .snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT id, title FROM chats
                 WHERE parent_chat_id = ?1 AND archived = 0 AND side_source_id IS NULL
                 ORDER BY created_at, id",
            )?;
            let rows = st.query_map([&chat.id], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<(String, String)>>>()
        })
        .unwrap_or_default();
    children
        .into_iter()
        .filter(|(id, _)| !settled(rt, id) && !IN_FLIGHT.lock().unwrap().contains(id))
        .map(|(id, title)| CascadeChild {
            doc: handoff_dir(rt, &stack).join(format!("{id}.md")),
            id,
            title,
        })
        .collect()
}

/// Tell each cascaded child to stop and write its document. A child whose notice cannot be
/// queued still lands in the roster, marked as having written nothing.
fn send_cascades(rt: &ChatRuntime, parent: &str, children: &[CascadeChild]) {
    for c in children {
        if let Err(e) = crate::submit::host_send(
            rt,
            HostSend {
                chat_id: &c.id,
                submission_id: &uuid::Uuid::new_v4().to_string(),
                text: &cascade_request(&c.doc),
                origin: SubmissionOrigin::App,
                page: None,
                // An instruction into a chat that is working: a steer.
                kind: Some(MessageKind::Steer),
                wait: None,
            },
        ) {
            log::warn!(
                "roll over of {parent}: child {} got no cascade notice: {}",
                c.id,
                e.message
            );
        }
    }
}

/// Wait for every cascaded child to go quiet, up to `cap`. The cascade notice itself keeps a
/// child unsettled until it has answered, so "settled" here means the child stopped, wrote
/// its document (or said why it could not) and ended its turn. Past the cap the roll over
/// carries on whatever the children are doing: losing the roll over is worse than leaving a
/// straggler, and the roster marks it.
async fn wait_for_children(rt: &ChatRuntime, children: &[CascadeChild], cap: Duration) {
    let deadline = tokio::time::Instant::now() + cap;
    while tokio::time::Instant::now() < deadline {
        if children.iter().all(|c| settled(rt, &c.id)) {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The roster the host appends after the parent's hand-off: what became of each cascaded
/// child, honestly — state at roll-over, and whether the document is really there (the file
/// exists and is not a stub) — with the successor's first job spelled out.
fn roster_for(rt: &ChatRuntime, children: &[CascadeChild]) -> String {
    let mut out = String::from(
        "## Running subagents (written by Helm)\n\
         The rolled-over chat had these subagents running. Their roll-over documents:\n",
    );
    for c in children {
        let state = if settled(rt, &c.id) {
            "idle"
        } else {
            "still working"
        };
        let doc = match std::fs::metadata(&c.doc) {
            Ok(m) if m.len() >= MIN_DOC_BYTES => {
                format!("wrote a roll-over document at {}", plain_path(&c.doc))
            }
            _ => "no roll-over document was written".to_owned(),
        };
        out.push_str(&format!(
            "- {} (chat id {}): {state}, {doc}.\n",
            c.title, c.id
        ));
    }
    out.push_str(
        "First job of the successor: start one new subagent per document above, each picking up \
         where that child stopped. For any child with no document, check what that chat is \
         doing now before starting anything new.",
    );
    out
}

/// Context size at which a quiet chat is asked to roll over, when the setting is unset
/// (`0` in the setting switches the asking off).
pub const DEFAULT_ROLLOVER_TOKENS: i64 = 300_000;

/// Chats the automatic trigger has already fired for. Once per process run: after a restart a
/// chat still over the threshold is asked again, which is bounded (a successor starts well under
/// it, and a failed attempt leaves the chat working or idle-but-asked) and harmless.
static AUTO_DONE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Chats with a roll over running, whichever way it started.
static IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// The IN_FLIGHT slot a roll over holds through the synchronous part of [`start`]. Dropping
/// the guard releases the chat again — an error on the way to the background flow (a failed
/// hand-off send, for one) must not leave the chat reading "already running" to every later
/// roll over until the app restarts. [`InFlight::disarm`] hands the slot to the spawned flow,
/// which removes it when the roll over ends.
struct InFlight {
    chat_id: String,
    handed_off: bool,
}

impl InFlight {
    /// Claim the chat's slot; `None` when a roll over already holds it.
    fn take(chat_id: &str) -> Option<Self> {
        IN_FLIGHT
            .lock()
            .unwrap()
            .insert(chat_id.to_owned())
            .then(|| Self {
                chat_id: chat_id.to_owned(),
                handed_off: false,
            })
    }

    /// The background flow owns the slot from here; it clears it itself when the flow ends.
    fn disarm(&mut self) {
        self.handed_off = true;
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if !self.handed_off {
            IN_FLIGHT.lock().unwrap().remove(&self.chat_id);
        }
    }
}
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

/// The first message of the successor chat. The old chat's title is a `helm://chats/<id>` link
/// so the successor's first message reaches back to the chat it came from; the id is kept in the
/// text as well for agents that read rather than click.
pub fn successor_message(old_title: &str, old_id: &str, handoff: &str) -> String {
    format!(
        "This chat continues [{}](helm://chats/{old_id}) (chat id {old_id}), which was rolled over because its context had grown large. Below is the hand-off its agent wrote. Check the actual state of the repository and any running work yourself, then carry on from \"Left to do\".\n\n---\n\n{handoff}",
        escape_link_text(old_title)
    )
}

/// Make a title safe inside Markdown link text: backslashes, square brackets and backticks get a
/// backslash so a title cannot break the link or open a code span, and newlines collapse to
/// spaces (the message stays one line where the link sits).
fn escape_link_text(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.chars() {
        match c {
            '\\' | '[' | ']' | '`' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
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

/// Run the roll-over flow for `chat`: cascade to any working children, then ask for a hand-off,
/// then finish in the background. Shared by the op, the automatic trigger and the agent tool.
/// A chat with a roll over already running is refused; `model` and `effort` must already be
/// validated. What the old chat or its children are told happens here, before the background
/// part starts, so `started: true` means the notices are in. With no working children that
/// includes the hand-off ask itself; with children it waits on them, and a failed ask travels
/// as a `chat_roll_over_failed` event.
fn start(
    rt: &Arc<ChatRuntime>,
    ctx: &OpCtx,
    chat: &ChatRecord,
    model: Option<String>,
    effort: Option<String>,
) -> Result<(), ApiError> {
    let Some(mut in_flight) = InFlight::take(&chat.id) else {
        return Err(refuse("A roll over for this chat is already running."));
    };
    let children = cascade_targets(rt, chat);
    send_cascades(rt, &chat.id, &children);
    // A chat with no working children behaves exactly as before: the ask goes out right here,
    // so `started: true` means it has been asked, and a failed send answers as an op error.
    // With children the ask waits in [`roll_forward`] until they have gone quiet.
    let ask = if children.is_empty() {
        let stack = chat.stack_id.clone().unwrap_or_else(|| chat.id.clone());
        let request_id = uuid::Uuid::new_v4().to_string();
        let mark = last_message_id(rt, &chat.id).map_err(crate::ops::sql)?;
        crate::submit::host_send(
            rt,
            HostSend {
                chat_id: &chat.id,
                submission_id: &request_id,
                text: &handoff_request(&handoff_dir(rt, &stack), false),
                origin: SubmissionOrigin::App,
                page: None,
                kind: None,
                wait: None,
            },
        )?;
        Some((mark, request_id))
    } else {
        None
    };
    let handle = rt.handle().clone();
    let rt = Arc::clone(rt);
    let ctx = ctx.clone();
    let chat = chat.clone();
    handle.spawn(async move {
        if let Err(why) = roll_forward(
            &ctx,
            &rt,
            &chat,
            children,
            (model, effort),
            CASCADE_WAIT,
            ask,
        )
        .await
        {
            log::warn!("roll over of {}: {why}", chat.id);
            ctx.events.emit(
                AppEvent::new("chat_roll_over_failed")
                    .with("chatId", chat.id.clone())
                    .with("message", why),
            );
        }
        IN_FLIGHT.lock().unwrap().remove(&chat.id);
    });
    in_flight.disarm();
    Ok(())
}

/// The roll-over flow past the guards: wait (bounded) for the cascaded children, ask the parent
/// for its hand-off, then carry on with the roster appended. `cap` is how long the children get
/// to go quiet; every real roll over passes [`CASCADE_WAIT`]. `ask` is the request [`start`]
/// already sent for a chat with no children (its mark and submission id); with children the ask
/// goes out here, after the wait. With no children this is exactly the old flow.
async fn roll_forward(
    ctx: &OpCtx,
    rt: &ChatRuntime,
    chat: &ChatRecord,
    children: Vec<CascadeChild>,
    target: Target,
    cap: Duration,
    ask: Option<(i64, String)>,
) -> Result<(), String> {
    if ask.is_none() {
        wait_for_children(rt, &children, cap).await;
    }
    let outcome = carry_past_handoff(rt, ctx, chat, &children, target, ask).await;
    // Everything past the wait happens once the children have already been told to stop and
    // write their documents: any failure there — the ask itself, the hand-off, the carry-on
    // — only says "Nothing was changed" of this chat, so it names the ones left holding.
    match outcome {
        Err(why) if !children.is_empty() => Err(failed_after_cascade(&why, &children)),
        other => other,
    }
}

/// The roll-over flow after the children settled: send the hand-off ask (unless [`start`]
/// already sent it for a childless chat), wait for the answer, then carry on with the roster
/// appended.
async fn carry_past_handoff(
    rt: &ChatRuntime,
    ctx: &OpCtx,
    chat: &ChatRecord,
    children: &[CascadeChild],
    target: Target,
    ask: Option<(i64, String)>,
) -> Result<(), String> {
    let (model, effort) = target;
    let (mark, request_id) = match ask {
        Some(mr) => mr,
        None => {
            let stack = chat.stack_id.clone().unwrap_or_else(|| chat.id.clone());
            let request_id = uuid::Uuid::new_v4().to_string();
            // The mark is taken when the request goes out, not when the roll over started: a
            // child's note can reach this chat while the children settle, and its reply must
            // not be read as the hand-off.
            let mark = last_message_id(rt, &chat.id).map_err(|e| e.to_string())?;
            crate::submit::host_send(
                rt,
                HostSend {
                    chat_id: &chat.id,
                    submission_id: &request_id,
                    text: &handoff_request(&handoff_dir(rt, &stack), !children.is_empty()),
                    origin: SubmissionOrigin::App,
                    page: None,
                    kind: None,
                    wait: None,
                },
            )
            .map_err(|e| e.message)?;
            (mark, request_id)
        }
    };
    let handoff = wait_for_handoff(rt, chat, mark, &request_id).await?;
    // The roster is read after the hand-off wait, not before it: a child that writes its
    // document while the parent is still answering counts as written.
    let roster = (!children.is_empty()).then(|| roster_for(rt, children));
    carry_on(ctx, rt, chat, &handoff, roster.as_deref(), model, effort).await
}

/// The failure of a cascading roll over whose hand-off never arrived: this chat kept its
/// state, but the children had already been stopped for it — the message names them.
fn failed_after_cascade(why: &str, children: &[CascadeChild]) -> String {
    let base = why.strip_suffix(" Nothing was changed.").unwrap_or(why);
    let names = children
        .iter()
        .map(|c| c.title.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{base} Nothing was changed to this chat, but its subagents were already told to stop \
         and keep what they wrote: {names}."
    )
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
/// pending and its last reply is the hand-off. The cascade still runs: the children are told
/// to write their own documents and the roster is appended after the parent's text, because
/// the parent wrote its hand-off before any of them existed.
fn start_with_handoff(
    rt: &Arc<ChatRuntime>,
    ctx: &OpCtx,
    chat: &ChatRecord,
    handoff: &str,
    model: Option<String>,
    effort: Option<String>,
) {
    let Some(mut in_flight) = InFlight::take(&chat.id) else {
        return;
    };
    let children = cascade_targets(rt, chat);
    send_cascades(rt, &chat.id, &children);
    let handle = rt.handle().clone();
    let rt = Arc::clone(rt);
    let ctx = ctx.clone();
    let chat = chat.clone();
    let handoff = handoff.to_owned();
    handle.spawn(async move {
        wait_for_children(&rt, &children, CASCADE_WAIT).await;
        let roster = (!children.is_empty()).then(|| roster_for(&rt, &children));
        if let Err(why) =
            carry_on(&ctx, &rt, &chat, &handoff, roster.as_deref(), model, effort).await
        {
            log::warn!("roll over of {}: {why}", chat.id);
            ctx.events.emit(
                AppEvent::new("chat_roll_over_failed")
                    .with("chatId", chat.id.clone())
                    .with("message", why),
            );
        }
        IN_FLIGHT.lock().unwrap().remove(&chat.id);
    });
    in_flight.disarm();
}

/// Wait for the chat's reply to the hand-off request. The newest assistant reply after `mark`
/// is the hand-off.
async fn wait_for_handoff(
    rt: &ChatRuntime,
    chat: &ChatRecord,
    mark: i64,
    request_id: &str,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + HANDOFF_WAIT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            withdraw_request(rt, &chat.id, request_id);
            return Err("The agent did not write a hand-off in time. Nothing was changed.".into());
        }
        tokio::time::sleep(POLL).await;
        if !settled(rt, &chat.id) {
            continue;
        }
        return handoff_after(rt, &chat.id, mark)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                "The agent ended its turn without a hand-off. Nothing was changed.".to_owned()
            });
    }
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
/// archive the old chat. `roster` is the machine-written list of the cascaded children; it rides
/// after the byte-cut so a long hand-off can never swallow it.
async fn carry_on(
    ctx: &OpCtx,
    rt: &ChatRuntime,
    chat: &ChatRecord,
    handoff: &str,
    roster: Option<&str>,
    model: Option<String>,
    effort: Option<String>,
) -> Result<(), String> {
    let body = match roster {
        Some(r) => format!("{}\n\n{r}", cut(handoff, MAX_HANDOFF_BYTES)),
        None => cut(handoff, MAX_HANDOFF_BYTES),
    };
    let message = successor_message(&chat.title, &chat.id, &body);
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
    // The successor keeps the old chat's title as it is: the work keeps its name across a roll
    // over, and the hand-off message inside the new chat says which chat it came from.
    let successor_title = chat.title.clone();
    let _ = crate::titles::rename(rt, &new_id, &successor_title);
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
            page_title: None,
            page_overview: None,
            // The successor's brief is Helm's own continuation word: no chat-to-chat kind.
            kind: None,
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
    // The successor's record changed under any window already holding it (it now carries the
    // unsent draft and the open-item state): say so before the window moves over.
    if let Ok(clock) = ctx.services.need::<Clock>() {
        let rev = clock.next();
        if let Ok(Some(rec)) = rt
            .db()
            .snapshot(|tx| crate::store::get_chat(tx, &new_id, rev))
        {
            ctx.events.emit(
                ChatEvent::Chat {
                    chat: Box::new(rec),
                }
                .to_app_event(),
            );
        }
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
    // The stack links on both rows, the succession row and the old chat's archive are one
    // transaction: a finished roll over has its whole end state — both sessions linked, the old
    // chat archived — or none of it. The succession row went in before the successor's first
    // message (the project's startup carry-on reads it while that message is on its way); it is
    // written here too so this transaction stands on its own.
    let now = helm_core::now_ms();
    let stack = chat.stack_id.clone().unwrap_or_else(|| chat.id.clone());
    let archived = rt
        .db()
        .write(|tx| link_and_archive(tx, &chat.id, &new_id, &stack, now))
        .map_err(|e| e.to_string())?
        .map_err(|r| {
            format!(
                "The new chat is started but the old one was not archived: {}",
                r.message
            )
        })?;
    if archived {
        // What the `chat_archive` op does after its write: retire or stop the actor, let the UI
        // hear the row change, and republish attention. The roll over has happened by now, so a
        // missing service stops the notice, not the flow.
        rt.archived(&chat.id);
        if let Ok(clock) = ctx.services.need::<Clock>() {
            let rev = clock.next();
            if let Ok(Some(rec)) = rt
                .db()
                .snapshot(|tx| crate::store::get_chat(tx, &chat.id, rev))
            {
                ctx.events.emit(
                    ChatEvent::Chat {
                        chat: Box::new(rec),
                    }
                    .to_app_event(),
                );
            }
        }
        rt.publish_attention();
    }
    // ifViewing is the old chat: the window follows only while the user is still looking at it,
    // otherwise the UI shows the title as a notice with an Open button.
    ctx.events.emit(
        AppEvent::new("shell_open_chat")
            .with("chatId", new_id)
            .with("ifViewing", chat.id.clone())
            .with("title", successor_title),
    );
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
    carry_composer(tx, old, new, now)?;
    let mut changed = vec![new.to_owned(), old.to_owned()];
    changed.extend(children);
    Ok(changed)
}

/// What the user had half-done in the old chat's composer goes with the work, so a roll over is
/// seamless: the unsent draft, the pictures and file or chat references staged beside it, and
/// the open-item state (which transcript cards are opened, whether the sidebar row is expanded).
/// They move rather than copy: the old session is archived and the successor is where the chat
/// lives now, so one composer holds the draft and a restored old chat cannot send it a second
/// time. Staged files are stored under the attachment's own id, so moving its row is enough.
fn carry_composer(
    tx: &rusqlite::Transaction<'_>,
    old: &str,
    new: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let (draft, revision, state): (Option<String>, i64, Option<String>) = tx.query_row(
        "SELECT draft, draft_revision, sidebar_state FROM chats WHERE id = ?1",
        [old],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if let Some(text) = draft.filter(|d| !d.is_empty()) {
        // A revision newer than either chat's, the way the composer numbers its own saves, so
        // the successor's composer takes the text up and the old one lets it go.
        let next = now.max(revision + 1);
        tx.execute(
            "UPDATE chats SET draft = ?1, draft_revision = MAX(?2, draft_revision + 1) WHERE id = ?3",
            rusqlite::params![text, next, new],
        )?;
        tx.execute(
            "UPDATE chats SET draft = '', draft_revision = ?1 WHERE id = ?2",
            rusqlite::params![next, old],
        )?;
    }
    if state.is_some() {
        tx.execute(
            "UPDATE chats SET sidebar_state = ?1 WHERE id = ?2",
            rusqlite::params![state, new],
        )?;
    }
    for table in ["chat_attachments", "chat_attachment_refs"] {
        tx.execute(
            &format!(
                "UPDATE {table} SET chat_id = ?1 WHERE chat_id = ?2 AND submission_id IS NULL"
            ),
            [new, old],
        )?;
    }
    Ok(())
}

/// The end of a roll over in one transaction: the stack links on both rows (the old session
/// points at the successor, the successor back at it, both carry the stack id), the recorded
/// succession its other readers use, and the old chat's archive. `Ok(true)` when this is what
/// archived it.
fn link_and_archive(
    tx: &rusqlite::Transaction<'_>,
    old: &str,
    new: &str,
    stack: &str,
    now: i64,
) -> rusqlite::Result<Result<bool, crate::archive::Refusal>> {
    tx.execute(
        "UPDATE chats SET stack_id = ?3, next_session_id = ?2 WHERE id = ?1",
        [old, new, stack],
    )?;
    tx.execute(
        "UPDATE chats SET stack_id = ?3, prev_session_id = ?1 WHERE id = ?2",
        [old, new, stack],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO chat_succession(chat_id, predecessor_id) VALUES (?1, ?2)",
        [new, old],
    )?;
    crate::archive::archive_in_tx(tx, old, now)
}

/// The roll-over stack of a chat: every session sharing its `stack_id`, oldest first, and the
/// newest of them — where the chat carries on. A chat no roll over has touched answers with a
/// stack of one, itself; a chat that does not exist answers `None`.
pub(crate) fn stack_of(
    tx: &rusqlite::Transaction<'_>,
    chat_id: &str,
) -> rusqlite::Result<Option<ChatStack>> {
    // The outer Option is the row; the inner one is the chat's stack id, null until some roll
    // over touches it.
    let found: Option<Option<String>> = tx
        .query_row("SELECT stack_id FROM chats WHERE id = ?1", [chat_id], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(stack) = found else {
        return Ok(None);
    };
    let mut st = tx.prepare(match &stack {
        Some(_) => {
            "SELECT id, title, archived, created_at, next_session_id FROM chats
                 WHERE stack_id = ?1 ORDER BY created_at, id"
        }
        None => {
            "SELECT id, title, archived, created_at, next_session_id FROM chats
                 WHERE id = ?1"
        }
    })?;
    let rows = match &stack {
        Some(s) => st.query_map([s.as_str()], map_session),
        None => st.query_map([chat_id], map_session),
    }?;
    let mut sessions: Vec<StackSession> = Vec::new();
    let mut latest = chat_id.to_owned();
    for row in rows {
        let (session, next) = row?;
        if next.is_none() {
            latest = session.id.clone();
        }
        sessions.push(session);
    }
    Ok(Some(ChatStack {
        stack_id: stack,
        latest_id: latest,
        sessions,
    }))
}

fn map_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<(StackSession, Option<String>)> {
    Ok((
        StackSession {
            id: r.get(0)?,
            title: r.get(1)?,
            archived: r.get(2)?,
            created_at: r.get(3)?,
        },
        r.get(4)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_roll_over_cascades_to_a_working_child_and_hands_both_over() {
        use crate::runtime_tests::{ack, echo, harness, reply};
        let mut h = harness();
        let (parent, child) = (h.create().await, h.create().await);
        h.db.write(|tx| {
            tx.execute(
                "UPDATE chats SET parent_chat_id = ?1, title = 'Fix the parser' WHERE id = ?2",
                [&parent, &child],
            )
        })
        .unwrap();
        // The parent answers once, so it has a reply to hand over, and its worker stays idle.
        let (psid, _) = h.send(&parent, "build the thing", "default").await;
        let mut pw = h.worker().await;
        pw.ready().await;
        let p0 = pw.expect("prompt").await;
        ack(&pw, &p0).await;
        pw.emit(json!({"type": "agent_start"})).await;
        echo(&pw, &psid).await;
        reply(&h, &pw, "pmsg_1", "Started.").await;
        pw.emit(json!({"type": "agent_settled", "outcome": "complete"}))
            .await;
        h.until("parent idle", |h| h.status(&parent) == "idle")
            .await;
        // The child is mid-turn when the roll over is asked: a cascade target.
        let (csid, _) = h.send(&child, "fix the grammar", "default").await;
        let mut cw = h.worker().await;
        cw.ready().await;
        let c0 = cw.expect("prompt").await;
        ack(&cw, &c0).await;
        cw.emit(json!({"type": "agent_start"})).await;
        echo(&cw, &csid).await;
        let started = h
            .call("chat_roll_over", json!({"chatId": parent}))
            .await
            .unwrap();
        assert_eq!(started["started"], true);
        // The notice reaches the working child at once, mid-turn, as a steer.
        let notice = cw.expect("prompt").await;
        assert_eq!(notice["streamingBehavior"], "steer");
        let notice_sid = notice["submissionId"].as_str().unwrap().to_owned();
        assert_eq!(
            h.col(&format!(
                "SELECT kind FROM submission_meta WHERE submission_id = '{notice_sid}'"
            )),
            ["steer"],
            "the notice carries its kind as well"
        );
        let doc = handoff_dir(&h.rt, &parent).join(format!("{child}.md"));
        let notice_text = notice["text"].as_str().unwrap().to_owned();
        assert!(
            notice_text.contains("roll-over document") && notice_text.contains(&plain_path(&doc)),
            "{notice_text}"
        );
        assert_eq!(
            h.col(&format!(
                "SELECT count(*) FROM submissions WHERE chat_id = '{parent}'
                 AND text LIKE 'This chat is being rolled over%'"
            )),
            ["0"],
            "the parent is not asked until the child has settled"
        );
        // The child "writes" its document, answers, and goes quiet. (No `reply` after the
        // first turn: its flush check expects the one streaming row, and now the parent has
        // an assistant row too.)
        std::fs::create_dir_all(doc.parent().unwrap()).unwrap();
        std::fs::write(
            &doc,
            "# Hand-off\nTask: fix the grammar. State: half done on branch fix-grammar. \
             Next: cover the semicolon cases.\n",
        )
        .unwrap();
        ack(&cw, &notice).await;
        cw.emit(json!({"type": "submission_delivered", "submissionId": notice["submissionId"]}))
            .await;
        cw.emit(json!({"type": "message_end", "message": {"id": "cmsg_1", "role": "assistant", "content": [{"type": "text", "text": "Wrote the roll-over document."}], "status": "complete"}}))
            .await;
        cw.emit(json!({"type": "agent_settled", "outcome": "complete"}))
            .await;
        // Only now the parent is asked, with the subagents section and the packet folder taught.
        let ask = pw.expect("prompt").await;
        let ask_text = ask["text"].as_str().unwrap().to_owned();
        assert!(
            ask_text.contains("Running subagents")
                && ask_text.contains("packet")
                && ask_text.contains(&plain_path(&handoff_dir(&h.rt, &parent))),
            "{ask_text}"
        );
        ack(&pw, &ask).await;
        pw.emit(json!({"type": "agent_start"})).await;
        pw.emit(json!({"type": "message_end", "message": {"id": "pmsg_2", "role": "assistant", "content": [{"type": "text", "text": "Goal: finish it. Left to do: part two."}], "status": "complete"}}))
            .await;
        pw.emit(json!({"type": "agent_settled", "outcome": "complete"}))
            .await;
        // The successor's first message: hand-off plus the machine-written roster.
        let mut sw = h.worker().await;
        sw.ready().await;
        let first = sw.expect("prompt").await;
        let brief = first["text"].as_str().unwrap().to_owned();
        assert!(
            brief.contains("Left to do: part two.")
                && brief.contains("Running subagents (written by Helm)")
                && brief.contains("Fix the parser")
                && brief.contains(&plain_path(&doc))
                && brief.contains("idle, wrote a roll-over document")
                && brief.contains("one new subagent per document"),
            "{brief}"
        );
        h.until("old chat archived", |h| {
            h.col(&format!("SELECT archived FROM chats WHERE id = '{parent}'")) == ["1"]
        })
        .await;
        let succ = h.col(&format!(
            "SELECT chat_id FROM chat_succession WHERE predecessor_id = '{parent}'"
        ))[0]
            .clone();
        assert_eq!(
            h.col(&format!(
                "SELECT COALESCE(parent_chat_id = '{succ}', 0) FROM chats WHERE id = '{child}'"
            )),
            ["1"],
            "the child answers to the successor now"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_straggler_child_never_stops_the_roll_over_past_the_wait_cap() {
        use crate::runtime_tests::{ack, attach_ops_host, harness, reply};
        let mut h = harness();
        attach_ops_host(&h);
        let (parent, with_doc, no_doc) = (h.create().await, h.create().await, h.create().await);
        h.db.write(|tx| {
            tx.execute(
                "UPDATE chats SET parent_chat_id = ?1, title = 'Slow with doc' WHERE id = ?2",
                [&parent, &with_doc],
            )?;
            tx.execute(
                "UPDATE chats SET parent_chat_id = ?1, title = 'Slow, deaf' WHERE id = ?2",
                [&parent, &no_doc],
            )?;
            // Both children are stuck 'running' with a submission outstanding: they never
            // settle, so the wait cap, not their answer, is what lets the roll over proceed.
            tx.execute(
                "UPDATE chats SET status = 'running' WHERE id IN (?1, ?2)",
                [&with_doc, &no_doc],
            )?;
            tx.execute(
                "INSERT INTO submissions(id, chat_id, text, state, created_at)
                 VALUES ('strag-1', ?1, 'work', 'delivered', 1)",
                [&with_doc],
            )
        })
        .unwrap();
        let rec =
            h.db.snapshot(|tx| crate::store::get_chat(tx, &parent, 0))
                .unwrap()
                .unwrap();
        let targets = cascade_targets(&h.rt, &rec);
        assert_eq!(targets.len(), 2, "both running children cascade");
        let doc = handoff_dir(&h.rt, &parent).join(format!("{with_doc}.md"));
        std::fs::create_dir_all(doc.parent().unwrap()).unwrap();
        // The roll over runs with a 50 ms cap: no child settles, so it carries on anyway.
        let seam = h.rt.sh.host.get().unwrap();
        let (ctx, rt) = (
            seam.ctx.clone(),
            seam.ctx.services.need::<ChatRuntime>().unwrap(),
        );
        let rec2 = rec.clone();
        let flow = tokio::spawn(async move {
            roll_forward(
                &ctx,
                &rt,
                &rec2,
                targets,
                (None, None),
                Duration::from_millis(50),
                None,
            )
            .await
        });
        let mut pw = h.worker().await;
        pw.ready().await;
        let ask = pw.expect("prompt").await;
        // The document lands while the parent is still writing its hand-off: the roster is
        // read only after that wait, so this child counts as having written one.
        std::fs::write(
            &doc,
            "# Hand-off\nTask: keep going on branch slow-tail. Worktree ../helm-wt-slow. \
             State: two files left. Next: the tail cases, then the review.\n",
        )
        .unwrap();
        ack(&pw, &ask).await;
        pw.emit(json!({"type": "agent_start"})).await;
        reply(&h, &pw, "pmsg_1", "Goal: carry on.").await;
        pw.emit(json!({"type": "agent_settled", "outcome": "complete"}))
            .await;
        flow.await
            .unwrap()
            .expect("the roll over carried on past the stragglers");
        let mut sw = h.worker().await;
        sw.ready().await;
        let brief = sw.expect("prompt").await;
        let brief = brief["text"].as_str().unwrap().to_owned();
        assert!(
            brief.contains("Goal: carry on.")
                && brief.contains("Slow with doc")
                && brief.contains("still working, wrote a roll-over document at")
                && brief.contains(&plain_path(&doc))
                && brief.contains("Slow, deaf")
                && brief.contains("no roll-over document was written"),
            "{brief}"
        );
        assert_eq!(
            h.col(&format!("SELECT archived FROM chats WHERE id = '{parent}'")),
            ["1"],
            "the stragglers did not block the archive"
        );
    }

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
                page: None,
                kind: None,
                wait: None,
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
    fn a_failed_roll_over_start_gives_the_chat_back_to_later_rolls() {
        // A roll over that fails on its way to the background flow must not keep the chat
        // "already running" until the app restarts: the guard drops the slot on the way out.
        let id = "in-flight-guard-test";
        IN_FLIGHT.lock().unwrap().remove(id);
        let guard = InFlight::take(id).expect("the slot is free");
        assert!(
            InFlight::take(id).is_none(),
            "a second roll over is refused while the first holds the slot"
        );
        drop(guard);
        assert!(
            !IN_FLIGHT.lock().unwrap().contains(id),
            "dropping the guard releases the chat again"
        );
        // Once the background flow has taken over, the flow clears the slot — not the guard.
        let mut guard = InFlight::take(id).expect("free once more");
        guard.disarm();
        drop(guard);
        assert!(
            IN_FLIGHT.lock().unwrap().contains(id),
            "the running flow still holds the chat"
        );
        IN_FLIGHT.lock().unwrap().remove(id);
    }

    #[test]
    fn a_cascaded_failure_names_the_children_it_had_stopped() {
        let kids = [
            CascadeChild {
                id: "kid-a".into(),
                title: "Fix the parser".into(),
                doc: PathBuf::from("a.md"),
            },
            CascadeChild {
                id: "kid-b".into(),
                title: "Slow, deaf".into(),
                doc: PathBuf::from("b.md"),
            },
        ];
        let m = failed_after_cascade(
            "The agent did not write a hand-off in time. Nothing was changed.",
            &kids,
        );
        assert!(
            m.starts_with("The agent did not write a hand-off in time.")
                && m.contains("Fix the parser, Slow, deaf")
                && !m.ends_with("Nothing was changed."),
            "{m}"
        );
    }

    #[test]
    fn the_successor_message_links_the_old_chat_and_the_cut_respects_characters() {
        // (title, what the message must contain): the title goes into a chat link, escaped so
        // it cannot break it, with the id kept in the plain text next to it.
        for (title, expect) in [
            ("Fix login", "[Fix login](helm://chats/abc) (chat id abc)"),
            (
                "Fix [a] \\ b",
                "[Fix \\[a\\] \\\\ b](helm://chats/abc) (chat id abc)",
            ),
            ("two\nlines", "[two lines](helm://chats/abc)"),
            ("code `a]`", "[code \\`a\\]\\`](helm://chats/abc)"),
        ] {
            let m = successor_message(title, "abc", "Goal: x");
            assert!(m.contains(expect) && m.ends_with("Goal: x"), "{m}");
        }
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
            crate::mute::set_created_by(tx, &old, &maker)?;
            // What was half-done in the old chat's composer: a draft, a staged picture, a staged
            // reference, a picture already sent (which stays with the message it went with), and
            // the open-item state.
            tx.execute(
                "UPDATE chats SET draft = 'half a thought', draft_revision = 5,
                        sidebar_state = '{\"v\":1,\"expanded\":true,\"openCards\":[3]}' WHERE id = ?1",
                [&old],
            )?;
            for (id, sub) in [("img-staged", None), ("img-sent", Some("s1"))] {
                tx.execute(
                    "INSERT INTO chat_attachments(id, chat_id, name, mime, bytes, width, height, submission_id, created_at)
                     VALUES (?1, ?2, 'a.png', 'image/png', 1, 1, 1, ?3, 1)",
                    rusqlite::params![id, &old, sub],
                )?;
            }
            tx.execute(
                "INSERT INTO chat_attachment_refs(id, chat_id, kind, target, name, created_at)
                 VALUES ('ref-staged', ?1, 'file', 'C:/w/x.txt', 'x.txt', 1)",
                [&old],
            )?;
            Ok::<_, rusqlite::Error>(())
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
            Some(new.clone()),
            "the old chat's children move under the successor"
        );
        // The composer goes with the work: the draft (under a revision newer than the old one's,
        // so the successor's composer takes it up), the staged attachments and the open cards.
        let composer = |id: &str| -> (String, i64, Option<String>, Vec<String>, i64) {
            db.snapshot(|tx| {
                let (draft, rev, state) = tx.query_row(
                    "SELECT draft, draft_revision, sidebar_state FROM chats WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                let mut st =
                    tx.prepare("SELECT id FROM chat_attachments WHERE chat_id = ?1 ORDER BY id")?;
                let held = st
                    .query_map([id], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                let refs = tx.query_row(
                    "SELECT count(*) FROM chat_attachment_refs WHERE chat_id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                Ok::<_, rusqlite::Error>((draft, rev, state, held, refs))
            })
            .unwrap()
        };
        let carried = composer(&new);
        assert_eq!(carried.0, "half a thought");
        assert!(carried.1 > 5, "newer than the old draft's revision");
        assert!(carried.2.as_deref().unwrap().contains("\"openCards\":[3]"));
        assert_eq!((carried.3, carried.4), (vec!["img-staged".to_owned()], 1));
        let left = composer(&old);
        assert_eq!(left.0, "", "the old chat lets the draft go");
        assert!(left.1 > 5);
        assert_eq!(
            (left.3, left.4),
            (vec!["img-sent".to_owned()], 0),
            "a picture already sent stays with its message"
        );
    }

    #[test]
    fn the_roll_over_end_links_both_sessions_and_archives_the_old_one_in_one_write() {
        let (_d, db) = crate::schema::tests::temp_db();
        let mk = || {
            db.write(|tx| crate::store::create_chat(tx, "C:/w/proj", None, None, 1))
                .unwrap()
        };
        let (old, new) = (mk(), mk());
        db.write(|tx| link_and_archive(tx, &old, &new, &old, 9))
            .unwrap()
            .unwrap();
        let row = |id: &str| -> (Option<String>, Option<String>, Option<String>, i64) {
            db.snapshot(|tx| {
                tx.query_row(
                    "SELECT stack_id, prev_session_id, next_session_id, archived FROM chats WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
            })
            .unwrap()
        };
        assert_eq!(
            row(&old),
            (Some(old.clone()), None, Some(new.clone()), 1),
            "the old session stays the stack's oldest and points at the successor"
        );
        assert_eq!(
            row(&new),
            (Some(old.clone()), Some(old.clone()), None, 0),
            "the successor carries the same stack id and points back"
        );
        assert_eq!(
            db.snapshot(|tx| {
                tx.query_row(
                    "SELECT predecessor_id FROM chat_succession WHERE chat_id = ?1",
                    [&new],
                    |r| r.get::<_, String>(0),
                )
            })
            .unwrap(),
            old,
            "the succession its other readers use went in with the same write"
        );
    }

    #[test]
    fn stack_of_lists_a_three_session_chain_oldest_first_and_names_the_latest() {
        let (_d, db) = crate::schema::tests::temp_db();
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 0);
                 INSERT INTO chats(id, project_id, title, created_at, updated_at, archived) VALUES
                   ('a', 'p', 'Work', 1, 1, 0), ('b', 'p', 'Work', 2, 2, 0),
                   ('c', 'p', 'Work', 3, 3, 0), ('x', 'p', 'Alone', 4, 4, 0);
                 INSERT INTO chat_succession(chat_id, predecessor_id) VALUES ('b', 'a'), ('c', 'b');",
            )?;
            crate::schema::link_old_rolls(tx)?;
            tx.execute("UPDATE chats SET archived = 1 WHERE id IN ('a', 'b')", [])
        })
        .unwrap();
        // Asked from any session of the stack, the same answer comes back.
        for from in ["a", "b", "c"] {
            let stack = db.snapshot(|tx| stack_of(tx, from)).unwrap().unwrap();
            assert_eq!(stack.stack_id.as_deref(), Some("a"));
            assert_eq!(stack.latest_id, "c");
            assert_eq!(
                stack
                    .sessions
                    .iter()
                    .map(|s| format!("{}{}", s.id, if s.archived { "!" } else { "" }))
                    .collect::<Vec<_>>(),
                ["a!", "b!", "c"],
                "oldest first, archived sessions marked as such"
            );
        }
        // A chat no roll over touched answers with itself alone, and no stack id.
        let lone = db.snapshot(|tx| stack_of(tx, "x")).unwrap().unwrap();
        assert_eq!(lone.stack_id, None);
        assert_eq!(lone.latest_id, "x");
        assert_eq!(lone.sessions.len(), 1);
        assert_eq!(lone.sessions[0].title, "Alone");
        assert!(db.snapshot(|tx| stack_of(tx, "nope")).unwrap().is_none());
    }
}
