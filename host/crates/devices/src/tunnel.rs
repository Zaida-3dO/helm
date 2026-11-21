//! Port forwarding: a server listening on one PC's loopback, opened in another PC's browser.
//!
//! The PC that runs the server is the *host*; the PC whose browser opens it is the *viewer*. Both
//! ends stay on loopback: the viewer listens on `127.0.0.1` only, and the host only ever connects
//! out to `127.0.0.1` (or `[::1]` when that refuses). No socket here listens on the network and no
//! connection reaches past this PC, so there is no firewall prompt and nothing on the LAN can use
//! a forward. The bytes travel over the hub line the PCs already share, as `dev_push` frames of
//! [`SUB`], so only a paired PC of the household can take part.
//!
//! A forward is made by a device call (the host checks the person, the switch and the port, then
//! [`Tunnels::host_open`] tries the server once); after that every browser connection on the
//! viewer is one *stream* over pushes. Every push names the forward `f` (the host's random id) and
//! the stream `c` (the viewer's number), so each PC can tell which side of which forward a push is
//! for even when both PCs forward to each other:
//!
//! | `t` | from | fields | what |
//! |---|---|---|---|
//! | `open` | viewer | `f, c` | a browser connected: reach the server |
//! | `opened` / `refused` | host | `f, c` | the server answered, or did not |
//! | `data` | both | `f, c, s, d` | `s` counts this direction's chunks from 0; `d` is base64 of 1 to [`CHUNK_BYTES`] bytes |
//! | `ack` | both | `f, c, upto` | this many bytes of the other direction reached the local socket |
//! | `close` | both | `f, c` | no more data this way (the socket's end of input); the stream ends when both ways closed |
//! | `reset` | both | `f, c` | the stream is gone: close it now |
//! | `end` | both | `f` | the forward is gone (stopped, idle, or the PC is quitting) |
//!
//! The hub queues whatever it is given, so both ends keep it bounded: a sender stops reading its
//! socket while [`WINDOW_BYTES`] of its data are unacknowledged, and a receiver resets a stream
//! whose sender goes past that. A push from a device the forward does not belong to, a gap in `s`,
//! an `ack` for bytes never sent, or data after `close` resets that stream (or is refused, for an
//! `open`). Payload bytes are never logged.
//!
//! A forward ends on Stop, after [`IDLE`] with no stream open, when the other PC's line goes
//! ([`Tunnels::drop_peer`]), or when either PC quits ([`Tunnels::shutdown`]); the table lives in
//! memory, so nothing survives a restart. Ending a forward or a stream drops its sockets.
//!
//! The engine reaches the outside through two seams: [`TunnelOut`] sends a push to another device
//! (the hub line in the app, an in-memory relay in tests) and [`Loopback`] dials and listens on
//! this PC's loopback ([`LocalLoopback`] in the app).

use crate::link::BoxFut;
use crate::wire::{code, WireError};
use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

/// The `sub` of every `dev_push` that carries a forward.
pub const SUB: &str = "tun";
/// The most raw bytes one `data` push carries: small, so a busy forward never holds up chat news
/// on the same line for long.
pub const CHUNK_BYTES: usize = 48 * 1024;
/// The most bytes one direction of a stream may have in flight before the sender waits for an ack.
pub const WINDOW_BYTES: u64 = 256 * 1024;
/// The most forwards one viewing PC holds at once (and the most a host keeps for one viewer).
pub const MAX_FORWARDS: usize = 4;
/// The most connections one forward carries at once; a browser opens a handful per page.
pub const MAX_STREAMS: usize = 16;
/// A forward with no connection open for this long ends.
pub const IDLE: Duration = Duration::from_secs(30 * 60);
/// How long the host waits for the server to accept, per address tried.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the viewer waits for the host to say whether it reached the server.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Remote-desktop and remote-management ports: never forwarded, whatever answers on them.
pub const REMOTE_ADMIN_PORTS: [u16; 4] = [3389, 5985, 5986, 47001];

/// The longest `d` a chunk of [`CHUNK_BYTES`] encodes to.
const MAX_ENCODED: usize = CHUNK_BYTES.div_ceil(3) * 4;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn refused(message: impl Into<String>) -> WireError {
    WireError::new(code::REFUSED, message)
}

/// Whether `port` may be forwarded at all. `reserved` names Helm's own listeners on this PC (the
/// page server's port and the like), which are never forwarded.
pub fn check_port(port: u16, reserved: &[u16]) -> Result<(), WireError> {
    if port < 1024 {
        return Err(refused(
            "Ports below 1024 are not forwarded: pick the port the server prints, such as 5173.",
        ));
    }
    if reserved.contains(&port) {
        return Err(refused(format!(
            "Port {port} is one of Helm's own, so it is not forwarded."
        )));
    }
    if REMOTE_ADMIN_PORTS.contains(&port) {
        return Err(refused(format!(
            "Port {port} is for controlling the PC remotely, so it is never forwarded."
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Seams
// ---------------------------------------------------------------------------------------------

/// The way out to the other devices.
pub trait TunnelOut: Send + Sync {
    /// Send `event` to device `to` as a `dev_push` of [`SUB`]. `false` when the line is down:
    /// nothing was sent, and the stream it belonged to is dropped.
    fn push(&self, to: &str, event: Value) -> bool;
    /// The list of forwards to this PC changed (one started or ended).
    fn changed(&self) {}
}

/// This PC's loopback. Implementations must never dial or listen on any other address.
pub trait Loopback: Send + Sync {
    /// Connect to a server on this PC.
    fn dial(&self, port: u16) -> BoxFut<'_, io::Result<TcpStream>>;
    /// Listen on `127.0.0.1:<port>`; port 0 asks for any free port.
    fn listen(&self, port: u16) -> BoxFut<'_, io::Result<TcpListener>>;
}

/// The real loopback: `127.0.0.1`, then `[::1]` for servers that only listen there.
pub struct LocalLoopback;

impl Loopback for LocalLoopback {
    fn dial(&self, port: u16) -> BoxFut<'_, io::Result<TcpStream>> {
        Box::pin(async move {
            let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            if let Ok(Ok(s)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(v4)).await {
                return Ok(s);
            }
            let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(v6)).await {
                Ok(r) => r,
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the server did not answer",
                )),
            }
        })
    }

    fn listen(&self, port: u16) -> BoxFut<'_, io::Result<TcpListener>> {
        Box::pin(TcpListener::bind(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            port,
        ))))
    }
}

