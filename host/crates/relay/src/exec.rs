//! Running an admitted outside call inside Helm, after [`crate::gate::admit`].
//!
//! - **Reads and stop** (`app_list_chats`, `app_read_chat`, `app_list_projects`,
//!   `app_list_attention`, `app_stop_chat`, and `app_make_room_for_speech`, kept so older hubs
//!   get a quick answer; it unloads nothing) go through Helm's own host-tool dispatcher as a
//!   **restricted** caller named `external:<client>`: the restricted gate is default-deny and
//!   lists only the vetted tools, and the dispatcher re-checks sizes, timeouts and the result limit.
//!   The hub says which of this PC's people the call is for, and a listing answers with only
//!   that person's chats — this PC's and, with `device` naming another or "any", the chats of
//!   other paired PCs that person owns or is shared into; `all: true` lists everything.
//! - **`app_send_message`** is Helm's own send with the gate's frame ("sent by another agent,
//!   not approval"), origin `other_chat`, like any cross-chat message; a call the hub verified
//!   for a linked person is framed as from an agent acting for them by name. The caller cannot
//!   word the frame. A chat another paired PC runs is not refused: the message goes over the hub
//!   to that PC the way the app's own cross-PC sends do. Sends are rate-limited per outside
//!   caller.
//!   Each outside caller also gets the chats crate's per-target limit (the same count and window
//!   as one chat messaging another), and a send counts against either limit only once it is
//!   stored, so a refused send never uses up the allowance.
//! - **`app_list_waiting` and `app_answer_waiting`** are for the hub's Remote page and are served
//!   by [`crate::remote`]; the answer is refused unless the switch in Settings → Hub is on. A
//!   message from the page is framed as the user's own (by name when the hub named the person; [`crate::gate::remote_frame`]).
//! - **Household calls** (another person's Helm, through the hub) run the same way as
//!   `peer:<person>` ([`AppExecutor::run_peer`]): the list answers with the shared fields only,
//!   a chat page leaves the folder out, and a message arrives framed as another person's
//!   ([`crate::household::frame`]), under the same send limits. Which of these the user allows is
//!   decided before they get here ([`crate::household::admit_peer`]).
//! - **Telegram** delivery into chats is [`crate::sink::ChatSink`].

use crate::client::{BoxFut, Executor, ToolFailure};
use crate::gate::{self, Admitted};
use crate::household;
use helm_chats::submit::{host_send, HostSend};
use helm_chats::types::SubmissionOrigin;
use helm_hosttools::{Caller, CallerBinding, ChatRequest, HostTools};
use helm_ipc::{EventSink, Services};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The allowed tools this executor answers itself; the rest go through the host-tool dispatcher.
const SERVED_HERE: &[&str] = &["app_send_message", "app_list_waiting", "app_answer_waiting"];

/// At most this many messages per outside caller per minute.
pub const SENDS_PER_MINUTE: usize = 20;

/// What the executor needs from the app, filled in once the op dispatcher exists.
pub struct Wiring {
    pub services: Arc<Services>,
    pub events: Arc<dyn EventSink>,
    pub tools: Arc<HostTools>,
}

/// Who a relayed send acts as when the chat runs on another PC.
#[derive(Clone, Copy)]
enum SendAs<'a> {
    /// The hub person the call was stamped for (none from an older hub: the default applies).
    Person(Option<&'a str>),
    /// A household peer: not one of this PC's people, so it cannot reach a chat on a third PC.
    Peer,
}

/// One stored relayed send: when, the outside caller, the target chat.
type SendLog = VecDeque<(Instant, String, String)>;

#[derive(Default)]
pub struct AppExecutor {
    wiring: OnceLock<Wiring>,
    sends: Mutex<SendLog>,
    /// Prompts the Remote page answered a moment ago ([`crate::remote`]).
    answered: crate::remote::Answered,
}

pub(crate) fn refused(text: impl Into<String>) -> ToolFailure {
    ToolFailure {
        text: text.into(),
        changed: false,
    }
}

/// The caller id an outside client runs under. Never a real chat id.
pub fn external_id(client: &str) -> String {
    let clean: String = client
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(64)
        .collect();
    format!("external:{clean}")
}

/// The caller id a household member runs under. Never a real chat id.
pub fn peer_id(person: &str) -> String {
    let clean: String = person
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(64)
        .collect();
    format!("peer:{clean}")
}

