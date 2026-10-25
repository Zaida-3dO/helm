//! The line itself: one outbound WebSocket from the PC to the hub, kept open.
//!
//! It tries the LAN address first and the fallback second, reconnecting with a backoff (1 s
//! doubling to 60 s). The token goes in the `Authorization` header of the upgrade request only.
//! Frames are the hub's line protocol (`hub/src/relay.rs`).
//!
//! - `call`: the gate ([`crate::gate::admit`]) first; a refusal is answered at once as "nothing
//!   changed" and the tool never runs. An admitted call runs through the [`Executor`].
//! - `telegram`: handed to the [`TelegramSink`]; acknowledged only once the sink took it, so a
//!   message is never lost to a drop between the hub and the chat.
//! - `telegram_stop`: the sink stops the running turn.
//! - Upward, [`LineHandle`] sends the Telegram status and final answer, and alerts Helm starts
//!   ([`crate::notify`]).

use crate::config::RelayConfig;
use crate::gate;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A tool failure: the text, and whether anything may have changed.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolFailure {
    pub text: String,
    pub changed: bool,
}

/// Runs an admitted call on the PC. `caller` is the hub's client name for the token.
pub trait Executor: Send + Sync {
    fn run<'a>(
        &'a self,
        caller: &'a str,
        call: gate::Admitted,
    ) -> BoxFut<'a, Result<Value, ToolFailure>>;
}

/// One Telegram message as the hub forwarded it.
#[derive(Clone, Debug, PartialEq)]
pub struct TelegramMessage {
    pub id: String,
    pub text: String,
    pub sent_at: i64,
    pub age_minutes: Option<i64>,
    pub bot: Option<String>,
}

/// Where Telegram messages go on the PC.
pub trait TelegramSink: Send + Sync {
    /// `Ok` once the message is safely in Helm (it is then acknowledged to the hub).
    fn deliver<'a>(&'a self, msg: TelegramMessage) -> BoxFut<'a, Result<(), String>>;
    fn stop<'a>(&'a self) -> BoxFut<'a, ()>;
    /// The user tapped button `index` of the approval ask `id` ([`LineHandle::telegram_ask`]).
    fn answer<'a>(&'a self, id: String, index: usize) -> BoxFut<'a, ()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineState {
    Off = 0,
    Connecting = 1,
    Online = 2,
    Offline = 3,
}

impl LineState {
    pub fn as_str(self) -> &'static str {
        match self {
            LineState::Off => "off",
            LineState::Connecting => "connecting",
            LineState::Online => "online",
            LineState::Offline => "offline",
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => LineState::Connecting,
            2 => LineState::Online,
            3 => LineState::Offline,
            _ => LineState::Off,
        }
    }
}

/// Shared with the rest of the app: the line's state, and the way up to the hub.
pub struct LineHandle {
    state: AtomicU8,
    up: mpsc::UnboundedSender<Value>,
}

impl LineHandle {
    pub fn state(&self) -> LineState {
        LineState::from_u8(self.state.load(Ordering::SeqCst))
    }
    fn set(&self, s: LineState) {
        self.state.store(s as u8, Ordering::SeqCst);
    }
    /// The one live status message on Telegram, edited in place by the hub.
    pub fn telegram_status(&self, text: &str) {
        let _ = self
            .up
            .send(json!({ "type": "telegram_status", "text": text }));
    }
    /// A message Helm starts (an alert). Sent only while the line is up, so it never waits in
    /// the queue for a later line; `false` when it was not sent.
    pub fn telegram_notify(&self, text: &str) -> bool {
        self.state() == LineState::Online
            && self
                .up
                .send(json!({ "type": "telegram_notify", "text": text }))
                .is_ok()
    }
    /// An approval ask: a new message with one button per option. Only a tap on one of them
    /// comes back (`telegram_answer`); `false` when it was not sent.
    pub fn telegram_ask(&self, id: &str, text: &str, options: &[String]) -> bool {
        self.state() == LineState::Online
            && self
                .up
                .send(json!({ "type": "telegram_ask", "id": id, "text": text, "options": options }))
                .is_ok()
    }
    /// The ask was settled in Helm or went away: the hub takes its buttons down.
    pub fn telegram_ask_done(&self, id: &str, text: &str) {
        let _ = self
            .up
            .send(json!({ "type": "telegram_ask_done", "id": id, "text": text }));
    }
    /// The turn's answer, sent by the hub as a new message; the status message goes.
    pub fn telegram_final(&self, text: &str) {
        let _ = self
            .up
            .send(json!({ "type": "telegram_final", "text": text }));
    }
}

