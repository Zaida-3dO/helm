//! The host side: answering other devices about the chats that run here.
//!
//! A [`Service`] hears requests (`dev_call` frames) from the [`DeviceLink`], runs them against a
//! [`ChatPort`] (the chats and profiles of this install), and sends the result back. What a
//! watcher may do is exactly the list in [`Op`]; anything else is refused, and the sentences say
//! to do it on the PC the chat runs on.
//!
//! Who asks decides the rest. A PC that is not paired with a person on the hub answers no one.
//! A device of the same person reads freely and runs chats while the switch "Let other devices
//! use chats on this PC" is on. A device of another person in the household needs the switch
//! "Let people in my household run their chats on this PC" and a profile here linked to them; it
//! then sees and runs only that person's own chats and the chats shared with them, answers what
//! waits only on their own chats, and starts chats only for themselves.
//!
//! Chat events from this install reach the service through [`Service::observe`] (the app's event
//! tap), and go to the watchers that subscribed as pushes ([`crate::subs`]).

use crate::link::{BoxFut, DeviceLink};
use crate::subs::{self, Out, Subs, CHATS_SUB, PUSH_ROW_BUDGET};
use crate::wire::{
    code, AnswerArgs, ArchiveArgs, Caller, ChatIdArgs, CreateArgs, DeviceFrame, DeviceInfo, Frame,
    Hello, Op, PageArgs, PeerDevice, RemoteChat, RenameArgs, SendArgs, SetModelArgs, ShareArgs,
    WireError, MAX_PAGE_ROWS, MAX_SEND_BYTES, SUB_ROWS,
};
use helm_chats::types::{
    ChatRecord, Interaction, InteractionAttention, InteractionMethod, MessageRow,
};
use helm_ipc::AppEvent;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub type PortResult<T> = Result<T, WireError>;

/// A chat as a watcher opens it: the record, the live thread, what waits to be read.
#[derive(Clone, Debug)]
pub struct ChatState {
    pub chat: RemoteChat,
    /// The thread state as the chat view shows it (interactions, tools, capabilities).
    pub thread: Value,
    /// The submissions sent but not yet settled.
    pub pending: Value,
    pub interactions: Vec<Interaction>,
}

/// Everything the service needs from this install. The production adapter reads the chats and
/// profiles tables and runs the chat ops; tests use [`crate::testing::FakePort`].
pub trait ChatPort: Send + Sync {
    /// This device's id and name.
    fn device(&self) -> DeviceInfo;
    /// Whether this person's other devices may run chats here.
    fn run_allowed(&self) -> bool;
    /// Whether people in the household may run their own chats here.
    fn household_run(&self) -> bool;
    /// Whether a profile on this install is linked to the hub person.
    fn person_known(&self, person_id: &str) -> bool;
    fn hello(&self) -> BoxFut<'_, PortResult<Hello>>;
    /// A chat record as other devices see it.
    fn remote_chat(&self, record: ChatRecord) -> RemoteChat;
    /// The chats other devices may list (those changed since the cursor, all of them for 0) and
    /// the cursor to give back next time.
    fn chats(&self, since: i64) -> BoxFut<'_, PortResult<(Vec<RemoteChat>, i64)>>;
    fn chat(&self, chat_id: String) -> BoxFut<'_, PortResult<ChatState>>;
    /// The newest `limit` active rows of a chat whose revision is above `since`, oldest first,
    /// and whether older ones were left out.
    fn rows_since(
        &self,
        chat_id: String,
        since: i64,
        limit: i64,
    ) -> BoxFut<'_, PortResult<(Vec<MessageRow>, bool)>>;
    /// A transcript page (the chat view's page shape) ending before the message, or the latest.
    fn page(
        &self,
        chat_id: String,
        before: Option<i64>,
        limit: usize,
    ) -> BoxFut<'_, PortResult<Value>>;
    /// Record a remote send; `false` when this `client_id` was seen before.
    fn claim_send(&self, client_id: &str, chat_id: &str) -> PortResult<bool>;
    /// Forget a claim whose send was refused, so the same id can be tried again.
    fn release_send(&self, client_id: &str);
    /// Send the text to the chat as the person's own words (origin `remote`). `from` names the
    /// household member who sent it when the chat is not theirs: then it is delivered as a
    /// message from them, never as the owner's words.
    fn send(
        &self,
        chat_id: String,
        client_id: String,
        text: String,
        from: Option<String>,
    ) -> BoxFut<'_, PortResult<()>>;
    fn stop(&self, chat_id: String) -> BoxFut<'_, PortResult<()>>;
    fn answer(&self, a: AnswerArgs) -> BoxFut<'_, PortResult<()>>;
    /// Start a chat for the local profile linked to the person; `unknown_person` if none is.
    fn create(&self, a: CreateArgs) -> BoxFut<'_, PortResult<String>>;
    fn set_model(&self, a: SetModelArgs) -> BoxFut<'_, PortResult<()>>;
    fn rename(&self, a: RenameArgs) -> BoxFut<'_, PortResult<()>>;
    fn archive(&self, a: ArchiveArgs) -> BoxFut<'_, PortResult<()>>;
    fn share(&self, a: ShareArgs) -> BoxFut<'_, PortResult<()>>;
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Who a request comes from, as this PC sees it.
#[derive(Clone, Debug, PartialEq)]
enum Asker {
    /// A device of the person this PC is paired as.
    Own,
    /// A device of another person in the household: their hub id and name.
    Household { person_id: String, name: String },
}

/// Requests answered once are remembered, so a second answer to one is refused even in the
/// instant before the chat drops it.
const ANSWERED_KEPT: usize = 256;

struct Inner {
    port: Arc<dyn ChatPort>,
    link: Arc<dyn DeviceLink>,
    subs: Mutex<Subs>,
    /// Which devices the hub says are up, from its last `devices` frame.
    online: Mutex<HashMap<String, bool>>,
    answered: Mutex<VecDeque<(String, String)>>,
    events: mpsc::UnboundedSender<AppEvent>,
    _handle: Handle,
}

/// Serves this install's chats to other devices.
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}

