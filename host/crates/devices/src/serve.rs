//! The host side: answering other devices about the chats that run here.
//!
//! A [`Service`] hears requests (`dev_call` frames) from the [`DeviceLink`], runs them against a
//! [`ChatPort`] (the chats and profiles of this install), and sends the result back. Requests that
//! only read need nothing but the pairing; requests that run something (send, stop, answer, create,
//! change a chat) need the switch "Let other devices use chats on this PC". What a watcher may do
//! is exactly the list in [`Op`]; anything else is refused, and the sentences say to do it on the
//! PC the chat runs on.
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
    /// Whether other devices may run chats here.
    fn run_allowed(&self) -> bool;
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
    /// Send the text to the chat as the person's own words (origin `remote`).
    fn send(&self, chat_id: String, client_id: String, text: String) -> BoxFut<'_, PortResult<()>>;
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
    handle: Handle,
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
            handle: handle.clone(),
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
            "chat" | "chat_removed" | "thread" | "pending" | "refresh" | "message" => {
                if !lock(&self.inner.subs).watchers().is_empty() {
                    let _ = self.inner.events.send(ev.clone());
                }
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
        if op.is_run() && !self.port.run_allowed() {
            return (Err(WireError::not_allowed()), None);
        }
        let uid = from.uid.as_str();
        match self.run_parsed(uid, op).await {
            Ok((v, sub)) => (Ok(v), sub),
            Err(e) => (Err(e), None),
        }
    }

    async fn run_parsed(
        &self,
        uid: &str,
        op: Op,
    ) -> Result<(Value, Option<String>), WireError> {
        let done = |v: Value| -> Result<(Value, Option<String>), WireError> { Ok((v, None)) };
        match op {
            Op::Hello => done(to_value(&self.port.hello().await?)?),
            Op::ChatsSub(a) => {
                lock(&self.subs).add_chats(uid);
                match self.port.chats(a.since).await {
                    Ok((chats, cursor)) => Ok((
                        json!({ "chats": chats, "cursor": cursor }),
                        Some(CHATS_SUB.to_owned()),
                    )),
                    Err(e) => {
                        lock(&self.subs).remove_chats(uid);
                        Err(e)
                    }
                }
            }
            Op::ChatSub(a) => {
                lock(&self.subs).add_chat(uid, &a.chat_id);
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
            Op::Send(a) => done(self.send(a).await?),
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

    async fn send(&self, a: SendArgs) -> Result<Value, WireError> {
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
            .send(a.chat_id, a.client_id.clone(), a.text)
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
fn confirm_options(i: &Interaction) -> Vec<String> {
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
        Some(InteractionAttention::Approval | InteractionAttention::Question | InteractionAttention::Plan)
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