// ---------------------------------------------------------------------------------------------
// What callers see
// ---------------------------------------------------------------------------------------------

/// A forward the viewer is about to start: the host has already agreed to it and named it.
#[derive(Clone, Debug)]
pub struct ViewSpec {
    pub host_uid: String,
    pub forward_id: String,
    /// The server's port on the host. The viewer listens on the same port when it is free.
    pub port: u16,
    pub chat_id: String,
    pub title: String,
}

/// A forward to this PC, as the list of forwards shows it. The device's name is the caller's to
/// add: the engine knows devices only by id.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewInfo {
    pub forward_id: String,
    pub chat_id: String,
    pub device_uid: String,
    pub port: u16,
    pub local_port: u16,
    pub url: String,
    pub title: String,
    pub streams: usize,
    /// Unix ms.
    pub opened_at: i64,
    pub last_used_at: i64,
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// This PC runs the server; the peer views it.
    Host,
    /// This PC's browser opens the peer's server.
    View,
}

/// What reaches a stream's local socket, in order.
enum Inbound {
    Data(Vec<u8>),
    /// The other side has no more to send.
    Fin,
}

struct Stream {
    inbound: mpsc::UnboundedSender<Inbound>,
    /// The writing side's receiver until the pumps start.
    inbound_rx: Option<mpsc::UnboundedReceiver<Inbound>>,
    /// The `s` the next `data` must carry.
    next_s: u64,
    /// Bytes taken from the peer, and of those the ones written to the local socket. Their
    /// difference is what this side holds; the peer may not push it past the window.
    received: u64,
    delivered: Arc<AtomicU64>,
    fin_in: bool,
    /// Bytes sent to the peer, and the peer's last ack.
    sent: Arc<AtomicU64>,
    acked: watch::Sender<u64>,
    /// The viewer, until the host answers `open`.
    opened: Option<oneshot::Sender<bool>>,
    tasks: Vec<AbortHandle>,
    /// Pump tasks still running; the stream ends cleanly when both finished.
    parts: u8,
}

impl Stream {
    fn new() -> Stream {
        let (tx, rx) = mpsc::unbounded_channel();
        Stream {
            inbound: tx,
            inbound_rx: Some(rx),
            next_s: 0,
            received: 0,
            delivered: Arc::default(),
            fin_in: false,
            sent: Arc::default(),
            acked: watch::Sender::new(0),
            opened: None,
            tasks: Vec::new(),
            parts: 0,
        }
    }