impl Service {
    /// Start serving: hear the link, and start the task that turns chat events into pushes.
    pub fn new(port: Arc<dyn ChatPort>, link: Arc<dyn DeviceLink>, handle: Handle) -> Service {
        let (events, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            port,
            link: link.clone(),
            subs: Mutex::new(Subs::default()),
            online: Mutex::new(HashMap::new()),
            answered: Mutex::new(VecDeque::new()),
            events,
            _handle: handle.clone(),
        });
        let weak = Arc::downgrade(&inner);
        let h = handle.clone();
        link.subscribe(Box::new(move |f| {
            let Some(inner) = weak.upgrade() else { return };
            match f {
                Frame::DevCall { id, from, op, args } => {
                    h.spawn(async move { inner.dev_call(id, from, op, args).await });
                }
                Frame::Devices { list } => inner.on_devices(&list),
                _ => {}
            }
        }));
        handle.spawn(consume(Arc::downgrade(&inner), rx));
        Service { inner }
    }

    /// The `device` frame: who this is and whether others may run chats here.
    pub fn device_frame(&self) -> Frame {
        let d = self.inner.port.device();
        Frame::Device(DeviceFrame {
            uid: d.uid,
            name: d.name,
            run: self.inner.port.run_allowed(),
        })
    }

    /// Tell the hub who this device is. Sent after the household switches on every new line, and
    /// whenever the name or the run switch changes.
    pub fn announce(&self) {
        self.inner.link.send(self.device_frame());
    }

    /// An event this install emitted. Chat events go to the watchers that subscribed; a changed
    /// device name or run switch is announced.
    pub fn observe(&self, ev: &AppEvent) {
        match ev.kind.as_str() {
            "device_changed" => self.announce(),
            "preferences" => {
                let changed = ev
                    .fields
                    .get("changed")
                    .and_then(Value::as_object)
                    .is_some_and(|m| m.contains_key("deviceRunFromOthers"));
                if changed {
                    self.announce();
                }
            }
            "chat" | "chat_removed" | "thread" | "pending" | "refresh" | "message"
                if !lock(&self.inner.subs).watchers().is_empty() =>
            {
                let _ = self.inner.events.send(ev.clone());
            }
            _ => {}
        }
    }

    /// The devices currently watching, for diagnostics and tests.
    pub fn watchers(&self) -> Vec<String> {
        lock(&self.inner.subs).watchers()
    }
}

/// Where the app's event tap hands events to the service once there is one.
#[derive(Default)]
pub struct EventTap(OnceLock<Service>);

