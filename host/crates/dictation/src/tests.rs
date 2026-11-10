//! Two broad tests. The first is a table of the small pure rules (the address rule, names and
//! languages, reading a reply, the silent recording). The second walks the whole feature against
//! in-process stand-ins on ephemeral loopback ports: a speech-to-text server and an Ollama. No
//! real Whisper, Strata or Ollama is ever contacted, and no recording leaves the machine.

use crate::settings::{check_stt_url, language_ok, model_ok};
use crate::transcribe::{read_reply, silent_wav, transcribe_with, Failure, MAX_WAV_BYTES};
use crate::{ops, register_host_tools, register_migrations, settings};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use helm_chats::types::LocalServerKind;
use helm_core::credentials::CredentialStore;
use helm_core::db::{Database, MigrationRegistry};
use helm_hosttools::{CallOrigin, HostToolRegistry, ToolCtx, ToolHandler, ToolResult};
use helm_ipc::{ApiError, EventSink, OpCtx, RecordingSink, Services};
use helm_localmodels::gpu::Gpu;
use helm_localmodels::service::Timings;
use helm_localmodels::settings::{Changes, ServerInput};
use helm_localmodels::LocalService;
use helm_platform::keychain::MemoryKeychain;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const GB: u64 = 1024 * 1024 * 1024;

