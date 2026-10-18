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
//! | [`telegram`] | the telegram origin, the active Telegram chat tag and its rotation |
//!
//! Ops: `relay_connection_get` → [`config::RelayConnectionView`] (booleans and the line state,
//! never a value) and `relay_connection_save` ([`config::RelaySaveArgs`]: absent keeps, empty
//! clears).

pub mod client;
pub mod config;
pub mod gate;
pub mod telegram;

use helm_core::credentials::CredentialStore;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use helm_platform::Platform;
use serde_json::Value;

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

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("relay_connection_get", OpClass::Blocking, connection_get)
        .blocking(
            "relay_connection_save",
            OpClass::Maintenance,
            connection_save,
        );
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