impl EventTap {
    pub fn attach(&self, service: Service) {
        let _ = self.0.set(service);
    }

    pub fn observe(&self, ev: &AppEvent) {
        if let Some(s) = self.0.get() {
            s.observe(ev);
        }
    }

    pub fn service(&self) -> Option<Service> {
        self.0.get().cloned()
    }
}

/// Turns chat events into pushes, in order, and sends the batches of rows when they are due.
async fn consume(inner: Weak<Inner>, mut rx: mpsc::UnboundedReceiver<AppEvent>) {
    loop {
        let deadline = match inner.upgrade() {
            Some(i) => lock(&i.subs).next_deadline(),
            None => return,
        };
        let ev = match deadline {
            Some(at) => {
                tokio::select! {
                    ev = rx.recv() => ev,
                    _ = tokio::time::sleep_until(at) => {
                        if let Some(i) = inner.upgrade() {
                            let outs = lock(&i.subs).flush_due(Instant::now());
                            i.send_outs(outs);
                        }
                        continue;
                    }
                }
            }
            None => rx.recv().await,
        };
        let Some(ev) = ev else { return };
        let Some(i) = inner.upgrade() else { return };
        i.on_event(&ev);
    }
}

impl Inner {
    fn send_outs(&self, outs: Vec<Out>) {
        for o in outs {
            self.link.send(Frame::DevPush(o.push));
        }
    }

    fn on_devices(&self, list: &[PeerDevice]) {
        let online: HashMap<String, bool> =
            list.iter().map(|d| (d.uid.clone(), d.online)).collect();
        // A watcher the hub no longer shows as up has its subscriptions dropped; it asks again
        // when it comes back.
        lock(&self.subs).retain_online(|uid| online.get(uid).copied().unwrap_or(false));
        *lock(&self.online) = online;
    }

    fn on_event(&self, ev: &AppEvent) {
        let chat_id = ev
            .fields
            .get("chatId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let outs = match ev.kind.as_str() {
            "chat" => {
                if !lock(&self.subs).watching_chats() {
                    return;
                }
                let Some(record) = ev
                    .fields
                    .get("chat")
                    .and_then(|c| serde_json::from_value::<ChatRecord>(c.clone()).ok())
                else {
                    return;
                };
                let remote = self.port.remote_chat(record);
                lock(&self.subs).chat_changed(remote.to_value())
            }
            "chat_removed" => lock(&self.subs).chat_removed(&chat_id),
            "thread" | "pending" | "refresh" => {
                lock(&self.subs).chat_event(&chat_id, ev.to_value())
            }
            "message" => {
                let mut rows: Vec<Value> = ev
                    .fields
                    .get("rows")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for r in &mut rows {
                    subs::trim_row(r);
                }
                let revision = ev
                    .fields
                    .get("revision")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                lock(&self.subs).add_rows(&chat_id, revision, &rows, Instant::now());
                Vec::new()
            }
            _ => Vec::new(),
        };
        self.send_outs(outs);
    }

    async fn dev_call(self: Arc<Self>, id: String, from: Caller, op: String, args: Value) {
        let (result, activate) = self.run_op(&from, &op, args).await;
        self.link.send(Frame::result(&id, result));
        if let Some(sub) = activate {
            let outs = lock(&self.subs).activate(&from.uid, &sub);
            self.send_outs(outs);
        }
    }

    /// Run one request. On success the subscription it made (if any) is returned, to be
    /// activated once the result has gone out.
    async fn run_op(
        &self,
        from: &Caller,
        op: &str,
        args: Value,
    ) -> (Result<Value, WireError>, Option<String>) {
        let op = match Op::parse(op, args) {
            Ok(op) => op,
            Err(e) => return (Err(e), None),
        };
        let run = async {
            let asker = self.asker(from)?;
            let op = match &asker {
                Asker::Own => {
                    if op.is_run() && !self.port.run_allowed() {
                        return Err(WireError::not_allowed());
                    }
                    op
                }
                Asker::Household { person_id, .. } => self.household_op(person_id, op).await?,
            };
            self.run_parsed(&from.uid, &asker, op).await
        };
        match run.await {
            Ok((v, sub)) => (Ok(v), sub),
            Err(e) => (Err(e), None),
        }
    }

