//! The device wire over the hub line.
//!
//! [`HubLink`] is the production [`DeviceLink`]: frames the device crate sends go up the line the
//! relay client keeps open, and the `devices`, `dev_call`, `dev_result` and `dev_push` frames the
//! hub sends come back down to whoever subscribed. A frame is sent only while the line is up and
//! is never queued for a later line, so a request that cannot go fails at once and says so.
//!
//! [`attach`] wires the host side: it builds the production chat port, starts the service that
//! answers other devices, hands it the app's event tap, and has the device announce itself after
//! the household switches on every new line.

use crate::client::{Client, DeviceSink, LineHandle, LineState};
use crate::{telegram, voice};
use helm_chats::ChatRuntime;
use helm_devices::host::HostPort;
use helm_devices::link::{BoxFut, DeviceLink, FrameCb};
use helm_devices::serve::{ChatPort, ChatState, PortResult};
use helm_devices::wire::{
    AnswerArgs, ArchiveArgs, CreateArgs, DeviceInfo, DevicePerson, Frame, Hello, RenameArgs,
    RemoteChat, SetModelArgs, ShareArgs,
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
    pub fn new(line: Arc<LineHandle>, read_person: PersonFn, read_people: PeopleFn) -> Arc<HubLink> {
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
    let service = Service::new(
        HostPort::new(dispatcher.clone()),
        link.clone(),
        rt.handle().clone(),
    );
    let announce = service.clone();
    link.on_up(move || announce.announce());
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
        slot.attach(mirror);
    }
    tap.attach(service.clone());
    client.attach_devices(link);
    // A line that came up before this ran has already passed its announcement.
    if line.state() == LineState::Online {
        service.announce();
    }
}