    fn abort(&self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

struct Fwd {
    role: Role,
    /// The other PC: the viewer for a host forward, the host for a viewer's.
    peer: String,
    /// The server's port on the host.
    port: u16,
    /// Where the viewer listens (the host keeps the server's port here).
    local_port: u16,
    chat_id: String,
    title: String,
    streams: HashMap<u64, Stream>,
    opened_at: i64,
    last_used_at: i64,
    /// Since when no stream has been open.
    idle_since: Option<Instant>,
    listener: Option<AbortHandle>,
}

impl Fwd {
    fn info(&self, id: &str) -> ViewInfo {
        ViewInfo {
            forward_id: id.to_owned(),
            chat_id: self.chat_id.clone(),
            device_uid: self.peer.clone(),
            port: self.port,
            local_port: self.local_port,
            url: format!("http://localhost:{}/", self.local_port),
            title: self.title.clone(),
            streams: self.streams.len(),
            opened_at: self.opened_at,
            last_used_at: self.last_used_at,
        }
    }

    fn close(&self) {
        if let Some(l) = &self.listener {
            l.abort();
        }
        for s in self.streams.values() {
            s.abort();
        }
    }
}

#[derive(Default)]
struct State {
    fwds: HashMap<String, Fwd>,
    /// The viewer's next stream number.
    next_c: u64,
}

impl State {
    /// The forward `f` when it is `role`'s and belongs to `peer`; anything else is not theirs.
    fn owned(&mut self, f: &str, peer: &str, role: Role) -> Option<&mut Fwd> {
        self.fwds
            .get_mut(f)
            .filter(|w| w.peer == peer && w.role == role)
    }

    /// As [`State::owned`], for a push that may be for either side of a forward.
    fn either(&mut self, f: &str, peer: &str) -> Option<&mut Fwd> {
        self.fwds.get_mut(f).filter(|w| w.peer == peer)
    }

    fn count(&self, role: Role, peer: Option<&str>) -> usize {
        self.fwds
            .values()
            .filter(|w| w.role == role && peer.is_none_or(|p| w.peer == p))
            .count()
    }
}

struct Inner {
    out: Arc<dyn TunnelOut>,
    net: Arc<dyn Loopback>,
    handle: Handle,
    idle: Duration,
    st: Mutex<State>,
}

/// Both ends of every forward this PC takes part in.
pub struct Tunnels {
    inner: Arc<Inner>,
    sweeper: AbortHandle,
}

impl Drop for Tunnels {
    fn drop(&mut self) {
        self.sweeper.abort();
        self.shutdown();
    }
}

impl Tunnels {
    pub fn new(out: Arc<dyn TunnelOut>, net: Arc<dyn Loopback>, handle: Handle) -> Tunnels {
        Tunnels::with_idle(out, net, handle, IDLE)
    }

    /// As [`Tunnels::new`], ending forwards after `idle` with no connection instead of [`IDLE`].
    pub fn with_idle(
        out: Arc<dyn TunnelOut>,
        net: Arc<dyn Loopback>,
        handle: Handle,
        idle: Duration,
    ) -> Tunnels {
        let inner = Arc::new(Inner {
            out,
            net,
            handle: handle.clone(),
            idle,
            st: Mutex::default(),
        });
        let every = (idle / 4).clamp(Duration::from_millis(10), Duration::from_secs(60));
        let sweeper = handle
            .spawn(sweep(Arc::downgrade(&inner), every))
            .abort_handle();
        Tunnels { inner, sweeper }
    }

    // ----------------------------------------------------------------------- host

    /// Whether `port` may be forwarded and something answers on it now: what an offer checks
    /// before it names the port to another PC.
    pub async fn check(&self, port: u16, reserved: &[u16]) -> Result<(), WireError> {
        check_port(port, reserved)?;
        self.probe(port).await
    }

    async fn probe(&self, port: u16) -> Result<(), WireError> {
        match self.inner.net.dial(port).await {
            // The test connection closes at once.
            Ok(_) => Ok(()),
            Err(_) => Err(refused(format!(
                "Nothing answers on port {port} on this PC. Start the server first."
            ))),
        }
    }

    /// The host's half of `forward_open`: device `viewer` (already checked by the caller) asks to
    /// forward `port`. Answers the new forward's id.
    pub async fn host_open(
        &self,
        viewer: &str,
        chat_id: &str,
        port: u16,
        reserved: &[u16],
    ) -> Result<String, WireError> {
        check_port(port, reserved)?;
        let full = || {
            refused(format!(
                "That PC already has {MAX_FORWARDS} ports forwarded from this one. Stop one first."
            ))
        };
        if lock(&self.inner.st).count(Role::Host, Some(viewer)) >= MAX_FORWARDS {
            return Err(full());
        }
        self.probe(port).await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = helm_core::now_ms();
        {
            let mut st = lock(&self.inner.st);
            if st.count(Role::Host, Some(viewer)) >= MAX_FORWARDS {
                return Err(full());
            }
            st.fwds.insert(
                id.clone(),
                Fwd {
                    role: Role::Host,
                    peer: viewer.to_owned(),
                    port,
                    local_port: port,
                    chat_id: chat_id.to_owned(),
                    title: String::new(),
                    streams: HashMap::new(),
                    opened_at: now,
                    last_used_at: now,
                    idle_since: Some(Instant::now()),
                    listener: None,
                },
            );
        }
        log::info!("tunnel: forwarding port {port} to device {viewer} ({id})");
        Ok(id)
    }

    /// The host's half of `forward_close`. `false` when `viewer` holds no such forward.
    pub fn host_close(&self, viewer: &str, forward_id: &str) -> bool {
        let found = lock(&self.inner.st)
            .owned(forward_id, viewer, Role::Host)
            .is_some();
        found && self.inner.end(forward_id, false)
    }

    // ----------------------------------------------------------------------- viewer

    /// Start listening for a forward the host agreed to: on the server's own port when it is
    /// free here (so the link the agent wrote works as written), else on any free port.
    pub async fn view_start(&self, req: ViewSpec) -> Result<ViewInfo, WireError> {
        let full = || {
            refused(format!(
                "This PC already has {MAX_FORWARDS} ports forwarded to it. Stop one first."
            ))
        };
        if lock(&self.inner.st).count(Role::View, None) >= MAX_FORWARDS {
            return Err(full());
        }
        let net = &self.inner.net;
        let listener = match net.listen(req.port).await {
            Ok(l) => l,
            Err(_) => net.listen(0).await.map_err(|_| {
                WireError::internal("This PC could not listen for the forwarded port.")
            })?,
        };
        let local_port = listener
            .local_addr()
            .map_err(|_| WireError::internal("This PC could not listen for the forwarded port."))?
            .port();
        let now = helm_core::now_ms();
        let info = {
            let mut st = lock(&self.inner.st);
            if st.count(Role::View, None) >= MAX_FORWARDS {
                return Err(full());
            }
            if st.fwds.contains_key(&req.forward_id) {
                return Err(WireError::new(
                    code::BAD_ARGS,
                    "That forward is already open.",
                ));
            }
            let accept = self
                .inner
                .handle
                .spawn(accept_loop(
                    Arc::downgrade(&self.inner),
                    req.forward_id.clone(),
                    listener,
                ))
                .abort_handle();
            let w = Fwd {
                role: Role::View,
                peer: req.host_uid.clone(),
                port: req.port,
                local_port,
                chat_id: req.chat_id,
                title: req.title,
                streams: HashMap::new(),
                opened_at: now,
                last_used_at: now,
                idle_since: Some(Instant::now()),
                listener: Some(accept),
            };
            let info = w.info(&req.forward_id);
            st.fwds.insert(req.forward_id.clone(), w);
            info
        };
        log::info!(
            "tunnel: port {} of device {} opens here on 127.0.0.1:{local_port} ({})",
            req.port,
            req.host_uid,
            req.forward_id
        );
        self.inner.out.changed();
        Ok(info)
    }

    /// Stop a forward to this PC and tell the host. `false` when there was none.
    pub fn view_stop(&self, forward_id: &str) -> bool {
        let found = lock(&self.inner.st)
            .fwds
            .get(forward_id)
            .is_some_and(|w| w.role == Role::View);
        found && self.inner.end(forward_id, true)
    }

    /// The forwards to this PC, oldest first.
    pub fn forwards(&self) -> Vec<ViewInfo> {
        let st = lock(&self.inner.st);
        let mut v: Vec<(i64, ViewInfo)> = st
            .fwds
            .iter()
            .filter(|(_, w)| w.role == Role::View)
            .map(|(id, w)| (w.opened_at, w.info(id)))
            .collect();
        v.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.forward_id.cmp(&b.1.forward_id))
        });
        v.into_iter().map(|(_, i)| i).collect()
    }

    /// The forward to this PC of `port` on device `host`, if there is one: open it again rather
    /// than asking for another.
    pub fn viewing(&self, host: &str, port: u16) -> Option<ViewInfo> {
        self.forwards()
            .into_iter()
            .find(|i| i.device_uid == host && i.port == port)
    }

    // ----------------------------------------------------------------------- both

    /// A `dev_push` of [`SUB`] from device `from` (as the hub stamped it).
    pub fn on_push(&self, from: &str, event: &Value) {
        let i = &self.inner;
        let f = event["f"].as_str().unwrap_or_default();
        let c = event["c"].as_u64();
        match (event["t"].as_str().unwrap_or_default(), c) {
            ("open", Some(c)) => i.host_stream(from, f, c),
            ("opened", Some(c)) => i.answered(from, f, c, true),
            ("refused", Some(c)) => i.answered(from, f, c, false),
            ("data", Some(c)) => i.data(from, f, c, event),
            ("ack", Some(c)) => i.ack(from, f, c, event["upto"].as_u64()),
            ("close", Some(c)) => i.fin(from, f, c),
            ("reset", Some(c)) => {
                let ours = lock(&i.st)
                    .either(f, from)
                    .is_some_and(|w| w.streams.contains_key(&c));
                if ours {
                    i.kill(f, c, false);
                }
            }
            ("end", _) => {
                let ours = lock(&i.st).either(f, from).is_some();
                if ours {
                    i.end(f, false);
                }
            }
            _ => {}
        }
    }

    /// Device `uid` went offline or this PC's line dropped: every forward with it ends, with
    /// nothing said (there is no line to say it on).
    pub fn drop_peer(&self, uid: &str) {
        self.retain_peers(|p| p != uid);
    }

    /// Keep only the forwards whose other PC `keep` names (the devices the hub shows as up); the
    /// rest end with nothing said. With `|_| false`, this PC's own line dropped.
    pub fn retain_peers(&self, keep: impl Fn(&str) -> bool) {
        let ids: Vec<String> = lock(&self.inner.st)
            .fwds
            .iter()
            .filter(|(_, w)| !keep(&w.peer))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.inner.end(&id, false);
        }
    }

    /// This PC stopped letting other devices use its chats: every forward of a port here ends,
    /// and the PCs viewing them are told.
    pub fn end_hosted(&self) {
        let ids: Vec<String> = lock(&self.inner.st)
            .fwds
            .iter()
            .filter(|(_, w)| w.role == Role::Host)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.inner.end(&id, true);
        }
    }

    /// This PC is quitting: every forward ends, and the other PCs are told.
    pub fn shutdown(&self) {
        let ids: Vec<String> = lock(&self.inner.st).fwds.keys().cloned().collect();
        for id in ids {
            self.inner.end(&id, true);
        }
    }
}

