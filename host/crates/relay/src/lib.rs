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
//! | [`remote`] | the hub's Remote page: the prompts that wait, and answering them (off by default) |
//! | [`voice`] | the voice chat: its tag and limits, the notes it is given, and the replies sent up for the phone |
//! | [`voice_sink`] | voice turns into the voice chat, and the working line and answers mirrored back |
//! | [`speakable`] | an answer cleaned, split and cut so it is good to hear |
//! | [`voice_watch`] | the voice chat watching the other chats: attention notes, coalesced and rate-limited, and telling the sink when the chat wakes |
//! | [`voice_show`] | the `app_voice_show` tool: pictures, tables, lists, links, text and files queued for the phone's screen |
//! | [`pair`] | pairing with the hub by a one-time code: token, names, the shared-memory server and the home profile in one step |
//! | [`setup`] | `app_stage_settings`: a chat stages pairing, the update folder and the Devices switches; Helm applies them when the user clicks Approve |
//! | [`household`] | other people's Helms through the hub: the three sharing switches, serving their calls, and asking theirs |
//! | [`knowledge_line`] | the household's knowledge files over the line: pull and push, the hub's people as profiles, and the sync loop |
//! | [`index_line`] | the hub's search index over the line: requests and their answers, and the hub's questions to this PC |
//! | [`devlink`] | the household's devices over the line: the device crate's frames up and down, and the service that answers other devices |
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
//! away-alerts switch, approving from Telegram, answering from the Remote page, and the model and
//! effort a new Telegram chat opens with) and
//! `relay_telegram_reset` (drop the tag, so the next message starts a new chat). The same policy
//! reaches chats through [`agent_settings`]: the `telegram` key of `app_get_settings` and
//! `app_update_settings`, where chats may change only the new chat's model and effort.
//! Per profile the hub is asked about its Telegram account: `relay_tg_link` (a one-tap link the
//! person taps on their phone), `relay_tg_status` (linked, and the handle) and `relay_tg_unlink`;
//! `relay_route_pin` asks the hub to send one of the profile's person's channels (telegram or
//! voice) to this PC. Each names its profile with `{profileId}` (absent: the home profile) and
//! needs that profile to be linked to a person on the hub.
//!
//! The voice chat (Settings → Hub's Voice block): `relay_voice_get` → [`voice_sink::VoiceView`]
//! (the chat the phone talks to, the model and effort a new voice chat opens with, the idle, age
//! and token limits, and what the attention watch wakes it for), `relay_voice_save`
//! ([`voice::PolicyFile`], whole) and `relay_voice_reset` (drop the tag, so the next turn starts a
//! new chat). Chats see the same policy through the `voice` key of `app_get_settings` and
//! `app_update_settings`, and may change only the model and effort.
//!
//! Identity and the household (Settings → Hub, the sidebar's profile popover). All of these are
//! the user's own; no agent tool reaches them, except that a chat may stage a pairing with
//! `app_stage_settings` ([`setup`]) for the user to approve.
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
pub mod devlink;
pub mod exec;
pub mod gate;
pub mod household;
pub mod hub_view;
pub mod index_line;
pub mod knowledge_line;
pub mod notify;
pub mod pair;
pub mod people;
pub mod remote;
pub mod setup;
pub mod sink;
pub mod speakable;
pub mod telegram;
pub mod voice;
pub mod voice_heard;
pub mod voice_show;
pub mod voice_sink;
pub mod voice_watch;

pub use agent_settings::settings_extension;

use helm_core::credentials::CredentialStore;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, EventSink, OpClass, OpCtx, OpRegistry, Services};
use helm_platform::Platform;
use serde_json::{json, Value};
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
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
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
    // A frame naming a hub person is only this PC's business when a profile of theirs is
    // linked here (the home profile's person always is).
    if let Some(db) = services.get::<helm_core::db::Database>() {
        client.restore_sharing(household::load(&db));
        let db_check = Arc::clone(&db);
        client.set_person_check(Arc::new(move |p: &str| {
            db_check
                .snapshot(|tx| helm_profiles::store::profile_for_person(tx, p).map(|o| o.is_some()))
                .unwrap_or(false)
        }));
        // The hub's welcome names the person this line acts as. Pairing links the home
        // profile to that person; a line configured with a token never pairs, so the
        // welcome is where the link happens, but only for a home profile that has no person yet.
        let db_link = Arc::clone(&db);
        let events = events.clone();
        client.set_home_link(Arc::new(move |p: &str, name: &str| {
            let linked = db_link
                .write(|tx| people::home_link(tx, p, name))
                .unwrap_or(false);
            if linked {
                events.emit(helm_ipc::AppEvent::new("profiles_changed"));
            }
        }));
    }
    // The hub's search index: the memory indexer drives it, and answers what the hub asks.
    let index = index_line::IndexLine::new(client.handle());
    client.attach_index(Arc::new(index_line::IndexSinkHandle(index.clone())));
    services.insert(index);
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
    knowledge_line::start(handle, ctx, &client);
    handle.spawn(client.run_supervised());
    if let (Some(rt), Some(db), Some(afk), Some(profile), Some(sink)) = (
        ctx.services.get::<helm_chats::ChatRuntime>(),
        ctx.services.get::<helm_core::db::Database>(),
        ctx.services.get::<helm_afk::AfkService>(),
        ctx.services.get::<Profile>(),
        ctx.services.get::<sink::ChatSink>(),
    ) {
        notify::watch_away(
            handle,
            (*rt).clone(),
            db,
            afk,
            line,
            sink,
            profile.dir.clone(),
        );
    }
    if let (Some(watch), Some(voice)) = (
        ctx.services.get::<voice_watch::VoiceWatch>(),
        ctx.services.get::<voice_sink::ChatVoice>(),
    ) {
        voice_watch::start(handle, &watch, voice);
    }
}

