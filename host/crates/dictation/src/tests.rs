//! Three broad tests. The first is a table of the small pure rules (the address rule, names and
//! languages, reading a reply, the silent recording). The second walks the whole feature against
//! in-process stand-ins on ephemeral loopback ports: a speech-to-text server and an Ollama. The
//! third walks a live session against a scripted engine. No real Whisper, Strata, Ollama, speech
//! service or microphone is ever contacted, and no recording leaves the machine.

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
    // The language a live session really asks Windows for: only the tags the speech service
    // has a model for are usable, so the chosen code has to land on one of them.
    for (chosen, system, supported, want) in [
        // A bare short code is not usually a tag of its own: the system language of the same
        // language wins (the default settings: `en` on a PC with `en-GB`).
        ("en", "en-GB", &["en-GB", "en-US"][..], Some("en-GB")),
        // An exact supported tag wins, and in the service's own spelling.
        ("en-US", "en-GB", &["en-GB", "en-US"][..], Some("en-US")),
        ("en-us", "en-GB", &["en-GB", "en-US"][..], Some("en-US")),
        // No system language of that language: the first supported tag with its prefix.
        ("pt", "en-GB", &["en-GB", "pt-BR"][..], Some("pt-BR")),
        ("en-US", "de-DE", &["en-GB", "pt-BR"][..], Some("en-GB")),
        // Nothing of that language anywhere: leave the choice to Windows.
        ("fr", "en-GB", &["en-GB", "en-US"][..], None),
        ("en", "de-DE", &[][..], None),
    ] {
        let supported: Vec<String> = supported.iter().map(|t| (*t).to_owned()).collect();
        assert_eq!(
            crate::live::pick_speech_tag(chosen, system, &supported).as_deref(),
            want,
            "{chosen} on {system} with {supported:?}"
        );
    }
    // The languages a session tries, best first: the pick, other tags of the chosen language,
    // then Windows' own; never the same tag twice, whatever its case.
    for (chosen, system, supported, want) in [
        (
            Some("en"),
            Some("en-GB"),
            &["en-GB", "en-US"][..],
            &["en-GB", "en-US"][..],
        ),
        (
            Some("en-US"),
            Some("en-GB"),
            &["en-GB", "en-US"][..],
            &["en-US", "en-GB"][..],
        ),
        (Some("fr"), Some("en-GB"), &["en-GB"][..], &["en-GB"][..]),
        (
            Some("pt"),
            Some("en-GB"),
            &["en-GB", "pt-BR", "pt-PT"][..],
            &["pt-BR", "pt-PT", "en-GB"][..],
        ),
        (None, Some("en-GB"), &["en-GB", "en-US"][..], &["en-GB"][..]),
        (Some("en"), None, &["EN-us"][..], &["EN-us"][..]),
        (None, None, &[][..], &[][..]),
    ] {
        let supported: Vec<String> = supported.iter().map(|t| (*t).to_owned()).collect();
        assert_eq!(
            crate::live::speech_candidates(chosen, system, &supported),
            want,
            "{chosen:?} on {system:?} with {supported:?}"
        );
    }
    // The way dictation goes first: live unless the user picked recording; nothing else is a mode.
    for (mode, ok) in [
        ("live", true),
        ("record", true),
        ("Live", false),
        ("both", false),
        ("", false),
    ] {
        assert_eq!(settings::mode_ok(mode), ok, "mode {mode:?}");
    }
    assert_eq!(settings::Settings::default().mode, "live");
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
                    chat_cap: None,
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
    // Live is the way dictation goes first; the view also says why live cannot run, when it cannot.
    assert_eq!(view["mode"], json!("live"));
    assert_eq!(
        view["liveAvailable"].as_bool().unwrap(),
        view["liveUnavailable"].is_null()
    );
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
        (json!({"mode": "both"}), "live or record"),
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
    assert_eq!(
        set(json!({"mode": "record"})).unwrap()["mode"],
        json!("record")
    );
    assert_eq!(set(json!({"mode": "live"})).unwrap()["mode"], json!("live"));
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
        transcribe_with(&s, &wav, Duration::from_secs(5)),
        Err(Failure::Unreachable)
    );
    *mode.lock().unwrap() = (200, "late".into(), 1_000);
    s.stt_url = Some(server.clone());
    assert_eq!(
        transcribe_with(&s, &wav, Duration::from_millis(200)),
        Err(Failure::Unreachable)
    );
    // An address that stopped passing the rule is refused before anything is sent.
    s.stt_url = Some("http://192.168.1.20:8000".into());
    assert!(matches!(
        transcribe_with(&s, &wav, Duration::from_secs(1)),
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

    // 6. Speech-to-text never unloads a local model: with a model loaded and the card short of
    //    the 4 GB reserve set above, the recording goes straight to the server.
    let (o_url, o_log) = ollama();
    env.use_ollama(&o_url);
    env.free.store(GB, Ordering::SeqCst);
    assert_eq!(
        say(json!({"wavBase64": wav_b64()})).unwrap(),
        json!({"text": "ok"})
    );
    assert_eq!(unloads(&o_log), 0);
    assert!(
        o_log.lock().unwrap().is_empty(),
        "the model server is not even asked"
    );

    // 7. The hub's old tool: only a caller that came down the hub line, and it unloads nothing.
    let why = env.tool("chat-1").unwrap_err().text();
    assert!(why.contains("hub"), "{why}");
    let freed = env.tool("external:hub").unwrap();
    assert_eq!(freed, json!({"freedGb": 0.0, "unloaded": 0}));
    assert!(o_log.lock().unwrap().is_empty());
}

// ---- live dictation against a scripted engine -------------------------------------------------

#[test]
fn a_live_session_streams_settles_and_honours_stop_cancel_and_failure() {
    use crate::live::{Engine, EngineFactory, LiveDictation, LiveMsg};
    use helm_ipc::{AppEvent, RecordingSink, Services};
    use std::collections::VecDeque;

    // An engine fed from a script; it waits like the real one so the session loop keeps its shape.
    struct Script {
        msgs: VecDeque<LiveMsg>,
        fail_start: Option<String>,
    }
    impl Engine for Script {
        fn start(&mut self) -> Result<(), String> {
            match self.fail_start.take() {
                Some(m) => Err(m),
                None => Ok(()),
            }
        }
        fn next(&mut self, wait: std::time::Duration) -> Option<LiveMsg> {
            match self.msgs.pop_front() {
                Some(m) => Some(m),
                None => {
                    std::thread::sleep(std::time::Duration::from_millis(15).min(wait));
                    None
                }
            }
        }
        fn stop(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn cancel(&mut self) {
            self.msgs.clear();
        }
    }
    #[derive(Default)]
    struct Factory {
        queued: Mutex<VecDeque<LiveMsg>>,
        fail_start: Mutex<Option<String>>,
        /// Every language a start asked for, in order, so the settings hand-off is checked too.
        langs: Mutex<Vec<Option<String>>>,
    }
    impl EngineFactory for Factory {
        fn create(&self, language: Option<&str>) -> Result<Box<dyn Engine>, String> {
            self.langs.lock().unwrap().push(language.map(str::to_owned));
            Ok(Box::new(Script {
                msgs: std::mem::take(&mut *self.queued.lock().unwrap()),
                fail_start: self.fail_start.lock().unwrap().take(),
            }))
        }
    }

    // (kind, final, text/message) for every event so far, as one comparable table.
    fn wire(sink: &RecordingSink) -> Vec<(String, Option<bool>, String)> {
        let evs: Vec<AppEvent> = sink.events.lock().unwrap().clone();
        evs.into_iter()
            .map(|e| {
                let f = e.fields.get("final").and_then(|v| v.as_bool());
                let t = e
                    .fields
                    .get("text")
                    .or_else(|| e.fields.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned();
                (e.kind, f, t)
            })
            .collect()
    }
    fn wait_len(sink: &RecordingSink, want: usize) {
        for _ in 0..200 {
            if wire(sink).len() >= want {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // The op reads the saved settings for the language, so this service has a database too.
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("live.db"), &reg).unwrap());
    let factory = Arc::new(Factory::default());
    let mut services = Services::new();
    services.insert(db.clone());
    services.insert(Arc::new(LiveDictation::with_factory(factory.clone())));
    let sink = Arc::new(RecordingSink::default());
    let ctx = OpCtx {
        services: Arc::new(services),
        events: sink.clone(),
    };
    let start = |v: &OpCtx| ops::live(v, json!({"action": "start"}));

    // The real service re-states the whole sentence in progress on every hypothesis, never just
    // the new words; the fake feeds each whole sentence straight to the session the way the
    // Windows handler now does, so `drive` sees exactly what a real session sends.
    let speak = |hypotheses: &[&str]| {
        let mut q = factory.queued.lock().unwrap();
        for h in hypotheses {
            q.push_back(LiveMsg::Replace((*h).to_owned()));
        }
    };

    // Speak, pause, speak again, stop: partials replace, a pause settles, stop settles the tail.
    speak(&[" Hello", " Hello there"]);
    factory
        .queued
        .lock()
        .unwrap()
        .push_back(LiveMsg::Final("Hello there.".into()));
    speak(&[" And more"]);
    assert_eq!(start(&ctx), Ok(json!({"ok": true})));
    wait_len(&sink, 4);
    assert_eq!(
        ops::live(&ctx, json!({"action": "stop"})),
        Ok(json!({"ok": true}))
    );
    assert_eq!(
        wire(&sink),
        vec![
            ("live_transcript".into(), Some(false), "Hello".into()),
            ("live_transcript".into(), Some(false), "Hello there".into()),
            ("live_transcript".into(), Some(true), "Hello there.".into()),
            ("live_transcript".into(), Some(false), "And more".into()),
            // stop flushed the sentence in progress: the user heard it, they keep it.
            ("live_transcript".into(), Some(true), "And more".into()),
        ]
    );

    // From now on the saved language says "auto": starts leave the choice to Windows.
    db.write(|tx| {
        settings::save(
            tx,
            &settings::Settings {
                language: "auto".to_owned(),
                ..Default::default()
            },
        )
    })
    .unwrap();

    // Mid-sentence corrections: every hypothesis carries the whole sentence and replaces the
    // one in progress, so a re-write never doubles up.
    speak(&["The quick", "The quick brown", "The quark brown"]);
    assert_eq!(start(&ctx), Ok(json!({"ok": true})));
    wait_len(&sink, 8);
    assert_eq!(
        ops::live(&ctx, json!({"action": "stop"})),
        Ok(json!({"ok": true}))
    );
    assert_eq!(
        &wire(&sink)[5..],
        &[
            ("live_transcript".into(), Some(false), "The quick".into()),
            (
                "live_transcript".into(),
                Some(false),
                "The quick brown".into()
            ),
            // the re-write replaced the sentence instead of appending to it
            (
                "live_transcript".into(),
                Some(false),
                "The quark brown".into()
            ),
            (
                "live_transcript".into(),
                Some(true),
                "The quark brown".into()
            ),
        ]
    );

    // A second session on the same service starts once the first thread is reaped.
    speak(&[" Keep"]);
    factory
        .queued
        .lock()
        .unwrap()
        .push_back(LiveMsg::Failed("the microphone went away".into()));
    assert_eq!(start(&ctx), Ok(json!({"ok": true})));
    wait_len(&sink, 12);
    assert_eq!(
        &wire(&sink)[9..12],
        &[
            ("live_transcript".into(), Some(false), "Keep".into()),
            // the words already heard settle, then the one-line reason:
            ("live_transcript".into(), Some(true), "Keep".into()),
            (
                "dictation_live".into(),
                None,
                "the microphone went away".into()
            ),
        ]
    );

    // Cancel throws the unfinished sentence away; nothing more lands.
    speak(&[" Gone"]);
    assert_eq!(start(&ctx), Ok(json!({"ok": true})));
    wait_len(&sink, 13);
    assert_eq!(
        ops::live(&ctx, json!({"action": "cancel"})),
        Ok(json!({"ok": true}))
    );
    assert_eq!(wire(&sink).len(), 13);
    assert_eq!(
        wire(&sink)[12],
        ("live_transcript".into(), Some(false), "Gone".into())
    );

    // One owner holds the microphone, and a second start takes it from the session in progress
    // rather than refusing: this is what frees the microphone when a page reload skipped the
    // cleanup and left a session listening. The old session's unfinished words are thrown away.
    speak(&[" Old"]);
    assert_eq!(start(&ctx), Ok(json!({"ok": true})));
    wait_len(&sink, 14);
    assert_eq!(start(&ctx), Ok(json!({"ok": true})));
    assert_eq!(wire(&sink).len(), 14);
    assert_eq!(
        ops::live(&ctx, json!({"action": "cancel"})),
        Ok(json!({"ok": true}))
    );

    // A service with no model answers in one line, and the page keeps the recording path.
    *factory.fail_start.lock().unwrap() = Some("no speech model here".into());
    let e = start(&ctx).unwrap_err();
    assert_eq!(e.code, "not_available");
    assert_eq!(e.message, "no speech model here");

    // The engine ending by itself settles its tail and frees the slot for the next start.
    speak(&[" tail"]);
    factory.queued.lock().unwrap().push_back(LiveMsg::Ended);
    assert_eq!(start(&ctx), Ok(json!({"ok": true})));
    wait_len(&sink, 16);
    assert_eq!(
        &wire(&sink)[14..16],
        &[
            ("live_transcript".into(), Some(false), "tail".into()),
            ("live_transcript".into(), Some(true), "tail".into()),
        ]
    );

    // The language hand-off: the first start asked with the saved language ("en", the default);
    // every start after the settings said "auto" left the choice to Windows.
    assert_eq!(
        *factory.langs.lock().unwrap(),
        vec![
            Some("en".to_owned()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ]
    );

    // The args rule.
    assert_eq!(
        code_of(ops::live(&ctx, json!({"action": "spin"}))),
        "bad_args"
    );
    assert_eq!(code_of(ops::live(&ctx, json!({}))), "bad_args");
}

/// What the pill says for each way the service can refuse to open. One line each, and each
/// says what to do about it.
#[cfg(windows)]
#[test]
fn a_service_that_refuses_to_open_is_explained_in_one_line() {
    use crate::live::start_failure;
    let err = |code: u32| windows::core::Error::from_hresult(windows::core::HRESULT(code as i32));
    for code in [0x8004_0154u32, 0x8004_01e4] {
        assert!(
            start_failure(&err(code)).contains("not part of this Windows"),
            "{code:#010x}"
        );
    }
    // The consent gate and the missing language model say where to click.
    assert!(start_failure(&err(0x8004_5509)).contains("Privacy & security"));
    assert!(start_failure(&err(0x8004_5004)).contains("no model for your Windows language"));
    // The generic invalid-operation code says what to check instead of showing the raw number.
    let invalid = start_failure(&err(0x8013_1509));
    assert!(
        invalid.contains("would not start") && !invalid.contains("0x8013"),
        "{invalid}"
    );
    // Anything else at least names the culprit.
    let odd = start_failure(&err(0x8007_0005));
    assert!(
        odd.contains("unavailable") && odd.contains("0x80070005"),
        "{odd}"
    );
}

/// The runtime reality check, run by hand on a Windows PC with `cargo test -p helm-dictation
/// the_windows_speech_service -- --ignored`: two seconds of silence through the real service.
/// It needs the Windows speech consent (Settings → Privacy & security → Speech) turned on.
/// No words are claimed here — only that the service activates, compiles its dictation model,
/// opens the microphone, stops cleanly and leaves no failure behind. Streaming words needs a
/// voice, which is the smoke test's job.
#[cfg(windows)]
#[test]
#[ignore = "opens the real microphone for two seconds"]
fn the_windows_speech_service_starts_stops_and_leaves_no_failure() {
    use crate::live::{LiveDictation, STATE_EVENT, TRANSCRIPT_EVENT};
    use std::sync::Arc;

    let sink = Arc::new(RecordingSink::default());
    let svc = LiveDictation::new();
    svc.start(sink.clone(), None)
        .expect("the speech service should open on a desktop Windows");
    std::thread::sleep(std::time::Duration::from_secs(2));
    svc.stop().expect("the session should stop");
    {
        let evs = sink.events.lock().unwrap();
        for e in evs.iter() {
            assert_eq!(
                e.kind, TRANSCRIPT_EVENT,
                "only words may come from silence: {e:?}"
            );
        }
        assert!(
            !evs.iter().any(|e| e.kind == STATE_EVENT),
            "a session of silence ended with a failure: {evs:?}"
        );
    }
    // The slot is free again: start, then cancel straight away.
    svc.start(sink.clone(), None)
        .expect("the service should open a second time");
    svc.cancel().expect("the session should cancel");
}