#[test]
fn the_small_rules() {
    // Where audio may go: this computer always, the user's network only on a tick, nothing else.
    for (raw, allow, want) in [
        ("http://127.0.0.1:8000", false, Ok("http://127.0.0.1:8000")),
        (
            "  HTTP://LocalHost:8000/  ",
            false,
            Ok("http://localhost:8000"),
        ),
        (
            "http://127.0.0.1:8000/v1",
            false,
            Ok("http://127.0.0.1:8000"),
        ),
        (
            "http://127.0.0.1:8000/v1/",
            false,
            Ok("http://127.0.0.1:8000"),
        ),
        ("http://[::1]:8000", false, Ok("http://[::1]:8000")),
        ("http://127.0.0.1", false, Ok("http://127.0.0.1:80")),
        (
            "http://192.168.1.20:8000",
            true,
            Ok("http://192.168.1.20:8000"),
        ),
        ("http://10.1.2.3:8000", true, Ok("http://10.1.2.3:8000")),
        ("http://172.20.0.5:8000", true, Ok("http://172.20.0.5:8000")),
        (
            "http://100.101.102.103:8000",
            true,
            Ok("http://100.101.102.103:8000"),
        ),
        ("http://[fd00::5]:8000", true, Ok("http://[fd00::5]:8000")),
        ("http://127.0.0.1:8000", true, Ok("http://127.0.0.1:8000")),
        ("http://192.168.1.20:8000", false, Err("Tick")),
        ("http://100.64.0.1:8000", false, Err("Tick")),
        ("http://[fe80::1]:8000", false, Err("Tick")),
        ("http://8.8.8.8:8000", true, Err("not on this computer")),
        ("http://172.32.0.1:8000", true, Err("not on this computer")),
        ("http://100.128.0.1:8000", true, Err("not on this computer")),
        ("http://0.0.0.0:8000", true, Err("not on this computer")),
        (
            "http://whisper.local:8000",
            true,
            Err("not on this computer"),
        ),
        (
            "http://localhost.evil.test:8000",
            true,
            Err("not on this computer"),
        ),
        (
            "http://[::ffff:8.8.8.8]:8000",
            true,
            Err("not on this computer"),
        ),
        ("https://127.0.0.1:8000", true, Err("http://")),
        ("ftp://127.0.0.1", true, Err("http://")),
        ("", true, Err("http://")),
        ("http://127.0.0.1:8000@evil.test:2", true, Err("parts")),
        ("http://user@127.0.0.1:8000", true, Err("parts")),
        ("http://127.0.0.1:8000/v1?x=1", true, Err("parts")),
        ("http://127.0.0.1:8000/other", true, Err("path")),
        ("http://127.0.0.1:0", true, Err("port")),
        ("http://127.0.0.1:99999", true, Err("port")),
        ("http://127.0.0.1:abc", true, Err("port")),
    ] {
        match (check_stt_url(raw, allow), want) {
            (Ok(got), Ok(w)) => assert_eq!(got, w, "{raw}"),
            (Err(e), Err(w)) => assert!(e.contains(w), "{raw}: {e}"),
            (got, want) => panic!("{raw} (allow {allow}): {got:?} vs {want:?}"),
        }
    }
    for (name, ok) in [
        ("large-v3-turbo", true),
        ("Systran/faster-whisper-large-v3", true),
        ("whisper-1", true),
        ("", false),
        ("two words", false),
        ("a\nb", false),
    ] {
        assert_eq!(model_ok(name), ok, "model {name:?}");
    }
    for (lang, ok) in [
        ("en", true),
        ("fr", true),
        ("pt-BR", true),
        ("auto", true),
        ("EN", false),
        ("english", false),
        ("e", false),
        ("", false),
    ] {
        assert_eq!(language_ok(lang), ok, "language {lang:?}");
    }
    // A reply is plain text, or JSON with a text field, or a JSON string.
    for (reply, want) in [
        ("hello there\n", "hello there"),
        ("  spaced out  ", "spaced out"),
        (r#"{"text": " from json "}"#, "from json"),
        (r#""just a string""#, "just a string"),
        (r#"{"other": 1}"#, r#"{"other": 1}"#),
        ("", ""),
    ] {
        assert_eq!(read_reply(reply), want, "{reply:?}");
    }
    // The recording the Test button sends: a real WAV header and a second of silence.
    let wav = silent_wav(1);
    assert_eq!(wav.len(), 44 + 32_000);
    assert_eq!((&wav[..4], &wav[8..16]), (&b"RIFF"[..], &b"WAVEfmt "[..]));
    assert!(wav[44..].iter().all(|b| *b == 0));
    // Every failure has a code the page keys on and a sentence with no audio or text in it.
    for (f, code) in [
        (Failure::NotSetUp, "not_set_up"),
        (Failure::Address("x".into()), "bad_address"),
        (Failure::TooBig, "too_big"),
        (Failure::NotWav, "bad_audio"),
        (Failure::Unreachable, "unreachable"),
        (Failure::Server(500), "server"),
        (Failure::Unreadable, "unreadable"),
    ] {
        assert_eq!(f.code(), code);
        assert!(!f.say().is_empty());
    }
}

// ---- in-process servers ---------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    content_type: String,
    body: Vec<u8>,
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// A tiny HTTP server on a loopback port. `route` answers `(status, body, delay in ms)`.
fn serve(route: impl Fn(&Seen) -> (u16, String, u64) + Send + Sync + 'static) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let log: Log = Arc::default();
    let (route, seen) = (Arc::new(route), log.clone());
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let (route, seen) = (route.clone(), seen.clone());
            std::thread::spawn(move || {
                let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && s.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let mut first = head.lines().next().unwrap_or("").split(' ');
                let (method, path) = (
                    first.next().unwrap_or("").to_owned(),
                    first.next().unwrap_or("").to_owned(),
                );
                let header = |name: &str| {
                    head.lines().find_map(|l| {
                        let (n, v) = l.split_once(':')?;
                        n.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
                    })
                };
                let len: usize = header("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                let _ = s.read_exact(&mut body);
                let req = Seen {
                    method,
                    path,
                    content_type: header("content-type").unwrap_or_default(),
                    body,
                };
                let (status, text, delay) = route(&req);
                seen.lock().unwrap().push(req);
                std::thread::sleep(Duration::from_millis(delay));
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            });
        }
    });
    (url, log)
}

/// The speech-to-text stand-in: answers by what `mode` holds.
fn stt(mode: Arc<Mutex<(u16, String, u64)>>) -> (String, Log) {
    serve(move |_| mode.lock().unwrap().clone())
}

/// An Ollama stand-in with one model loaded.
fn ollama() -> (String, Log) {
    serve(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/ps") => (
            200,
            json!({"models": [{"name": "m:1b", "size_vram": GB}]}).to_string(),
            0,
        ),
        ("POST", "/api/generate") => (200, "{}".into(), 0),
        _ => (404, "{}".into(), 0),
    })
}

