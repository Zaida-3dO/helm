//! The host side: answering other devices about the chats that run here.
//!
//! A [`Service`] hears requests (`dev_call` frames) from the [`DeviceLink`], runs them against a
//! [`ChatPort`] (the chats and profiles of this install), and sends the result back. What a
//! watcher may do is exactly the list in [`Op`]; anything else is refused, and the sentences say
//! to do it on the PC the chat runs on.
//!
//! Who asks decides the rest. A PC that is not paired with a person on the hub answers no one.
//! A device, whoever it acts for, reads freely and runs chats while the switch "Let other
//! devices use chats on this PC" is on. The person says who is acting, not what they may touch: a chat is not hidden from a person who acts here, but attribution holds —
//! a chat started from afar belongs to the acting person, and their words on a chat that is not
//! theirs arrive framed as a message from them, never as the owner's own words. The list a
//! watcher subscribes to holds every chat of this PC, whoever it belongs to.
//!
//! Chat events from this install reach the service through [`Service::observe`] (the app's event
//! tap), and go to the watchers that subscribed as pushes ([`crate::subs`]).

use crate::link::{BoxFut, DeviceLink};
use crate::subs::{self, Out, Subs, CHATS_SUB, PUSH_ROW_BUDGET};
use crate::tunnel::{self, LocalLoopback, TunnelOut, Tunnels};
use crate::wire::{
    code, AnswerArgs, ArchiveArgs, AttachPutArgs, AttachmentGetArgs, AttachmentRef, BlobReadArgs,
    BoardCancelArgs, BoardReadArgs, Caller, ChatIdArgs, CheckpointArgs, ConsentDecideArgs,
    CreateArgs, DefaultsArgs, DeviceFrame, DeviceInfo, DevicePerson, ForkArgs, ForwardCloseArgs,
    ForwardOpenArgs, Frame, Hello, MoveProfileArgs, MuteArgs, Op, OrganizeArgs, PageArgs,
    PageIdArgs, PagesReadArgs, PanelStageArgs, PeerDevice, ProjectCreateArgs, ProjectSetArgs, Push,
    QuickOpenArgs, RemoteChat, RenameArgs, RollOverArgs, SendArgs, SetModelArgs, SetPermissionArgs,
    ShareArgs, SnoozeArgs, SubmissionArgs, ToolStopArgs, VerbositySetArgs, WireError,
    MAX_CHUNK_BYTES, MAX_PAGE_ROWS, MAX_SEND_ATTACHMENTS, MAX_SEND_BYTES, MAX_UPLOAD_BYTES,
    SUB_ROWS,
};
use base64::Engine as _;
use helm_chats::types::{
    ChatRecord, Interaction, InteractionAttention, InteractionMethod, MessageRow,
};
use helm_ipc::AppEvent;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub type PortResult<T> = Result<T, WireError>;

/// One side-panel read of the chat's project folder: the app op it answers with, and its
/// arguments (which name the chat and, for the path reads, one path or a list of them).
/// The port checks the paths before anything opens.
#[derive(Clone, Debug)]
pub struct PanelRead {
    pub local_op: &'static str,
    pub args: Value,
}

/// Who is asking for pages: the hub person acting. Every pinned page is open to them; the
/// person only puts the pages filed under their own profiles first.
#[derive(Clone, Debug)]
pub struct PageScope {
    pub person_id: String,
}

/// A file carried from another device, with its bytes, ready to hand to the chat.
#[derive(Clone, Debug)]
pub struct SentFile {
    /// `image` or `file`.
    pub kind: String,
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// A chat as a watcher opens it: the record, the live thread, what waits to be read.
#[derive(Clone, Debug)]
pub struct ChatState {
    pub chat: RemoteChat,
    /// The thread state as the chat view shows it (interactions, tools, capabilities).
    pub thread: Value,
    /// The submissions sent but not yet settled.
    pub pending: Value,
    pub interactions: Vec<Interaction>,
    /// The Allow cards waiting on the person, as the consent store shows them. A card can be
    /// answered on any device that has the chat open; the first answer wins.
    pub consent: Vec<Value>,
}

/// The most the consent cards of one answer or push may weigh together.
pub const CONSENT_BUDGET: usize = 64 * 1024;

/// The PC a chat's last message from another device was typed on, as the hub stamped the
/// sender. The name is cleaned with [`pc_name`].
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypedOn {
    pub device_uid: String,
    pub name: String,
    /// When the message arrived (Unix ms).
    pub at: i64,
}

/// How many chats' typing PCs are remembered; the oldest go first.
const TYPED_KEPT: usize = 1024;

/// A device name as it may stand in a message line: no brackets or control characters, at most
/// 40 characters, and words of its own when nothing is left.
pub fn pc_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '[' | ']'))
        .take(40)
        .collect();
    match clean.trim() {
        "" => "another PC".to_owned(),
        t => t.to_owned(),
    }
}

