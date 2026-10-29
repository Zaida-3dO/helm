//! The service against in-process fake servers on ephemeral loopback ports. No real Strata or
//! Ollama is ever contacted: every address in these tests is one a fake bound a moment ago.

use crate::gpu::Gpu;
use crate::service::{override_from, LocalService, Override, Timings};
use crate::settings::{self, Changes, ServerInput};
use helm_chats::local::{LocalModels, LocalTurn};
use helm_chats::types::LocalServerKind;
use helm_core::credentials::CredentialStore;
use helm_core::db::{Database, MigrationRegistry};
use helm_ipc::{EventSink, RecordingSink};
use helm_platform::keychain::MemoryKeychain;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const GB: u64 = 1024 * 1024 * 1024;
const KEY: &str = "sk-fake-test-key";

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    auth: String,
    body: Value,
}

/// A tiny HTTP server on a loopback port that answers by `route` and records every request.
struct Fake {
    base: String,
    log: Arc<Mutex<Vec<Req>>>,
}

impl Fake {
    fn start(route: impl Fn(&Req) -> (u16, Value) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log: Arc<Mutex<Vec<Req>>> = Arc::default();
        let route = Arc::new(route);
        let log2 = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (route, log) = (route.clone(), log2.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let (method, path) = (
                        parts.next().unwrap_or("").to_owned(),
                        parts.next().unwrap_or("").to_owned(),
                    );
                    let (mut auth, mut len) = (String::new(), 0usize);
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                            break;
                        }
                        let lower = h.to_ascii_lowercase();
                        if lower.starts_with("authorization:") {
                            auth = h[14..].trim().to_owned();
                        }
                        if lower.starts_with("content-length:") {
                            len = h[15..].trim().parse().unwrap_or(0);
                        }
                    }
                    let mut raw = vec![0u8; len];
                    let _ = reader.read_exact(&mut raw);
                    let req = Req {
                        method,
                        path,
                        auth,
                        body: serde_json::from_slice(&raw).unwrap_or(Value::Null),
                    };
                    log.lock().unwrap().push(req.clone());
                    let (status, body) = route(&req);
                    let body = body.to_string();
                    let out = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(out.as_bytes());
                });
            }
        });
        Self { base, log }
    }

    fn calls(&self, method: &str, path: &str) -> Vec<Req> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method && r.path == path)
            .cloned()
            .collect()
    }

    fn total(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

/// A loopback address nothing listens on.
fn dead_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", l.local_addr().unwrap())
}

#[derive(Default)]
struct OllamaState {
    installed: Vec<(String, u64)>,
    loaded: Vec<String>,
}

fn fake_ollama(state: Arc<Mutex<OllamaState>>) -> Fake {
    Fake::start(move |r| {
        let mut st = state.lock().unwrap();
        let name = r.body.get("model").and_then(Value::as_str).unwrap_or("").to_owned();
        match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/api/tags") => (
                200,
                json!({"models": st.installed.iter().map(|(n, s)| json!({"name": n, "size": s})).collect::<Vec<_>>()}),
            ),
            ("GET", "/api/ps") => (
                200,
                json!({"models": st.loaded.iter().map(|n| json!({"name": n, "size_vram": 5 * GB})).collect::<Vec<_>>()}),
            ),
            ("POST", "/api/show") => {
                let ctx = if name.starts_with("llama3") { 131_072 } else { 40_960 };
                (200, json!({"model_info": {"general.architecture": "x", "x.context_length": ctx}}))
            }
            ("POST", "/api/create") => {
                st.installed.push((name, 5 * GB));
                (200, json!({"status": "success"}))
            }
            ("DELETE", "/api/delete") => {
                st.installed.retain(|(n, _)| *n != name);
                (200, json!({}))
            }
            ("POST", "/api/generate") => {
                if r.body["keep_alive"] == json!(0) {
                    st.loaded.retain(|n| *n != name);
                } else if !st.loaded.contains(&name) {
                    st.loaded.push(name);
                }
                (200, json!({"done": true}))
            }
            _ => (404, json!({"error": "not found"})),
        }
    })
}

struct StrataState {
    loaded: Option<String>,
}

/// Strata wants the key; unloading hands 12 GB back to `free`.
fn fake_strata(state: Arc<Mutex<StrataState>>, free: Arc<AtomicU64>) -> Fake {
    Fake::start(move |r| {
        if r.auth != format!("Bearer {KEY}") {
            return (401, json!({"error": "bad key"}));
        }
        let mut st = state.lock().unwrap();
        match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/health") => (200, json!({"status": "ok", "loaded": st.loaded})),
            ("GET", "/v1/models") => (200, json!({"data": [{"id": "qwen-flash", "max_model_len": 262_144}]})),
            ("POST", "/unload") => {
                if st.loaded.take().is_some() {
                    free.fetch_add(12 * GB, Ordering::SeqCst);
                }
                (200, json!({}))
            }
            _ => (404, json!({})),
        }
    })
}