impl Inner {
    fn push(&self, to: &str, event: Value) -> bool {
        self.out.push(to, event)
    }

    /// End forward `f`: its listener and every stream close. `tell` says `end` to the peer.
    fn end(&self, f: &str, tell: bool) -> bool {
        let Some(w) = lock(&self.st).fwds.remove(f) else {
            return false;
        };
        w.close();
        if tell {
            self.push(&w.peer, json!({"t": "end", "f": f}));
        }
        log::info!("tunnel: forward of port {} ended ({f})", w.port);
        if w.role == Role::View {
            self.out.changed();
        }
        true
    }

    /// Drop stream `c` of forward `f`, closing its socket. `tell` says `reset` to the peer.
    fn kill(&self, f: &str, c: u64, tell: bool) {
        let peer = {
            let mut st = lock(&self.st);
            let Some(w) = st.fwds.get_mut(f) else { return };
            let Some(s) = w.streams.remove(&c) else {
                return;
            };
            s.abort();
            if w.streams.is_empty() {
                w.idle_since = Some(Instant::now());
            }
            w.peer.clone()
        };
        if tell {
            self.push(&peer, json!({"t": "reset", "f": f, "c": c}));
        }
    }

    /// Say `reset` to `to` about a stream this PC does not hold for it, so it stops sending.
    fn unknown(&self, to: &str, f: &str, c: u64) {
        self.push(to, json!({"t": "reset", "f": f, "c": c}));
    }

    /// One of a stream's two pumps finished cleanly; the stream ends when both have.
    fn part_done(&self, f: &str, c: u64) {
        let mut st = lock(&self.st);
        let Some(w) = st.fwds.get_mut(f) else { return };
        let Some(s) = w.streams.get_mut(&c) else {
            return;
        };
        s.parts = s.parts.saturating_sub(1);
        if s.parts == 0 {
            w.streams.remove(&c);
            if w.streams.is_empty() {
                w.idle_since = Some(Instant::now());
            }
        }
    }

    fn touch(&self, f: &str) {
        if let Some(w) = lock(&self.st).fwds.get_mut(f) {
            w.last_used_at = helm_core::now_ms();
        }
    }

    /// Add stream `c` to forward `w`: the forward is in use again.
    fn add_stream(w: &mut Fwd, c: u64, s: Stream) {
        w.streams.insert(c, s);
        w.idle_since = None;
        w.last_used_at = helm_core::now_ms();
    }

    fn keep_task(&self, f: &str, c: u64, t: AbortHandle) {
        let mut st = lock(&self.st);
        match st.fwds.get_mut(f).and_then(|w| w.streams.get_mut(&c)) {
            Some(s) => s.tasks.push(t),
            // The stream ended before its task was noted.
            None => t.abort(),
        }
    }

