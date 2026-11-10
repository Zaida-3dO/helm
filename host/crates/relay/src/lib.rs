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
//! | [`voice`] | the voice chat: its tag and limits, the notes it is given, and the replies sent up for the phone |
//! | [`voice_sink`] | voice turns into the voice chat, and the working line and answers mirrored back |
//! | [`speakable`] | an answer cleaned, split and cut so it is good to hear |
//! | [`voice_watch`] | the voice chat watching the other chats: attention notes, coalesced and rate-limited, and telling the sink when the chat wakes |
//! | [`voice_show`] | the `app_voice_show` tool: pictures, tables, lists, links, text and files queued for the phone's screen |
//! | [`pair`] | pairing with the hub by a one-time code: token, names, the shared-memory server and the home profile in one step |
//! | [`household`] | other people's Helms through the hub: the three sharing switches, serving their calls, and asking theirs |
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
//! away-alerts switch, approving from Telegram, and the model and effort a new Telegram chat
//! opens with) and
//! `relay_telegram_reset` (drop the tag, so the next message starts a new chat). The same policy
//! reaches chats through [`agent_settings`]: the `telegram` key of `app_get_settings` and
//! `app_update_settings`, where chats may change only the new chat's model and effort.
//!
//! The voice chat (Settings → Hub's Voice block): `relay_voice_get` → [`voice_sink::VoiceView`]
//! (the chat the phone talks to, the model and effort a new voice chat opens with, the idle, age
//! and token limits, and what the attention watch wakes it for), `relay_voice_save`
//! ([`voice::PolicyFile`], whole) and `relay_voice_reset` (drop the tag, so the next turn starts a
//! new chat). Chats see the same policy through the `voice` key of `app_get_settings` and
//! `app_update_settings`, and may change only the model and effort.
//!
//! Identity and the household (Settings → Hub, the sidebar's profile popover). All of these are
//! the user's own; no agent tool reaches them.
//!
//! - `hub_pair {address, fallback?, code}` ([`pair`]): answers `{connection, personId,
//!   personName, machineId, machineName, memoryServer, profileId}`. `relay_connection_get` now
//!   also carries `personName` and `machineName` (empty unless paired by code).
//! - `household_get` → `{chats, transcripts, messages}` and `household_set` (the same keys, each
//!   optional; answers the values now in force; event `household_changed`). All three are off
//!   until turned on; a switch counts only while the one before it is on.
//! - `peer_people {}` → the hub's `{people: [{personId, name, machines: [{machineId, name,
//!   online, ...}]}]}`; `peer_list_chats {personId, machineId?, limit?, offset?}` → `{chats:
//!   [{chatId, title, status, projectName, profileName, updatedAt, needsAttention}],
//!   nextOffset?}`; `peer_read_chat {personId, machineId?, chatId, beforeMessageId?, limit?}` →
//!   the same page `app_read_chat` gives; `peer_send {personId, machineId?, chatId, message}`.
//!   They fail with code `unavailable` and a plain sentence while the line is down or the other
//!   person is not sharing. Profiles' own ops are in `helm-profiles`.

pub mod agent_settings;
pub mod approve;
pub mod client;
pub mod config;
pub mod exec;
pub mod gate;
pub mod household;
pub mod notify;
pub mod pair;
pub mod sink;
pub mod speakable;
pub mod telegram;
pub mod voice;
pub mod voice_sink;
pub mod voice_show;
pub mod voice_watch;
pub mod voice_heard;

pub use agent_settings::settings_extension;

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
    let voice = Arc::new(voice_sink::ChatVoice::new(
        executor.clone(),
        profile.dir.clone(),
    ));
    let client = client::Client::new(
        Arc::new(move || config::effective(&creds)),
        executor.clone(),
        sink.clone(),
    );
    sink.attach_line(client.handle());
    voice.attach_line(client.handle());
    client.attach_voice(voice.clone());
    // The household switches the user last chose (all off until they choose).
    if let Some(db) = services.get::<helm_core::db::Database>() {
        client.restore_sharing(household::load(&db));
    }
    services.insert(sink);
    services.insert(voice.shown());
    services.insert(voice);
    services.insert(Arc::new(voice_watch::VoiceWatch::default()));
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
    if let (Some(watch), Some(voice)) = (
        ctx.services.get::<voice_watch::VoiceWatch>(),
        ctx.services.get::<voice_sink::ChatVoice>(),
    ) {
        voice_watch::start(handle, &watch, voice);
    }
}

/// This crate's host tools: `app_telegram_send` and `app_telegram_ask` ([`notify`]) and
/// `app_voice_show` ([`voice_show`]).
pub fn register_host_tools(r: &mut helm_hosttools::HostToolRegistry) {
    notify::register_host_tools(r);
    voice_show::register_host_tools(r);
}

/// Give the Telegram sink the op dispatcher: it creates the Telegram chat, stops it on `/stop`
/// and answers its questions through Helm's own ops.
pub fn attach_ops(dispatcher: &Arc<helm_ipc::Dispatcher>) {
    if let Some(s) = dispatcher.ctx().services.get::<sink::ChatSink>() {
        s.attach_ops(dispatcher.clone());
    }
    if let Some(v) = dispatcher.ctx().services.get::<voice_sink::ChatVoice>() {
        v.attach_ops(dispatcher.clone());
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
        .blocking("relay_telegram_reset", OpClass::Blocking, telegram_reset)
        .blocking("relay_voice_get", OpClass::Blocking, voice_get)
        .blocking("relay_voice_save", OpClass::Blocking, voice_save)
        .blocking("relay_voice_reset", OpClass::Blocking, voice_reset);
    pair::register_ops(reg);
    household::register_ops(reg);
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
        ApiError::bad_args(
            "Expected maxAgeHours, maxTokens, awayAlerts, approvals, and optional model and effort.",
        )
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

fn voice_view(ctx: &OpCtx) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let db = ctx.services.need::<helm_core::db::Database>()?;
    to_value(&voice_sink::voice_view(&db, &profile.dir, helm_core::now_ms()))
}

fn voice_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    voice_view(ctx)
}

fn voice_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let f: voice::PolicyFile = serde_json::from_value(args).map_err(|_| {
        ApiError::bad_args(
            "Expected model, effort, idleMinutes, maxAgeHours, maxTokens and watch (off, flags or flags+done).",
        )
    })?;
    f.check().map_err(ApiError::bad_args)?;
    let profile = ctx.services.need::<Profile>()?;
    voice::save_policy(&voice::policy_path(&profile.dir), &f)
        .map_err(|_| ApiError::internal("The voice limits could not be saved."))?;
    voice_view(ctx)
}

fn voice_reset(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    voice::save_tag(&voice::tag_path(&profile.dir), None)
        .map_err(|_| ApiError::internal("The voice chat tag could not be removed."))?;
    voice_view(ctx)
}
