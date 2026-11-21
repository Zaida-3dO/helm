//! The device wire over the hub line.
//!
//! [`HubLink`] is the production [`DeviceLink`]: frames the device crate sends go up the line the
//! relay client keeps open, and the `devices`, `dev_call`, `dev_result` and `dev_push` frames the
//! hub sends come back down to whoever subscribed. A frame is sent only while the line is up and
//! is never queued for a later line, so a request that cannot go fails at once and says so.
//!
//! [`attach`] wires the host side: it builds the production chat port, starts the service that
//! answers other devices (and carries the ports forwarded between PCs), hands it the app's event
//! tap, and has the device announce itself after the household switches on every new line.

use crate::client::{Client, DeviceSink, LineHandle, LineState};
use crate::{telegram, voice};
use helm_chats::ChatRuntime;
use helm_devices::host::HostPort;
use helm_devices::link::{BoxFut, DeviceLink, FrameCb};
use helm_devices::serve::{ChatPort, ChatState, PortResult, SentFile};
use helm_devices::wire::{
    AnswerArgs, ArchiveArgs, ConsentDecideArgs, CreateArgs, DeviceInfo, DevicePerson, ForkArgs,
    Frame, Hello, MoveProfileArgs, MuteArgs, OrganizeArgs, RemoteChat, RenameArgs, RollOverArgs,
    SetModelArgs, ShareArgs,
};
use helm_devices::{EventTap, Service};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type Hook = Arc<dyn Fn() + Send + Sync>;
type Cb = Arc<dyn Fn(Frame) + Send + Sync>;
type PersonFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;
type PeopleFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub struct HubLink {
    line: Arc<LineHandle>,
    /// Reads the hub person this machine is paired as from the saved connection.
    read_person: PersonFn,
    /// Reads the hub people of this install's linked profiles, the home profile's first: who
    /// this PC acts for.
    read_people: PeopleFn,
    /// That person, read when the link was made and again on every new line (a new pairing
    /// always brings a new line).
    person: Mutex<Option<String>>,
    // Callbacks are kept as `Arc`s and copied out before they run, so one that calls back into
    // the link (to subscribe, or to send) never finds its locks held.
    frames: Mutex<Vec<Cb>>,
    ups: Mutex<Vec<Hook>>,
    downs: Mutex<Vec<Hook>>,
}

impl HubLink {
    pub fn new(
        line: Arc<LineHandle>,
        read_person: PersonFn,
        read_people: PeopleFn,
    ) -> Arc<HubLink> {
        let person = read_person();
        Arc::new(HubLink {
            line,
            read_person,
            read_people,
            person: Mutex::new(person),
            frames: Mutex::new(Vec::new()),
            ups: Mutex::new(Vec::new()),
            downs: Mutex::new(Vec::new()),
        })
    }

    /// Run `hook` whenever a new line comes up, after the household switches went up.
    pub fn on_up(&self, hook: impl Fn() + Send + Sync + 'static) {
        lock(&self.ups).push(Arc::new(hook));
    }

    /// Run `hook` whenever the line goes down.
    pub fn on_down(&self, hook: impl Fn() + Send + Sync + 'static) {
        lock(&self.downs).push(Arc::new(hook));
    }
}

impl DeviceLink for HubLink {
    fn send(&self, frame: Frame) -> bool {
        self.line.device_frame(frame.to_value())
    }

    fn subscribe(&self, cb: FrameCb) {
        lock(&self.frames).push(Arc::from(cb));
    }

    fn up(&self) -> bool {
        self.line.state() == LineState::Online
    }

    fn person_id(&self) -> Option<String> {
        lock(&self.person).clone()
    }

    /// The hub people this PC acts for, read from the profiles table as it is now — a profile
    /// linked or removed since the line came up counts at once. With no profiles table yet the
    /// line's own person is all there is, as before.
    fn people(&self) -> Vec<String> {
        let people = (self.read_people)();
        if people.is_empty() {
            self.person_id().into_iter().collect()
        } else {
            people
        }
    }
}