/// Everything the service needs from this install. The production adapter reads the chats and
/// profiles tables and runs the chat ops; tests use [`crate::testing::FakePort`].
pub trait ChatPort: Send + Sync {
    /// This device's id and name.
    fn device(&self) -> DeviceInfo;
    /// Whether this person's other devices may run chats here.
    fn run_allowed(&self) -> bool;
    /// The hub people this PC acts for, one per linked profile, the home profile's first,
    /// with the channels it takes for each. They are announced in the `device` frame.
    fn people(&self) -> Vec<DevicePerson> {
        Vec::new()
    }
    fn hello(&self) -> BoxFut<'_, PortResult<Hello>>;
    /// The hello as hub person `person_id` sees it: their profile's project pins (and the ones
    /// pinned before profiles), not every profile's.
    fn hello_as(&self, person_id: String) -> BoxFut<'_, PortResult<Hello>> {
        let _ = person_id;
        self.hello()
    }
    /// Helm's own listeners on this PC's loopback, which are never forwarded to another PC.
    fn reserved_ports(&self) -> Vec<u16> {
        Vec::new()
    }
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
    /// The same page with the roll-over stack switches: forward and centred walks, and the
    /// whole-stack flag. The default answers a plain latest/before page.
    fn page_stack(
        &self,
        chat_id: String,
        before: Option<i64>,
        after: Option<i64>,
        around: Option<i64>,
        stack: bool,
        limit: usize,
    ) -> BoxFut<'_, PortResult<Value>> {
        let _ = (after, around, stack);
        self.page(chat_id, before, limit)
    }
    /// The roll-over stack of a chat, as the chats crate answers `chat_stack`. A PC that
    /// runs chats over the device link answers it; the others refuse.
    fn chat_stack(&self, _chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        Box::pin(async {
            Err(WireError::new(
                code::REFUSED,
                "No chat stacks are served here.",
            ))
        })
    }
    /// Find across a chat's whole roll-over stack, as the chats crate answers `stack_find`.
    fn stack_find(
        &self,
        _chat_id: String,
        _query: String,
        _include_activity: Option<bool>,
        _limit: Option<i64>,
    ) -> BoxFut<'_, PortResult<Value>> {
        Box::pin(async {
            Err(WireError::new(
                code::REFUSED,
                "No chat stacks are served here.",
            ))
        })
    }
    /// Record a remote send; `false` when this `client_id` was seen before.
    fn claim_send(&self, client_id: &str, chat_id: &str) -> PortResult<bool>;
    /// Forget a claim whose send was refused, so the same id can be tried again.
    fn release_send(&self, client_id: &str);
    /// Send the text to the chat as the person's own words (origin `remote`). `from` names the
    /// household member who sent it when the chat is not theirs: then it is delivered as a
    /// message from them, never as the owner's words. `device` is the PC it was typed on, as the
    /// hub stamped the sender, its name already cleaned with [`pc_name`]; the agent is told.
    /// `files` are the attachments carried with it, exactly as a local send hands them over.
    fn send(
        &self,
        chat_id: String,
        client_id: String,
        text: String,
        from: Option<String>,
        device: DeviceInfo,
        files: Vec<SentFile>,
    ) -> BoxFut<'_, PortResult<()>>;
    /// Stop the chat's turn; with `no_resend`, what the agent had not read is not sent again.
    fn stop(&self, chat_id: String, no_resend: bool) -> BoxFut<'_, PortResult<()>>;
    fn answer(&self, a: AnswerArgs) -> BoxFut<'_, PortResult<()>>;
    /// The Allow cards of a chat that wait on the person, newest checks aside: the pending ones,
    /// oldest first, as the consent store lists them.
    fn consent(&self, chat_id: String) -> BoxFut<'_, PortResult<Vec<Value>>>;
    /// Answer an Allow card from another device, naming that device for the record. The consent
    /// store decides by compare-and-set: the first answer wins, a second is refused.
    fn consent_decide(
        &self,
        a: ConsentDecideArgs,
        via: String,
        household: bool,
    ) -> BoxFut<'_, PortResult<()>>;
    /// Where a chat's Allow requests stand — id, state and outcome only, pending and recently
    /// decided. A watcher reads this to make a tool row's chip say Allowed or Refused too.
    fn consent_states(&self, chat_id: String) -> BoxFut<'_, PortResult<Vec<Value>>>;
    /// Start a chat for the local profile linked to the person; `unknown_person` if none is.
    fn create(&self, a: CreateArgs) -> BoxFut<'_, PortResult<String>>;
    fn set_model(&self, a: SetModelArgs) -> BoxFut<'_, PortResult<()>>;
    fn rename(&self, a: RenameArgs) -> BoxFut<'_, PortResult<()>>;
    fn archive(&self, a: ArchiveArgs) -> BoxFut<'_, PortResult<()>>;
    fn share(&self, a: ShareArgs) -> BoxFut<'_, PortResult<()>>;
    /// Copy this chat here. The answer is whatever this install's fork op answers with.
    fn fork(&self, a: ForkArgs) -> BoxFut<'_, PortResult<Value>>;
    fn compact(&self, chat_id: String) -> BoxFut<'_, PortResult<()>>;
    fn roll_over(&self, a: RollOverArgs) -> BoxFut<'_, PortResult<Value>>;
    /// What the chat has used, as this install's usage records say. A read.
    fn cost(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>>;
    /// Write the chat to a Markdown file on this PC and answer with where it went.
    fn export_markdown(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>>;
    /// Move, nest, pin or reorder the chat in this install's sidebar.
    fn organize(&self, a: OrganizeArgs) -> BoxFut<'_, PortResult<Value>>;
    fn mark_unread(&self, chat_id: String) -> BoxFut<'_, PortResult<()>>;
    fn set_muted(&self, a: MuteArgs) -> BoxFut<'_, PortResult<()>>;
    /// Label the chat with one of this install's profiles.
    fn move_profile(&self, a: MoveProfileArgs) -> BoxFut<'_, PortResult<()>>;
    /// The composer's permission pick, applied here for the chat.
    fn set_permission(&self, a: SetPermissionArgs) -> BoxFut<'_, PortResult<()>>;
    /// The chat's reply-length level, as the model picker reads it.
    fn verbosity_get(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>>;
    fn verbosity_set(&self, a: VerbositySetArgs) -> BoxFut<'_, PortResult<Value>>;
    /// Whether the side panel's context eye is on for the chat here.
    fn context_on(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>>;
    /// The chat's project files matching the mention query, looked up here.
    fn quick_open(&self, a: QuickOpenArgs) -> BoxFut<'_, PortResult<Value>>;
    /// The chat's project folder path, as this install sees it.
    fn chat_folder(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>>;
    /// One picture of a message: `{mime, bytes}` as the chat's own op answers, or a refusal in
    /// words when it is too large to bring across.
    fn attachment(&self, a: AttachmentGetArgs) -> BoxFut<'_, PortResult<Value>>;
    /// The pages pinned here that this person may open: `{pages}`, as the Pages list shows them.
    fn pages_list(&self, who: PageScope) -> BoxFut<'_, PortResult<Value>>;
    /// What a pinned page needs to run on another PC: `{name, title, size, files: [{path, size}], pin}`.
    fn pages_files(&self, who: PageScope, a: PageIdArgs) -> BoxFut<'_, PortResult<Value>>;
    /// One chunk of the page's document or of one file beside it: `{data, size, eof}`.
    fn pages_read(&self, who: PageScope, a: PagesReadArgs) -> BoxFut<'_, PortResult<Value>>;
    /// One side-panel read from the chat's project folder here — a file, a folder listing, a
    /// change stamp, an archive listing or a path resolution. The paths must lie inside that
    /// folder; nothing else on this PC is reachable this way.
    fn panel_read(&self, a: PanelRead) -> BoxFut<'_, PortResult<Value>>;
    /// Stage or unstage one file of the chat's repository here.
    fn panel_stage(&self, a: PanelStageArgs, stage: bool) -> BoxFut<'_, PortResult<Value>>;
    /// Whether a task board address and token are saved on this install.
    fn board_connection(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>>;
    /// Run one Tasks-view read of this install's task board.
    fn board_read(&self, a: BoardReadArgs) -> BoxFut<'_, PortResult<Value>>;
    /// Stop one of those reads that the asking view no longer waits for.
    fn board_cancel(&self, a: BoardCancelArgs) -> BoxFut<'_, PortResult<Value>>;
    /// Start loading this chat's model, as the first keystroke of a local chat does.
    fn warm(&self, chat_id: String) -> BoxFut<'_, PortResult<()>>;
    fn tool_stop(&self, a: ToolStopArgs) -> BoxFut<'_, PortResult<Value>>;
    /// Withdraw or recover one of the chat's queued messages.
    fn submission(&self, a: SubmissionArgs) -> BoxFut<'_, PortResult<()>>;
    /// What going back to a message means for a chat here.
    fn checkpoint(&self, a: CheckpointArgs) -> BoxFut<'_, PortResult<Value>>;
    /// The needs-you and flag rows of this install's chats among `chat_ids`.
    fn attention_rows(&self, chat_ids: Vec<String>) -> BoxFut<'_, PortResult<Value>>;
    /// This install's board-item links for `chat_ids`.
    fn item_links(&self, chat_ids: Vec<String>) -> BoxFut<'_, PortResult<Value>>;
    fn snooze(&self, a: SnoozeArgs) -> BoxFut<'_, PortResult<()>>;
    fn address(&self, chat_id: String) -> BoxFut<'_, PortResult<()>>;
    /// The three fields of this install's new-chat defaults that the model picker shows.
    fn defaults_get(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>>;
    /// Save those three defaults here.
    fn defaults_set(&self, a: DefaultsArgs) -> BoxFut<'_, PortResult<Value>>;
    /// Make a project here: a board when a task board is connected, else a folder.
    fn project_create(&self, a: ProjectCreateArgs) -> BoxFut<'_, PortResult<Value>>;
    /// Pin a project here, or point a board at one of this install's folder projects.
    fn project_set(&self, a: ProjectSetArgs) -> BoxFut<'_, PortResult<Value>>;
    /// [`ChatPort::project_set`] for hub person `person_id`: a pin is theirs, kept under their
    /// profile here.
    fn project_set_as(
        &self,
        person_id: String,
        a: ProjectSetArgs,
    ) -> BoxFut<'_, PortResult<Value>> {
        let _ = person_id;
        self.project_set(a)
    }
    /// The chat as other devices see it, for a push after a sidebar change. `Ok(None)` when
    /// the chat is not this install's.
    fn chat_brief(&self, chat_id: String) -> BoxFut<'_, PortResult<Option<RemoteChat>>>;
    /// A gallery or option board an agent showed in a chat here: this install's own op
    /// (`shown_gallery_get`, `shown_image_thumb`, `option_board_state` or
    /// `option_board_answer`), answered as it answers, within one frame.
    fn shown(&self, local_op: &'static str, args: Value) -> BoxFut<'_, PortResult<Value>>;
    /// One chunk of something big of a chat here: `{offset, total, mime, name, data}`. A file
    /// stays inside the chat's project folder, an output is that chat's own, and the whole is at
    /// most [`crate::wire::MAX_BLOB_BYTES`].
    fn blob_read(&self, a: BlobReadArgs) -> BoxFut<'_, PortResult<Value>>;
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Who a request comes from, as the hub's stamp says. The person names who is acting; it is not
/// a limit on what they may touch.
#[derive(Clone, Debug, PartialEq)]
enum Asker {
    /// Acting as the person this PC is itself paired with.
    Own,
    /// Acting as anyone else in the household: their hub id and name.
    Household { person_id: String, name: String },
}

/// Requests answered once are remembered, so a second answer to one is refused even in the
/// instant before the chat drops it.
const ANSWERED_KEPT: usize = 256;

/// How long an upload waits for the send that names it, and how many wait at once. Older ones
/// are dropped when a new upload starts.
const UPLOAD_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);
const UPLOADS_KEPT: usize = 32;
/// The most bytes held for uploads waiting on a send: in all, and for any one device.
const HELD_TOTAL_BYTES: usize = 100 * 1024 * 1024;
const HELD_DEVICE_BYTES: usize = 50 * 1024 * 1024;

/// One file arriving chunk by chunk, joined and kept until a send names it.
struct Upload {
    /// The device that sends it: nobody else may continue, name or take it.
    owner: String,
    chat_id: String,
    kind: String,
    name: String,
    mime: String,
    bytes: Vec<u8>,
    /// The sequence number the next chunk must carry.
    next_seq: u32,
    /// Set by the chunk that ends the file; only complete files may be sent.
    done: bool,
    touched: Instant,
}

struct Inner {
    port: Arc<dyn ChatPort>,
    link: Arc<dyn DeviceLink>,
    subs: Mutex<Subs>,
    /// Which devices the hub says are up, from its last `devices` frame.
    online: Mutex<HashMap<String, bool>>,
    answered: Mutex<VecDeque<(String, String)>>,
    /// Files in transit to chats that run here, by upload id.
    uploads: Mutex<HashMap<String, Upload>>,
    /// The PC each chat's last message from another device was typed on, until one is typed
    /// here. Kept in memory only: after a restart nothing is known.
    typed: Mutex<HashMap<String, TypedOn>>,
    events: mpsc::UnboundedSender<AppEvent>,
    /// The ports forwarded from this PC to others, and from others to this one.
    tunnels: Arc<Tunnels>,
    tunnel_line: Arc<TunnelLine>,
    _handle: Handle,
}

/// The forwards' way out: pushes of [`tunnel::SUB`] on the hub line.
struct TunnelLine {
    link: Arc<dyn DeviceLink>,
    seq: AtomicU64,
    changed: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl TunnelOut for TunnelLine {
    fn push(&self, to: &str, event: Value) -> bool {
        self.link.send(Frame::DevPush(Push {
            to: Some(to.to_owned()),
            from: None,
            sub: tunnel::SUB.to_owned(),
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            event,
        }))
    }

    fn changed(&self) {
        if let Some(f) = self.changed.get() {
            f();
        }
    }
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
        let tunnel_line = Arc::new(TunnelLine {
            link: link.clone(),
            seq: AtomicU64::new(0),
            changed: OnceLock::new(),
        });
        let tunnels = Arc::new(Tunnels::new(
            tunnel_line.clone(),
            Arc::new(LocalLoopback),
            handle.clone(),
        ));
        let inner = Arc::new(Inner {
            port,
            link: link.clone(),
            subs: Mutex::new(Subs::default()),
            online: Mutex::new(HashMap::new()),
            answered: Mutex::new(VecDeque::new()),
            uploads: Mutex::new(HashMap::new()),
            typed: Mutex::new(HashMap::new()),
            events,
            tunnels,
            tunnel_line,
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
                Frame::DevPush(p) if p.sub == tunnel::SUB => {
                    if let Some(from) = p.from.as_deref() {
                        inner.tunnels.on_push(from, &p.event);
                    }
                }
                _ => {}
            }
        }));
        handle.spawn(consume(Arc::downgrade(&inner), rx));
        Service { inner }
    }

    /// The `device` frame: who this is, whether others may run chats here, and the hub people
    /// this PC acts for.
    pub fn device_frame(&self) -> Frame {
        let d = self.inner.port.device();
        Frame::Device(DeviceFrame {
            uid: d.uid,
            name: d.name,
            run: self.inner.port.run_allowed(),
            people: self.inner.port.people(),
        })
    }

    /// Tell the hub who this device is and who it acts for. Sent after the household switches
    /// on every new line, and whenever the name, the run switch or the linked people change.
    pub fn announce(&self) {
        self.inner.link.send(self.device_frame());
    }

    /// An event this install emitted. Chat events go to the watchers that subscribed; a changed
    /// device name, run switch or set of linked profiles is announced.
    pub fn observe(&self, ev: &AppEvent) {
        if ev.kind == "message" {
            self.inner.typed_here(ev);
        }
        match ev.kind.as_str() {
            "device_changed" | "profiles_changed" => self.announce(),
            "preferences" => {
                let changed = ev
                    .fields
                    .get("changed")
                    .and_then(Value::as_object)
                    .is_some_and(|m| m.contains_key("deviceRunFromOthers"));
                if changed {
                    self.announce();
                    // Switched off: the ports forwarded from here stop with it.
                    if !self.inner.port.run_allowed() {
                        self.inner.tunnels.end_hosted();
                    }
                }
            }
            "chat" | "chat_removed" | "thread" | "pending" | "refresh" | "message" | "consent"
            | "sidebar"
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

    /// The ports forwarded from this PC to others and from others to this one.
    pub fn tunnels(&self) -> Arc<Tunnels> {
        self.inner.tunnels.clone()
    }

    /// Run `f` whenever the list of ports forwarded to this PC changes.
    pub fn on_forwards_changed(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.inner.tunnel_line.changed.set(Box::new(f));
    }

    /// The line to the hub went down: every forward, either way, ends.
    pub fn line_down(&self) {
        self.inner.tunnels.retain_peers(|_| false);
    }

    /// Whether this PC lets other devices use its chats (and forward its ports).
    pub fn run_allowed(&self) -> bool {
        self.inner.port.run_allowed()
    }

    /// Helm's own listeners here, which are never forwarded.
    pub fn reserved_ports(&self) -> Vec<u16> {
        self.inner.port.reserved_ports()
    }

    /// The PC the chat's last message was typed on when that was another device; `None` when
    /// it was typed here, or nothing is known since Helm started.
    pub fn typed_on(&self, chat_id: &str) -> Option<TypedOn> {
        lock(&self.inner.typed).get(chat_id).cloned()
    }
}

/// Where the app's event tap hands events to the service once there is one. The tap also
/// carries the mirror so a profile change re-announces the people *and* re-syncs the copies.
#[derive(Default)]
pub struct EventTap {
    service: OnceLock<Service>,
    mirror: OnceLock<crate::mirror::Mirror>,
}

impl EventTap {
    pub fn attach(&self, service: Service) {
        let _ = self.service.set(service);
    }

    /// Attach this PC's mirror so `profiles_changed` can widen or narrow the copies' read sets.
    pub fn attach_mirror(&self, mirror: crate::mirror::Mirror) {
        let _ = self.mirror.set(mirror);
    }

    pub fn observe(&self, ev: &AppEvent) {
        if let Some(s) = self.service.get() {
            s.observe(ev);
        }
        if ev.kind == "profiles_changed" {
            if let Some(m) = self.mirror.get() {
                m.people_changed();
            }
        }
    }

    pub fn service(&self) -> Option<Service> {
        self.service.get().cloned()
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
        i.on_event(&ev).await;
    }
}

impl Inner {
    fn send_outs(&self, outs: Vec<Out>) {
        for o in outs {
            self.link.send(Frame::DevPush(o.push));
        }
    }

    /// A message typed on another device has arrived for a chat here.
    fn note_typed(&self, chat_id: &str, device: &DeviceInfo) {
        let mut typed = lock(&self.typed);
        if typed.len() >= TYPED_KEPT && !typed.contains_key(chat_id) {
            let oldest = typed
                .iter()
                .min_by_key(|(_, t)| t.at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                typed.remove(&k);
            }
        }
        typed.insert(
            chat_id.to_owned(),
            TypedOn {
                device_uid: device.uid.clone(),
                name: device.name.clone(),
                at: helm_core::now_ms(),
            },
        );
    }

    /// A user row typed in this install's own composer, newer than the last message from
    /// another device, means the person is at this PC now.
    fn typed_here(&self, ev: &AppEvent) {
        let mut typed = lock(&self.typed);
        let chat_id = ev
            .fields
            .get("chatId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(since) = typed.get(chat_id).map(|t| t.at) else {
            return;
        };
        let here = ev
            .fields
            .get("rows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|r| {
                r.get("role").and_then(Value::as_str) == Some("user")
                    && r.pointer("/details/origin")
                        .and_then(Value::as_str)
                        .is_none_or(|o| o == "typed")
                    && r.get("timestamp")
                        .and_then(Value::as_i64)
                        .is_some_and(|t| t > since)
            });
        if here {
            typed.remove(chat_id);
        }
    }

    fn on_devices(&self, list: &[PeerDevice]) {
        let online: HashMap<String, bool> =
            list.iter().map(|d| (d.uid.clone(), d.online)).collect();
        // A watcher the hub no longer shows as up has its subscriptions dropped; it asks again
        // when it comes back.
        lock(&self.subs).retain_online(|uid| online.get(uid).copied().unwrap_or(false));
        // So do its forwards, both ways.
        self.tunnels
            .retain_peers(|uid| online.get(uid).copied().unwrap_or(false));
        *lock(&self.online) = online;
    }

    async fn on_event(&self, ev: &AppEvent) {
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
            // Moving, nesting, pinning or reordering a chat raises the projects crate's
            // `sidebar` event, not a `chat` one; without this the copies would keep the old
            // placement. Each moved chat is re-read and pushed whole.
            "sidebar" => {
                let ids: Vec<String> = ev
                    .fields
                    .get("chatIds")
                    .and_then(Value::as_array)
                    .map(|l| {
                        l.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut outs = Vec::new();
                for id in ids {
                    if let Ok(Some(chat)) = self.port.chat_brief(id).await {
                        outs.extend(lock(&self.subs).chat_changed(chat.to_value()));
                    }
                }
                outs
            }
            // A card appeared, closed or expired: watchers holding the chat hear the whole
            // pending set again, so any device's card shows the same list at the same time.
            "consent" if !lock(&self.subs).watching_chat(&chat_id) => Vec::new(),
            "consent" => {
                let cards = match self.port.consent(chat_id.clone()).await {
                    Ok(mut cards) => {
                        subs::fit_rows(&mut cards, CONSENT_BUDGET);
                        cards
                    }
                    Err(_) => Vec::new(),
                };
                let event = json!({ "kind": "consent", "chatId": &chat_id, "pending": cards });
                lock(&self.subs).chat_event(&chat_id, event)
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
        // Abandoned uploads go on the clock of any request, not only the next upload.
        lock(&self.uploads).retain(|_, u| u.touched.elapsed() <= UPLOAD_TTL);
        let op_name = op;
        let op = match Op::parse(op, args) {
            Ok(op) => op,
            Err(e) => return (Err(e), None),
        };
        let run = async {
            let (asker, acting) = self.asker(from)?;
            // The switch for other devices gates every op that runs or changes a chat here,
            // whoever is asking.
            if op.is_run() && !self.port.run_allowed() {
                return Err(WireError::not_allowed());
            }
            if op.is_run() {
                log::info!(
                    "devices: {op_name} from device:{} acting as {}",
                    from.name,
                    from.person.as_deref().unwrap_or("this PC's person")
                );
            }
            // A chat started from afar belongs to the acting person: the hub's verified id
            // overwrites whatever the request named.
            let op = match op {
                Op::Create(mut a) => {
                    a.person_id = acting;
                    Op::Create(a)
                }
                op => op,
            };
            self.run_parsed(from, &asker, op).await
        };
        match run.await {
            Ok((v, sub)) => (Ok(v), sub),
            Err(e) => (Err(e), None),
        }
    }

    /// Who is asking, as the hub stamped it, and their id. A PC paired with no person answers no
    /// one; a caller with no person is refused too.
    fn asker(&self, from: &Caller) -> Result<(Asker, String), WireError> {
        let Some(me) = self.link.person_id().filter(|p| !p.is_empty()) else {
            return Err(WireError::new(
                code::NOT_ALLOWED,
                "This PC is not paired with a person on the hub, so other devices can't use its chats.",
            ));
        };
        match from.person_id.as_deref().filter(|p| !p.is_empty()) {
            Some(p) if p == me => Ok((Asker::Own, p.to_owned())),
            Some(p) => Ok((
                Asker::Household {
                    person_id: p.to_owned(),
                    name: from
                        .person
                        .clone()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| "someone in the household".to_owned()),
                },
                p.to_owned(),
            )),
            None => Err(WireError::new(
                code::NOT_ALLOWED,
                "This PC only answers devices paired with a person on the hub.",
            )),
        }
    }

    async fn run_parsed(
        &self,
        from: &Caller,
        asker: &Asker,
        op: Op,
    ) -> Result<(Value, Option<String>), WireError> {
        let uid = from.uid.as_str();
        let done = |v: Value| -> Result<(Value, Option<String>), WireError> { Ok((v, None)) };
        match op {
            Op::Hello => {
                let person = from.person_id.clone().unwrap_or_default();
                done(to_value(&self.port.hello_as(person).await?)?)
            }
            Op::ChatsSub(a) => {
                {
                    let mut subs = lock(&self.subs);
                    subs.add_chats(uid);
                }
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
                {
                    let mut subs = lock(&self.subs);
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
                        let mut consent = state.consent;
                        subs::fit_rows(&mut consent, CONSENT_BUDGET);
                        Ok((
                            json!({
                                "chat": state.chat,
                                "thread": state.thread,
                                "pending": state.pending,
                                "consent": consent,
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
            Op::ChatStack(a) => done(self.port.chat_stack(a.chat_id).await?),
            Op::StackFind(a) => done(
                self.port
                    .stack_find(a.chat_id, a.query, a.include_activity, a.limit)
                    .await?,
            ),
            Op::AttachPut(a) => {
                self.attach_put(uid, a)?;
                done(json!({}))
            }
            Op::Send(a) => {
                // A household member's words on a chat that is not theirs are marked as theirs.
                let household = match asker {
                    Asker::Household { person_id, name } => {
                        let chat = self.port.chat(a.chat_id.clone()).await?.chat.to_value();
                        (!subs::owned_by(&chat, person_id)).then(|| name.clone())
                    }
                    Asker::Own => None,
                };
                done(self.send(from, a, household).await?)
            }
            Op::Stop(a) => {
                self.port.stop(a.chat_id, a.no_resend).await?;
                done(json!({}))
            }
            Op::Answer(a) => {
                self.answer(a).await?;
                done(Value::Null)
            }
            Op::ConsentDecide(a) => {
                if a.request_id.trim().is_empty()
                    || !matches!(a.decision.as_str(), "allow" | "deny")
                {
                    return Err(WireError::bad_args(
                        "A decision is allow or deny for a named request.",
                    ));
                }
                let via = device_via(from);
                // A change Helm applies itself (settings, pairing, update folder) is for this PC's
                // own person; another person in the household may answer plain cards only.
                let household = matches!(asker, Asker::Household { .. });
                self.port.consent_decide(a, via, household).await?;
                done(json!({}))
            }
            Op::ConsentStates(a) => {
                let mut states = self.port.consent_states(a.chat_id).await?;
                subs::fit_rows(&mut states, CONSENT_BUDGET);
                done(json!({ "states": states }))
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
            Op::Fork(a) => done(self.port.fork(a).await?),
            Op::Compact(a) => {
                self.port.compact(a.chat_id).await?;
                done(Value::Null)
            }
            Op::RollOver(a) => done(self.port.roll_over(a).await?),
            Op::Cost(a) => done(self.port.cost(a.chat_id).await?),
            Op::ExportMarkdown(a) => done(self.port.export_markdown(a.chat_id).await?),
            Op::Organize(a) => done(self.port.organize(a).await?),
            Op::MarkUnread(a) => {
                self.port.mark_unread(a.chat_id).await?;
                done(json!({}))
            }
            Op::Mute(a) => {
                self.port.set_muted(a).await?;
                done(json!({}))
            }
            Op::MoveProfile(a) => {
                self.port.move_profile(a).await?;
                done(json!({}))
            }
            Op::SetPermission(a) => {
                self.port.set_permission(a).await?;
                done(json!({}))
            }
            Op::VerbosityGet(a) => done(self.port.verbosity_get(a.chat_id).await?),
            Op::VerbositySet(a) => done(self.port.verbosity_set(a).await?),
            Op::ContextOn(a) => done(self.port.context_on(a.chat_id).await?),
            Op::QuickOpen(a) => done(self.port.quick_open(a).await?),
            Op::ChatFolder(a) => done(self.port.chat_folder(a.chat_id).await?),
            Op::AttachmentGet(a) => done(self.port.attachment(a).await?),
            Op::PagesList => done(self.port.pages_list(self.page_scope(asker)).await?),
            Op::PagesFiles(a) => done(self.port.pages_files(self.page_scope(asker), a).await?),
            Op::PagesRead(a) => done(self.port.pages_read(self.page_scope(asker), a).await?),
            Op::FileView(a) => done(
                self.port
                    .panel_read(PanelRead {
                        local_op: "file_read",
                        args: serde_json::to_value(&a)
                            .map_err(|e| WireError::internal(e.to_string()))?,
                    })
                    .await?,
            ),
            Op::FileStat(a) => done(
                self.port
                    .panel_read(PanelRead {
                        local_op: "file_stat",
                        args: serde_json::to_value(&a)
                            .map_err(|e| WireError::internal(e.to_string()))?,
                    })
                    .await?,
            ),
            Op::ArchiveList(a) => done(
                self.port
                    .panel_read(PanelRead {
                        local_op: "archive_list",
                        args: serde_json::to_value(&a)
                            .map_err(|e| WireError::internal(e.to_string()))?,
                    })
                    .await?,
            ),
            Op::PathResolve(a) => done(
                self.port
                    .panel_read(PanelRead {
                        local_op: "path_resolve",
                        args: serde_json::to_value(&a)
                            .map_err(|e| WireError::internal(e.to_string()))?,
                    })
                    .await?,
            ),
            Op::Changes(a) => done(
                self.port
                    .panel_read(PanelRead {
                        local_op: "changes",
                        args: json!({"chatId": a.chat_id}),
                    })
                    .await?,
            ),
            Op::Diff(a) => done(
                self.port
                    .panel_read(PanelRead {
                        local_op: "diff",
                        args: serde_json::to_value(&a)
                            .map_err(|e| WireError::internal(e.to_string()))?,
                    })
                    .await?,
            ),
            Op::Stage(a) => done(self.port.panel_stage(a, true).await?),
            Op::Unstage(a) => done(self.port.panel_stage(a, false).await?),
            Op::BoardConnection(a) => done(self.port.board_connection(a.chat_id).await?),
            Op::BoardRead(a) => done(self.port.board_read(a).await?),
            Op::BoardCancel(a) => done(self.port.board_cancel(a).await?),
            Op::Warm(a) => {
                self.port.warm(a.chat_id).await?;
                done(Value::Null)
            }
            Op::ToolStop(a) => done(self.port.tool_stop(a).await?),
            Op::Submission(a) => {
                self.port.submission(a).await?;
                done(json!({}))
            }
            Op::Checkpoint(a) => done(self.port.checkpoint(a).await?),
            Op::Attention(a) => done(self.port.attention_rows(a.chat_ids).await?),
            Op::ItemLinks(a) => done(self.port.item_links(a.chat_ids).await?),
            Op::Snooze(a) => {
                self.port.snooze(a).await?;
                done(json!({}))
            }
            Op::Address(a) => {
                self.port.address(a.chat_id).await?;
                done(json!({}))
            }
            Op::DefaultsGet(a) => done(self.port.defaults_get(a.chat_id).await?),
            Op::DefaultsSet(a) => {
                // The new-chat defaults include the default permission mode, which grants
                // authority: they are this PC's own person's pick.
                if matches!(asker, Asker::Household { .. }) {
                    return Err(WireError::not_allowed());
                }
                done(self.port.defaults_set(a).await?)
            }
            Op::ProjectCreate(a) => done(self.port.project_create(a).await?),
            Op::ProjectSet(a) => {
                let person = from.person_id.clone().unwrap_or_default();
                done(self.port.project_set_as(person, a).await?)
            }
            Op::ForwardOpen(a) => done(self.forward_open(from, asker, a).await?),
            Op::ForwardClose(ForwardCloseArgs { forward_id }) => {
                self.tunnels.host_close(uid, &forward_id);
                done(json!({}))
            }
            Op::ShownGallery(a) => done(self.port.shown("shown_gallery_get", to_value(&a)?).await?),
            Op::ShownImage(a) => done(self.port.shown("shown_image_thumb", to_value(&a)?).await?),
            Op::OptionBoard(a) => done(self.port.shown("option_board_state", to_value(&a)?).await?),
            Op::OptionBoardAnswer(a) => done(
                self.port
                    .shown("option_board_answer", to_value(&a)?)
                    .await?,
            ),
            Op::BlobRead(a) => done(self.port.blob_read(a).await?),
        }
    }

    fn page_scope(&self, asker: &Asker) -> PageScope {
        match asker {
            Asker::Own => PageScope {
                person_id: self.link.person_id().unwrap_or_default(),
            },
            Asker::Household { person_id, .. } => PageScope {
                person_id: person_id.clone(),
            },
        }
    }

    async fn page(&self, a: PageArgs) -> Result<Value, WireError> {
        let limit = a.limit.unwrap_or(MAX_PAGE_ROWS).clamp(1, MAX_PAGE_ROWS);
        let mut page = if a.stack || a.after_message_id.is_some() || a.around_message_id.is_some() {
            self.port
                .page_stack(
                    a.chat_id,
                    a.before_message_id,
                    a.after_message_id,
                    a.around_message_id,
                    a.stack,
                    limit,
                )
                .await?
        } else {
            self.port
                .page(a.chat_id, a.before_message_id, limit)
                .await?
        };
        if let Some(Value::Array(rows)) = page.get_mut("messages") {
            subs::fit_rows(rows, PUSH_ROW_BUDGET);
            // Dropping the oldest rows must not leave a boundary mark pointing at a row that
            // no longer crosses: a mark survives only while every anchor it names is kept.
            let kept: std::collections::HashSet<i64> = rows
                .iter()
                .filter_map(|r| r.get("id").and_then(Value::as_i64))
                .collect();
            if let Some(Value::Array(marks)) = page.get_mut("sessionMarks") {
                marks.retain(|m| {
                    ["aboveMessageId", "belowMessageId"].iter().all(|k| {
                        match m.get(*k).and_then(Value::as_i64) {
                            Some(id) => kept.contains(&id),
                            None => true,
                        }
                    })
                });
            }
        }
        Ok(page)
    }

    /// Join one chunk into a file on its way to a chat that runs here. The first chunk names
    /// the file; after that they must arrive in order. The bytes wait until a send names the
    /// upload id.
    fn attach_put(&self, owner: &str, a: AttachPutArgs) -> Result<(), WireError> {
        if uuid::Uuid::parse_str(&a.upload_id).is_err() {
            return Err(WireError::bad_args("uploadId must be a UUID."));
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(&a.data)
            .map_err(|_| WireError::bad_args("data must be base64."))?;
        if data.len() > MAX_CHUNK_BYTES {
            return Err(WireError::new(
                code::BAD_ARGS,
                "A chunk carries at most 640 KiB.",
            ));
        }
        let mut uploads = lock(&self.uploads);
        uploads.retain(|_, u| u.touched.elapsed() <= UPLOAD_TTL);
        if uploads.get(&a.upload_id).is_some_and(|u| u.owner != owner) {
            return Err(WireError::new(
                code::REFUSED,
                "That upload id belongs to another device.",
            ));
        }
        let held = |uploads: &HashMap<String, Upload>, own: Option<&str>| -> usize {
            uploads
                .values()
                .filter(|u| own.is_none_or(|o| u.owner == o))
                .map(|u| u.bytes.len())
                .sum()
        };
        if held(&uploads, None) + data.len() > HELD_TOTAL_BYTES
            || held(&uploads, Some(owner)) + data.len() > HELD_DEVICE_BYTES
        {
            return Err(WireError::new(
                code::REFUSED,
                "This PC is holding as many uploaded files as it can; send again in a moment.",
            ));
        }
        if a.seq == 0 {
            if !matches!(a.kind.as_str(), "image" | "file") || a.name.trim().is_empty() {
                return Err(WireError::bad_args(
                    "The first chunk names the file and whether it is an image or a file.",
                ));
            }
            // A full house drops the oldest of this device's own uploads, never another's.
            if uploads.values().filter(|u| u.owner == owner).count() >= UPLOADS_KEPT {
                let oldest = uploads
                    .iter()
                    .filter(|(_, u)| u.owner == owner)
                    .min_by_key(|(_, u)| u.touched)
                    .map(|(k, _)| k.clone());
                if let Some(k) = oldest {
                    uploads.remove(&k);
                }
            }
            uploads.insert(
                a.upload_id.clone(),
                Upload {
                    owner: owner.to_owned(),
                    chat_id: a.chat_id.clone(),
                    kind: a.kind.clone(),
                    name: a.name.clone(),
                    mime: a.mime.clone(),
                    bytes: data,
                    next_seq: 1,
                    done: a.eof,
                    touched: Instant::now(),
                },
            );
            return Ok(());
        }
        let Some(u) = uploads.get_mut(&a.upload_id) else {
            return Err(WireError::new(
                code::REFUSED,
                "That upload was not started, or it has been dropped; start it again.",
            ));
        };
        if u.chat_id != a.chat_id {
            return Err(WireError::bad_args(
                "That upload id belongs to another chat.",
            ));
        }
        if u.done {
            return Err(WireError::new(
                code::REFUSED,
                "That upload is already complete.",
            ));
        }
        if a.seq != u.next_seq {
            return Err(WireError::new(
                code::REFUSED,
                "Chunks must arrive in order.",
            ));
        }
        if u.bytes.len() + data.len() > MAX_UPLOAD_BYTES {
            uploads.remove(&a.upload_id);
            return Err(WireError::new(
                code::REFUSED,
                "A file sent from another device is at most 25 MiB.",
            ));
        }
        u.bytes.extend_from_slice(&data);
        u.next_seq += 1;
        u.done = a.eof;
        u.touched = Instant::now();
        Ok(())
    }

    /// The bytes an earlier run of `attach_put` calls left for this send, in the order it
    /// names them. A missing or unfinished upload is refused: the sender starts it again.
    fn take_files(
        &self,
        owner: &str,
        chat_id: &str,
        refs: &[AttachmentRef],
    ) -> Result<Vec<SentFile>, WireError> {
        if refs.len() > MAX_SEND_ATTACHMENTS {
            return Err(WireError::new(
                code::BAD_ARGS,
                "A message can carry at most 20 attachments.",
            ));
        }
        let mut files = Vec::with_capacity(refs.len());
        if refs.is_empty() {
            return Ok(files);
        }
        let uploads = lock(&self.uploads);
        for r in refs {
            let bad = || {
                WireError::new(
                    code::REFUSED,
                    "A file did not finish uploading; send again.",
                )
            };
            let u = uploads.get(&r.upload_id).ok_or_else(bad)?;
            if u.owner != owner || u.chat_id != chat_id || !u.done {
                return Err(bad());
            }
            files.push(SentFile {
                kind: u.kind.clone(),
                name: u.name.clone(),
                mime: u.mime.clone(),
                bytes: u.bytes.clone(),
            });
        }
        Ok(files)
    }

    /// Forward a port of this PC to the asking one. The chat must be here and, for someone
    /// other than this PC's own person, theirs or shared with them; the port policy and the
    /// check that something answers are the tunnel's.
    async fn forward_open(
        &self,
        from: &Caller,
        asker: &Asker,
        a: ForwardOpenArgs,
    ) -> Result<Value, WireError> {
        let chat = self
            .port
            .chat_brief(a.chat_id.clone())
            .await?
            .ok_or_else(WireError::not_found)?
            .to_value();
        if let Asker::Household { person_id, .. } = asker {
            let shared = chat
                .get("sharedWith")
                .and_then(Value::as_array)
                .is_some_and(|l| l.iter().any(|p| p.as_str() == Some(person_id)));
            if !subs::owned_by(&chat, person_id) && !shared {
                return Err(WireError::new(
                    code::REFUSED,
                    "Only the person this chat belongs to, or someone it is shared with, can open its ports from another PC.",
                ));
            }
        }
        let id = self
            .tunnels
            .host_open(&from.uid, &a.chat_id, a.port, &self.port.reserved_ports())
            .await?;
        Ok(json!({ "forwardId": id }))
    }

    async fn send(
        &self,
        sender: &Caller,
        a: SendArgs,
        from: Option<String>,
    ) -> Result<Value, WireError> {
        let owner = sender.uid.as_str();
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
        let files = self.take_files(owner, &a.chat_id, &a.attachments)?;
        if !self.port.claim_send(&a.client_id, &a.chat_id)? {
            // A repeat spends nothing, so the uploads it named are let go at once.
            let mut uploads = lock(&self.uploads);
            for r in &a.attachments {
                uploads.remove(&r.upload_id);
            }
            return Ok(json!({ "accepted": true, "duplicate": true }));
        }
        // Both modes are delivered the way the PC delivers any message it did not type itself:
        // an idle chat starts a turn, a working one takes it in at its next tool boundary.
        let device = DeviceInfo {
            uid: sender.uid.clone(),
            name: pc_name(&sender.name),
        };
        let chat_id = a.chat_id.clone();
        let sent = self
            .port
            .send(
                a.chat_id,
                a.client_id.clone(),
                a.text,
                from,
                device.clone(),
                files,
            )
            .await;
        match sent {
            Ok(()) => {
                self.note_typed(&chat_id, &device);
                // The uploads are spent only once the send went through; a refused one can be
                // tried again with the same ids.
                let mut uploads = lock(&self.uploads);
                for r in &a.attachments {
                    uploads.remove(&r.upload_id);
                }
                Ok(json!({ "accepted": true }))
            }
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

/// Where an Allow card was answered from, for the consent record: the asking device by name.
fn device_via(from: &Caller) -> String {
    let name: String = from
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    format!("device:{}", name.trim())
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

/// Whether the answer is one the watcher may give for the request it waits on. Every card that
/// waits on the person — questions, plan approvals, permission and trust prompts, secret input —
/// can be answered from any device that has the chat open; the gates are the device switches, not
/// the kind of card. The label must be one the request shows, and what the answer says must agree
/// with it, so a mistaken or doctored answer cannot approve what the person did not see.
pub fn check_answer(i: &Interaction, a: &AnswerArgs) -> Result<(), WireError> {
    let mismatch = || WireError::new(code::REFUSED, "That is not what the request shows.");
    // Cancelling approves nothing.
    if a.cancelled == Some(true) {
        return Ok(());
    }
    // Going to another step of a question approves nothing either; it must be one of its steps.
    if let Some(step) = a.goto {
        let question = matches!(i.method, InteractionMethod::Select)
            && i.attention != Some(InteractionAttention::Plan);
        return if question && i.steps.is_some_and(|n| step < n) {
            Ok(())
        } else {
            Err(mismatch())
        };
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
