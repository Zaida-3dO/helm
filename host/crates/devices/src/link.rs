//! The line to the other devices, as the rest of this crate sees it.
//!
//! [`DeviceLink`] is the seam: the relay line is the production implementation (it carries these
//! frames to the hub and back), and [`MemRouter`] is an in-process stand-in for the hub that tests
//! use, so nothing here ever touches a network. [`Rpc`] turns the link's request and result frames
//! into an awaitable call.

use crate::wire::{
    code, iso_utc, Caller, DeviceFrame, Frame, PeerDevice, WireError, MAX_FRAME_BYTES,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Called for every frame the hub sends (`devices`, `dev_call`, `dev_result`, `dev_push`), in
/// order, one at a time.
pub type FrameCb = Box<dyn Fn(Frame) + Send + Sync>;

pub trait DeviceLink: Send + Sync {
    /// Send a frame up. `false` when the line is down: nothing was sent.
    fn send(&self, frame: Frame) -> bool;
    /// Hear every frame that comes down.
    fn subscribe(&self, cb: FrameCb);
    /// Whether the line to the hub is up.
    fn up(&self) -> bool;
    /// The hub person this machine is paired as, if it is paired with one. A request from a
    /// device of another person is a household request, scoped to that person's own chats.
    fn person_id(&self) -> Option<String>;
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Rpc
// ---------------------------------------------------------------------------------------------

/// How long a request waits before it gives up. The hub answers `timeout` after 30 seconds, so
/// this only matters when the hub itself went quiet.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(32);

type Waiting = oneshot::Sender<Result<Value, WireError>>;
type Arrival = Box<dyn FnOnce() + Send>;

struct Pend {
    tx: Waiting,
    /// Run on the link's own task the moment the result arrives, before whatever follows it on
    /// the line is heard: it marks where in the stream of frames the result fell.
    arrival: Option<Arrival>,
}

/// Requests to other devices and the results that come back for them.
pub struct Rpc {
    link: Arc<dyn DeviceLink>,
    pending: Mutex<HashMap<String, Pend>>,
}

impl Rpc {
    pub fn new(link: Arc<dyn DeviceLink>) -> Arc<Rpc> {
        let rpc = Arc::new(Rpc {
            link: link.clone(),
            pending: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&rpc);
        link.subscribe(Box::new(move |f| {
            if let (
                Some(rpc),
                Frame::DevResult {
                    id,
                    ok,
                    result,
                    error,
                },
            ) = (weak.upgrade(), f)
            {
                let r = if ok {
                    Ok(result.unwrap_or(Value::Null))
                } else {
                    Err(error.unwrap_or_else(|| WireError::internal("The device did not say why.")))
                };
                let found = lock(&rpc.pending).remove(&id);
                if let Some(p) = found {
                    if let Some(arrived) = p.arrival {
                        arrived();
                    }
                    let _ = p.tx.send(r);
                }
            }
        }));
        rpc
    }

    /// Ask the device `to` to run `op`. Fails at once while the line is down, and with the
    /// hub's `offline`, `timeout`, `scope` or `too_large` or the device's own error otherwise.
    pub async fn call(&self, to: &str, op: &str, args: Value) -> Result<Value, WireError> {
        self.request(to, op, args, None).await
    }

    /// As [`Rpc::call`], and `arrival` runs the moment the result comes down the line, before
    /// any frame that follows it is heard (a subscription's first news comes right behind its
    /// result, so the caller can tell which news belongs to it).
    pub async fn call_then(
        &self,
        to: &str,
        op: &str,
        args: Value,
        arrival: impl FnOnce() + Send + 'static,
    ) -> Result<Value, WireError> {
        self.request(to, op, args, Some(Box::new(arrival))).await
    }

    async fn request(
        &self,
        to: &str,
        op: &str,
        args: Value,
        arrival: Option<Arrival>,
    ) -> Result<Value, WireError> {
        if !self.link.up() {
            return Err(WireError::new(
                code::OFFLINE,
                "Helm is not connected to the hub.",
            ));
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id.clone(), Pend { tx, arrival });
        let sent = self.link.send(Frame::Dev {
            id: id.clone(),
            to: to.to_owned(),
            op: op.to_owned(),
            args,
        });
        if !sent {
            lock(&self.pending).remove(&id);
            return Err(WireError::new(
                code::OFFLINE,
                "Helm is not connected to the hub.",
            ));
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(WireError::new(
                code::OFFLINE,
                "The hub line dropped before the answer came.",
            )),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(WireError::new(
                    code::TIMEOUT,
                    "The device did not answer in time.",
                ))
            }
        }
    }

    /// The line is gone: every request still waiting fails now.
    pub fn fail_all(&self, e: WireError) {
        let waiting: Vec<Pend> = lock(&self.pending).drain().map(|(_, p)| p).collect();
        for p in waiting {
            let _ = p.tx.send(Err(e.clone()));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// MemRouter
// ---------------------------------------------------------------------------------------------

/// How long the stand-in hub waits for a device to answer.
pub const ROUTER_TIMEOUT: Duration = Duration::from_secs(30);

struct Entry {
    name: String,
    person_id: Option<String>,
    person: Option<String>,
    peer: bool,
    run: bool,
    /// The device has said who it is.
    known: bool,
    up: bool,
    last_seen_ms: Option<i64>,
    tx: mpsc::UnboundedSender<Frame>,
}

#[derive(Default)]
struct State {
    links: BTreeMap<String, Entry>,
    /// Requests in flight: the hub's id → (the asking device, the asker's own id).
    calls: HashMap<String, (String, String)>,
    next: u64,
}

impl State {
    fn deliver(&self, uid: &str, frame: Frame) {
        if let Some(e) = self.links.get(uid).filter(|e| e.up) {
            let _ = e.tx.send(frame);
        }
    }

    fn broadcast(&self) {
        for (uid, e) in &self.links {
            if e.peer && e.known && e.up {
                let list = self
                    .links
                    .iter()
                    .filter(|(_, o)| o.known)
                    .map(|(ouid, o)| PeerDevice {
                        uid: ouid.clone(),
                        name: o.name.clone(),
                        person_id: o.person_id.clone(),
                        person: o.person.clone(),
                        online: o.up,
                        last_seen_at: o.last_seen_ms.map(iso_utc),
                        run: o.run,
                        is_self: ouid == uid,
                    })
                    .collect();
                let _ = e.tx.send(Frame::Devices { list });
            }
        }
    }

    fn fail(&self, to: &str, id: &str, code: &str, message: &str) {
        self.deliver(to, Frame::result(id, Err(WireError::new(code, message))));
    }
}

/// An in-process stand-in for the hub: it knows the devices that connect to it, tells the ones
/// allowed to use the household who is there, routes requests and their results by device id, and
/// gives the same errors the hub gives (`offline`, `timeout`, `scope`, `too_large`).
#[derive(Clone, Default)]
pub struct MemRouter(Arc<Mutex<State>>);

impl MemRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect a device. `peer` says whether its line may use the household. It is up at once
    /// but unknown to the others until it sends its `device` frame. Needs a running tokio
    /// runtime: frames reach the device on a task.
    pub fn connect(
        &self,
        uid: &str,
        person_id: Option<&str>,
        person: Option<&str>,
        peer: bool,
    ) -> Arc<MemLink> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        let cbs: Arc<Mutex<Vec<FrameCb>>> = Arc::default();
        {
            let mut st = lock(&self.0);
            st.links.insert(
                uid.to_owned(),
                Entry {
                    name: uid.to_owned(),
                    person_id: person_id.map(str::to_owned),
                    person: person.map(str::to_owned),
                    peer,
                    run: false,
                    known: false,
                    up: true,
                    last_seen_ms: None,
                    tx,
                },
            );
        }
        let cbs2 = cbs.clone();
        tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                for cb in lock(&cbs2).iter() {
                    cb(f.clone());
                }
            }
        });
        Arc::new(MemLink {
            router: self.clone(),
            uid: uid.to_owned(),
            person_id: person_id.map(str::to_owned),
            cbs,
        })
    }

    fn handle(&self, from: &str, frame: Frame) -> bool {
        let mut st = lock(&self.0);
        let Some((peer, caller)) = st.links.get(from).filter(|e| e.up).map(|e| {
            (
                e.peer,
                Caller {
                    uid: from.to_owned(),
                    name: e.name.clone(),
                    person_id: e.person_id.clone(),
                    person: e.person.clone(),
                },
            )
        }) else {
            return false;
        };
        match frame {
            Frame::Device(DeviceFrame { uid: _, name, run }) => {
                if let Some(me) = st.links.get_mut(from) {
                    me.name = name;
                    me.run = run;
                    me.known = true;
                }
                st.broadcast();
            }
            Frame::Dev { id, to, op, args } => {
                if !peer {
                    st.fail(
                        from,
                        &id,
                        code::SCOPE,
                        "This line may not use the household.",
                    );
                    return true;
                }
                if !st.links.get(&to).is_some_and(|t| t.up) {
                    st.fail(from, &id, code::OFFLINE, "That device is offline.");
                    return true;
                }
                let hub_id = format!("hub-{}", st.next);
                st.next += 1;
                let call = Frame::DevCall {
                    id: hub_id.clone(),
                    from: caller,
                    op,
                    args,
                };
                if call.size() > MAX_FRAME_BYTES {
                    st.fail(from, &id, code::TOO_LARGE, "That request is too large.");
                    return true;
                }
                st.calls.insert(hub_id.clone(), (from.to_owned(), id));
                st.deliver(&to, call);
                let router = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(ROUTER_TIMEOUT).await;
                    let mut st = lock(&router.0);
                    if let Some((asker, id)) = st.calls.remove(&hub_id) {
                        st.fail(
                            &asker,
                            &id,
                            code::TIMEOUT,
                            "The device did not answer in time.",
                        );
                    }
                });
            }
            Frame::DevResult {
                id,
                ok,
                result,
                error,
            } => {
                if let Some((asker, orig)) = st.calls.remove(&id) {
                    let f = Frame::DevResult {
                        id: orig.clone(),
                        ok,
                        result,
                        error,
                    };
                    if f.size() > MAX_FRAME_BYTES {
                        st.fail(&asker, &orig, code::TOO_LARGE, "That answer is too large.");
                    } else {
                        st.deliver(&asker, f);
                    }
                }
            }
            Frame::DevPush(mut p) => {
                if !peer {
                    return true;
                }
                if let Some(to) = p.to.take() {
                    p.from = Some(from.to_owned());
                    let f = Frame::DevPush(p);
                    if f.size() <= MAX_FRAME_BYTES {
                        st.deliver(&to, f);
                    }
                }
            }
            // Only the hub sends these.
            Frame::Devices { .. } | Frame::DevCall { .. } => {}
        }
        true
    }

    fn set_up(&self, uid: &str, up: bool) {
        let mut st = lock(&self.0);
        if let Some(e) = st.links.get_mut(uid) {
            if e.up && !up {
                e.last_seen_ms = Some(helm_core::now_ms());
            }
            e.up = up;
        }
        st.broadcast();
    }

    fn is_up(&self, uid: &str) -> bool {
        lock(&self.0).links.get(uid).is_some_and(|e| e.up)
    }
}

/// One device's end of a [`MemRouter`].
pub struct MemLink {
    router: MemRouter,
    uid: String,
    person_id: Option<String>,
    cbs: Arc<Mutex<Vec<FrameCb>>>,
}

impl MemLink {
    /// Drop the line or bring it back; the others hear about it in `devices`.
    pub fn set_up(&self, up: bool) {
        self.router.set_up(&self.uid, up);
    }
}

impl DeviceLink for MemLink {
    fn send(&self, frame: Frame) -> bool {
        self.router.handle(&self.uid, frame)
    }

    fn subscribe(&self, cb: FrameCb) {
        lock(&self.cbs).push(cb);
    }

    fn up(&self) -> bool {
        self.router.is_up(&self.uid)
    }

    fn person_id(&self) -> Option<String> {
        self.person_id.clone()
    }
}