impl DeviceSink for HubLink {
    fn frame(&self, frame: Value) {
        // A frame of a type this build does not know is ignored, as an old Helm ignores ours.
        let Some(f) = Frame::from_value(frame) else {
            return;
        };
        let cbs = lock(&self.frames).clone();
        for cb in cbs {
            cb(f.clone());
        }
    }

    fn line_up(&self) {
        let person = (self.read_person)();
        *lock(&self.person) = person;
        let hooks = lock(&self.ups).clone();
        for h in hooks {
            h();
        }
    }

    fn line_down(&self) {
        let hooks = lock(&self.downs).clone();
        for h in hooks {
            h();
        }
    }
}

/// The host port with this crate's channel claims bolted on. The plain port reads the profiles
/// table only, so its Telegram and voice claims are the defaults (the home profile's on, the
/// others' off); this answers `people` from each profile's own policy files, which is what the
/// switches in Settings → Hub actually set. Everything else goes straight through.
struct ChannelPort {
    port: Arc<HostPort>,
    db: Arc<helm_core::db::Database>,
    profile_dir: PathBuf,
}

impl ChatPort for ChannelPort {
    fn people(&self) -> Vec<DevicePerson> {
        let rows = match self
            .db
            .snapshot(|tx| helm_profiles::store::linked_claims(tx))
        {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        crate::people::migrate(&self.profile_dir, &self.db);
        rows.into_iter()
            .map(|(profile_id, person_id, is_home)| {
                let tg = crate::people::telegram_paths(&self.profile_dir, &profile_id);
                let vo = crate::people::voice_paths(&self.profile_dir, &profile_id);
                DevicePerson {
                    person_id,
                    telegram: telegram::takes_telegram(&telegram::load_file(&tg.policy), is_home),
                    voice: voice::takes_voice(&voice::load_file(&vo.policy), is_home),
                }
            })
            .collect()
    }

    fn device(&self) -> DeviceInfo {
        self.port.device()
    }
    fn run_allowed(&self) -> bool {
        self.port.run_allowed()
    }
    fn hello(&self) -> BoxFut<'_, PortResult<Hello>> {
        self.port.hello()
    }
    fn hello_as(&self, person_id: String) -> BoxFut<'_, PortResult<Hello>> {
        self.port.hello_as(person_id)
    }
    fn reserved_ports(&self) -> Vec<u16> {
        self.port.reserved_ports()
    }
    fn remote_chat(&self, record: helm_chats::types::ChatRecord) -> RemoteChat {
        self.port.remote_chat(record)
    }
    fn chats(&self, since: i64) -> BoxFut<'_, PortResult<(Vec<RemoteChat>, i64)>> {
        self.port.chats(since)
    }
    fn chat(&self, chat_id: String) -> BoxFut<'_, PortResult<ChatState>> {
        self.port.chat(chat_id)
    }
    fn rows_since(
        &self,
        chat_id: String,
        since: i64,
        limit: i64,
    ) -> BoxFut<'_, PortResult<(Vec<helm_chats::types::MessageRow>, bool)>> {
        self.port.rows_since(chat_id, since, limit)
    }
    fn page(
        &self,
        chat_id: String,
        before: Option<i64>,
        limit: usize,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.page(chat_id, before, limit)
    }
    fn claim_send(&self, client_id: &str, chat_id: &str) -> PortResult<bool> {
        self.port.claim_send(client_id, chat_id)
    }
    fn release_send(&self, client_id: &str) {
        self.port.release_send(client_id)
    }
    fn send(
        &self,
        chat_id: String,
        client_id: String,
        text: String,
        from: Option<String>,
        device: DeviceInfo,
        files: Vec<SentFile>,
    ) -> BoxFut<'_, PortResult<()>> {
        self.port
            .send(chat_id, client_id, text, from, device, files)
    }
    fn stop(&self, chat_id: String, no_resend: bool) -> BoxFut<'_, PortResult<()>> {
        self.port.stop(chat_id, no_resend)
    }
    fn answer(&self, a: AnswerArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.answer(a)
    }
    fn consent(&self, chat_id: String) -> BoxFut<'_, PortResult<Vec<Value>>> {
        self.port.consent(chat_id)
    }
    fn consent_decide(
        &self,
        a: ConsentDecideArgs,
        via: String,
        household: bool,
    ) -> BoxFut<'_, PortResult<()>> {
        self.port.consent_decide(a, via, household)
    }
    fn consent_states(&self, chat_id: String) -> BoxFut<'_, PortResult<Vec<Value>>> {
        self.port.consent_states(chat_id)
    }
    fn create(&self, a: CreateArgs) -> BoxFut<'_, PortResult<String>> {
        self.port.create(a)
    }
    fn set_model(&self, a: SetModelArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.set_model(a)
    }
    fn rename(&self, a: RenameArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.rename(a)
    }
    fn archive(&self, a: ArchiveArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.archive(a)
    }
    fn share(&self, a: ShareArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.share(a)
    }
    fn fork(&self, a: ForkArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.fork(a)
    }
    fn compact(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.port.compact(chat_id)
    }
    fn roll_over(&self, a: RollOverArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.roll_over(a)
    }
    fn cost(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.port.cost(chat_id)
    }
    fn export_markdown(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.port.export_markdown(chat_id)
    }
    fn organize(&self, a: OrganizeArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.organize(a)
    }
    fn mark_unread(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.port.mark_unread(chat_id)
    }
    fn set_muted(&self, a: MuteArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.set_muted(a)
    }
    fn move_profile(&self, a: MoveProfileArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.move_profile(a)
    }
    fn set_permission(
        &self,
        a: helm_devices::wire::SetPermissionArgs,
    ) -> BoxFut<'_, PortResult<()>> {
        self.port.set_permission(a)
    }
    fn verbosity_get(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.port.verbosity_get(chat_id)
    }
    fn verbosity_set(
        &self,
        a: helm_devices::wire::VerbositySetArgs,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.verbosity_set(a)
    }
    fn context_on(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.port.context_on(chat_id)
    }
    fn quick_open(&self, a: helm_devices::wire::QuickOpenArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.quick_open(a)
    }
    fn chat_folder(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.port.chat_folder(chat_id)
    }
    fn panel_read(&self, a: helm_devices::serve::PanelRead) -> BoxFut<'_, PortResult<Value>> {
        self.port.panel_read(a)
    }
    fn attachment(
        &self,
        a: helm_devices::wire::AttachmentGetArgs,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.attachment(a)
    }
    fn pages_list(&self, who: helm_devices::serve::PageScope) -> BoxFut<'_, PortResult<Value>> {
        self.port.pages_list(who)
    }
    fn pages_files(
        &self,
        who: helm_devices::serve::PageScope,
        a: helm_devices::wire::PageIdArgs,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.pages_files(who, a)
    }
    fn pages_read(
        &self,
        who: helm_devices::serve::PageScope,
        a: helm_devices::wire::PagesReadArgs,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.pages_read(who, a)
    }
    fn panel_stage(
        &self,
        a: helm_devices::wire::PanelStageArgs,
        stage: bool,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.panel_stage(a, stage)
    }
    fn board_connection(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.port.board_connection(chat_id)
    }
    fn board_read(&self, a: helm_devices::wire::BoardReadArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.board_read(a)
    }
    fn board_cancel(
        &self,
        a: helm_devices::wire::BoardCancelArgs,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.board_cancel(a)
    }
    fn warm(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.port.warm(chat_id)
    }
    fn tool_stop(&self, a: helm_devices::wire::ToolStopArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.tool_stop(a)
    }
    fn submission(&self, a: helm_devices::wire::SubmissionArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.submission(a)
    }
    fn checkpoint(&self, a: helm_devices::wire::CheckpointArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.checkpoint(a)
    }
    fn attention_rows(&self, chat_ids: Vec<String>) -> BoxFut<'_, PortResult<Value>> {
        self.port.attention_rows(chat_ids)
    }
    fn item_links(&self, chat_ids: Vec<String>) -> BoxFut<'_, PortResult<Value>> {
        self.port.item_links(chat_ids)
    }
    fn snooze(&self, a: helm_devices::wire::SnoozeArgs) -> BoxFut<'_, PortResult<()>> {
        self.port.snooze(a)
    }
    fn address(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.port.address(chat_id)
    }
    fn defaults_get(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.port.defaults_get(chat_id)
    }
    fn defaults_set(&self, a: helm_devices::wire::DefaultsArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.defaults_set(a)
    }
    fn project_create(
        &self,
        a: helm_devices::wire::ProjectCreateArgs,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.project_create(a)
    }
    fn project_set(&self, a: helm_devices::wire::ProjectSetArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.project_set(a)
    }
    fn project_set_as(
        &self,
        person_id: String,
        a: helm_devices::wire::ProjectSetArgs,
    ) -> BoxFut<'_, PortResult<Value>> {
        self.port.project_set_as(person_id, a)
    }
    fn chat_brief(&self, chat_id: String) -> BoxFut<'_, PortResult<Option<RemoteChat>>> {
        self.port.chat_brief(chat_id)
    }
    fn shown(&self, local_op: &'static str, args: Value) -> BoxFut<'_, PortResult<Value>> {
        self.port.shown(local_op, args)
    }
    fn blob_read(&self, a: helm_devices::wire::BlobReadArgs) -> BoxFut<'_, PortResult<Value>> {
        self.port.blob_read(a)
    }
}