/// This crate's host tools: `app_telegram_send` and `app_telegram_ask` ([`notify`]),
/// `app_voice_show` ([`voice_show`]) and `app_stage_settings` ([`setup`]).
pub fn register_host_tools(r: &mut helm_hosttools::HostToolRegistry) {
    notify::register_host_tools(r);
    voice_show::register_host_tools(r);
    setup::register_host_tools(r);
}

/// This crate's allow kinds: `protected_settings`, which Helm applies on Approve ([`setup`]).
pub fn register_allow_kinds(k: &mut helm_consent::AllowKinds) {
    setup::register_allow_kinds(k);
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
    // Other devices of the household read and drive this PC's chats through the same dispatcher.
    devlink::attach(dispatcher);
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
        .blocking("relay_voice_reset", OpClass::Blocking, voice_reset)
        .async_op("relay_tg_link", tg_link)
        .async_op("relay_tg_status", tg_status)
        .async_op("relay_tg_unlink", tg_unlink)
        .async_op("relay_route_pin", route_pin);
    pair::register_ops(reg);
    household::register_ops(reg);
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn connection_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let creds = ctx_credentials(ctx)?;
    let (config, problem) = config::effective_checked(&creds);
    let mut v = config::view(&config, line_state(ctx));
    if let Some(p) = problem {
        v.config_problem = p.reason().to_owned();
    }
    if let Some(h) = ctx.services.get::<client::LineHandle>() {
        v.line_refusal = h.last_refusal();
    }
    to_value(&v)
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

/// The profile a channel op acts on: `{profileId}` when it names one that exists here, else
/// the home profile. Each linked profile keeps its own Telegram and voice files and switches;
/// the settings window says which profile it is showing.
fn channel_profile(ctx: &OpCtx, args: &Value) -> Result<String, ApiError> {
    let db = ctx.services.need::<helm_core::db::Database>()?;
    // A Settings op may be the first thing to read the channel files after a start.
    if let Some(profile) = ctx.services.get::<Profile>() {
        people::migrate(&profile.dir, &db);
    }
    let named = args
        .get("profileId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match named {
        None => db
            .snapshot(|tx| helm_profiles::store::home(tx))
            .ok()
            .flatten()
            .map(|p| p.id)
            .ok_or_else(|| ApiError::internal("Helm has no home profile yet.")),
        Some(wanted) => db
            .snapshot(|tx| helm_profiles::store::get(tx, wanted))
            .ok()
            .flatten()
            .map(|p| p.id)
            .ok_or_else(|| ApiError::bad_args("There is no profile with that id.")),
    }
}

/// The hub person a channel op acts on: the profile from [`channel_profile`], linked to a person
/// on the hub. An unlinked profile gets one plain sentence — the settings pane says so too.
fn channel_person(ctx: &OpCtx, args: &Value) -> Result<String, ApiError> {
    let pid = channel_profile(ctx, args)?;
    let db = ctx.services.need::<helm_core::db::Database>()?;
    db.snapshot(|tx| helm_profiles::store::get(tx, &pid))
        .ok()
        .flatten()
        .and_then(|p| p.hub_person_id)
        .ok_or_else(|| ApiError::bad_args("This profile has no person on the hub yet."))
}

/// Ask the hub one thing about the profile's Telegram account (`tg_link`, `tg_status` or
/// `tg_unlink`) and wait for the answer that carries the same id. The hub's frame, passed on.
async fn tg_ask(ctx: &OpCtx, kind: &str, args: &Value) -> Result<Value, ApiError> {
    let person = channel_person(ctx, args)?;
    let line = ctx
        .services
        .get::<client::LineHandle>()
        .ok_or_else(|| ApiError::new("unavailable", "Helm is not connected to the hub."))?;
    line.request(json!({ "type": kind, "personId": person }))
        .await
        .map_err(|e| ApiError::new("unavailable", e))
}

/// `relay_tg_link`: ask the hub for a one-tap link the person taps on their phone. The hub
/// answers with the address and when the code behind it stops working.
async fn tg_link(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let v = tg_ask(&ctx, "tg_link", &args).await?;
    Ok(json!({
        "url": v.get("url").cloned().unwrap_or(Value::Null),
        "expiresAt": v.get("expiresAt").cloned().unwrap_or(Value::Null),
    }))
}

/// `relay_tg_status`: whether this profile's hub person has a linked Telegram account, and the
/// bot's handle if so.
async fn tg_status(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let v = tg_ask(&ctx, "tg_status", &args).await?;
    let linked = v.get("linked").and_then(Value::as_bool).unwrap_or(false);
    let mut out = json!({ "linked": linked });
    if linked {
        if let Some(h) = v.get("handle") {
            out["handle"] = h.clone();
        }
    }
    Ok(out)
}

/// `relay_tg_unlink`: drop this profile's hub person's Telegram link from the hub.
async fn tg_unlink(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let v = tg_ask(&ctx, "tg_unlink", &args).await?;
    Ok(json!({ "unlinked": v.get("unlinked").cloned().unwrap_or(Value::Null) }))
}

/// `relay_route_pin`: ask the hub to send this profile's person's channel to this PC from now on
/// (`channel`: `telegram` or `voice`). The hub keeps the pin; the bot's own command hands the
/// choice back.
async fn route_pin(ctx: OpCtx, args: Value) -> Result<Value, ApiError> {
    let person = channel_person(&ctx, &args)?;
    let channel = args
        .get("channel")
        .and_then(Value::as_str)
        .filter(|c| *c == "telegram" || *c == "voice")
        .ok_or_else(|| ApiError::bad_args("Expected channel to be telegram or voice."))?;
    let line = ctx
        .services
        .get::<client::LineHandle>()
        .ok_or_else(|| ApiError::new("unavailable", "Helm is not connected to the hub."))?;
    if !line.route_pin(&person, channel) {
        return Err(ApiError::new(
            "unavailable",
            "Helm is not connected to the hub.",
        ));
    }
    Ok(json!({ "pinned": true }))
}

fn telegram_view(ctx: &OpCtx, args: &Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let db = ctx.services.need::<helm_core::db::Database>()?;
    let pid = channel_profile(ctx, args)?;
    to_value(&sink::telegram_view(
        &db,
        &profile.dir,
        &pid,
        helm_core::now_ms(),
    ))
}

fn telegram_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    telegram_view(ctx, &args)
}

