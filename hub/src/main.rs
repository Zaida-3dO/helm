use helm_hub::auth::Tokens;
use helm_hub::import::import_dir;
use helm_hub::mcp::Options;
use helm_hub::relay::Line;
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use helm_hub::telegram::{Bridge, HttpApi, HttpVoice, Receive, TelegramConfig};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

const USAGE: &str = "helm-hub: the Helm hub (memory, relay line, Telegram bot)

USAGE:
  helm-hub serve                       run the MCP server
  helm-hub import <dir> [--source S]   import a folder of Markdown memories
  helm-hub --version

ENVIRONMENT:
  HUB_DB            SQLite file (default /data/hub.db)
  HUB_BIND          listen address for serve (default 0.0.0.0:8000)
  HUB_TOKENS        client:token[:ro|rw|ro+app|rw+app|relay|telegram],... (required for serve)
  HUB_ALLOW_MUTATE  1 to offer memory_update and memory_delete
  HUB_TELEGRAM_BOT_TOKEN      the bot's token; without it the bot stays off
  HUB_TELEGRAM_ALLOWED_USERS  numeric Telegram user ids; empty denies everyone
  HUB_TELEGRAM_API            Bot API base (default the public API; a fake in tests)
  HUB_TELEGRAM_QUEUE_MINUTES  how long an offline message waits (default 120)
  HUB_TELEGRAM_QUEUE_MAX      how many offline messages wait (default 50)
  HUB_TELEGRAM_RECEIVE        poll (default): the hub polls the bot's updates;
                              off: another service that already runs the bot posts
                              them to /telegram/update with a telegram-scope token
  HUB_TELEGRAM_STT_URL        an OpenAI-compatible speech-to-text endpoint for voice
                              notes, e.g. http://<host>:<port>/v1/audio/transcriptions;
                              unset: voice notes are off
  HUB_TELEGRAM_STT_MODEL      the model it is asked for (default large-v3-turbo)
  HUB_TELEGRAM_STT_LANGUAGE   the language of the voice notes (default en)";

fn db_path() -> PathBuf {
    std::env::var_os("HUB_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/data/hub.db"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => serve(),
        Some("import") => import(&args[1..]),
        Some("--version") | Some("-V") => {
            println!("helm-hub {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") | Some("help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command {other:?}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn import(args: &[String]) -> ExitCode {
    let mut dir = None;
    let mut source = "default".to_owned();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--source" => match it.next() {
                Some(s) => source = s.clone(),
                None => {
                    eprintln!("--source needs a value");
                    return ExitCode::from(2);
                }
            },
            d if dir.is_none() => dir = Some(PathBuf::from(d)),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let mut store = match Store::open(&db_path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    match import_dir(&mut store, &dir, &source) {
        Ok(r) => {
            println!(
                "imported {} of {} files: {} new, {} updated, {} unchanged; {} skipped (index files), {} failed",
                r.imported(),
                r.files,
                r.inserted,
                r.updated,
                r.unchanged,
                r.skipped.len(),
                r.failed.len()
            );
            for s in &r.skipped {
                println!("  skipped {s}");
            }
            for f in &r.failed {
                println!("  failed  {f}");
            }
            println!(
                "the store now holds {} memories",
                store.count().unwrap_or(-1)
            );
            if r.failed.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("import failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve() -> ExitCode {
    let tokens = match Tokens::parse(&std::env::var("HUB_TOKENS").unwrap_or_default()) {
        Ok(t) => t,
        Err(e) => {
            // Never serve without authentication.
            eprintln!("HUB_TOKENS: {e}; refusing to start");
            return ExitCode::FAILURE;
        }
    };
    let store = match Store::open(&db_path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    let telegram = match TelegramConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}; refusing to start");
            return ExitCode::FAILURE;
        }
    };
    let allow_mutate = std::env::var("HUB_ALLOW_MUTATE").is_ok_and(|v| v == "1" || v == "true");
    let bind = std::env::var("HUB_BIND").unwrap_or_else(|_| "0.0.0.0:8000".into());
    let clients: Vec<String> = tokens.clients().map(|c| c.name.clone()).collect();
    let (line, upstream) = Line::new();
    let line = Arc::new(line);
    let bridge = match telegram.disabled_reason() {
        Some(why) => {
            eprintln!("telegram: off ({why})");
            None
        }
        None => {
            let token = telegram.token.clone().unwrap_or_default();
            let api = Arc::new(HttpApi::new(&telegram.api_base, &token));
            let mut bridge = Bridge::new(api, line.clone(), &telegram);
            if let Some(stt) = &telegram.stt {
                bridge =
                    bridge.with_voice(Arc::new(HttpVoice::new(&telegram.api_base, &token, stt)));
            }
            Some(Arc::new(bridge))
        }
    };
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        tokens,
        options: Options { allow_mutate },
        line: line.clone(),
        telegram: bridge.clone(),
    });

    // One thread is plenty for a personal memory store and keeps RSS small.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(&bind).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot listen on {bind}: {e}");
                return ExitCode::FAILURE;
            }
        };
        if let Some(bridge) = bridge {
            eprintln!(
                "telegram: on, {} allowed user(s), {}; voice notes {}",
                telegram.allowed.len(),
                match telegram.receive {
                    Receive::Poll => "polling for updates",
                    Receive::Off => "not polling; updates are posted to /telegram/update",
                },
                if telegram.stt.is_some() { "on" } else { "off" }
            );
            tokio::spawn(bridge.run(upstream));
        }
        eprintln!(
            "helm-hub {} listening on {bind}; clients: {}; update/delete {}",
            env!("CARGO_PKG_VERSION"),
            clients.join(", "),
            if allow_mutate { "on" } else { "off" }
        );
        match axum::serve(listener, router(state))
            .with_graceful_shutdown(shutdown_signal())
            .await
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("server error: {e}");
                ExitCode::FAILURE
            }
        }
    })
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