    /// Who is asking. A PC paired with no person answers no one; a caller with no person is
    /// refused too.
    fn asker(&self, from: &Caller) -> Result<Asker, WireError> {
        let Some(me) = self.link.person_id().filter(|p| !p.is_empty()) else {
            return Err(WireError::new(
                code::NOT_ALLOWED,
                "This PC is not paired with a person on the hub, so other devices can't use its chats.",
            ));
        };
        match from.person_id.as_deref().filter(|p| !p.is_empty()) {
            Some(p) if p == me => Ok(Asker::Own),
            Some(p) => Ok(Asker::Household {
                person_id: p.to_owned(),
                name: from
                    .person
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| "someone in the household".to_owned()),
            }),
            None => Err(WireError::new(
                code::NOT_ALLOWED,
                "This PC only answers devices paired with a person on the hub.",
            )),
        }
    }

    /// Check a household member's request against the switch and their own chats, and pin a
    /// new chat to them.
    async fn household_op(&self, person_id: &str, op: Op) -> Result<Op, WireError> {
        if !self.port.household_run() {
            return Err(WireError::new(
                code::NOT_ALLOWED,
                "This PC doesn't let people in the household run their chats here.",
            ));
        }
        if !self.port.person_known(person_id) {
            return Err(WireError::new(
                code::NOT_ALLOWED,
                "No one on this PC is linked to you, so you can't use its chats.",
            ));
        }
        let (chat_id, own_only) = match op {
            Op::Hello | Op::ChatsSub(_) | Op::ChatsUnsub | Op::ChatUnsub(_) => return Ok(op),
            Op::Create(mut a) => {
                a.person_id = person_id.to_owned();
                return Ok(Op::Create(a));
            }
            Op::ChatSub(ref a) => (a.chat_id.clone(), false),
            Op::Page(ref a) => (a.chat_id.clone(), false),
            Op::Send(ref a) => (a.chat_id.clone(), false),
            Op::Stop(ref a) => (a.chat_id.clone(), false),
            Op::SetModel(ref a) => (a.chat_id.clone(), false),
            Op::Rename(ref a) => (a.chat_id.clone(), false),
            Op::Archive(ref a) => (a.chat_id.clone(), false),
            // What a chat waits on, and who it is shared with, are for the person whose chat it is.
            Op::Answer(ref a) => (a.chat_id.clone(), true),
            Op::Share(ref a) => (a.chat_id.clone(), true),
        };
        let chat = self.port.chat(chat_id).await?.chat.to_value();
        if !subs::visible_to(&chat, person_id) {
            return Err(WireError::new(
                code::NOT_FOUND,
                "That chat is not on this device, or not shared with you.",
            ));
        }
        if own_only && !subs::owned_by(&chat, person_id) {
            return Err(WireError::new(
                code::REFUSED,
                "Only the person whose chat this is can do that from another device.",
            ));
        }
        Ok(op)
    }

    async fn run_parsed(
        &self,
        uid: &str,
        asker: &Asker,
        op: Op,
    ) -> Result<(Value, Option<String>), WireError> {
        let done = |v: Value| -> Result<(Value, Option<String>), WireError> { Ok((v, None)) };
        let scope = match asker {
            Asker::Own => None,
            Asker::Household { person_id, .. } => Some(person_id.as_str()),
        };
        match op {
            Op::Hello => done(to_value(&self.port.hello().await?)?),
            Op::ChatsSub(a) => {
                {
                    let mut subs = lock(&self.subs);
                    subs.set_scope(uid, scope);
                    subs.add_chats(uid);
                }
                match self.port.chats(a.since).await {
                    Ok((mut chats, cursor)) => {
                        if let Some(p) = scope {
                            chats.retain(|c| subs::visible_to(&c.to_value(), p));
                            lock(&self.subs).listed(uid, chats.iter().map(|c| c.id().to_owned()));
                        }
                        Ok((
                            json!({ "chats": chats, "cursor": cursor }),
                            Some(CHATS_SUB.to_owned()),
                        ))
                    }
                    Err(e) => {
                        lock(&self.subs).remove_chats(uid);
                        Err(e)
                    }
                }
            }
            Op::ChatSub(a) => {
                {
                    let mut subs = lock(&self.subs);
                    subs.set_scope(uid, scope);
                    subs.add_chat(uid, &a.chat_id);
                }
                let read = async {
                    let state = self.port.chat(a.chat_id.clone()).await?;
                    let (rows, more) = self
                        .port
                        .rows_since(a.chat_id.clone(), a.since, SUB_ROWS)
                        .await?;
                    Ok::<_, WireError>((state, rows, more))
                };
                match read.await {
                    Ok((state, rows, more)) => {
                        let mut rows: Vec<Value> = rows
                            .iter()
                            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
                            .collect();
                        // Newest rows matter most: drop the oldest until the answer fits.
                        let mut more = more;
                        fit_rows_newest(&mut rows, &mut more);
                        Ok((
                            json!({
                                "chat": state.chat,
                                "thread": state.thread,
                                "pending": state.pending,
                                "rows": rows,
                                "more": more,
                            }),
                            Some(subs::chat_sub_id(&a.chat_id)),
                        ))
                    }
                    Err(e) => {
                        lock(&self.subs).remove_chat(uid, &a.chat_id);
                        Err(e)
                    }
                }
            }
            Op::ChatUnsub(ChatIdArgs { chat_id }) => {
                lock(&self.subs).remove_chat(uid, &chat_id);
                done(json!({}))
            }
            Op::ChatsUnsub => {
                lock(&self.subs).remove_chats(uid);
                done(json!({}))
            }
            Op::Page(a) => done(self.page(a).await?),
            Op::Send(a) => {
                // A household member's words on a chat that is not theirs are marked as theirs.
                let from = match asker {
                    Asker::Household { person_id, name } => {
                        let chat = self.port.chat(a.chat_id.clone()).await?.chat.to_value();
                        (!subs::owned_by(&chat, person_id)).then(|| name.clone())
                    }
                    Asker::Own => None,
                };
                done(self.send(a, from).await?)
            }
            Op::Stop(a) => {
                self.port.stop(a.chat_id).await?;
                done(json!({}))
            }
            Op::Answer(a) => {
                self.answer(a).await?;
                done(Value::Null)
            }
            Op::Create(a) => {
                let chat_id = self.port.create(a).await?;
                done(json!({ "chatId": chat_id }))
            }
            Op::SetModel(a) => {
                self.port.set_model(a).await?;
                done(json!({}))
            }
            Op::Rename(a) => {
                self.port.rename(a).await?;
                done(json!({}))
            }
            Op::Archive(a) => {
                self.port.archive(a).await?;
                done(json!({}))
            }
            Op::Share(a) => {
                self.port.share(a).await?;
                done(json!({}))
            }
        }
    }

    async fn page(&self, a: PageArgs) -> Result<Value, WireError> {
        let limit = a.limit.unwrap_or(MAX_PAGE_ROWS).clamp(1, MAX_PAGE_ROWS);
        let mut page = self
            .port
            .page(a.chat_id, a.before_message_id, limit)
            .await?;
        if let Some(Value::Array(rows)) = page.get_mut("messages") {
            subs::fit_rows(rows, PUSH_ROW_BUDGET);
        }
        Ok(page)
    }

    async fn send(&self, a: SendArgs, from: Option<String>) -> Result<Value, WireError> {
        if a.text.trim().is_empty() {
            return Err(WireError::bad_args("There is nothing to send."));
        }
        if a.text.len() > MAX_SEND_BYTES {
            return Err(WireError::new(
                code::BAD_ARGS,
                "A message sent from another device is at most 16 KiB.",
            ));
        }
        if uuid::Uuid::parse_str(&a.client_id).is_err() {
            return Err(WireError::bad_args("clientId must be a UUID."));
        }
        if !self.port.claim_send(&a.client_id, &a.chat_id)? {
            return Ok(json!({ "accepted": true, "duplicate": true }));
        }
        // Both modes are delivered the way the PC delivers any message it did not type itself:
        // an idle chat starts a turn, a working one takes it in at its next tool boundary.
        let sent = self
            .port
            .send(a.chat_id, a.client_id.clone(), a.text, from)
            .await;
        match sent {
            Ok(()) => Ok(json!({ "accepted": true })),
            Err(e) => {
                self.port.release_send(&a.client_id);
                Err(e)
            }
        }
    }

    async fn answer(&self, a: AnswerArgs) -> Result<(), WireError> {
        let key = (a.chat_id.clone(), a.request_id.clone());
        if lock(&self.answered).contains(&key) {
            return Err(WireError::new(
                code::REFUSED,
                "That request was already answered.",
            ));
        }
        let state = self.port.chat(a.chat_id.clone()).await?;
        let Some(waiting) = state.interactions.iter().find(|i| i.id == a.request_id) else {
            return Err(WireError::new(
                code::REFUSED,
                "That request is no longer waiting.",
            ));
        };
        check_answer(waiting, &a)?;
        {
            let mut done = lock(&self.answered);
            done.push_back(key.clone());
            if done.len() > ANSWERED_KEPT {
                done.pop_front();
            }
        }
        if let Err(e) = self.port.answer(a).await {
            lock(&self.answered).retain(|k| *k != key);
            return Err(e);
        }
        Ok(())
    }
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, WireError> {
    serde_json::to_value(v).map_err(|e| WireError::internal(e.to_string()))
}