/// The arguments of a save without `profileId`, which names the profile and is not a setting.
fn without_profile(args: &Value) -> Value {
    let mut a = args.clone();
    if let Some(o) = a.as_object_mut() {
        o.remove("profileId");
    }
    a
}

fn telegram_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let f: telegram::PolicyFile = serde_json::from_value(without_profile(&args)).map_err(|_| {
        ApiError::bad_args(
            "Expected maxAgeHours, maxTokens, awayAlerts, approvals, remoteAnswers, takeTelegram, and optional model and effort.",
        )
    })?;
    f.check().map_err(ApiError::bad_args)?;
    let profile = ctx.services.need::<Profile>()?;
    let pid = channel_profile(ctx, &args)?;
    let paths = people::telegram_paths(&profile.dir, &pid);
    let took = telegram::load_file(&paths.policy).take_telegram;
    let takes = f.take_telegram;
    telegram::save_policy(&paths.policy, f)
        .map_err(|_| ApiError::internal("The Telegram limits could not be saved."))?;
    if took != takes {
        // The hub routes by what this PC last said it takes: say it again now.
        ctx.events.emit(helm_ipc::AppEvent::new("device_changed"));
    }
    telegram_view(ctx, &args)
}

fn telegram_reset(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let pid = channel_profile(ctx, &args)?;
    let paths = people::telegram_paths(&profile.dir, &pid);
    telegram::save_tag(&paths.tag, None)
        .map_err(|_| ApiError::internal("The Telegram chat tag could not be removed."))?;
    telegram_view(ctx, &args)
}

fn voice_view(ctx: &OpCtx, args: &Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let db = ctx.services.need::<helm_core::db::Database>()?;
    let pid = channel_profile(ctx, args)?;
    to_value(&voice_sink::voice_view(
        &db,
        &profile.dir,
        &pid,
        helm_core::now_ms(),
    ))
}

fn voice_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    voice_view(ctx, &args)
}

fn voice_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let f: voice::PolicyFile = serde_json::from_value(without_profile(&args)).map_err(|_| {
        ApiError::bad_args(
            "Expected model, effort, idleMinutes, maxAgeHours, maxTokens, watch (off, flags or flags+done) and takeVoice.",
        )
    })?;
    f.check().map_err(ApiError::bad_args)?;
    let profile = ctx.services.need::<Profile>()?;
    let pid = channel_profile(ctx, &args)?;
    let paths = people::voice_paths(&profile.dir, &pid);
    let took = voice::load_file(&paths.policy).take_voice;
    let takes = f.take_voice;
    voice::save_policy(&paths.policy, &f)
        .map_err(|_| ApiError::internal("The voice limits could not be saved."))?;
    if took != takes {
        ctx.events.emit(helm_ipc::AppEvent::new("device_changed"));
    }
    voice_view(ctx, &args)
}

fn voice_reset(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let pid = channel_profile(ctx, &args)?;
    let paths = people::voice_paths(&profile.dir, &pid);
    voice::save_tag(&paths.tag, None)
        .map_err(|_| ApiError::internal("The voice chat tag could not be removed."))?;
    voice_view(ctx, &args)
}
