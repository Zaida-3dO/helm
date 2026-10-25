//! Helm's outbound relay line to the Helm hub.
//!
//! Helm keeps ONE outbound, token-authenticated WebSocket to the hub on the home server, so the
//! PC needs no inbound port, no SSH server and no shell keys. Down that line come tool calls from
//! agents outside Helm (through the hub's MCP) and messages from Helm's own Telegram bot;
//! up it go the Telegram status and answers.
//!
//! | module | what |
//! |---|---|
//! | [`config`] | the hub addresses and token, sealed in the credential store, write-only |
//! | [`gate`] | the external-caller classification, argument refusals and "not approval" frame |
//! | [`client`] | the line: connect, reconnect, answer calls, deliver and acknowledge Telegram |
//! | [`telegram`] | the telegram origin, the active Telegram chat tag, its rotation and the phone renderer |
//! | [`sink`] | Telegram messages into the active chat, and the status and answer mirrored back |
//! | [`notify`] | Helm speaking first: the `app_telegram_send` tool and alerts while the user is away |
//! | [`approve`] | approving from Telegram: buttons for permission prompts and plan approvals, off by default |
//!
//! Wiring: [`register_services`] adds the line (its [`client::LineHandle`] and
//! [`client::Client`]) and [`start`] connects it once the host-tool dispatcher exists;
//! [`attach_ops`] hands the Telegram sink the op dispatcher (chat create, stop, answer). With no
//! saved connection the line stays off and nothing is attempted.
//!
//! Ops: `relay_connection_get` → [`config::RelayConnectionView`] (booleans and the line state,
//! never a value) and `relay_connection_save` ([`config::RelaySaveArgs`]: absent keeps, empty
//! clears). For Settings → Hub's Telegram section: `relay_telegram_get` →
//! [`sink::TelegramView`], `relay_telegram_save` ([`telegram::PolicyFile`]: the limits, the
//! away-alerts switch and approving from Telegram) and
//! `relay_telegram_reset` (drop the tag, so the next message starts a new chat).

pub mod approve;
pub mod client;
pub mod config;
pub mod exec;
pub mod gate;
pub mod notify;
pub mod sink;
pub mod telegram;

use helm_core::credentials::CredentialStore;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, EventSink, OpClass, OpCtx, OpRegistry, Services};
use helm_platform::Platform;
use serde_json::Value;
use std::sync::Arc;

pub fn credentials(profile: &Profile, platform: &Platform) -> CredentialStore {
    CredentialStore::new(
        &profile.path("credentials"),
        &profile.hash,
        platform.keychain.clone(),
    )
}

fn ctx_credentials(ctx: &OpCtx) -> Result<CredentialStore, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let platform = ctx.services.need::<Platform>()?;
    Ok(credentials(&profile, &platform))
}

fn line_state(ctx: &OpCtx) -> &'static str {
    ctx.services
        .get::<client::LineHandle>()
        .map(|h| h.state().as_str())
        .unwrap_or("offline")
}

/// The line and its executor, as shared services. Needs the profile and platform already in place.
pub fn register_services(services: &mut Services, _events: &Arc<dyn EventSink>) {
    let (Some(profile), Some(platform)) = (services.get::<Profile>(), services.get::<Platform>())
    else {
        log::warn!("relay: no profile or platform; the hub line stays off");
        return;
    };
    let creds = Arc::new(credentials(&profile, &platform));
    let executor = Arc::new(exec::AppExecutor::default());
    let sink = Arc::new(sink::ChatSink::new(executor.clone(), profile.dir.clone()));
    let client = client::Client::new(
        Arc::new(move || config::effective(&creds)),
        executor.clone(),
        sink.clone(),
    );
    sink.attach_line(client.handle());
    services.insert(sink);
    services.insert(client.handle());
    services.insert(executor);
    services.insert(client);
}

/// Connect the line: hand the executor Helm's host-tool dispatcher and start the client on the
/// app's runtime.
pub fn start(handle: &tokio::runtime::Handle, ctx: &OpCtx, tools: Arc<helm_hosttools::HostTools>) {
    let (Some(client), Some(executor)) = (
        ctx.services.get::<client::Client>(),
        ctx.services.get::<exec::AppExecutor>(),
    ) else {
        return;
    };
    executor.wire(exec::Wiring {
        services: ctx.services.clone(),
        events: ctx.events.clone(),
        tools,
    });
    let line = client.handle();
    handle.spawn(client.run());
    if let (Some(rt), Some(afk), Some(profile), Some(sink)) = (
        ctx.services.get::<helm_chats::ChatRuntime>(),
        ctx.services.get::<helm_afk::AfkService>(),
        ctx.services.get::<Profile>(),
        ctx.services.get::<sink::ChatSink>(),
    ) {
        notify::watch_away(handle, (*rt).clone(), afk, line, sink, profile.dir.clone());
    }
}

/// This crate's host tool: `app_telegram_send` ([`notify`]).
pub fn register_host_tools(r: &mut helm_hosttools::HostToolRegistry) {
    notify::register_host_tools(r);
}

/// Give the Telegram sink the op dispatcher: it creates the Telegram chat, stops it on `/stop`
/// and answers its questions through Helm's own ops.
pub fn attach_ops(dispatcher: &Arc<helm_ipc::Dispatcher>) {
    if let Some(s) = dispatcher.ctx().services.get::<sink::ChatSink>() {
        s.attach_ops(dispatcher.clone());
    }
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("relay_connection_get", OpClass::Blocking, connection_get)
        .blocking(
            "relay_connection_save",
            OpClass::Maintenance,
            connection_save,
        )
        .blocking("relay_telegram_get", OpClass::Blocking, telegram_get)
        .blocking("relay_telegram_save", OpClass::Blocking, telegram_save)
        .blocking("relay_telegram_reset", OpClass::Blocking, telegram_reset);
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn connection_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let creds = ctx_credentials(ctx)?;
    to_value(&config::view(&config::effective(&creds), line_state(ctx)))
}

fn connection_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    // The input is never echoed in an error: it may be the token.
    let a: config::RelaySaveArgs = serde_json::from_value(args).map_err(|_| {
        ApiError::bad_args("Expected lanUrl, fallbackUrl and token, each optional.")
    })?;
    let creds = ctx_credentials(ctx)?;
    let saved = config::save(&creds, a).map_err(ApiError::bad_args)?;
    if let Some(c) = ctx.services.get::<client::Client>() {
        c.reconfigured.notify_waiters();
    }
    to_value(&config::view(&saved, line_state(ctx)))
}

fn telegram_view(ctx: &OpCtx) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let db = ctx.services.need::<helm_core::db::Database>()?;
    to_value(&sink::telegram_view(&db, &profile.dir, helm_core::now_ms()))
}

fn telegram_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    telegram_view(ctx)
}

fn telegram_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let f: telegram::PolicyFile = serde_json::from_value(args).map_err(|_| {
        ApiError::bad_args("Expected maxAgeHours, maxTokens, awayAlerts and approvals.")
    })?;
    f.check().map_err(ApiError::bad_args)?;
    let profile = ctx.services.need::<Profile>()?;
    telegram::save_policy(&telegram::policy_path(&profile.dir), f)
        .map_err(|_| ApiError::internal("The Telegram limits could not be saved."))?;
    telegram_view(ctx)
}

fn telegram_reset(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    telegram::save_tag(&telegram::tag_path(&profile.dir), None)
        .map_err(|_| ApiError::internal("The Telegram chat tag could not be removed."))?;
    telegram_view(ctx)
}