struct FakeGpu(Arc<AtomicU64>);

impl Gpu for FakeGpu {
    fn free_bytes(&self) -> Option<u64> {
        Some(self.0.load(Ordering::SeqCst))
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    svc: LocalService,
    db: Arc<Database>,
    free: Arc<AtomicU64>,
    sink: Arc<RecordingSink>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    helm_chats::register_migrations(&mut reg);
    crate::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
    let creds = CredentialStore::new(&dir.path().join("credentials"), "p", Arc::new(MemoryKeychain::default()));
    let free = Arc::new(AtomicU64::new(4 * GB));
    let sink = Arc::new(RecordingSink::default());
    let events: Arc<dyn EventSink> = sink.clone();
    let svc = LocalService::new(
        db.clone(),
        creds,
        events,
        Arc::new(FakeGpu(free.clone())),
        dir.path().join("catalogue").join("local.json"),
    )
    .with_timings(Timings {
        memory_wait: Duration::from_millis(200),
        memory_poll: Duration::from_millis(20),
    });
    Rig { _dir: dir, svc, db, free, sink }
}

fn servers(strata: &str, ollama: &str) -> Changes {
    let input = |id: &str, name: &str, kind, url: &str| ServerInput {
        id: Some(id.into()),
        name: name.into(),
        kind,
        base_url: url.into(),
        enabled: true,
        model: None,
    };
    Changes {
        servers: Some(vec![
            input("strata", "Strata", LocalServerKind::Strata, strata),
            input("ollama", "Ollama", LocalServerKind::Ollama, ollama),
        ]),
        ..Default::default()
    }
}

fn turn(server: &str, model: &str) -> LocalTurn {
    LocalTurn {
        chat_id: "c-new".into(),
        server_id: server.into(),
        model: model.into(),
        launched: None,
        in_use: Vec::new(),
    }
}

fn busy_chat(db: &Database, id: &str, title: &str, provider: &str, model: &str) {
    db.write(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
            [],
        )?;
        tx.execute(
            "INSERT INTO chats(id, project_id, title, status, provider, model, created_at, updated_at)
             VALUES (?1, 'p', ?2, 'running', ?3, ?4, 1, 1)",
            rusqlite::params![id, title, provider, model],
        )
    })
    .unwrap();
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(3);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_catalogue_is_asked_for_only_on_request_and_keys_stay_hidden() {
    let r = rig();
    let ollama = Arc::new(Mutex::new(OllamaState {
        installed: vec![
            ("qwen3:8b".into(), 5 * GB),
            ("llama3:latest".into(), 4 * GB),
            ("qwen3:8b-helm-32k".into(), 5 * GB),
        ],
        loaded: vec!["llama3:latest".into()],
    }));
    let strata = Arc::new(Mutex::new(StrataState { loaded: Some("qwen-flash".into()) }));
    let (o, s) = (fake_ollama(ollama.clone()), fake_strata(strata, r.free.clone()));
    r.svc.save(&servers(&s.base, &o.base)).unwrap();

    // Reading the catalogue, the state and the settings contacts nobody.
    assert!(r.svc.catalogue(false).is_empty());
    let state = r.svc.view();
    assert_eq!(state.servers.len(), 2);
    assert_eq!((o.total(), s.total()), (0, 0), "nothing is asked until someone asks");

    // Without the key Strata refuses; the picker still gets Ollama's models.
    let rows = r.svc.catalogue(true);
    let names: Vec<&str> = rows.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["qwen3:8b", "llama3:latest"], "Helm's own variants are hidden");
    let q = &rows[0];
    assert_eq!(q.id, "local/ollama/qwen3:8b");
    assert_eq!(q.server, "Ollama");
    assert_eq!(q.context_tokens, Some(40_960), "the model's own limit, under the setting");
    assert_eq!((q.loaded, q.reachable, q.size_bytes), (Some(false), true, Some(5 * GB)));
    assert_eq!(rows[1].context_tokens, Some(65_536), "the setting, under the model's limit");
    assert_eq!(rows[1].loaded, Some(true));
    let view = r.svc.view();
    assert_eq!(view.servers[0].reachable, Some(false));
    assert!(view.servers[0].note.as_deref().unwrap().contains("refused the key"));
    assert!(!view.servers[0].has_key);

    // The key goes to Strata as a bearer token and to nobody else; no result carries it.
    r.svc.set_key("strata", &format!("  {KEY} ")).unwrap();
    assert!(r.svc.set_key("nope", "x").is_err());
    assert!(r.svc.set_key("strata", "two\nlines").is_err());
    let rows = r.svc.catalogue(true);
    let strata_row = rows.iter().find(|m| m.kind == LocalServerKind::Strata).unwrap();
    assert_eq!(strata_row.id, "local/strata/qwen-flash");
    assert_eq!((strata_row.context_tokens, strata_row.loaded, strata_row.reachable), (Some(262_144), Some(true), true));
    assert!(o.log.lock().unwrap().iter().all(|q| q.auth.is_empty()), "Ollama never sees Strata's key");
    let shown = serde_json::to_string(&r.svc.view()).unwrap();
    assert!(!shown.contains(KEY) && r.svc.view().servers[0].has_key);
    // The cache is on disk and survives a restart.
    assert!(r.svc.catalogue(false).len() >= 3);
    assert!(r._dir.path().join("catalogue").join("local.json").is_file());

    // Test reports a sentence either way.
    let t = r.svc.test("ollama").unwrap();
    assert!(t.ok && t.message.contains("2 models") && t.models == Some(2), "{t:?}");
    r.svc.forget_key("strata").unwrap();
    assert!(!r.svc.test("strata").unwrap().ok);
    assert!(r.svc.test("nope").is_err());

    // A server that stops answering stays listed, greyed, with a reason.
    r.svc.save(&servers(&s.base, &dead_url())).unwrap();
    let rows = r.svc.catalogue(true);
    let gone = rows.iter().find(|m| m.name == "qwen3:8b").unwrap();
    assert!(!gone.reachable && gone.loaded.is_none());
    assert!(r.svc.view().servers[1].note.as_deref().unwrap().contains("did not answer"));
    // Switching a server off hides its models; removing one also removes its key.
    let mut off = servers(&s.base, &o.base);
    off.servers.as_mut().unwrap()[1].enabled = false;
    r.svc.save(&off).unwrap();
    assert!(r.svc.catalogue(false).iter().all(|m| m.kind == LocalServerKind::Strata));
    assert!(r.sink.kinds().iter().any(|k| k == "local_models"));

    // The test override: release builds ignore it; `off` and a bad address contact nobody.
    for (raw, debug, want) in [
        (None, true, None),
        (Some("off"), false, None),
        (Some("off"), true, Some(Override::Off)),
        (Some("http://127.0.0.1:9"), true, Some(Override::Url("http://127.0.0.1:9".into()))),
        (Some("http://example.com:9"), true, Some(Override::Off)),
        (Some(" "), true, None),
    ] {
        assert_eq!(override_from(raw, debug), want, "{raw:?} {debug}");
    }
    let conn = crate::net::Conn { base: crate::net::OFF.into(), key: None };
    let err = crate::net::Http::default().get(&conn, "/health", crate::net::QUICK).unwrap_err();
    assert!(matches!(err, crate::net::NetError::Unreachable(_)));
}

