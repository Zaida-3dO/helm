use helm_hub::admin;
use helm_hub::auth::Tokens;
use helm_hub::embed::{self, QueryEmbedder};
use helm_hub::import::{check_root, import_dir, parse_args, record_root};
use helm_hub::lines::Lines;
use helm_hub::mcp::Options;
use helm_hub::registry::Registry;
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use helm_hub::stt::Stt;
use helm_hub::telegram::{
    now_ms, Bridge, HttpApi, HttpFiles, HttpVoice, Receive, Telegram, TelegramApi, TelegramConfig,
};
use helm_hub::voice::{VoiceBridge, VoiceConfig};
use helm_hub::wake::Wake;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

fn usage() -> String {
    format!(
        "helm-hub: the Helm hub (memory, relay lines, Telegram bot)

USAGE:
  helm-hub serve                       run the server
  helm-hub import <dir> --source S [--prune] [--retag]
                                       import a folder of Markdown memories, subfolders
                                       included; --source is required; --prune removes
                                       this source's memories whose files are gone;
                                       a folder keeps the source it was first imported
                                       under unless --retag is given
  helm-hub share-existing [--dry-run] [--keep ID,ID] [--list FILE] [--revert]
                                       the one-off move of memories that were private by
                                       default to shared: --dry-run lists them and changes
                                       nothing; a run backs the database up first, writes the
                                       ids it moved to a file and records them; --keep leaves
                                       those ids private; --revert undoes the latest run
{}
  helm-hub --version

ENVIRONMENT:
  HUB_DB            SQLite file (default /data/hub.db)
  HUB_BIND          listen address for serve (default 0.0.0.0:8000)
  HUB_TOKENS        client:token[:scope],... Optional now that machines are paired with
                    `helm-hub pair`; the entries here are clients of HUB_LEGACY_PERSON.
                    scope: ro|rw|relay|app|peer|telegram|voice|remote, several joined with +
                    (rw+app); telegram, voice and remote stand alone
  HUB_LEGACY_PERSON the person who owns the HUB_TOKENS clients, the memories stored before
                    people existed, and Telegram users with no person (default owner); set
                    it to the name your own PC is paired under before the first start
  HUB_ALLOW_MUTATE  1 to offer memory_update and memory_delete
  HUB_EMBED_URL     an OpenAI-style embeddings server (base URL, .../v1 or .../v1/embeddings) that embeds
                    search queries when the asker's PC cannot; unset: keywords only then
  HUB_EMBED_MODEL   the embedding model all vectors must come from (default nomic-embed-text)
  TZ                Europe/London reads dates in queries in UK time; anything else reads as UTC
  HUB_TELEGRAM_BOT_TOKEN      the bot's token; without it the bot stays off
  HUB_TELEGRAM_ALLOWED_USERS  numeric Telegram user ids, each optionally followed by :person
                              (123456:Sam) to route that user to that person's Helm;
                              empty denies everyone
  HUB_TELEGRAM_API            Bot API base (default the public API; a fake in tests)
  HUB_TELEGRAM_QUEUE_MINUTES  how long an offline message waits (default 120)
  HUB_TELEGRAM_QUEUE_MAX      how many offline messages wait (default 50)
  HUB_TELEGRAM_RECEIVE        poll (default): the hub polls the bot's updates;
                              off: another service that already runs the bot posts
                              them to /telegram/update with a telegram-scope token
  HUB_TELEGRAM_STT_URL        an OpenAI-compatible speech-to-text endpoint for voice
                              notes, e.g. http://<host>:<port>/v1/audio/transcriptions;
                              unset: recordings go without a transcript
  HUB_TELEGRAM_STT_MODEL      the model it is asked for (default large-v3-turbo)
  HUB_TELEGRAM_STT_LANGUAGE   the language of the voice notes (default en)
  HUB_TELEGRAM_STT_TIMEOUT_MINUTES
                              how long one transcription may take (1-60, default 10).
                              The phone's voice line uses the same server
  HUB_VOICE                   on (default) or off: the voice line for a phone with a
                              voice-scope token (`client add --scopes voice`)
  HUB_VOICE_QUEUE_MINUTES     how long a spoken or typed turn waits for Helm (1-1440,
                              default 30)
  HUB_VOICE_QUEUE_MAX         how many turns wait for Helm (1-200, default 20)
  HUB_VOICE_OUTBOX_MINUTES    how long a reply waits for the phone (1-10080, default 120)
  HUB_WAKE                    off (default), udp or ha: how the hub wakes a PC that is off or
                              asleep (only when a person taps Wake or sends /wake). udp sends
                              a magic packet by broadcast; ha asks Home Assistant to
  HUB_WAKE_BROADCAST          the LAN broadcast address for udp (default: the one the PC's
                              launcher reports)
  HUB_WAKE_HA_URL             Home Assistant's address, for ha (http:// or https://)
  HUB_WAKE_HA_TOKEN           a Home Assistant long-lived access token, for ha (a secret)
  HUB_WATCHDOG_SECS           the hub exits, for its container to restart it, when it has not
                              answered for this long (default 180; 0 turns it off)",
        admin::USAGE
    )
}