/// Start answering other devices: needs the relay client, the app's event tap and the chat
/// runtime among the services, and the op dispatcher for what changes a chat.
pub fn attach(dispatcher: &Arc<helm_ipc::Dispatcher>) {
    let services = &dispatcher.ctx().services;
    let (Some(client), Some(tap), Some(rt)) = (
        services.get::<Client>(),
        services.get::<EventTap>(),
        services.get::<ChatRuntime>(),
    ) else {
        return;
    };
    let line = client.handle();
    let reader = client.clone();
    let read_people: PeopleFn = match services.get::<helm_core::db::Database>() {
        Some(db) => {
            let db = db.clone();
            Arc::new(move || {
                db.snapshot(|tx| helm_profiles::store::linked_person_ids(tx))
                    .unwrap_or_default()
            }) as PeopleFn
        }
        None => Arc::new(Vec::new),
    };
    let link = HubLink::new(
        line.clone(),
        Arc::new(move || reader.paired_person()),
        read_people,
    );
    let port: Arc<dyn ChatPort> = match (
        services.get::<helm_core::db::Database>(),
        services.get::<helm_core::profile::Profile>(),
    ) {
        (Some(db), Some(prof)) => Arc::new(ChannelPort {
            port: HostPort::new(dispatcher.clone()),
            db: db.clone(),
            profile_dir: prof.dir.clone(),
        }),
        // Without the profiles table the plain port answers with the old single-person claim.
        _ => HostPort::new(dispatcher.clone()),
    };
    let service = Service::new(port, link.clone(), rt.handle().clone());
    let announce = service.clone();
    link.on_up(move || announce.announce());
    // Ports forwarded either way end with the line; the window hears when its list changes.
    let down = service.clone();
    link.on_down(move || down.line_down());
    let events = dispatcher.ctx().events.clone();
    service.on_forwards_changed(move || {
        events.emit(helm_ipc::AppEvent::new("devices_forwards_changed"))
    });
    // The watching side: this PC's copy of the other devices' chats, and the routing of the
    // chat ops that name one.
    if let (Some(db), Some(slot), Some(routes)) = (
        services.get::<helm_core::db::Database>(),
        services.get::<helm_devices::MirrorSlot>(),
        services.get::<helm_chats::remote::RemoteSlot>(),
    ) {
        let mirror = helm_devices::Mirror::new(
            db,
            link.clone(),
            dispatcher.ctx().events.clone(),
            rt.handle().clone(),
            services.get::<helm_devices::PeopleSync>(),
        );
        let up = mirror.clone();
        link.on_up(move || up.line_up());
        let down = mirror.clone();
        link.on_down(move || down.line_down());
        routes.attach(Arc::new(mirror.clone()));
        tap.attach_mirror(mirror.clone());
        slot.attach(mirror);
    }
    tap.attach(service.clone());
    client.attach_devices(link);
    // A line that came up before this ran has already passed its announcement.
    if line.state() == LineState::Online {
        service.announce();
    }
}