    /// Host: the viewer's browser connected; reach the server.
    fn host_stream(self: &Arc<Self>, from: &str, f: &str, c: u64) {
        let port = {
            let mut st = lock(&self.st);
            match st.owned(f, from, Role::Host) {
                Some(w) if w.streams.len() < MAX_STREAMS && !w.streams.contains_key(&c) => {
                    Self::add_stream(w, c, Stream::new());
                    Some(w.port)
                }
                _ => None,
            }
        };
        let Some(port) = port else {
            self.push(from, json!({"t": "refused", "f": f, "c": c}));
            return;
        };
        let i = self.clone();
        let (peer, fid) = (from.to_owned(), f.to_owned());
        let t = self.handle.spawn(async move {
            match i.net.dial(port).await {
                Ok(sock) => {
                    // Said before the pumps start, so `opened` reaches the viewer before data.
                    if i.push(&peer, json!({"t": "opened", "f": fid, "c": c})) {
                        i.start_pumps(&fid, c, sock);
                    } else {
                        i.kill(&fid, c, false);
                    }
                }
                Err(_) => {
                    i.kill(&fid, c, false);
                    i.push(&peer, json!({"t": "refused", "f": fid, "c": c}));
                }
            }
        });
        self.keep_task(f, c, t.abort_handle());
    }

    /// Viewer: a browser connected to forward `f`'s listener.
    fn view_stream(self: &Arc<Self>, f: &str, sock: TcpStream) {
        let (otx, orx) = oneshot::channel();
        let (peer, c) = {
            let mut st = lock(&self.st);
            let c = st.next_c;
            let Some(w) = st.fwds.get_mut(f).filter(|w| w.role == Role::View) else {
                return;
            };
            // Over the limit the connection closes at once; browsers try again.
            if w.streams.len() >= MAX_STREAMS {
                return;
            }
            let mut s = Stream::new();
            s.opened = Some(otx);
            Self::add_stream(w, c, s);
            let peer = w.peer.clone();
            st.next_c += 1;
            (peer, c)
        };
        if !self.push(&peer, json!({"t": "open", "f": f, "c": c})) {
            self.kill(f, c, false);
            return;
        }
        let i = self.clone();
        let fid = f.to_owned();
        let t = self.handle.spawn(async move {
            match tokio::time::timeout(OPEN_TIMEOUT, orx).await {
                Ok(Ok(true)) => i.start_pumps(&fid, c, sock),
                // Refused: the host holds nothing for it.
                Ok(Ok(false)) => i.kill(&fid, c, false),
                _ => i.kill(&fid, c, true),
            }
        });
        self.keep_task(f, c, t.abort_handle());
    }

    /// Viewer: the host's answer to `open`.
    fn answered(&self, from: &str, f: &str, c: u64, ok: bool) {
        let waiting = lock(&self.st)
            .owned(f, from, Role::View)
            .and_then(|w| w.streams.get_mut(&c))
            .and_then(|s| s.opened.take());
        if let Some(tx) = waiting {
            let _ = tx.send(ok);
        }
    }

    /// Both: connect stream `c` to its local socket and start moving bytes both ways. The socket
    /// is dropped (closed) when the stream ended meanwhile.
    fn start_pumps(self: &Arc<Self>, f: &str, c: u64, sock: TcpStream) {
        let _ = sock.set_nodelay(true);
        let (rd, wr) = sock.into_split();
        let mut st = lock(&self.st);
        let Some(w) = st.fwds.get_mut(f) else { return };
        let peer = w.peer.clone();
        let Some(s) = w.streams.get_mut(&c) else {
            return;
        };
        let Some(rx) = s.inbound_rx.take() else {
            return;
        };
        let up = self.handle.spawn(self.clone().read_pump(
            peer.clone(),
            f.to_owned(),
            c,
            rd,
            s.sent.clone(),
            s.acked.subscribe(),
        ));
        let down = self.handle.spawn(self.clone().write_pump(
            peer,
            f.to_owned(),
            c,
            wr,
            rx,
            s.delivered.clone(),
        ));
        s.tasks.push(up.abort_handle());
        s.tasks.push(down.abort_handle());
        s.parts = 2;
    }

    /// Local socket to the peer, never more than the window ahead of the peer's acks.
    async fn read_pump(
        self: Arc<Self>,
        peer: String,
        f: String,
        c: u64,
        mut rd: OwnedReadHalf,
        sent: Arc<AtomicU64>,
        mut acked: watch::Receiver<u64>,
    ) {
        let mut buf = vec![0u8; CHUNK_BYTES];
        let mut s = 0u64;
        loop {
            let room = loop {
                let inflight = sent.load(Ordering::SeqCst) - *acked.borrow_and_update();
                if inflight < WINDOW_BYTES {
                    break (WINDOW_BYTES - inflight) as usize;
                }
                if acked.changed().await.is_err() {
                    return;
                }
            };
            let want = room.min(CHUNK_BYTES);
            match rd.read(&mut buf[..want]).await {
                Ok(0) => {
                    if self.push(&peer, json!({"t": "close", "f": f, "c": c})) {
                        self.part_done(&f, c);
                    } else {
                        self.kill(&f, c, false);
                    }
                    return;
                }
                Ok(n) => {
                    sent.fetch_add(n as u64, Ordering::SeqCst);
                    let d = B64.encode(&buf[..n]);
                    if !self.push(&peer, json!({"t": "data", "f": f, "c": c, "s": s, "d": d})) {
                        self.kill(&f, c, false);
                        return;
                    }
                    s += 1;
                    self.touch(&f);
                }
                Err(_) => {
                    self.kill(&f, c, true);
                    return;
                }
            }
        }
    }

    /// The peer's bytes to the local socket, acknowledged as they are written.
    async fn write_pump(
        self: Arc<Self>,
        peer: String,
        f: String,
        c: u64,
        mut wr: OwnedWriteHalf,
        mut rx: mpsc::UnboundedReceiver<Inbound>,
        delivered: Arc<AtomicU64>,
    ) {
        while let Some(m) = rx.recv().await {
            match m {
                Inbound::Data(b) => {
                    if wr.write_all(&b).await.is_err() {
                        self.kill(&f, c, true);
                        return;
                    }
                    let n = b.len() as u64;
                    let upto = delivered.fetch_add(n, Ordering::SeqCst) + n;
                    if !self.push(&peer, json!({"t": "ack", "f": f, "c": c, "upto": upto})) {
                        self.kill(&f, c, false);
                        return;
                    }
                }
                Inbound::Fin => {
                    let _ = wr.shutdown().await;
                    self.part_done(&f, c);
                    return;
                }
            }
        }
    }