#[test]
fn a_turn_gets_its_model_ready_and_the_memory_checked_first() {
    let r = rig();
    let ollama = Arc::new(Mutex::new(OllamaState {
        installed: vec![("qwen3:8b".into(), 5 * GB), ("qwen3:8b-helm-32k".into(), 5 * GB)],
        loaded: vec![],
    }));
    let strata = Arc::new(Mutex::new(StrataState { loaded: Some("qwen-flash".into()) }));
    let (o, s) = (fake_ollama(ollama.clone()), fake_strata(strata.clone(), r.free.clone()));
    r.svc.save(&servers(&s.base, &o.base)).unwrap();
    r.svc.set_key("strata", KEY).unwrap();
    let lines = Mutex::new(Vec::<String>::new());
    let progress = |l: &str| lines.lock().unwrap().push(l.to_owned());

    // 1. A model that needs 6 GB with 4 free: Strata's idle model is unloaded, the variant made
    //    for the context setting, the older one removed, the model loaded with Helm's keep-alive.
    let mut t = turn("ollama", "qwen3:8b");
    t.in_use = vec!["somebody-else:1b-helm-8k".into()];
    let name = r.svc.prepare(&t, &progress).unwrap();
    assert_eq!(name, "qwen3:8b-helm-64k");
    assert_eq!(lines.lock().unwrap().as_slice(), ["Loading qwen3:8b…"]);
    assert_eq!(s.calls("POST", "/unload").len(), 1);
    let made = o.calls("POST", "/api/create");
    assert_eq!(made[0].body, json!({"model": "qwen3:8b-helm-64k", "from": "qwen3:8b", "parameters": {"num_ctx": 65_536}, "stream": false}));
    let removed = o.calls("DELETE", "/api/delete");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].body, json!({"model": "qwen3:8b-helm-32k"}));
    let load = &o.calls("POST", "/api/generate")[0];
    assert_eq!((load.body["model"].as_str(), load.body["keep_alive"].as_str()), (Some("qwen3:8b-helm-64k"), Some("15m")));
    assert!(load.body.get("prompt").is_none(), "loading sends no prompt");

    // 2. Already loaded: no line, no check, only the keep-alive timer is set again. A variant a
    //    worker still uses is never removed.
    lines.lock().unwrap().clear();
    ollama.lock().unwrap().installed.push(("qwen3:8b-helm-16k".into(), 5 * GB));
    let mut t = turn("ollama", "qwen3:8b");
    t.launched = Some("qwen3:8b-helm-64k".into());
    t.in_use = vec!["qwen3:8b-helm-16k".into()];
    assert_eq!(r.svc.prepare(&t, &progress).unwrap(), "qwen3:8b-helm-64k");
    assert!(lines.lock().unwrap().is_empty());
    assert_eq!(o.calls("DELETE", "/api/delete").len(), 1, "the variant in use stays");
    assert_eq!(s.calls("POST", "/unload").len(), 1);

    // 3. Strata's model is mid-reply in another chat: no turn, and nothing of Strata's is unloaded.
    ollama.lock().unwrap().loaded.clear();
    strata.lock().unwrap().loaded = Some("qwen-flash".into());
    r.free.store(4 * GB, Ordering::SeqCst);
    busy_chat(&r.db, "c-busy", "Refactor parser", "strata", "qwen-flash");
    let loads_before = o.calls("POST", "/api/generate").len();
    let why = r.svc.prepare(&turn("ollama", "qwen3:8b"), &progress).unwrap_err();
    assert_eq!(
        why,
        "qwen3:8b can't load while Strata is answering in Refactor parser. Send again when it finishes."
    );
    assert_eq!(s.calls("POST", "/unload").len(), 1, "a model that is answering is left alone");
    assert_eq!(o.calls("POST", "/api/generate").len(), loads_before);

    // 4. Strata: its model loads on the first request, so only the line and the check happen.
    let mut mine = turn("strata", "qwen-flash");
    mine.chat_id = "c-busy".into();
    strata.lock().unwrap().loaded = Some("qwen-flash".into());
    lines.lock().unwrap().clear();
    assert_eq!(r.svc.prepare(&mine, &progress).unwrap(), "qwen-flash");
    assert!(lines.lock().unwrap().is_empty(), "already loaded");
    strata.lock().unwrap().loaded = None;
    assert_eq!(r.svc.prepare(&mine, &progress).unwrap(), "qwen-flash");
    assert_eq!(lines.lock().unwrap().as_slice(), ["Loading qwen-flash…"]);

    // 5. After a turn the timer is set again, but only for a model that is still loaded.
    ollama.lock().unwrap().loaded.push("qwen3:8b-helm-64k".into());
    let before = o.calls("POST", "/api/generate").len();
    r.svc.finished("ollama", "qwen3:8b-helm-64k");
    wait_for("the keep-alive reset", || o.calls("POST", "/api/generate").len() > before);
    ollama.lock().unwrap().loaded.clear();
    let before = o.calls("POST", "/api/generate").len();
    r.svc.finished("ollama", "qwen3:8b-helm-64k");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(o.calls("POST", "/api/generate").len(), before, "an unloaded model is not loaded again");

    // 6. Problems read as sentences, and a server that is off or gone starts no turn.
    assert!(r.svc.prepare(&turn("ollama", "missing:1b"), &progress).unwrap_err().contains("not installed"));
    assert!(r.svc.prepare(&turn("nope", "x"), &progress).unwrap_err().contains("not set up"));
    r.svc.save(&servers(&s.base, &dead_url())).unwrap();
    assert!(r.svc.prepare(&turn("ollama", "qwen3:8b"), &progress).unwrap_err().contains("did not answer"));

    // 7. What a worker is launched with: the key (or a stand-in), the window and lean tools.
    r.svc.save(&servers(&s.base, &o.base)).unwrap();
    let launch = r.svc.server("ollama", "qwen3:8b-helm-64k").unwrap();
    assert_eq!((launch.base_url.as_str(), launch.key.as_str()), (o.base.as_str(), "local"));
    assert_eq!((launch.context_tokens, launch.max_output_tokens, launch.lean), (Some(65_536), Some(16_384), true));
    let launch = r.svc.server("strata", "qwen-flash").unwrap();
    assert_eq!(launch.key, KEY);
    assert!(!format!("{launch:?}").contains(KEY), "a launch never prints its key");
    assert!(r.svc.knows("strata") && !r.svc.knows("other"));
    let mut off = servers(&s.base, &o.base);
    off.servers.as_mut().unwrap()[0].enabled = false;
    r.svc.save(&off).unwrap();
    assert!(r.svc.server("strata", "qwen-flash").unwrap_err().contains("switched off"));
    let none = settings::LocalSettings::default();
    assert_eq!(none.compare_with.key(), "sonnet");
}