fn db_path() -> PathBuf {
    std::env::var_os("HUB_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/data/hub.db"))
}

/// The person who owns what existed before people did.
fn legacy_person() -> String {
    std::env::var("HUB_LEGACY_PERSON")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "owner".to_owned())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => serve(),
        Some("import") => import(&args[1..]),
        Some("share-existing") => share_existing(&args[1..]),
        Some(c) if admin::is_admin_command(c) => admin_command(&args),
        Some("--version") | Some("-V") => {
            println!("helm-hub {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") | Some("help") => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command {other:?}\n\n{}", usage());
            ExitCode::from(2)
        }
    }
}

fn admin_command(args: &[String]) -> ExitCode {
    let mut registry = match Registry::open(&db_path()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    match admin::run(&mut registry, args, now_ms()) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}\n\nrun `helm-hub help` for the commands");
            ExitCode::FAILURE
        }
    }
}

fn import(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
    let mut registry = match Registry::open(&db_path()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    let owner = match registry.ensure_person(&legacy_person()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot set up the owner: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut store = match Store::open(&db_path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Imported memories are the owner's own, shared with the household.
    store.set_default_owner(Some(owner.id));
    if let Err(e) = store.adopt_orphans(owner.id) {
        eprintln!("cannot adopt older memories: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = check_root(&store, &args.dir, &args.source, args.retag) {
        eprintln!("{e}");
        return ExitCode::from(2);
    }
    match import_dir(&mut store, &args.dir, &args.source, args.prune) {
        Ok(r) => {
            // Only a finished import changes what the folder is remembered as.
            if let Err(e) = record_root(&store, &args.dir, &args.source) {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
            if args.retag {
                println!(
                    "retagged under source {:?}: {} memories were rewritten, {} added",
                    args.source, r.updated, r.inserted
                );
            }
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
            if args.prune {
                println!("pruned {} memories whose files are gone", r.pruned.len());
                for p in &r.pruned {
                    println!("  pruned  {p}");
                }
                if r.files == 0 {
                    println!("  no memory files found, so nothing was pruned");
                }
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

fn share_existing(args: &[String]) -> ExitCode {
    use helm_hub::share;
    let args = match share::parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "{e}

{}",
                usage()
            );
            return ExitCode::from(2);
        }
    };
    let db = db_path();
    let mut store = match Store::open(&db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    let now = now_ms();
    if args.dry_run {
        match (
            share::plan(store.conn(), &args.keep),
            share::cutoff(store.conn()),
        ) {
            (Ok(plan), Ok(cutoff)) => {
                if let Some(c) = cutoff {
                    println!(
                        "an earlier run stands: only memories last changed before {c} are looked at"
                    );
                }
                println!(
                    "{} private memories would move to shared (id, name, source, title):",
                    plan.flip.len()
                );
                for r in &plan.flip {
                    println!("  {}", r.line());
                }
                if !plan.secret.is_empty() {
                    println!(
                        "{} stay private because they look like they hold a credential:",
                        plan.secret.len()
                    );
                    for r in &plan.secret {
                        println!("  {}", r.line());
                    }
                }
                println!("nothing was changed; --keep <ids> leaves memories private");
                return ExitCode::SUCCESS;
            }
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("cannot read the memories: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let backup = match share::backup(store.conn(), &db, now) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("the backup failed, so nothing was changed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("backup of the database: {}", backup.display());
    if args.revert {
        return match share::revert(store.conn_mut(), now) {
            Ok(Some((batch, ids))) => {
                println!(
                    "put {} memories of batch {batch} back to private",
                    ids.len()
                );
                ExitCode::SUCCESS
            }
            Ok(None) => {
                println!("no earlier run is left to undo");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("the undo failed: {e}");
                ExitCode::FAILURE
            }
        };
    }
    match share::share(store.conn_mut(), &args.keep, now) {
        Ok(done) => {
            let list = args.list.unwrap_or_else(|| share::list_path(&db, now));
            let mut text: String = done
                .moved
                .iter()
                .map(|r| {
                    format!(
                        "{}
",
                        r.line()
                    )
                })
                .collect();
            for r in &done.secret {
                text.push_str(&format!(
                    "skipped: looks like a secret	{}
",
                    r.line()
                ));
            }
            if let Err(e) = std::fs::write(&list, text) {
                eprintln!(
                    "the move stands (batch {}), but the list of moved ids could not be written: {e};                      `helm-hub share-existing --revert` still undoes it",
                    done.batch
                );
                return ExitCode::FAILURE;
            }
            println!(
                "moved {} memories to shared (batch {}); the list is in {}",
                done.moved.len(),
                done.batch,
                list.display()
            );
            if !done.secret.is_empty() {
                println!(
                    "{} memories look like they hold a credential and stay private (listed as skipped)",
                    done.secret.len()
                );
            }
            println!("`helm-hub share-existing --revert` undoes it");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("the move failed and nothing was changed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve() -> ExitCode {
    let spec = std::env::var("HUB_TOKENS").unwrap_or_default();
    let tokens = if spec.trim().is_empty() {
        Tokens::default()
    } else {
        match Tokens::parse(&spec) {
            Ok(t) => t,
            Err(e) => {
                // Never serve with a half-understood list of who may connect.
                eprintln!("HUB_TOKENS: {e}; refusing to start");
                return ExitCode::FAILURE;
            }
        }
    };
    let legacy = legacy_person();
    let mut registry = match Registry::open(&db_path()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = registry.sync_legacy(&tokens, &legacy) {
        eprintln!("HUB_TOKENS: {e}; refusing to start");
        return ExitCode::FAILURE;
    }
    let owner = match registry.ensure_person(&legacy) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("HUB_LEGACY_PERSON: {e}; refusing to start");
            return ExitCode::FAILURE;
        }
    };
    let mut store = match Store::open(&db_path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Memories stored before people existed become the owner's, shared with the household.
    match store.adopt_orphans(owner.id) {
        Ok(0) => {}
        Ok(n) => eprintln!(
            "memory: {n} older memories are now private to {}",
            owner.name
        ),
        Err(e) => {
            eprintln!("cannot adopt older memories: {e}; refusing to start");
            return ExitCode::FAILURE;
        }
    }
    let telegram_cfg = match TelegramConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}; refusing to start");
            return ExitCode::FAILURE;
        }
    };
    let voice_cfg = match VoiceConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}; refusing to start");
            return ExitCode::FAILURE;
        }
    };
    let allow_mutate = std::env::var("HUB_ALLOW_MUTATE").is_ok_and(|v| v == "1" || v == "true");
    let bind = std::env::var("HUB_BIND").unwrap_or_else(|_| "0.0.0.0:8000".into());
    let registry = Arc::new(Mutex::new(registry));
    let (lines, upstream) = Lines::new(registry.clone());
    let lines = Arc::new(lines);
    let wake = match Wake::from_env() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}; refusing to start");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("wake: {}", wake.describe());
    lines.launchers().set_wake(wake);
    let telegram = match telegram_cfg.disabled_reason() {
        Some(why) => {
            eprintln!("telegram: off ({why})");
            None
        }
        None => {
            let token = telegram_cfg.token.clone().unwrap_or_default();
            let api: Arc<dyn TelegramApi> = Arc::new(HttpApi::new(&telegram_cfg.api_base, &token));
            let decorate = |bridge: Bridge| {
                let mut bridge =
                    bridge.with_files(Arc::new(HttpFiles::new(&telegram_cfg.api_base, &token)));
                if let Some(stt) = &telegram_cfg.stt {
                    bridge = bridge.with_voice(Arc::new(HttpVoice::new(
                        &telegram_cfg.api_base,
                        &token,
                        stt,
                    )));
                }
                bridge
            };
            let mut resolve = |name: &str| -> Result<i64, String> {
                let mut r = registry
                    .lock()
                    .map_err(|_| "the registry is unavailable".to_owned())?;
                r.ensure_person(name)
                    .map(|p| p.id)
                    .map_err(|e| e.to_string())
            };
            match Telegram::build(
                api,
                &lines,
                &telegram_cfg,
                owner.id,
                &mut resolve,
                &decorate,
            ) {
                Ok(t) => Some(Arc::new(t)),
                Err(e) => {
                    eprintln!("HUB_TELEGRAM_ALLOWED_USERS: {e}; refusing to start");
                    return ExitCode::FAILURE;
                }
            }
        }
    };
    let clients: Vec<String> = registry
        .lock()
        .map(|r| r.clients().unwrap_or_default())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.revoked_at.is_none())
        .map(|c| format!("{}/{}", c.person, c.machine))
        .collect();
    if clients.is_empty() {
        eprintln!(
            "no clients yet: nobody can connect until one is added (helm-hub pair or client add)"
        );
    }
    // The voice line shares the Telegram bot's speech-to-text settings, bot or no bot.
    let voice = voice_cfg.map(|cfg| {
        let mut bridge = VoiceBridge::new(lines.clone(), cfg);
        if let Some(stt) = &telegram_cfg.stt {
            bridge = bridge.with_stt(Arc::new(Stt::new(stt)));
        }
        Arc::new(bridge)
    });
    let voice_upstream = voice.as_ref().map(|_| lines.voice_receiver());
    let mut state = AppState::new(
        store,
        registry.clone(),
        Options { allow_mutate },
        lines.clone(),
        telegram.clone(),
    );
    state.voice = voice.clone();
    // Queries are embedded by the asker's PC first, then by this server, else not at all.
    let embed_url = std::env::var("HUB_EMBED_URL").ok();
    let embed_model =
        std::env::var("HUB_EMBED_MODEL").unwrap_or_else(|_| embed::DEFAULT_MODEL.into());
    state.embedder = Arc::new(QueryEmbedder::new(
        lines.clone(),
        embed_url.clone(),
        &embed_model,
    ));
    eprintln!(
        "search: meaning model {embed_model}; NAS embedder {}",
        if embed_url.is_some() {
            "configured"
        } else {
            "off (the PC embeds, else keywords only)"
        }
    );
    let state = Arc::new(state);

    // One thread is plenty for a household's memory store and keeps RSS small.
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
        helm_hub::watchdog::start(watchdog_limit());
        if let Some(bot) = telegram {
            eprintln!(
                "telegram: on, {} allowed user(s), {}; transcription {}",
                telegram_cfg.allowed.len(),
                match telegram_cfg.receive {
                    Receive::Poll => "polling for updates",
                    Receive::Off => "not polling; updates are posted to /telegram/update",
                },
                if telegram_cfg.stt.is_some() {
                    "on"
                } else {
                    "off"
                }
            );
            tokio::spawn(bot.run(upstream));
        }
        if let (Some(bridge), Some(frames)) = (voice, voice_upstream) {
            eprintln!(
                "voice: on, transcription {}",
                if bridge.has_stt() { "on" } else { "off" }
            );
            tokio::spawn(bridge.run(frames));
        } else {
            eprintln!("voice: off (HUB_VOICE=off)");
        }
        eprintln!(
            "helm-hub {} listening on {bind}; clients: {}; update/delete {}",
            env!("CARGO_PKG_VERSION"),
            clients.join(", "),
            if allow_mutate { "on" } else { "off" }
        );
        match axum::serve(
            listener,
            router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
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

/// How long the hub may go without answering before it exits, from `HUB_WATCHDOG_SECS`: 180 s
/// by default, and zero turns the watchdog off. A value that is not a number is said and ignored.
fn watchdog_limit() -> std::time::Duration {
    const DEFAULT_SECS: u64 = 180;
    let secs = match std::env::var("HUB_WATCHDOG_SECS") {
        Ok(v) if !v.trim().is_empty() => v.trim().parse::<u64>().unwrap_or_else(|_| {
            eprintln!("HUB_WATCHDOG_SECS is not a number of seconds; using {DEFAULT_SECS}");
            DEFAULT_SECS
        }),
        _ => DEFAULT_SECS,
    };
    std::time::Duration::from_secs(secs)
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