    /// Both: bytes from the peer for stream `c`.
    fn data(&self, from: &str, f: &str, c: u64, ev: &Value) {
        enum Next {
            Fine,
            Unknown,
            Reset,
        }
        let next = {
            let mut st = lock(&self.st);
            match st.either(f, from).and_then(|w| w.streams.get_mut(&c)) {
                None => Next::Unknown,
                Some(s) => {
                    let bytes = match (ev["s"].as_u64(), ev["d"].as_str()) {
                        (Some(n), Some(d))
                            if n == s.next_s && !s.fin_in && d.len() <= MAX_ENCODED =>
                        {
                            B64.decode(d)
                                .ok()
                                .filter(|b| !b.is_empty() && b.len() <= CHUNK_BYTES)
                        }
                        _ => None,
                    };
                    match bytes {
                        Some(b) => {
                            s.next_s += 1;
                            s.received += b.len() as u64;
                            let held = s.received - s.delivered.load(Ordering::SeqCst);
                            if held > WINDOW_BYTES || s.inbound.send(Inbound::Data(b)).is_err() {
                                Next::Reset
                            } else {
                                Next::Fine
                            }
                        }
                        None => Next::Reset,
                    }
                }
            }
        };
        match next {
            Next::Fine => {}
            Next::Unknown => self.unknown(from, f, c),
            Next::Reset => self.kill(f, c, true),
        }
    }

    /// Both: the peer wrote `upto` bytes of this side's data to its socket.
    fn ack(&self, from: &str, f: &str, c: u64, upto: Option<u64>) {
        let bad = {
            let mut st = lock(&self.st);
            match st.either(f, from).and_then(|w| w.streams.get_mut(&c)) {
                None => return,
                Some(s) => match upto {
                    Some(u) if u >= *s.acked.borrow() && u <= s.sent.load(Ordering::SeqCst) => {
                        s.acked.send_replace(u);
                        false
                    }
                    _ => true,
                },
            }
        };
        if bad {
            self.kill(f, c, true);
        }
    }

    /// Both: the peer has no more to send on stream `c`.
    fn fin(&self, from: &str, f: &str, c: u64) {
        let bad = {
            let mut st = lock(&self.st);
            match st.either(f, from).and_then(|w| w.streams.get_mut(&c)) {
                None => return,
                Some(s) if s.fin_in => true,
                Some(s) => {
                    s.fin_in = true;
                    s.inbound.send(Inbound::Fin).is_err()
                }
            }
        };
        if bad {
            self.kill(f, c, true);
        }
    }
}