/// Drop the oldest rows until the rest fit one frame, setting `more` when any went.
fn fit_rows_newest(rows: &mut Vec<Value>, more: &mut bool) {
    subs::fit_rows(rows, PUSH_ROW_BUDGET * 2);
    let mut total: usize = rows.iter().map(|r| r.to_string().len()).sum();
    while total > PUSH_ROW_BUDGET && rows.len() > 1 {
        total -= rows[0].to_string().len();
        rows.remove(0);
        *more = true;
    }
}

/// The labels a confirmation shows: the request's own options, or Allow and Deny (Approve plan
/// and Keep planning for a plan).
pub(crate) fn confirm_options(i: &Interaction) -> Vec<String> {
    match i.options.as_deref() {
        Some(o) if !o.is_empty() => o.to_vec(),
        _ if i.attention == Some(InteractionAttention::Plan) => {
            vec!["Approve plan".to_owned(), "Keep planning".to_owned()]
        }
        _ => vec!["Allow".to_owned(), "Deny".to_owned()],
    }
}

/// Whether the answer is one the watcher may give for the request it waits on. Questions,
/// plan approvals and tool-permission prompts may be answered from another device; a folder-trust
/// prompt and a secret input never are. The label must be one the request shows, and what the
/// answer says must agree with it, so a mistaken or doctored answer cannot approve what the
/// person did not see.
pub fn check_answer(i: &Interaction, a: &AnswerArgs) -> Result<(), WireError> {
    if !matches!(
        i.attention,
        Some(
            InteractionAttention::Approval
                | InteractionAttention::Question
                | InteractionAttention::Plan
        )
    ) || i.secret == Some(true)
    {
        return Err(WireError::elsewhere());
    }
    let mismatch = || WireError::new(code::REFUSED, "That is not what the request shows.");
    // Cancelling approves nothing.
    if a.cancelled == Some(true) {
        return Ok(());
    }
    let label = a.label.trim();
    match i.method {
        InteractionMethod::Select => {
            let value = a
                .value
                .as_deref()
                .ok_or_else(|| WireError::bad_args("A choice needs a value."))?;
            let listed = i.options.iter().flatten().any(|o| o == label);
            if value != label || !(listed || i.allow_other == Some(true)) {
                return Err(mismatch());
            }
            if i.attention == Some(InteractionAttention::Plan) {
                if let Some(c) = a.confirmed {
                    if c != (label == "Approve plan") {
                        return Err(mismatch());
                    }
                }
            }
        }
        InteractionMethod::Confirm => {
            let confirmed = a
                .confirmed
                .ok_or_else(|| WireError::bad_args("A confirmation needs confirmed."))?;
            let options = confirm_options(i);
            let yes = options
                .iter()
                .find(|o| matches!(o.as_str(), "Approve plan" | "Allow"))
                .or_else(|| options.first())
                .cloned()
                .unwrap_or_default();
            let listed = options.iter().any(|o| o == label);
            if !listed || confirmed != (label == yes) {
                return Err(mismatch());
            }
        }
        InteractionMethod::Input | InteractionMethod::Editor => {
            if a.value.is_none() {
                return Err(WireError::bad_args("An answer needs a value."));
            }
            let shown = i.title.as_deref().unwrap_or_default().trim();
            if label.is_empty() || (!shown.is_empty() && label != shown) {
                return Err(mismatch());
            }
        }
        InteractionMethod::Dismiss => {}
    }
    Ok(())
}