pub struct Client {
    config: Arc<dyn Fn() -> RelayConfig + Send + Sync>,
    executor: Arc<dyn Executor>,
    sink: Arc<dyn TelegramSink>,
    handle: Arc<LineHandle>,
    up_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>,
    /// Hub ids of Telegram messages being delivered now: the hub's resend of one of them is
    /// dropped, since the delivery under way acknowledges it.
    delivering: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Woken when the saved connection changes, so a new address or token applies at once.
    pub reconfigured: tokio::sync::Notify,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// The hub pings every 30 s; a line silent for 100 s is treated as gone.
const SILENCE: Duration = Duration::from_secs(100);
const MAX_FRAME: usize = 256 * 1024;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl Client {
    pub fn new(
        config: Arc<dyn Fn() -> RelayConfig + Send + Sync>,
        executor: Arc<dyn Executor>,
        sink: Arc<dyn TelegramSink>,
    ) -> Arc<Self> {
        let (up, up_rx) = mpsc::unbounded_channel();
        Arc::new(Client {
            config,
            executor,
            sink,
            handle: Arc::new(LineHandle {
                state: AtomicU8::new(LineState::Off as u8),
                up,
            }),
            up_rx: tokio::sync::Mutex::new(up_rx),
            delivering: Arc::default(),
            reconfigured: tokio::sync::Notify::new(),
        })
    }

    pub fn handle(&self) -> Arc<LineHandle> {
        self.handle.clone()
    }

    async fn connect(url: &str, token: &str) -> Result<Ws, String> {
        let mut req = url
            .into_client_request()
            .map_err(|_| "The hub address is not valid.".to_owned())?;
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| "The relay token is not valid.".to_owned())?,
        );
        let mut cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
        cfg.max_message_size = Some(MAX_FRAME);
        cfg.max_frame_size = Some(MAX_FRAME);
        match tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_tungstenite::connect_async_with_config(req, Some(cfg), false),
        )
        .await
        {
            Ok(Ok((ws, _))) => Ok(ws),
            // Never the error's text: it can carry the request.
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r))) => Err(format!(
                "the hub refused the line (HTTP {})",
                r.status().as_u16()
            )),
            Ok(Err(_)) => Err("the hub could not be reached".into()),
            Err(_) => Err("the hub did not answer in time".into()),
        }
    }

    /// Keep the line up until the task is dropped.
    pub async fn run(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let cfg = (self.config)();
            if !cfg.usable() {
                self.handle.set(LineState::Off);
                tokio::select! {
                    _ = self.reconfigured.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                }
                continue;
            }
            self.handle.set(LineState::Connecting);
            let mut connected = None;
            for url in cfg.urls() {
                match Self::connect(&url, &cfg.token).await {
                    Ok(ws) => {
                        connected = Some(ws);
                        break;
                    }
                    Err(e) => log::info!("relay: {e}"),
                }
            }
            match connected {
                Some(ws) => {
                    backoff = Duration::from_secs(1);
                    self.handle.set(LineState::Online);
                    log::info!("relay: line up");
                    self.serve(ws).await;
                    log::info!("relay: line down");
                    self.handle.set(LineState::Offline);
                }
                None => self.handle.set(LineState::Offline),
            }
            tokio::select! {
                _ = self.reconfigured.notified() => backoff = Duration::from_secs(1),
                _ = tokio::time::sleep(backoff) => backoff = (backoff * 2).min(MAX_BACKOFF),
            }
        }
    }

    /// Serve one connection until it ends (or the configuration changes).
    pub async fn serve(&self, ws: Ws) {
        let (mut sink_ws, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
        let mut up_rx = self.up_rx.lock().await;
        let silence = tokio::time::sleep(SILENCE);
        tokio::pin!(silence);
        loop {
            tokio::select! {
                frame = stream.next() => {
                    silence.as_mut().reset(tokio::time::Instant::now() + SILENCE);
                    match frame {
                        Some(Ok(Message::Text(t))) => self.on_frame(t.as_str(), &out_tx),
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => {} // pings are answered by the library
                    }
                }
                Some(v) = out_rx.recv() => {
                    if sink_ws.send(Message::Text(v.to_string().into())).await.is_err() { break; }
                }
                Some(v) = up_rx.recv() => {
                    if sink_ws.send(Message::Text(v.to_string().into())).await.is_err() { break; }
                }
                _ = &mut silence => break,
                _ = self.reconfigured.notified() => break,
            }
        }
        let _ = sink_ws.close().await;
    }

    fn on_frame(&self, text: &str, out: &mpsc::UnboundedSender<Value>) {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        match v.get("type").and_then(Value::as_str) {
            Some("call") => {
                let (Some(id), Some(tool)) = (s("id"), s("tool")) else {
                    return;
                };
                let caller = s("client").unwrap_or_default();
                let args = v.get("args").cloned().unwrap_or(Value::Null);
                let executor = self.executor.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let reply = match gate::admit(&tool, &args) {
                        Err(text) => json!({ "type": "result", "id": id, "ok": false,
                                             "error": text, "changed": "no" }),
                        Ok(admitted) => match executor.run(&caller, admitted).await {
                            Ok(r) => json!({ "type": "result", "id": id, "ok": true, "result": r }),
                            Err(f) => {
                                json!({ "type": "result", "id": id, "ok": false, "error": f.text,
                                              "changed": if f.changed { "maybe" } else { "no" } })
                            }
                        },
                    };
                    let _ = out.send(reply);
                });
            }
            Some("telegram") => {
                let (Some(id), Some(text)) = (s("id"), s("text")) else {
                    return;
                };
                let delivering = self.delivering.clone();
                if !delivering
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone())
                {
                    return;
                }
                let msg = TelegramMessage {
                    id: id.clone(),
                    text,
                    sent_at: v.get("sentAt").and_then(Value::as_i64).unwrap_or(0),
                    age_minutes: v.get("ageMinutes").and_then(Value::as_i64),
                    bot: s("bot"),
                };
                let sink = self.sink.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let r = sink.deliver(msg).await;
                    delivering
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&id);
                    match r {
                        Ok(()) => {
                            let _ = out.send(json!({ "type": "telegram_ack", "id": id }));
                        }
                        // Not acknowledged: the hub keeps it and sends it again on the next line.
                        Err(e) => log::warn!("relay: a Telegram message was not delivered: {e}"),
                    }
                });
            }
            Some("telegram_stop") => {
                let sink = self.sink.clone();
                tokio::spawn(async move { sink.stop().await });
            }
            Some("telegram_answer") => {
                let (Some(id), Some(index)) = (s("id"), v.get("index").and_then(Value::as_u64))
                else {
                    return;
                };
                let sink = self.sink.clone();
                tokio::spawn(async move { sink.answer(id, index as usize).await });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    const TOKEN: &str = "relay-test-token-0123456789abcdef";

    #[derive(Default)]
    struct Exec {
        ran: Mutex<Vec<(String, gate::Admitted)>>,
    }
    impl Executor for Exec {
        fn run<'a>(
            &'a self,
            caller: &'a str,
            call: gate::Admitted,
        ) -> BoxFut<'a, Result<Value, ToolFailure>> {
            self.ran.lock().unwrap().push((caller.to_owned(), call));
            Box::pin(async { Ok(json!({ "chats": [] })) })
        }
    }

    #[derive(Default)]
    struct Sink {
        got: Mutex<Vec<TelegramMessage>>,
        stops: Mutex<usize>,
        refuse: bool,
    }
    impl TelegramSink for Sink {
        fn deliver<'a>(&'a self, msg: TelegramMessage) -> BoxFut<'a, Result<(), String>> {
            self.got.lock().unwrap().push(msg);
            let r = if self.refuse {
                Err("no".to_owned())
            } else {
                Ok(())
            };
            Box::pin(async move { r })
        }
        fn stop<'a>(&'a self) -> BoxFut<'a, ()> {
            *self.stops.lock().unwrap() += 1;
            Box::pin(async {})
        }
        fn answer<'a>(&'a self, _id: String, _index: usize) -> BoxFut<'a, ()> {
            Box::pin(async {})
        }
    }

    #[allow(clippy::result_large_err)] // the handshake callback's error type is the library's
    /// A one-connection fake hub. Returns its URL, the Authorization header it saw, and the
    /// accepted socket.
    async fn fake_hub() -> (
        String,
        tokio::task::JoinHandle<(
            Option<String>,
            tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        )>,
    ) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/relay", l.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let seen = Arc::new(Mutex::new(None));
            let seen2 = seen.clone();
            let ws =
                tokio_tungstenite::accept_hdr_async(s, move |req: &Request, resp: Response| {
                    *seen2.lock().unwrap() = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    Ok(resp)
                })
                .await
                .unwrap();
            let auth = seen.lock().unwrap().clone();
            (auth, ws)
        });
        (url, task)
    }

    async fn next(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Value {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Message::Text(t) = m {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }

    fn client(urls: (&str, &str), exec: Arc<Exec>, sink: Arc<Sink>) -> Arc<Client> {
        let cfg = RelayConfig {
            lan_url: urls.0.to_owned(),
            fallback_url: urls.1.to_owned(),
            token: TOKEN.to_owned(),
        };
        Client::new(Arc::new(move || cfg.clone()), exec, sink)
    }

    #[tokio::test]
    async fn the_line_authenticates_gates_calls_and_acknowledges_telegram() {
        let (url, hub) = fake_hub().await;
        // The LAN address is dead: the fallback is used.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let a = l.local_addr().unwrap();
            drop(l);
            format!("ws://{a}/relay")
        };
        let exec = Arc::new(Exec::default());
        let sink = Arc::new(Sink::default());
        let c = client((&dead, &url), exec.clone(), sink.clone());
        let task = tokio::spawn(c.clone().run());
        let (auth, mut ws) = hub.await.unwrap();
        assert_eq!(auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));

        // A denied tool is refused by the PC's gate and never runs.
        ws.send(Message::Text(
            json!({"type": "call", "id": "1", "client": "remote",
            "tool": "app_update_settings", "args": {}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let r = next(&mut ws).await;
        assert_eq!(
            (r["id"].as_str(), r["ok"].as_bool(), r["changed"].as_str()),
            (Some("1"), Some(false), Some("no"))
        );
        assert!(r["error"]
            .as_str()
            .unwrap()
            .contains("not available to callers outside Helm"));
        // A refused argument too.
        ws.send(Message::Text(
            json!({"type": "call", "id": "2", "client": "remote",
            "tool": "app_read_chat", "args": {"chatId": "c", "includeTools": true}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        assert_eq!(next(&mut ws).await["changed"], "no");
        assert!(exec.ran.lock().unwrap().is_empty(), "nothing ran");
        // An allowed one runs, attributed to the hub's client name.
        ws.send(Message::Text(
            json!({"type": "call", "id": "3", "client": "remote",
            "tool": "app_list_chats", "args": {"query": "x"}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let r = next(&mut ws).await;
        assert_eq!(r["ok"], true);
        assert_eq!(exec.ran.lock().unwrap()[0].0, "remote");

        // Telegram: delivered, then acknowledged.
        ws.send(Message::Text(
            json!({"type": "telegram", "id": "m1", "text": "hi",
            "sentAt": 5, "ageMinutes": 3, "bot": "b"})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let ack = next(&mut ws).await;
        assert_eq!(ack, json!({"type": "telegram_ack", "id": "m1"}));
        assert_eq!(sink.got.lock().unwrap()[0].age_minutes, Some(3));
        ws.send(Message::Text(
            json!({"type": "telegram_stop"}).to_string().into(),
        ))
        .await
        .unwrap();
        // Status goes up the line.
        c.handle().telegram_status("Running tests");
        assert_eq!(
            next(&mut ws).await,
            json!({"type": "telegram_status", "text": "Running tests"})
        );
        for _ in 0..100 {
            if *sink.stops.lock().unwrap() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*sink.stops.lock().unwrap(), 1);
        assert_eq!(c.handle().state(), LineState::Online);
        task.abort();
    }

    #[tokio::test]
    async fn an_undelivered_telegram_message_is_not_acknowledged() {
        let (url, hub) = fake_hub().await;
        let sink = Arc::new(Sink {
            refuse: true,
            ..Default::default()
        });
        let c = client((&url, ""), Arc::new(Exec::default()), sink.clone());
        let task = tokio::spawn(c.clone().run());
        let (_, mut ws) = hub.await.unwrap();
        ws.send(Message::Text(
            json!({"type": "telegram", "id": "m1", "text": "hi"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        // Let the delivery attempt finish before the marker goes up.
        for _ in 0..100 {
            if !sink.got.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.handle().telegram_final("marker");
        // The next frame up is the marker, not an ack.
        assert_eq!(next(&mut ws).await["type"], "telegram_final");
        assert_eq!(sink.got.lock().unwrap().len(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn without_a_configuration_the_line_stays_off() {
        let c = Client::new(
            Arc::new(RelayConfig::default),
            Arc::new(Exec::default()),
            Arc::new(Sink::default()),
        );
        let task = tokio::spawn(c.clone().run());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(c.handle().state(), LineState::Off);
        task.abort();
    }
}