/// Viewer: take the browser's connections to forward `f` until the forward ends.
async fn accept_loop(inner: Weak<Inner>, f: String, listener: TcpListener) {
    loop {
        let sock = match listener.accept().await {
            Ok((sock, _)) => sock,
            Err(_) => {
                // A failed accept (the browser gave up mid-handshake) is not the listener's end.
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Some(i) = inner.upgrade() else { return };
        i.view_stream(&f, sock);
    }
}

/// End forwards that have had no connection for the idle time, on both sides.
async fn sweep(inner: Weak<Inner>, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        let Some(i) = inner.upgrade() else { return };
        let stale: Vec<String> = lock(&i.st)
            .fwds
            .iter()
            .filter(|(_, w)| w.idle_since.is_some_and(|t| t.elapsed() >= i.idle))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            i.end(&id, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// One direction of one stream: (sender, forward, stream).
    type Way = (String, String, u64);

    /// An in-memory hub for the two engines: delivers pushes in order, stamped with the sender,
    /// keeps every push that is not data, and measures each direction's unacknowledged bytes.
    #[derive(Default)]
    struct Relay {
        engines: Mutex<HashMap<String, Arc<Tunnels>>>,
        tx: Mutex<Option<mpsc::UnboundedSender<(String, String, Value)>>>,
        said: Mutex<Vec<(String, String, Value)>>,
        /// (sender, f, c) → (bytes sent, bytes the other side acknowledged).
        flight: Mutex<BTreeMap<Way, (u64, u64)>>,
        most_in_flight: AtomicU64,
    }

    impl Relay {
        fn start() -> Arc<Relay> {
            let relay = Arc::new(Relay::default());
            let (tx, mut rx) = mpsc::unbounded_channel::<(String, String, Value)>();
            *lock(&relay.tx) = Some(tx);
            let r = relay.clone();
            tokio::spawn(async move {
                while let Some((from, to, ev)) = rx.recv().await {
                    r.measure(&from, &to, &ev);
                    let engine = lock(&r.engines).get(&to).cloned();
                    if let Some(e) = engine {
                        e.on_push(&from, &ev);
                    }
                }
            });
            relay
        }

        fn measure(&self, from: &str, to: &str, ev: &Value) {
            let f = ev["f"].as_str().unwrap_or_default().to_owned();
            let c = ev["c"].as_u64().unwrap_or_default();
            let mut fl = lock(&self.flight);
            match ev["t"].as_str() {
                Some("data") => {
                    let n = B64.decode(ev["d"].as_str().unwrap()).unwrap().len() as u64;
                    let e = fl.entry((from.to_owned(), f, c)).or_default();
                    e.0 += n;
                    self.most_in_flight.fetch_max(e.0 - e.1, Ordering::SeqCst);
                }
                Some("ack") => {
                    let e = fl.entry((to.to_owned(), f, c)).or_default();
                    e.1 = ev["upto"].as_u64().unwrap();
                }
                _ => lock(&self.said).push((from.to_owned(), to.to_owned(), ev.clone())),
            }
        }

        fn engine(
            self: &Arc<Self>,
            uid: &str,
            net: Arc<dyn Loopback>,
            idle: Duration,
        ) -> Arc<Tunnels> {
            let out = Arc::new(Out {
                me: uid.to_owned(),
                relay: self.clone(),
            });
            let t = Arc::new(Tunnels::with_idle(out, net, Handle::current(), idle));
            lock(&self.engines).insert(uid.to_owned(), t.clone());
            t
        }

        /// Whether `from` said `t` to `to`.
        fn said(&self, from: &str, to: &str, t: &str) -> bool {
            lock(&self.said)
                .iter()
                .any(|(a, b, ev)| a == from && b == to && ev["t"] == t)
        }
    }

    struct Out {
        me: String,
        relay: Arc<Relay>,
    }

    impl TunnelOut for Out {
        fn push(&self, to: &str, event: Value) -> bool {
            lock(&self.relay.tx)
                .as_ref()
                .is_some_and(|tx| tx.send((self.me.clone(), to.to_owned(), event)).is_ok())
        }
    }

    /// The real loopback, with some ports standing for others: both engines share one machine, so
    /// the host's "server port" is mapped to where the test server really listens, leaving the
    /// port itself free for the viewer. A port mapped to 0 has nothing listening and refuses at
    /// once (the real refusal takes seconds on Windows).
    #[derive(Default)]
    struct Net(Mutex<HashMap<u16, u16>>);

    impl Loopback for Net {
        fn dial(&self, port: u16) -> BoxFut<'_, io::Result<TcpStream>> {
            let real = lock(&self.0).get(&port).copied().unwrap_or(port);
            Box::pin(async move {
                if real == 0 {
                    return Err(io::ErrorKind::ConnectionRefused.into());
                }
                LocalLoopback.dial(real).await
            })
        }
        fn listen(&self, port: u16) -> BoxFut<'_, io::Result<TcpListener>> {
            LocalLoopback.listen(port)
        }
    }

    async fn echo_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                    let _ = w.shutdown().await;
                });
            }
        });
        port
    }

    async fn free_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    }

    async fn until(what: &str, mut ok: impl FnMut() -> bool) {
        for _ in 0..500 {
            if ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting until {what}");
    }

    struct Pair {
        relay: Arc<Relay>,
        desk: Arc<Tunnels>,
        laptop: Arc<Tunnels>,
        net: Arc<Net>,
    }

    /// "Desk" runs servers; "Laptop" views them.
    fn pair(idle: Duration) -> Pair {
        let relay = Relay::start();
        let net = Arc::new(Net::default());
        let desk = relay.engine("Desk", net.clone(), idle);
        let laptop = relay.engine("Laptop", Arc::new(LocalLoopback), idle);
        Pair {
            relay,
            desk,
            laptop,
            net,
        }
    }

    /// Forward a stand-in port of an echo server from Desk to Laptop.
    async fn forward(p: &Pair) -> ViewInfo {
        let echo = echo_server().await;
        let port = free_port().await;
        lock(&p.net.0).insert(port, echo);
        let id = p
            .desk
            .host_open("Laptop", "chat-1", port, &[])
            .await
            .unwrap();
        p.laptop
            .view_start(ViewSpec {
                host_uid: "Desk".into(),
                forward_id: id,
                port,
                chat_id: "chat-1".into(),
                title: "Preview".into(),
            })
            .await
            .unwrap()
    }

    async fn round_trip(sock: &mut TcpStream, msg: &[u8]) {
        sock.write_all(msg).await.unwrap();
        let mut back = vec![0u8; msg.len()];
        sock.read_exact(&mut back).await.unwrap();
        assert_eq!(back, msg);
    }

    /// Whether the far end closed `sock` (end of input or an error) within a few seconds.
    async fn closed(sock: &mut TcpStream) -> bool {
        let mut b = [0u8; 16];
        matches!(
            tokio::time::timeout(Duration::from_secs(5), sock.read(&mut b)).await,
            Ok(Ok(0)) | Ok(Err(_))
        )
    }

    fn streams(t: &Tunnels) -> usize {
        lock(&t.inner.st)
            .fwds
            .values()
            .map(|w| w.streams.len())
            .sum()
    }

    #[tokio::test]
    async fn a_forward_carries_a_megabyte_both_ways_and_ends_when_idle() {
        let p = pair(Duration::from_millis(400));
        let info = forward(&p).await;
        // The server's port was free here, so the link works as written.
        assert_eq!(info.local_port, info.port);
        assert_eq!(info.url, format!("http://localhost:{}/", info.port));
        assert_eq!(p.laptop.viewing("Desk", info.port), Some(info.clone()));

        // A megabyte out and back through the window, then the browser's end of input closes the
        // stream both ways.
        let sock = TcpStream::connect(("127.0.0.1", info.local_port))
            .await
            .unwrap();
        let (mut r, mut w) = sock.into_split();
        let sent: Vec<u8> = (0..1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        let out = sent.clone();
        let ((), back) = tokio::join!(
            async move {
                w.write_all(&out).await.unwrap();
                w.shutdown().await.unwrap();
            },
            async move {
                let mut back = Vec::new();
                r.read_to_end(&mut back).await.unwrap();
                back
            }
        );
        assert_eq!(back.len(), sent.len());
        assert!(back == sent, "the bytes came back changed");
        let most = p.relay.most_in_flight.load(Ordering::SeqCst);
        assert!(most > 0 && most <= WINDOW_BYTES, "in flight: {most}");
        until("both ends drop the stream", || {
            streams(&p.desk) == 0 && streams(&p.laptop) == 0
        })
        .await;

        // A second forward of the same port: taken here now, so it opens on a free one.
        let id = p
            .desk
            .host_open("Laptop", "chat-1", info.port, &[])
            .await
            .unwrap();
        let other = p
            .laptop
            .view_start(ViewSpec {
                host_uid: "Desk".into(),
                forward_id: id,
                port: info.port,
                chat_id: "chat-1".into(),
                title: String::new(),
            })
            .await
            .unwrap();
        assert_ne!(other.local_port, info.port);
        assert_eq!(p.laptop.forwards().len(), 2);

        // With no connection open, both forwards end on both PCs and the listeners close.
        until("idle forwards end", || {
            p.laptop.forwards().is_empty() && lock(&p.desk.inner.st).fwds.is_empty()
        })
        .await;
        assert!(p.relay.said("Laptop", "Desk", "end"));
        // The port is free again: the listener closed.
        until("the listener closes", || {
            std::net::TcpListener::bind(("127.0.0.1", info.local_port)).is_ok()
        })
        .await;
    }

    #[tokio::test]
    async fn streams_close_on_gaps_strangers_and_lost_lines() {
        let p = pair(IDLE);
        let info = forward(&p).await;
        let f = info.forward_id.clone();
        let mut sock = TcpStream::connect(("127.0.0.1", info.local_port))
            .await
            .unwrap();
        round_trip(&mut sock, b"hello").await;
        let c = *lock(&p.desk.inner.st).fwds[&f]
            .streams
            .keys()
            .next()
            .unwrap();

        // Another device naming this forward is refused and changes nothing.
        let d = B64.encode(b"forged");
        p.desk.on_push(
            "Other",
            &json!({"t": "data", "f": f, "c": c, "s": 1, "d": d}),
        );
        p.desk
            .on_push("Other", &json!({"t": "open", "f": f, "c": 99}));
        p.desk.on_push("Other", &json!({"t": "end", "f": f}));
        p.laptop
            .on_push("Other", &json!({"t": "reset", "f": f, "c": c}));
        round_trip(&mut sock, b"still here").await;
        assert!(p.relay.said("Desk", "Other", "reset"));
        assert!(p.relay.said("Desk", "Other", "refused"));
        assert_eq!(streams(&p.desk), 1);
        assert_eq!(p.laptop.forwards().len(), 1);

        // A gap in the sequence resets the stream on both PCs and the browser's socket closes.
        p.desk.on_push(
            "Laptop",
            &json!({"t": "data", "f": f, "c": c, "s": 99, "d": d}),
        );
        assert!(closed(&mut sock).await);
        until("both ends drop the stream", || {
            streams(&p.desk) == 0 && streams(&p.laptop) == 0
        })
        .await;

        // An ack for bytes never sent does the same.
        let mut sock = TcpStream::connect(("127.0.0.1", info.local_port))
            .await
            .unwrap();
        round_trip(&mut sock, b"again").await;
        let c = *lock(&p.desk.inner.st).fwds[&f]
            .streams
            .keys()
            .next()
            .unwrap();
        p.desk.on_push(
            "Laptop",
            &json!({"t": "ack", "f": f, "c": c, "upto": 1_000_000}),
        );
        assert!(closed(&mut sock).await);

        // The line to the other PC drops: the forward ends on both and open sockets close.
        let mut sock = TcpStream::connect(("127.0.0.1", info.local_port))
            .await
            .unwrap();
        round_trip(&mut sock, b"last").await;
        p.laptop.drop_peer("Desk");
        p.desk.drop_peer("Laptop");
        assert!(closed(&mut sock).await);
        assert!(p.laptop.forwards().is_empty());
        assert!(lock(&p.desk.inner.st).fwds.is_empty());
    }

    #[tokio::test]
    async fn ports_follow_the_policy_and_the_limits() {
        for (port, ok) in [
            (0, false),
            (80, false),
            (443, false),
            (1023, false),
            (1024, true),
            (3000, true),
            (3389, false),
            (5173, true),
            (5985, false),
            (5986, false),
            (7000, false),
            (47001, false),
            (65535, true),
        ] {
            let r = check_port(port, &[7000]);
            assert_eq!(r.is_ok(), ok, "port {port}");
            if let Err(e) = r {
                assert_eq!(e.code, code::REFUSED);
            }
        }

        let p = pair(IDLE);
        // Nothing answers: refused before any forward exists.
        let silent = free_port().await;
        lock(&p.net.0).insert(silent, 0);
        let e = p
            .desk
            .host_open("Laptop", "c", silent, &[])
            .await
            .unwrap_err();
        assert_eq!(e.code, code::REFUSED);
        assert!(p.desk.check(silent, &[]).await.is_err());
        let echo = echo_server().await;
        p.desk.check(echo, &[]).await.unwrap();
        assert!(p.desk.check(echo, &[echo]).await.is_err());

        // Four forwards per viewing PC; another PC still gets its own four.
        for _ in 0..MAX_FORWARDS {
            p.desk.host_open("Laptop", "c", echo, &[]).await.unwrap();
        }
        let e = p
            .desk
            .host_open("Laptop", "c", echo, &[])
            .await
            .unwrap_err();
        assert_eq!(e.code, code::REFUSED);
        p.desk.host_open("Other", "c", echo, &[]).await.unwrap();

        // And four to this PC, whichever PCs they come from.
        for n in 0..MAX_FORWARDS {
            p.laptop
                .view_start(ViewSpec {
                    host_uid: format!("Host{n}"),
                    forward_id: format!("f{n}"),
                    port: echo,
                    chat_id: "c".into(),
                    title: String::new(),
                })
                .await
                .unwrap();
        }
        let e = p
            .laptop
            .view_start(ViewSpec {
                host_uid: "Desk".into(),
                forward_id: "f9".into(),
                port: echo,
                chat_id: "c".into(),
                title: String::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, code::REFUSED);
        assert!(p.laptop.view_stop("f0"));
        assert!(!p.laptop.view_stop("f0"));
        assert_eq!(p.laptop.forwards().len(), MAX_FORWARDS - 1);
    }
}
