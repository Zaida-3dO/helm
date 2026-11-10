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
use helm_chats::ChatRuntime;
use helm_devices::host::HostPort;
use helm_devices::link::{DeviceLink, FrameCb};
use helm_devices::wire::Frame;
use helm_devices::{EventTap, Service};
use serde_json::Value;
use std::sync::{Arc, Mutex};

type Hook = Box<dyn Fn() + Send + Sync>;
type PersonFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub struct HubLink {
    line: Arc<LineHandle>,
    /// Reads the hub person this machine is paired as from the saved connection.
    read_person: PersonFn,
    /// That person, read when the link was made and again on every new line (a new pairing
    /// always brings a new line).
    person: Mutex<Option<String>>,
    frames: Mutex<Vec<FrameCb>>,
    ups: Mutex<Vec<Hook>>,
    downs: Mutex<Vec<Hook>>,
}

impl HubLink {
    pub fn new(line: Arc<LineHandle>, read_person: PersonFn) -> Arc<HubLink> {
        let person = read_person();
        Arc::new(HubLink {
            line,
            read_person,
            person: Mutex::new(person),
            frames: Mutex::new(Vec::new()),
            ups: Mutex::new(Vec::new()),
            downs: Mutex::new(Vec::new()),
        })
    }

    /// Run `hook` whenever a new line comes up, after the household switches went up.
    pub fn on_up(&self, hook: impl Fn() + Send + Sync + 'static) {
        lock(&self.ups).push(Box::new(hook));
    }

    /// Run `hook` whenever the line goes down.
    pub fn on_down(&self, hook: impl Fn() + Send + Sync + 'static) {
        lock(&self.downs).push(Box::new(hook));
    }
}

impl DeviceLink for HubLink {
    fn send(&self, frame: Frame) -> bool {
        self.line.device_frame(frame.to_value())
    }

    fn subscribe(&self, cb: FrameCb) {
        lock(&self.frames).push(cb);
    }

    fn up(&self) -> bool {
        self.line.state() == LineState::Online
    }

    fn person_id(&self) -> Option<String> {
        lock(&self.person).clone()
    }
}

impl DeviceSink for HubLink {
    fn frame(&self, frame: Value) {
        // A frame of a type this build does not know is ignored, as an old Helm ignores ours.
        let Some(f) = Frame::from_value(frame) else {
            return;
        };
        for cb in lock(&self.frames).iter() {
            cb(f.clone());
        }
    }

    fn line_up(&self) {
        *lock(&self.person) = (self.read_person)();
        for h in lock(&self.ups).iter() {
            h();
        }
    }

    fn line_down(&self) {
        for h in lock(&self.downs).iter() {
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
    let link = HubLink::new(line.clone(), Arc::new(move || reader.paired_person()));
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