impl AppExecutor {
    pub fn wire(&self, w: Wiring) {
        let _ = self.wiring.set(w);
    }

    /// What the app wired in, once it has.
    pub(crate) fn wiring(&self) -> Option<&Wiring> {
        self.wiring.get()
    }

    /// Whether `caller` may send to `to` now: at most [`SENDS_PER_MINUTE`] a minute, and the chats
    /// crate's per-target limit ([`helm_chats::tools::PER_PAIR`] per
    /// [`helm_chats::tools::RATE_WINDOW`]). Checks only; [`Self::record_send`] counts a send.
    fn check_send(
        log: &mut SendLog,
        caller: &str,
        to: &str,
        now: Instant,
    ) -> Result<(), ToolFailure> {
        let window = helm_chats::tools::RATE_WINDOW.max(Duration::from_secs(60));
        while log
            .front()
            .is_some_and(|(t, _, _)| now.duration_since(*t) >= window)
        {
            log.pop_front();
        }
        let minute = log
            .iter()
            .filter(|(t, c, _)| c == caller && now.duration_since(*t) < Duration::from_secs(60))
            .count();
        if minute >= SENDS_PER_MINUTE {
            return Err(refused(format!(
                "At most {SENDS_PER_MINUTE} messages a minute from outside Helm; try again shortly."
            )));
        }
        let pair = log
            .iter()
            .filter(|(t, c, d)| {
                c == caller && d == to && now.duration_since(*t) < helm_chats::tools::RATE_WINDOW
            })
            .count();
        if pair >= helm_chats::tools::PER_PAIR {
            return Err(refused(format!(
                "{} messages have gone to that chat from outside Helm in the last 10 minutes; wait before sending more.",
                helm_chats::tools::PER_PAIR
            )));
        }
        Ok(())
    }

    fn record_send(log: &mut SendLog, caller: &str, to: &str, now: Instant) {
        log.push_back((now, caller.to_owned(), to.to_owned()));
    }

    /// `framed` wraps the message in the fixed frame for this kind of caller. It is `Sync`
    /// because a cross-PC send holds it across the wait for the other PC.
    async fn send_message(
        &self,
        w: &Wiring,
        caller: &str,
        as_person: SendAs<'_>,
        args: &Value,
        framed: &(dyn Fn(&str) -> String + Sync),
    ) -> Result<Value, ToolFailure> {
        let to = args["chatId"].as_str().unwrap_or("");
        if uuid::Uuid::parse_str(to).is_err() || to.len() != 36 {
            return Err(refused("chatId must be a chat's exact 36-character id."));
        }
        // `mode` comes from older phone builds; whatever it says, the message is delivered now.
        let db = w
            .services
            .get::<helm_core::db::Database>()
            .ok_or_else(|| refused("Helm's database is not available."))?;
        let target: Option<(String, bool)> = db
            .snapshot(|tx| {
                use rusqlite::OptionalExtension;
                tx.query_row(
                    "SELECT title, archived FROM chats WHERE id = ?1",
                    [to],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
                )
                .optional()
            })
            .map_err(|_| refused("The chat could not be looked up."))?;
        let Some((title, archived)) = target else {
            // Not a chat that runs here: if this PC keeps a copy of one that runs on another
            // paired PC, the message crosses to it like any other cross-PC send.
            return self
                .send_to_other_pc(w, caller, as_person, to, args, framed)
                .await;
        };
        if archived {
            return Err(refused("That chat is archived."));
        }
        // Held across the send, so two concurrent sends cannot both pass the last free place.
        let mut log = self.sends.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        Self::check_send(&mut log, caller, to, now)?;
        let rt = w
            .services
            .get::<helm_chats::ChatRuntime>()
            .ok_or_else(|| refused("Helm's chats are not available."))?;
        let text = framed(args["message"].as_str().unwrap_or(""));
        let sid = uuid::Uuid::new_v4().to_string();
        let sent = host_send(
            &rt,
            HostSend {
                chat_id: to,
                submission_id: &sid,
                text: &text,
                origin: SubmissionOrigin::OtherChat,
            },
        )
        .map_err(|e| refused(e.message))?;
        // Counted only once it is stored: a refused send never uses up the allowance.
        Self::record_send(&mut log, caller, to, now);
        drop(log);
        Ok(json!({
            "delivered": sent.delivered,
            "mode": args["mode"],
            "chatId": to,
            "title": title,
        }))
    }

