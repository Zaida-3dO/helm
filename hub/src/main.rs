use helm_hub::auth::Tokens;
use helm_hub::import::import_dir;
use helm_hub::mcp::Options;
use helm_hub::server::{router, AppState};
use helm_hub::store::Store;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

const USAGE: &str = "helm-hub: the Helm hub memory service

USAGE:
  helm-hub serve                       run the MCP server
  helm-hub import <dir> [--source S]   import a folder of Markdown memories
  helm-hub --version

ENVIRONMENT:
  HUB_DB            SQLite file (default /data/hub.db)
  HUB_BIND          listen address for serve (default 0.0.0.0:8000)
  HUB_TOKENS        client:token[:ro|rw],... (required for serve)
  HUB_ALLOW_MUTATE  1 to offer memory_update and memory_delete";

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
    let allow_mutate = std::env::var("HUB_ALLOW_MUTATE").is_ok_and(|v| v == "1" || v == "true");
    let bind = std::env::var("HUB_BIND").unwrap_or_else(|_| "0.0.0.0:8000".into());
    let clients: Vec<String> = tokens.clients().map(|c| c.name.clone()).collect();
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        tokens,
        options: Options { allow_mutate },
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