struct FakeGpu(Arc<AtomicU64>);

impl Gpu for FakeGpu {
    fn free_bytes(&self) -> Option<u64> {
        Some(self.0.load(Ordering::SeqCst))
    }
}

struct Env {
    _dir: tempfile::TempDir,
    services: Arc<Services>,
    sink: Arc<RecordingSink>,
    models: Arc<LocalService>,
    free: Arc<AtomicU64>,
    tools: HostToolRegistry,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        helm_localmodels::register_migrations(&mut reg);
        register_migrations(&mut reg);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        let sink = Arc::new(RecordingSink::default());
        let events: Arc<dyn EventSink> = sink.clone();
        let free = Arc::new(AtomicU64::new(8 * GB));
        let models = Arc::new(
            LocalService::new(
                db.clone(),
                CredentialStore::new(
                    &dir.path().join("credentials"),
                    "test-profile",
                    Arc::new(MemoryKeychain::default()),
                ),
                events,
                Arc::new(FakeGpu(free.clone())),
                dir.path().join("catalogue").join("local.json"),
            )
            .with_timings(Timings {
                memory_wait: Duration::from_millis(100),
                memory_poll: Duration::from_millis(10),
                start_wait: Duration::from_millis(100),
                start_poll: Duration::from_millis(10),
            }),
        );
        // Speech-to-text is set up in these tests, so the speech reserve counts.
        models.set_speech_probe(Arc::new(|| true));
        // No model server is configured until a test points one at a stand-in.
        models
            .save(&Changes {
                servers: Some(Vec::new()),
                ..Default::default()
            })
            .unwrap();
        let mut s = Services::new();
        s.insert(db);
        s.insert(models.clone());
        let mut tools = HostToolRegistry::new();
        register_host_tools(&mut tools);
        Env {
            _dir: dir,
            services: Arc::new(s),
            sink,
            models,
            free,
            tools,
        }
    }

    fn op(&self) -> OpCtx {
        OpCtx {
            services: self.services.clone(),
            events: self.sink.clone(),
        }
    }

    fn use_ollama(&self, url: &str) {
        self.models
            .save(&Changes {
                servers: Some(vec![ServerInput {
                    id: Some("ollama".into()),
                    name: "Ollama".into(),
                    kind: LocalServerKind::Ollama,
                    base_url: url.into(),
                    enabled: true,
                    model: None,
                    auto_start: true,
                    folder: None,
                    preset: None,
                }]),
                ..Default::default()
            })
            .unwrap();
    }

    fn tool(&self, caller: &str) -> ToolResult {
        let ctx = ToolCtx {
            caller_chat_id: caller.into(),
            is_child: false,
            tool_call_id: None,
            origin: CallOrigin::Agent,
            services: self.services.clone(),
            events: self.sink.clone(),
            ops: None,
        };
        match &self.tools.get("app_make_room_for_speech").unwrap().handler {
            ToolHandler::Blocking(f) => f(&ctx, json!({})),
            ToolHandler::Async(_) => unreachable!("this tool is blocking"),
        }
    }
}

fn wav_b64() -> String {
    STANDARD.encode(silent_wav(1))
}

/// An error's code, with a check that it carries neither audio nor text.
fn code_of(r: Result<Value, ApiError>) -> String {
    let e = r.expect_err("an error");
    assert!(
        !e.message.contains("hello") && e.message.len() < 300,
        "an error echoed something: {}",
        e.message
    );
    e.code
}

fn unloads(log: &Log) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|r| {
            r.path == "/api/generate"
                && String::from_utf8_lossy(&r.body).contains("\"keep_alive\":0")
        })
        .count()
}