    /// The same send for a chat another paired PC runs: the copy here names it and says whether
    /// it is archived, and the message goes through the mirror's own send, over the hub, under
    /// the same limits. Nothing limits which chat an outside caller may reach; the frame and the
    /// person it reads as on the other PC are the attribution, as always.
    async fn send_to_other_pc(
        &self,
        w: &Wiring,
        caller: &str,
        as_person: SendAs<'_>,
        to: &str,
        args: &Value,
        framed: &(dyn Fn(&str) -> String + Sync),
    ) -> Result<Value, ToolFailure> {
        let SendAs::Person(person) = as_person else {
            // A household peer is not one of this PC's people; the other PC would read the
            // message as someone else's. It reaches only the chats that run here.
            return Err(refused(
                "That chat runs on another PC; household callers can only reach chats that run here.",
            ));
        };
        let remote = w
            .services
            .get::<helm_chats::remote::RemoteSlot>()
            .and_then(|s| s.get())
            .filter(|r| r.owns(to).is_some())
            .ok_or_else(|| refused("There is no chat with that id."))?;
        let db = w
            .services
            .get::<helm_core::db::Database>()
            .ok_or_else(|| refused("Helm's database is not available."))?;
        let copy: (String, bool) = db
            .snapshot(|tx| {
                use rusqlite::OptionalExtension;
                Ok::<_, rusqlite::Error>(
                    tx.query_row(
                        "SELECT record FROM mirror_chats WHERE chat_id = ?1",
                        [to],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .map(|rec| {
                        let v: Value = serde_json::from_str(&rec).unwrap_or(Value::Null);
                        (
                            v["title"].as_str().unwrap_or("").to_owned(),
                            v["archived"].as_bool().unwrap_or(false),
                        )
                    }),
                )
            })
            .map_err(|_| refused("The chat could not be looked up."))?
            .ok_or_else(|| refused("There is no chat with that id."))?;
        if copy.1 {
            return Err(refused("That chat is archived."));
        }
        // The place in the limit is taken up front — the lock cannot be held across the wait
        // for the other PC — and given back if the send does not arrive.
        let now = {
            let mut log = self.sends.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            Self::check_send(&mut log, caller, to, now)?;
            Self::record_send(&mut log, caller, to, now);
            now
        };
        let text = framed(args["message"].as_str().unwrap_or(""));
        if let Err(e) = remote
            .send_as(
                helm_chats::types::ChatSendArgs {
                    chat_id: to.to_owned(),
                    submission_id: uuid::Uuid::new_v4().to_string(),
                    text,
                    action: helm_chats::types::SendAction::Steer,
                    draft_revision: 0,
                    // Never the composer's: an outside send does not touch the draft on the other PC.
                    origin: Some(helm_chats::types::SendOrigin::Action),
                    attachments: None,
                    context: None,
                },
                person.map(str::to_owned),
            )
            .await
        {
            let mut back = self.sends.lock().unwrap_or_else(|p| p.into_inner());
            back.retain(|(t, c, d)| !(*t == now && c == caller && d == to));
            return Err(refused(e.message));
        }
        Ok(json!({
            "delivered": true,
            "mode": args["mode"],
            "chatId": to,
            "title": copy.0,
        }))
    }

    async fn dispatch(
        &self,
        w: &Wiring,
        caller: &str,
        call: Admitted,
    ) -> Result<Value, ToolFailure> {
        let restricted: Vec<String> = gate::allowed_tools()
            .into_iter()
            .filter(|t| !SERVED_HERE.contains(t))
            .map(str::to_owned)
            .collect();
        self.dispatch_as(w, external_id(caller), restricted, call)
            .await
    }

    /// Run one tool through Helm's dispatcher as the restricted caller `chat_id`, which may use
    /// only the tools in `restricted`.
    async fn dispatch_as(
        &self,
        w: &Wiring,
        chat_id: String,
        restricted: Vec<String>,
        call: Admitted,
    ) -> Result<Value, ToolFailure> {
        let who = Caller {
            chat_id,
            restricted: Some(restricted),
        };
        let req = ChatRequest {
            id: uuid::Uuid::new_v4().to_string(),
            operation: call.tool,
            args: call.args,
            caller: Some(CallerBinding {
                child: false,
                tool_call_id: None,
            }),
        };
        let r = w
            .tools
            .call(w.services.clone(), w.events.clone(), &who, &req)
            .await;
        if r["ok"] == true {
            Ok(r["result"].clone())
        } else {
            Err(ToolFailure {
                text: r["error"].as_str().unwrap_or("The tool failed.").to_owned(),
                changed: r["changed"].as_str() != Some("no"),
            })
        }
    }

    /// The chats an agent has flagged for the user's attention. A database without the table (the
    /// chats crate owns it) has none.
    fn flagged(w: &Wiring) -> HashSet<String> {
        let Some(db) = w.services.get::<helm_core::db::Database>() else {
            return HashSet::new();
        };
        db.snapshot(|tx| {
            let mut st = tx.prepare("SELECT chat_id FROM chat_flags")?;
            let ids = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<HashSet<_>, _>>()?;
            Ok::<_, rusqlite::Error>(ids)
        })
        .unwrap_or_default()
    }

    /// The chats the acting person sees: their profile's own on this PC, those shared with it,
    /// (for the home profile) any chat still without an owner, and the chats other paired PCs
    /// run that this PC's copies say belong to that person or are shared with them. Nobody
    /// resolved means nothing is visible.
    fn visible_ids(w: &Wiring, person: Option<&str>) -> HashSet<String> {
        let Some(db) = w.services.get::<helm_core::db::Database>() else {
            return HashSet::new();
        };
        let Ok(profile) = crate::people::profile_for(&db, person) else {
            return HashSet::new();
        };
        let mut ids: HashSet<String> = db
            .snapshot(|tx| helm_profiles::store::chat_ids_for(tx, &profile.id))
            .unwrap_or_default()
            .into_iter()
            .collect();
        // The other PCs' chats, as far as the copies here know: the profile's hub person owns
        // them, or they are shared with that person. A profile with no hub person, or a PC that
        // keeps no copies, adds none.
        let hub: Option<String> = db
            .snapshot(|tx| {
                use rusqlite::OptionalExtension;
                Ok::<_, rusqlite::Error>(
                    tx.query_row(
                        "SELECT hub_person_id FROM profiles WHERE id = ?1",
                        [&profile.id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .flatten(),
                )
            })
            .unwrap_or(None);
        if let Some(h) = hub {
            let mirrored: Vec<String> = db
                .snapshot(|tx| {
                    let mut st = tx.prepare(
                        "SELECT chat_id FROM mirror_chats
                         WHERE person_id = ?1 OR instr(COALESCE(shared_with, ''), ?2) > 0",
                    )?;
                    let rows = st.query_map(rusqlite::params![h, format!("\"{h}\"")], |r| {
                        r.get::<_, String>(0)
                    })?;
                    rows.collect::<rusqlite::Result<_>>()
                })
                .unwrap_or_default();
            ids.extend(mirrored);
        }
        ids
    }

    /// Keep only visible chats in a listing result: the items of `app_list_chats` and the groups
    /// of `app_list_attention` (whose rows all carry `chatId`). Anything else passes through.
    fn filter_visible(mut r: Value, ids: &HashSet<String>) -> Value {
        let keep = |rows: &mut Vec<Value>| {
            rows.retain(|row| {
                row.get("chatId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| ids.contains(id))
            })
        };
        if let Some(rows) = r.get_mut("chats").and_then(Value::as_array_mut) {
            keep(rows);
        }
        for key in [
            "needsYou", "running", "unread", "stalled", "flagged", "muted",
        ] {
            if let Some(rows) = r.get_mut(key).and_then(Value::as_array_mut) {
                keep(rows);
            }
        }
        r
    }

    /// The name of the profile a hub-stamped person is on this PC, for a frame.
    fn person_name(w: &Wiring, person: Option<&str>) -> Option<String> {
        let p = person?;
        let db = w.services.get::<helm_core::db::Database>()?;
        crate::people::profile_for(&db, Some(p))
            .ok()
            .map(|pr| pr.name)
    }

    async fn peer(&self, w: &Wiring, person: &str, call: Admitted) -> Result<Value, ToolFailure> {
        if call.tool == "app_send_message" {
            let key = format!("peer:{person}");
            return self
                .send_message(w, &key, SendAs::Peer, &call.args, &|b| {
                    household::frame(person, b)
                })
                .await;
        }
        let tool = call.tool.clone();
        let r = self
            .dispatch_as(w, peer_id(person), vec![tool.clone()], call)
            .await?;
        match tool.as_str() {
            "app_list_chats" => Ok(household::shared_chat_list(&r, &Self::flagged(w))),
            "app_read_chat" => Ok(household::shared_transcript(r)),
            _ => Err(refused(
                "That is not something household callers can ask for.",
            )),
        }
    }
}

impl Executor for AppExecutor {
    fn run<'a>(
        &'a self,
        caller: &'a str,
        person: Option<&'a str>,
        call: Admitted,
    ) -> BoxFut<'a, Result<Value, ToolFailure>> {
        Box::pin(async move {
            let Some(w) = self.wiring.get() else {
                return Err(refused("Helm is still starting; try again in a moment."));
            };
            let (tool, remote) = (call.tool.clone(), call.remote);
            let all = call.args.get("all").and_then(Value::as_bool) == Some(true);
            match tool.as_str() {
                // The Remote page's messages are the user's own words, and say so.
                "app_send_message" if remote => {
                    let named = Self::person_name(w, person);
                    let framed = |b: &str| gate::remote_frame(named.as_deref(), b);
                    self.send_message(w, caller, SendAs::Person(person), &call.args, &framed)
                        .await
                }
                "app_send_message" => {
                    // A call the hub verified for one of this PC's linked people is framed as
                    // acting for them, by name; otherwise it is the plain outside-agent frame.
                    let named = Self::person_name(w, person);
                    let framed = |b: &str| match &named {
                        Some(n) => gate::person_frame(n, b),
                        None => gate::frame(caller, b),
                    };
                    self.send_message(w, caller, SendAs::Person(person), &call.args, &framed)
                        .await
                }
                // The gate has already refused these for any other caller; this is the second wall.
                "app_list_waiting" | "app_answer_waiting" if !remote => Err(refused(
                    "That is only available from the hub's Remote page.",
                )),
                "app_list_waiting" => crate::remote::list_waiting(w, person, all),
                "app_answer_waiting" => {
                    crate::remote::answer(w, &self.answered, &call.args, person).await
                }
                _ => {
                    let mut call = call;
                    let lists_chats = tool == "app_list_chats";
                    // A chat list is the calling person's own unless they say otherwise: the
                    // person goes into the tool's own filter, so paging counts only their chats.
                    // A person or `all` the caller named is theirs to ask for, and a call with
                    // no person to name is cut down after the fact.
                    let own_list = lists_chats
                        && !all
                        && call.args.get("person").is_none()
                        && person.is_some_and(|p| !p.trim().is_empty());
                    if own_list {
                        if let (Some(p), Some(a)) = (person, call.args.as_object_mut()) {
                            a.insert("person".to_owned(), Value::from(p.trim().to_lowercase()));
                        }
                    }
                    let named_person = lists_chats && call.args.get("person").is_some();
                    let r = self.dispatch(w, caller, call).await?;
                    if all
                        || named_person
                        || !matches!(tool.as_str(), "app_list_chats" | "app_list_attention")
                    {
                        return Ok(r);
                    }
                    Ok(Self::filter_visible(r, &Self::visible_ids(w, person)))
                }
            }
        })
    }

    fn run_peer<'a>(
        &'a self,
        person: &'a str,
        call: Admitted,
    ) -> BoxFut<'a, Result<Value, ToolFailure>> {
        Box::pin(async move {
            let Some(w) = self.wiring.get() else {
                return Err(refused("Helm is still starting; try again in a moment."));
            };
            self.peer(w, person, call).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_callers_never_get_a_chat_shaped_id() {
        assert_eq!(external_id("remote"), "external:remote");
        assert_eq!(external_id("a b/../c"), "external:abc");
        assert!(uuid::Uuid::parse_str(&external_id("x")).is_err());
        assert_eq!(peer_id("Mary Ann"), "peer:MaryAnn");
        assert_eq!(peer_id("../x"), "peer:x");
        assert!(uuid::Uuid::parse_str(&peer_id("x")).is_err());
    }

    #[test]
    fn sends_are_rate_limited_per_caller_and_per_target_and_counted_once_stored() {
        let mut log = SendLog::new();
        let t0 = Instant::now();
        let ok = |log: &mut SendLog, c: &str, to: &str, t: Instant| {
            AppExecutor::check_send(log, c, to, t).map(|_| AppExecutor::record_send(log, c, to, t))
        };
        // Per target: the chats crate's pair limit.
        for _ in 0..helm_chats::tools::PER_PAIR {
            assert!(ok(&mut log, "remote", "chat-a", t0).is_ok());
        }
        let e = ok(&mut log, "remote", "chat-a", t0).unwrap_err();
        assert!(e.text.contains("to that chat"), "{}", e.text);
        assert!(!e.changed);
        assert!(ok(&mut log, "remote", "chat-b", t0).is_ok(), "per target");
        assert!(ok(&mut log, "other", "chat-a", t0).is_ok(), "per caller");
        // A check that is never recorded (the send was refused) uses up nothing.
        for _ in 0..100 {
            assert!(AppExecutor::check_send(&mut log, "remote", "chat-c", t0).is_ok());
        }
        // Per caller per minute.
        let mut log = SendLog::new();
        for i in 0..SENDS_PER_MINUTE {
            assert!(ok(&mut log, "remote", &format!("c{i}"), t0).is_ok());
        }
        let e = ok(&mut log, "remote", "fresh", t0).unwrap_err();
        assert!(e.text.contains("a minute"), "{}", e.text);
        assert!(ok(&mut log, "remote", "fresh", t0 + Duration::from_secs(61)).is_ok());
        // The pair window is ten minutes, not one.
        let mut log = SendLog::new();
        for _ in 0..helm_chats::tools::PER_PAIR {
            assert!(ok(&mut log, "remote", "a", t0).is_ok());
        }
        assert!(ok(&mut log, "remote", "a", t0 + Duration::from_secs(61)).is_err());
        assert!(ok(&mut log, "remote", "a", t0 + helm_chats::tools::RATE_WINDOW).is_ok());
    }

    #[tokio::test]
    async fn an_unwired_executor_refuses_and_changes_nothing() {
        let e = AppExecutor::default();
        let p = e
            .run_peer("Pat", gate::admit("app_list_chats", &json!({})).unwrap())
            .await
            .unwrap_err();
        assert!(!p.changed);
        let r = e
            .run(
                "remote",
                None,
                gate::admit("app_list_chats", &json!({})).unwrap(),
            )
            .await
            .unwrap_err();
        assert!(!r.changed);
    }

    #[test]
    fn listings_keep_only_the_visible_chats() {
        let ids: HashSet<String> = ["a", "c"].into_iter().map(str::to_owned).collect();
        // app_list_chats items, and every group of app_list_attention, key on chatId.
        let chats = json!({
            "chats": [{"chatId": "a"}, {"chatId": "b"}, {"chatId": "c"}],
            "nextOffset": 3,
        });
        let r = AppExecutor::filter_visible(chats, &ids);
        let left: Vec<&str> = r["chats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["chatId"].as_str().unwrap())
            .collect();
        assert_eq!(left, ["a", "c"]);
        assert_eq!(r["nextOffset"], 3);
        let attention = json!({
            "needsYou": [{"chatId": "b"}],
            "running": [{"chatId": "a"}, {"chatId": "z"}],
            "unread": [],
            "stalled": [{"chatId": "c"}],
            "flagged": [{"chatId": "b"}, {"chatId": "c"}],
            "muted": [{"chatId": "a"}],
        });
        let r = AppExecutor::filter_visible(attention, &ids);
        assert_eq!(r["needsYou"].as_array().unwrap().len(), 0);
        assert_eq!(r["running"].as_array().unwrap().len(), 1);
        assert_eq!(r["stalled"].as_array().unwrap().len(), 1);
        assert_eq!(r["flagged"].as_array().unwrap().len(), 1);
        assert_eq!(r["muted"].as_array().unwrap().len(), 1);
        // A shape with nothing keyed by chatId passes through untouched.
        let other = json!({"projects": [{"id": "p"}]});
        assert_eq!(AppExecutor::filter_visible(other.clone(), &ids), other);
    }
}