#[test]
fn a_recording_goes_to_the_server_and_text_comes_back() {
    let env = Env::new();
    let mode = Arc::new(Mutex::new((200, "hello world\n".to_owned(), 0)));
    let (server, seen) = stt(mode.clone());
    let say = |args: Value| ops::transcribe(&env.op(), args);

    // 1. Nothing is saved yet: dictation is not set up, and nothing is sent.
    let view = ops::get(&env.op(), json!({})).unwrap();
    assert_eq!(view["configured"], json!(false));
    assert_eq!(view["language"], json!("en"));
    assert_eq!(view["model"], json!("large-v3-turbo"));
    assert_eq!(view["speechReserveGb"], json!(2));
    assert_eq!(code_of(say(json!({"wavBase64": wav_b64()}))), "not_set_up");
    assert!(seen.lock().unwrap().is_empty());

    // 2. Saving checks the address, the names and the reserve, whole or not at all.
    let set = |args: Value| ops::set(&env.op(), args);
    for (args, why) in [
        (
            json!({"sttUrl": "http://example.com:8000"}),
            "not on this computer",
        ),
        (json!({"sttUrl": "https://127.0.0.1:8000"}), "http://"),
        (json!({"sttUrl": "http://192.168.1.20:8000"}), "Tick"),
        (json!({"language": "english"}), "language"),
        (json!({"model": "two words"}), "model"),
        (json!({"speechReserveGb": 99}), "from 0 to 24"),
        (json!({"surprise": 1}), "Expected"),
    ] {
        let e = set(args).unwrap_err();
        assert_eq!(e.code, "bad_args");
        assert!(e.message.contains(why), "{why}: {}", e.message);
    }
    let view = set(json!({"sttUrl": format!("{server}/v1"), "speechReserveGb": 4})).unwrap();
    assert_eq!(view["sttUrl"], json!(server));
    assert_eq!(view["configured"], json!(true));
    assert_eq!(view["speechReserveGb"], json!(4));
    assert_eq!(env.models.settings().speech_reserve_gb, 4);
    assert!(env.sink.kinds().contains(&"dictation".to_owned()));
    // A network address needs the tick, and the tick cannot be taken back while one is saved.
    set(json!({"allowPrivateNetwork": true, "sttUrl": "http://192.168.1.20:8000"})).unwrap();
    let e = set(json!({"allowPrivateNetwork": false})).unwrap_err();
    assert!(e.message.contains("Tick"), "{}", e.message);
    set(json!({"allowPrivateNetwork": false, "sttUrl": server.as_str()})).unwrap();

    // 3. A recording is posted as the multipart form a Whisper server takes, and the text read
    //    back is trimmed. Nothing but the audio and the three fields is sent.
    let got = say(json!({"wavBase64": wav_b64()})).unwrap();
    assert_eq!(got, json!({"text": "hello world"}));
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        ("POST", "/v1/audio/transcriptions")
    );
    assert!(req
        .content_type
        .starts_with("multipart/form-data; boundary="));
    let body = String::from_utf8_lossy(&req.body).into_owned();
    for part in [
        "name=\"model\"\r\n\r\nlarge-v3-turbo\r\n",
        "name=\"language\"\r\n\r\nen\r\n",
        "name=\"response_format\"\r\n\r\ntext\r\n",
        "name=\"file\"; filename=\"turn.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF",
    ] {
        assert!(body.contains(part), "missing {part:?}");
    }
    assert!(req.body.len() > 32_000, "the whole recording went");
    // JSON replies are read, an empty reply is no text, "auto" leaves the language out, and a
    // saved model name is the one sent.
    *mode.lock().unwrap() = (200, r#"{"text": " from json "}"#.into(), 0);
    assert_eq!(
        say(json!({"wavBase64": wav_b64()})).unwrap(),
        json!({"text": "from json"})
    );
    *mode.lock().unwrap() = (200, String::new(), 0);
    set(json!({"language": "auto", "model": "whisper-1"})).unwrap();
    assert_eq!(
        say(json!({"wavBase64": wav_b64()})).unwrap(),
        json!({"text": ""})
    );
    let body = String::from_utf8_lossy(&seen.lock().unwrap().last().unwrap().body).into_owned();
    assert!(!body.contains("name=\"language\"") && body.contains("whisper-1"));

    // 4. Problems are answered with a code and no echo, and the bad ones never reach the server.
    let sent = seen.lock().unwrap().len();
    let too_big = STANDARD.encode(vec![0u8; MAX_WAV_BYTES + 1]);
    assert_eq!(code_of(say(json!({"wavBase64": too_big}))), "too_big");
    assert_eq!(
        code_of(say(
            json!({"wavBase64": STANDARD.encode("hello, not a wav file at all, nope, no way, none")})
        )),
        "bad_audio"
    );
    assert_eq!(
        code_of(say(json!({"wavBase64": "!!! not base64 !!!"}))),
        "bad_audio"
    );
    assert_eq!(code_of(say(json!({"wavBase64": 7}))), "bad_args");
    assert_eq!(seen.lock().unwrap().len(), sent);
    *mode.lock().unwrap() = (503, "{}".into(), 0);
    assert_eq!(code_of(say(json!({"wavBase64": wav_b64()}))), "server");
    // Nothing listening: refused at once. Answering too slowly: given up on.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let mut s = settings::Settings {
        stt_url: Some(dead),
        ..Default::default()
    };
    let wav = silent_wav(1);
    assert_eq!(
        transcribe_with(&s, &wav, Duration::from_secs(5), || {}),
        Err(Failure::Unreachable)
    );
    *mode.lock().unwrap() = (200, "late".into(), 1_000);
    s.stt_url = Some(server.clone());
    assert_eq!(
        transcribe_with(&s, &wav, Duration::from_millis(200), || {}),
        Err(Failure::Unreachable)
    );
    // An address that stopped passing the rule is refused before anything is sent.
    s.stt_url = Some("http://192.168.1.20:8000".into());
    assert!(matches!(
        transcribe_with(&s, &wav, Duration::from_secs(1), || {}),
        Err(Failure::Address(_))
    ));

    // 5. The Test button sends one second of silence and answers, whatever comes back.
    *mode.lock().unwrap() = (200, "".into(), 0);
    let t = ops::test(&env.op(), json!({})).unwrap();
    assert_eq!(t["ok"], json!(true));
    let last = seen.lock().unwrap().last().unwrap().clone();
    assert!(last.body.len() > 32_000);
    *mode.lock().unwrap() = (500, "{}".into(), 0);
    let t = ops::test(&env.op(), json!({})).unwrap();
    assert_eq!(
        (
            t["ok"].clone(),
            t["message"].as_str().unwrap().contains("500")
        ),
        (json!(false), true)
    );
    set(json!({"sttUrl": ""})).unwrap();
    let t = ops::test(&env.op(), json!({})).unwrap();
    assert!(t["message"].as_str().unwrap().contains("isn't set up"));
    set(json!({"sttUrl": server.as_str()})).unwrap();
    *mode.lock().unwrap() = (200, "ok".into(), 0);

    // 6. A card with less free memory than the reserve has its idle models unloaded first; one
    //    with enough is left alone, and the recording goes either way.
    let (o_url, o_log) = ollama();
    env.use_ollama(&o_url);
    env.free.store(GB, Ordering::SeqCst); // under the 4 GB reserve set above
    assert_eq!(
        say(json!({"wavBase64": wav_b64()})).unwrap(),
        json!({"text": "ok"})
    );
    assert_eq!(unloads(&o_log), 1);
    env.free.store(16 * GB, Ordering::SeqCst);
    say(json!({"wavBase64": wav_b64()})).unwrap();
    assert_eq!(unloads(&o_log), 1, "no unload when the reserve is free");
    // A recording that is refused makes no room.
    env.free.store(GB, Ordering::SeqCst);
    assert_eq!(code_of(say(json!({"wavBase64": "AAAA"}))), "bad_audio");
    assert_eq!(unloads(&o_log), 1);

    // 7. The hub's tool: only a caller that came down the hub line, and it says what it freed.
    let why = env.tool("chat-1").unwrap_err().text();
    assert!(why.contains("hub"), "{why}");
    assert_eq!(unloads(&o_log), 1);
    let freed = env.tool("external:hub").unwrap();
    assert_eq!(freed["unloaded"], json!(1));
    assert_eq!(unloads(&o_log), 2);
}
