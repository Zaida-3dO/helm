//! Wiring a saved remote server to a browser sign-in, on top of the OAuth client in
//! [`crate::oauth`].
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `mcp_oauth_connect` | blocking | `{serverId}` → [`OAuthStatus`](crate::types::OAuthStatus) (starts the browser sign-in, waits for it to finish, keeps the tokens) |
//! | `mcp_oauth_status` | blocking | `{serverId}` → [`OAuthStatus`](crate::types::OAuthStatus) (connected or not; never a token) |
//! | `mcp_oauth_disconnect` | maintenance | `{serverId}` → `null` (forgets the stored sign-in) |
//!
//! The worker's side of a long chat is the host tool named [`HEADER_TOOL`]: when the server
//! refuses the header the chat connected with, the worker asks for a current one, and this
//! module answers it (refreshing first when the token is old, expired or just refused). The
//! answer travels to that chat's worker on its stdin, never through a file.
//!
//! The rules:
//! - Tokens live only in the credential store, one entry per server, bound to the server's id
//!   and to its address: an entry is used only for the address it was signed in to, and a
//!   refresh goes only to the token address found at that sign-in. A server that is removed,
//!   or whose address or sign-in settings change, loses its entry.
//! - Nothing here returns, logs or prints a token: op results say only that the server is
//!   connected and until when the token stands; refusals are plain words.
//! - A connect is one whole flow on the calling op's thread: discover, take or get a client id,
//!   open the system browser, wait on the loopback callback, keep the tokens. It gives up with
//!   the client module's five-minute wait.
//! - Refreshes of one server never overlap: a refresh token may be good for one use only.

use crate::config::McpStore;
use crate::oauth::discovery::same_address;
use crate::oauth::{self, StoredAuth, TokenStore};
use crate::types::{McpServer, OAuthConfig, OAuthStatus, TransportKind};
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The host tool the worker asks for a current `Authorization` header. Never listed to agents.
pub const HEADER_TOOL: &str = "app_mcp_oauth_header";

/// The worker line that carries the answer to [`HEADER_TOOL`].
pub const HEADER_LINE: &str = "mcp_oauth_header";

/// An access token with less than this left is refreshed before use.
const REFRESH_MARGIN: Duration = Duration::from_secs(300);

/// A refused token is not refreshed again when the stored one was made this recently and the
/// caller could not say which token it was refused with.
const FRESH_FOR: u64 = 30;

/// The words a not-connected server is reported with, in the chat's server list as well.
pub const NOT_CONNECTED: &str = "Not connected: open Settings → MCP and click Connect.";

/// The credential entry for one server's sign-in, and the identity its envelope is bound to.
pub(crate) fn entry_name(server_id: &str) -> String {
    format!("mcp-oauth-{server_id}")
}

fn entry_identity(server_id: &str) -> String {
    format!("mcp-oauth:{server_id}")
}

/// Forget a server's sign-in (a missing entry is fine). Nothing is revoked at the server.
pub(crate) fn forget_sign_in(creds: &helm_core::credentials::CredentialStore, server_id: &str) {
    let _ = TokenStore::forget(creds, &entry_name(server_id));
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("mcp_oauth_connect", OpClass::Blocking, connect_op)
        .blocking("mcp_oauth_status", OpClass::Blocking, status_op)
        .blocking("mcp_oauth_disconnect", OpClass::Maintenance, disconnect_op);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServerArgs {
    server_id: String,
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The stored server, checked for what a browser sign-in needs. The reasons are shown to the
// user, so they name nothing but the server's state.
fn oauth_server(store: &McpStore, id: &str) -> Result<McpServer, String> {
    let s = store
        .load()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "That server no longer exists.".to_owned())?;
    if s.transport.kind == TransportKind::Stdio {
        return Err("Only a server at an address signs in through your browser.".into());
    }
    if !s.enabled {
        return Err("Turn the server on first.".into());
    }
    if s.transport.url.as_deref().unwrap_or("").is_empty() {
        return Err("The server has no address to sign in to.".into());
    }
    if s.transport.oauth.is_none() {
        return Err("The server is not set up for a browser sign-in.".into());
    }
    Ok(s)
}

/// What is kept about a sign-in, without any token material. A sign-in made for another
/// address than the server has now does not count.
fn status_of(store: &McpStore, id: &str) -> OAuthStatus {
    let none = OAuthStatus {
        connected: false,
        expires_at: None,
    };
    let Some(server) = store
        .load()
        .ok()
        .and_then(|l| l.into_iter().find(|s| s.id == id))
    else {
        return none;
    };
    match bound_auth(store, &server) {
        Ok(auth) => OAuthStatus {
            connected: true,
            expires_at: auth.tokens.expires_at,
        },
        Err(_) => none,
    }
}

/// Tell every running chat to read its servers again, so a sign-in that just finished (or was
/// just forgotten) reaches chats without waiting for a settings change.
fn rebroadcast(ctx: &OpCtx) {
    if let Some(cell) = ctx.services.get::<crate::lines::ServicesCell>() {
        cell.broadcast();
    }
}

/// How a connect attempt failed: a worded refusal for the user, or a fault on our side.
#[derive(Debug)]
enum ConnectFail {
    Bad(String),
    Internal(String),
}

/// The client secret a server's sign-in settings carry, if any (sealed in the credential store
/// when the server was saved; a value typed but not saved yet is used as typed).
fn client_secret(store: &McpStore, server: &McpServer) -> Result<Option<String>, String> {
    let Some(r) = server
        .transport
        .oauth
        .as_ref()
        .and_then(|o| o.client_secret.as_ref())
    else {
        return Ok(None);
    };
    match (r.secret_id.as_deref(), r.value.as_deref()) {
        (Some(id), _) if !id.is_empty() => {
            let bytes = store
                .creds()
                .read(id, &crate::config::identity(&server.id, id, None))
                .map_err(|_| {
                    "The saved client secret could not be read. Enter it again in Settings → MCP."
                        .to_owned()
                })?;
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| "The saved client secret is not text.".to_owned())
        }
        (_, Some(v)) if !v.is_empty() => Ok(Some(v.to_owned())),
        _ => Ok(None),
    }
}

/// The listener for a sign-in: on the redirect address the settings name (its port and path),
/// else on a port tried first (the one a handed-out client id was registered for), else any
/// free one.
fn listen(cfg: &OAuthConfig, prefer: Option<&str>) -> Result<oauth::Bound, ConnectFail> {
    use ConnectFail::Bad;
    if let Some(raw) = cfg.redirect_uri.as_deref().filter(|r| !r.is_empty()) {
        let u = url::Url::parse(raw).map_err(|_| {
            Bad("The redirect address in the sign-in settings is not valid.".into())
        })?;
        let port = u.port().unwrap_or(0);
        let bound = oauth::bind(port, u.path()).map_err(|_| {
            Bad(format!(
                "Helm could not listen on port {port} for the sign-in. Another program may be using it."
            ))
        })?;
        return Ok(bound);
    }
    if let Some(u) = prefer.and_then(|p| url::Url::parse(p).ok()) {
        // The listener of the sign-in before may still be closing: ask again for a moment.
        for _ in 0..20 {
            if let Ok(bound) = oauth::bind(u.port().unwrap_or(0), u.path()) {
                if bound.redirect_uri() == u.as_str() {
                    return Ok(bound);
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    oauth::bind(0, "/callback").map_err(|e| Bad(e.to_string()))
}

/// One whole browser sign-in for a saved server: discover, take or get a client id, hand the
/// sign-in address to `open` (the system browser), wait for the callback, keep the tokens.
fn connect(
    store: &McpStore,
    open: &dyn helm_platform::OpenExternal,
    id: &str,
) -> Result<OAuthStatus, ConnectFail> {
    use ConnectFail::{Bad, Internal};
    let server = oauth_server(store, id).map_err(Bad)?;
    let url = server.transport.url.clone().unwrap_or_default();
    let cfg = server.transport.oauth.clone().unwrap_or_default();
    let agent = oauth::http_agent();

    // Where the sign-in lives, as the server publishes it.
    let as_info = oauth::discover(&agent, &url, None).map_err(|e| Bad(e.to_string()))?;

    // The listener comes first: a client id handed out by the server is registered for the very
    // address the browser will come back to.
    let entered = cfg
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (bound, client, registered_redirect) = match entered {
        Some(cid) => {
            let secret = client_secret(store, &server).map_err(Bad)?;
            let bound = listen(&cfg, None)?;
            (
                bound,
                oauth::Client::pre_registered(cid, secret.as_deref()),
                None,
            )
        }
        None => {
            // A client id handed out earlier for this server is used again when its redirect
            // address can be listened on again, so repeated sign-ins do not pile up ids.
            let kept = load_auth(store, id).ok().flatten().filter(|a| {
                a.registered_redirect.is_some()
                    && a.resource.as_deref().is_some_and(|r| same_address(r, &url))
                    && a.auth_server
                        .as_ref()
                        .is_some_and(|s| s.issuer == as_info.issuer)
            });
            let bound = listen(
                &cfg,
                kept.as_ref().and_then(|a| a.registered_redirect.as_deref()),
            )?;
            let here = bound.redirect_uri();
            match kept {
                Some(a) if a.registered_redirect.as_deref() == Some(here.as_str()) => {
                    (bound, a.client, a.registered_redirect)
                }
                _ => {
                    let redirect = bound.redirect_uri();
                    let client = oauth::register(&agent, &as_info, &redirect)
                        .map_err(|e| Bad(e.to_string()))?;
                    (bound, client, Some(redirect))
                }
            }
        }
    };
    let scopes: Vec<String> = cfg
        .scope
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let pending = oauth::begin_on(bound, &agent, &as_info, &client, &scopes, Some(&url))
        .map_err(|e| Bad(e.to_string()))?;
    if let Err(e) = open.open_url(&pending.auth_url) {
        return Err(Internal(format!(
            "The system browser could not be opened: {e}"
        )));
    }
    // The wait is the client module's: five minutes, one callback, this computer only.
    let tokens = pending.finish().map_err(|e| Bad(e.to_string()))?;

    // The server may have been changed while the browser was open: tokens for an address it no
    // longer has are not kept.
    let still = oauth_server(store, id).map_err(Bad)?;
    if still.transport.url.as_deref() != Some(url.as_str())
        || still.transport.oauth != server.transport.oauth
    {
        return Err(Bad(
            "The server was changed while you were signing in. Connect again.".into(),
        ));
    }
    let auth = StoredAuth {
        client,
        tokens,
        resource: Some(url),
        auth_server: Some(as_info),
        registered_redirect,
        stored_at: None,
    };
    {
        let lock = store.refresh_lock(id);
        let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
        TokenStore::save(store.creds(), &entry_name(id), &entry_identity(id), &auth)
            .map_err(|e| Internal(e.to_string()))?;
    }
    Ok(status_of(store, id))
}

fn connect_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ServerArgs = parse(args)?;
    let store = ctx.services.need::<McpStore>()?;
    let platform = ctx.services.need::<helm_platform::Platform>()?;
    let out = match connect(&store, platform.external.as_ref(), &a.server_id) {
        Ok(s) => s,
        Err(ConnectFail::Bad(w)) => return Err(ApiError::bad_args(w)),
        Err(ConnectFail::Internal(w)) => return Err(ApiError::internal(w)),
    };
    rebroadcast(ctx);
    to_value(&out)
}

fn status_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ServerArgs = parse(args)?;
    let store = ctx.services.need::<McpStore>()?;
    to_value(&status_of(&store, &a.server_id))
}

fn disconnect_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ServerArgs = parse(args)?;
    let store = ctx.services.need::<McpStore>()?;
    let found = store
        .load()
        .map_err(crate::ops::api_error)?
        .iter()
        .any(|s| s.id == a.server_id);
    if !found {
        return Err(ApiError::bad_args("That server no longer exists."));
    }
    // Forgetting is enough: a missing entry is simply not connected. Nothing is revoked at the
    // server itself.
    TokenStore::forget(store.creds(), &entry_name(&a.server_id))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    rebroadcast(ctx);
    Ok(Value::Null)
}

/// The stored sign-in for a server, if it has one.
fn load_auth(store: &McpStore, server_id: &str) -> Result<Option<StoredAuth>, String> {
    TokenStore::load(
        store.creds(),
        &entry_name(server_id),
        &entry_identity(server_id),
    )
    .map_err(|e| e.to_string())
}

/// The stored sign-in for `server`, only when it was made for the address the server has now (an
/// edited, removed-and-re-added or draft server at another address is not connected, and
/// nothing is attached or sent for it).
fn bound_auth(store: &McpStore, server: &McpServer) -> Result<StoredAuth, String> {
    let auth = load_auth(store, &server.id)?.ok_or_else(|| NOT_CONNECTED.to_owned())?;
    let url = server.transport.url.as_deref().unwrap_or("");
    let same = auth
        .resource
        .as_deref()
        .is_some_and(|r| same_address(r, url));
    if !same || auth.auth_server.is_none() {
        return Err(NOT_CONNECTED.to_owned());
    }
    Ok(auth)
}

/// A short, non-secret stand-in for an access token: the first 8 hex digits of its SHA-256. The
/// worker names the token it was refused with this way.
pub(crate) fn fingerprint(access: &str) -> String {
    let token = access.strip_prefix("Bearer ").unwrap_or(access);
    helm_core::sha256_hex(token.as_bytes())[..8].to_owned()
}

/// What a caller asking for the access token wants.
#[derive(Clone, Copy)]
pub(crate) enum Want<'a> {
    /// The stored token, refreshed first when it is nearly spent.
    IfDue,
    /// A token for a caller the server just refused: refreshed unless the stored one is already
    /// newer than the one refused (named by its [`fingerprint`] when the caller knows it).
    Refused(Option<&'a str>),
}

fn needs_refresh(auth: &StoredAuth, want: Want) -> bool {
    if auth.tokens.expires_within(REFRESH_MARGIN) {
        return true;
    }
    match want {
        Want::IfDue => false,
        Want::Refused(Some(fp)) => fingerprint(&auth.tokens.access_token) == fp,
        Want::Refused(None) => auth
            .stored_at
            .is_none_or(|at| now_secs().saturating_sub(at) > FRESH_FOR),
    }
}

/// The access token to use for `server` now. A refresh the sign-in service refuses falls back to
/// the stored token and lets the server itself decide.
///
/// Refreshes of one server are serialised: the stored sign-in is read again once the turn comes,
/// and a token another caller has just renewed is used as it is. A refresh goes only to the
/// token address stored at sign-in. This reaches the network only when a refresh is due; `Err`
/// words never carry token material.
pub(crate) fn current_access(
    store: &McpStore,
    server: &McpServer,
    want: Want,
) -> Result<String, String> {
    let auth = bound_auth(store, server)?;
    if !needs_refresh(&auth, want) {
        return Ok(auth.tokens.access_token);
    }
    let lock = store.refresh_lock(&server.id);
    let _turn = lock.lock().unwrap_or_else(|p| p.into_inner());
    let auth = bound_auth(store, server)?;
    if !needs_refresh(&auth, want) {
        return Ok(auth.tokens.access_token);
    }
    let spent = auth.tokens.refresh_token.clone().unwrap_or_default();
    let (Some(as_info), false) = (auth.auth_server.as_ref(), spent.is_empty()) else {
        return Ok(auth.tokens.access_token);
    };
    let agent = oauth::http_agent();
    let Ok(tokens) = oauth::refresh(
        &agent,
        as_info,
        &auth.client,
        &spent,
        auth.resource.as_deref(),
    ) else {
        return Ok(auth.tokens.access_token);
    };
    // Kept only when the sign-in is still the one that was refreshed and the server still has
    // the address it was made for; otherwise it was forgotten meanwhile and stays forgotten.
    let unchanged = load_auth(store, &server.id)
        .ok()
        .flatten()
        .is_some_and(|now| now.tokens.refresh_token.as_deref() == Some(spent.as_str()))
        && store
            .load()
            .ok()
            .and_then(|l| l.into_iter().find(|s| s.id == server.id))
            .is_some_and(|s| {
                auth.resource
                    .as_deref()
                    .is_some_and(|r| same_address(r, s.transport.url.as_deref().unwrap_or("")))
            });
    if unchanged {
        let saved = StoredAuth {
            tokens: tokens.clone(),
            ..auth
        };
        // A token that could not be saved still works for now; the next connect or refresh
        // will notice.
        let _ = TokenStore::save(
            store.creds(),
            &entry_name(&server.id),
            &entry_identity(&server.id),
            &saved,
        );
    }
    Ok(tokens.access_token)
}

/// The header the worker's retry sends, as the host tool answers it. `refused` names the token
/// the server just refused (see [`fingerprint`]).
pub(crate) fn current_header(
    store: &McpStore,
    server: &McpServer,
    refused: Option<&str>,
) -> Result<String, String> {
    let access = current_access(store, server, Want::Refused(refused))?;
    Ok(format!("Bearer {access}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::store;
    use crate::oauth::fake::{Cfg, Fake};
    use crate::types::{McpServer, OAuthConfig, Transport, TransportKind};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A stand-in for the browser: opens the sign-in address, follows its one redirect to the
    /// loopback callback, and is done — exactly what the system browser does.
    struct FakeBrowser;

    impl helm_platform::OpenExternal for FakeBrowser {
        fn open_url(&self, url: &str) -> Result<(), String> {
            let (status, loc) = raw_get(url);
            if status != 302 {
                return Err(format!("the sign-in page answered {status}"));
            }
            raw_get(&loc.ok_or("the sign-in page sent no redirect")?);
            Ok(())
        }
    }

    fn raw_get(url: &str) -> (u16, Option<String>) {
        let u = url::Url::parse(url).unwrap();
        let host = u.host_str().unwrap().to_owned();
        let port = u.port_or_known_default().unwrap();
        let mut s = std::net::TcpStream::connect((host.as_str(), port)).unwrap();
        let path = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        };
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = Vec::new();
        let _ = s.set_read_timeout(Some(Duration::from_secs(8)));
        let _ = s.read_to_end(&mut out);
        let text = String::from_utf8_lossy(&out).into_owned();
        let head = text.split("\r\n\r\n").next().unwrap_or("");
        let status: u16 = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let loc = head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("location")
                .then(|| v.trim().to_owned())
        });
        (status, loc)
    }

    fn svc(url: &str) -> McpServer {
        McpServer {
            id: "svc".into(),
            name: "Svc".into(),
            enabled: true,
            transport: Transport {
                kind: TransportKind::Http,
                command: None,
                args: vec![],
                env: BTreeMap::new(),
                url: Some(url.into()),
                headers: BTreeMap::new(),
                oauth: Some(OAuthConfig::default()),
            },
            allow_private_http: false,
            preset: None,
        }
    }

    fn saved(store: &McpStore, s: McpServer) {
        store
            .mutate(None, |l| {
                l.push(s);
                Ok(())
            })
            .unwrap();
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn a_connect_keeps_the_sign_in_and_the_header_then_forgets_it() {
        let fake = Fake::start(Cfg {
            require_resource: true,
            ..Cfg::default()
        });
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        saved(&st, svc(&fake.resource_url()));

        let status = connect(&st, &FakeBrowser, "svc").unwrap();
        assert!(status.connected);
        assert!(status.expires_at.is_some());
        // The status result never carries the token itself.
        let said = serde_json::to_string(&status).unwrap();
        assert!(!said.contains("fake-access"), "{said}");
        assert!(!said.contains("fake-refresh"), "{said}");

        // Resolving the server now sends the bearer and redacts it everywhere.
        let (_, r) = st.resolve_enabled(&|_| None).unwrap();
        assert_eq!(r[0].headers["Authorization"], "Bearer fake-access-1");
        assert!(r[0].oauth);
        assert!(r[0].error.is_none());
        assert!(r[0].secrets.iter().any(|s| s == "fake-access-1"));

        // The worker's ask, naming the token it was refused with, refreshes without waiting for
        // the expiry; a second ask naming the same refused token finds the renewed one waiting.
        let server = st.load().unwrap().remove(0);
        let refused = fingerprint("fake-access-1");
        let header = current_header(&st, &server, Some(&refused)).unwrap();
        assert_eq!(header, "Bearer fake-access-2");
        assert_eq!(
            current_header(&st, &server, Some(&refused)).unwrap(),
            "Bearer fake-access-2"
        );
        assert_eq!(fake.refreshes_served(), 1);
        // A caller that cannot name the refused token gets the stored one while it is fresh.
        assert_eq!(
            current_header(&st, &server, None).unwrap(),
            "Bearer fake-access-2"
        );
        assert_eq!(fake.refreshes_served(), 1);
        // The client id was handed out once, for the address the sign-in came back on.
        assert_eq!(fake.registrations(), 1);
        // The refreshed set is what is kept, and resolving again reuses it.
        let kept = TokenStore::load(st.creds(), "mcp-oauth-svc", "mcp-oauth:svc")
            .unwrap()
            .unwrap();
        assert_eq!(kept.tokens.access_token, "fake-access-2");
        let (_, r) = st.resolve_enabled(&|_| None).unwrap();
        assert_eq!(r[0].headers["Authorization"], "Bearer fake-access-2");

        // Signing in again reuses that client id (the fake only accepts the address it was
        // registered for) and keeps the one sign-in entry.
        connect(&st, &FakeBrowser, "svc").unwrap();
        assert_eq!(fake.registrations(), 1);
        assert!(status_of(&st, "svc").connected);

        // Disconnecting forgets it: the server reports as not connected, without a credential.
        TokenStore::forget(st.creds(), "mcp-oauth-svc").unwrap();
        let (_, r) = st.resolve_enabled(&|_| None).unwrap();
        assert_eq!(r[0].error.as_deref(), Some(NOT_CONNECTED));
        assert!(!r[0].headers.contains_key("Authorization"));
        assert!(status_of(&st, "svc").expires_at.is_none());
        assert!(!status_of(&st, "svc").connected);
    }

    #[test]
    fn a_token_with_under_five_minutes_left_is_refreshed_before_it_is_sent() {
        let fake = Fake::start(Cfg::default());
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        saved(&st, svc(&fake.resource_url()));
        connect(&st, &FakeBrowser, "svc").unwrap();

        // Push the kept token's expiry into the near future; the next resolve must refresh.
        let mut kept = TokenStore::load(st.creds(), "mcp-oauth-svc", "mcp-oauth:svc")
            .unwrap()
            .unwrap();
        kept.tokens.expires_at = Some(now_secs() + 60);
        TokenStore::save(st.creds(), "mcp-oauth-svc", "mcp-oauth:svc", &kept).unwrap();

        let (_, r) = st.resolve_enabled(&|_| None).unwrap();
        assert_eq!(r[0].headers["Authorization"], "Bearer fake-access-2");
        let kept = TokenStore::load(st.creds(), "mcp-oauth-svc", "mcp-oauth:svc")
            .unwrap()
            .unwrap();
        assert_eq!(kept.tokens.access_token, "fake-access-2");
        assert!(!kept.tokens.expires_within(REFRESH_MARGIN));
    }

    /// A sign-in belongs to the address it was made for: another address, a removed and
    /// re-added server, a draft elsewhere, or changed sign-in settings get no bearer, and no
    /// refresh token is sent anywhere.
    #[test]
    fn a_sign_in_is_used_only_for_the_address_it_was_made_for() {
        let fake = Fake::start(Cfg::default());
        let other = Fake::start(Cfg::default());
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        saved(&st, svc(&fake.resource_url()));
        connect(&st, &FakeBrowser, "svc").unwrap();
        let entry = |st: &McpStore| load_auth(st, "svc").unwrap();
        let bearer = |r: &crate::types::ResolvedServer| r.headers.get("Authorization").cloned();

        // A draft at another address (a connection test of an unsaved edit) gets nothing, and
        // asking for a fresh token for it never reaches either server.
        let mut draft = svc(&other.resource_url());
        let r = st.resolve_draft(&draft);
        assert_eq!(bearer(&r), None);
        assert_eq!(r.error.as_deref(), Some(NOT_CONNECTED));
        assert_eq!(
            current_header(&st, &draft, None).unwrap_err(),
            NOT_CONNECTED
        );
        assert_eq!(
            current_access(&st, &draft, Want::Refused(Some("00000000"))).unwrap_err(),
            NOT_CONNECTED
        );
        assert_eq!((fake.refreshes_served(), other.refreshes_served()), (0, 0));
        // The draft at the saved address does get it (the test of what is saved).
        draft.transport.url = Some(fake.resource_url());
        assert_eq!(
            bearer(&st.resolve_draft(&draft)).as_deref(),
            Some("Bearer fake-access-1")
        );

        // Saving the server at a new address forgets the sign-in; so does removing it, and so
        // does changing the sign-in settings. A re-added server of the same id starts clean.
        for change in [
            |l: &mut Vec<McpServer>, u: &str| l[0].transport.url = Some(u.to_owned()),
            |l: &mut Vec<McpServer>, _: &str| {
                l[0].transport.oauth = Some(OAuthConfig {
                    scope: Some("other".into()),
                    ..Default::default()
                })
            },
            |l: &mut Vec<McpServer>, _: &str| l.clear(),
        ] {
            st.mutate(None, |l| {
                l.clear();
                l.push(svc(&fake.resource_url()));
                Ok(())
            })
            .unwrap();
            connect(&st, &FakeBrowser, "svc").unwrap();
            assert!(entry(&st).is_some());
            st.mutate(None, |l| {
                change(l, &other.resource_url());
                Ok(())
            })
            .unwrap();
            assert!(entry(&st).is_none());
            let (_, r) = st.resolve_enabled(&|_| None).unwrap();
            assert!(r.iter().all(|s| bearer(s).is_none()));
        }
        // A saving that changes nothing about the address or the sign-in keeps it.
        st.mutate(None, |l| {
            l.clear();
            l.push(svc(&fake.resource_url()));
            Ok(())
        })
        .unwrap();
        connect(&st, &FakeBrowser, "svc").unwrap();
        st.mutate(None, |l| {
            l[0].name = "Renamed".into();
            Ok(())
        })
        .unwrap();
        assert!(status_of(&st, "svc").connected);
        // And an entry that somehow outlives its server's address is still not used.
        let mut moved = st.load().unwrap().remove(0);
        moved.transport.url = Some(other.resource_url());
        assert_eq!(
            current_access(&st, &moved, Want::IfDue).unwrap_err(),
            NOT_CONNECTED
        );
        assert_eq!(other.refreshes_served(), 0);
    }

    /// Refreshing: a server that does not rotate keeps its one refresh token for good; one that
    /// does rotate is never asked twice with the same one, however many callers ask at once.
    #[test]
    fn refreshes_keep_the_refresh_token_and_never_overlap() {
        let kept_refresh = |st: &McpStore| {
            load_auth(st, "svc")
                .unwrap()
                .unwrap()
                .tokens
                .refresh_token
                .unwrap()
        };
        // 1. Answers that leave the refresh token out: refreshed again and again.
        let quiet = Fake::start(Cfg {
            no_refresh_echo: true,
            ..Cfg::default()
        });
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        saved(&st, svc(&quiet.resource_url()));
        connect(&st, &FakeBrowser, "svc").unwrap();
        let server = st.load().unwrap().remove(0);
        let mut last = "fake-access-1".to_owned();
        for round in 2..=3 {
            let header = current_header(&st, &server, Some(&fingerprint(&last))).unwrap();
            last = format!("fake-access-{round}");
            assert_eq!(header, format!("Bearer {last}"));
            assert_eq!(kept_refresh(&st), "fake-refresh-1");
        }
        assert_eq!(quiet.refreshes_served(), 2);

        // 2. A server that rotates: several callers refused with the same token at once all end
        //    with the one renewed token, from a single refresh, and the next refresh works.
        let rotating = Fake::start(Cfg {
            rotate_refresh: true,
            ..Cfg::default()
        });
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        saved(&st, svc(&rotating.resource_url()));
        connect(&st, &FakeBrowser, "svc").unwrap();
        let server = st.load().unwrap().remove(0);
        let refused = fingerprint("fake-access-1");
        let gate = std::sync::Barrier::new(4);
        let answers: Vec<String> = std::thread::scope(|sc| {
            let jobs: Vec<_> = (0..4)
                .map(|_| {
                    sc.spawn(|| {
                        gate.wait();
                        current_header(&st, &server, Some(&refused)).unwrap()
                    })
                })
                .collect();
            jobs.into_iter().map(|j| j.join().unwrap()).collect()
        });
        assert!(
            answers.iter().all(|a| a == "Bearer fake-access-2"),
            "{answers:?}"
        );
        assert_eq!(rotating.refreshes_served(), 1);
        assert_eq!(kept_refresh(&st), "fake-refresh-1.1");
        // The refresh token that is kept is the live one: the next refresh is accepted.
        let next = current_header(&st, &server, Some(&fingerprint("fake-access-2"))).unwrap();
        assert_eq!(next, "Bearer fake-access-3");
        assert_eq!(rotating.refreshes_served(), 2);
    }

    /// The header travels to the asking chat's worker on its own line; the answer to the tool
    /// call (which is written to a file in the chat's folder) says only that it was sent.
    #[test]
    fn the_header_reaches_the_worker_on_its_own_line_and_never_in_the_answer() {
        use helm_hosttools::lines::WorkerBroadcast;
        use helm_hosttools::{Broadcaster, CallOrigin, ToolCtx};
        use helm_ipc::{NullSink, Services};
        use serde_json::json;
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Worker {
            lines: Mutex<Vec<(String, Value)>>,
            running: bool,
        }
        impl WorkerBroadcast for Worker {
            fn broadcast_to_workers(&self, _: &Value) {}
            fn send_to_worker(&self, chat: &str, line: &Value) -> bool {
                self.lines.lock().unwrap().push((chat.into(), line.clone()));
                self.running
            }
        }

        let fake = Fake::start(Cfg::default());
        let d = tempfile::tempdir().unwrap();
        let st = Arc::new(store(d.path()));
        saved(&st, svc(&fake.resource_url()));
        connect(&st, &FakeBrowser, "svc").unwrap();
        let ask = |worker: Arc<Worker>, args: Value| {
            let mut services = Services::new();
            services.insert(st.clone());
            services.insert(Arc::new(Broadcaster(worker)));
            let ctx = ToolCtx {
                caller_chat_id: "chat-7".into(),
                is_child: false,
                tool_call_id: None,
                origin: CallOrigin::Agent,
                services: Arc::new(services),
                events: Arc::new(NullSink),
                ops: None,
            };
            crate::tools::oauth_header_tool(&ctx, args)
        };
        let refused = fingerprint("fake-access-1");

        let w = Arc::new(Worker {
            running: true,
            ..Default::default()
        });
        let answer = ask(w.clone(), json!({"serverId": "svc", "refused": refused})).unwrap();
        assert_eq!(answer, json!({"delivered": true}));
        let sent = w.lines.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "chat-7", "only the asking chat's worker is told");
        assert_eq!(
            sent[0].1,
            json!({"type": HEADER_LINE, "serverId": "svc", "header": "Bearer fake-access-2"})
        );
        drop(sent);

        // A chat whose worker is gone is told so, in words with no token in them; a refused
        // token that is not a fingerprint is ignored.
        let gone = Arc::new(Worker::default());
        let err = ask(gone, json!({"serverId": "svc", "refused": "not hex!"})).unwrap_err();
        assert!(err.text().contains("not running"), "{}", err.text());
        assert!(!err.text().contains("fake-"));
        // A server that is not set up for the sign-in, or is unknown, is refused plainly.
        for bad in [json!({"serverId": "nope"}), json!({})] {
            assert!(ask(w.clone(), bad).is_err());
        }
    }

    /// A client id and secret entered once (the secret is sealed on save, never read back) and
    /// a fixed return address are both honoured.
    #[test]
    fn an_entered_client_id_secret_and_return_address_are_used() {
        let fake = Fake::start(Cfg {
            expect_client: Some(("pre-id".into(), "pre-secret".into())),
            no_register: true,
            ..Cfg::default()
        });
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let mut s = svc(&fake.resource_url());
        s.transport.oauth = Some(OAuthConfig {
            client_id: Some("pre-id".into()),
            client_secret: Some(crate::types::ValueRef {
                profiles: Default::default(),
                kind: crate::types::RefKind::Literal,
                value: Some("pre-secret".into()),
                secret_id: None,
                prefix: None,
            }),
            redirect_uri: Some(format!("http://127.0.0.1:{free}/callback")),
            ..Default::default()
        });
        saved(&st, s);
        let on_disk = std::fs::read_to_string(d.path().join(crate::config::FILE_NAME)).unwrap();
        assert!(
            !on_disk.contains("pre-secret"),
            "the secret is sealed on save"
        );
        let snapshot = serde_json::to_string(&st.snapshot().unwrap()).unwrap();
        assert!(!snapshot.contains("pre-secret"));

        let status = connect(&st, &FakeBrowser, "svc").unwrap();
        assert!(status.connected);
        assert_eq!(
            fake.last_redirect().as_deref(),
            Some(format!("http://127.0.0.1:{free}/callback").as_str())
        );
        // A client id that was entered is not a registration to reuse.
        assert_eq!(
            load_auth(&st, "svc").unwrap().unwrap().registered_redirect,
            None
        );
        // A secret that cannot be read again is said in words, without the secret.
        let secret_file = std::fs::read_dir(d.path().join("credentials"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("lit_"))
            })
            .unwrap();
        std::fs::remove_file(secret_file).unwrap();
        let words = match connect(&st, &FakeBrowser, "svc") {
            Err(ConnectFail::Bad(w)) => w,
            other => panic!("{other:?}"),
        };
        assert!(words.contains("client secret"), "{words}");
        assert!(!words.contains("pre-secret"));
    }

    #[test]
    fn every_way_a_server_cannot_be_signed_in_to_is_said_in_words() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());

        // Nothing kept yet: the resolve says connect, and no chat tries without a credential.
        saved(&st, svc("https://mcp.example.test/mcp"));
        let (_, r) = st.resolve_enabled(&|_| None).unwrap();
        assert_eq!(r[0].error.as_deref(), Some(NOT_CONNECTED));
        assert_eq!(
            current_header(&st, &st.load().unwrap()[0], None).unwrap_err(),
            NOT_CONNECTED
        );

        // A program on this PC, a turned-off server and a server without the browser sign-in
        // are each refused before anything is opened.
        let mut program = svc("https://mcp.example.test/mcp");
        program.id = "prog".into();
        program.transport.kind = TransportKind::Stdio;
        program.transport.url = None;
        program.transport.oauth = None;
        program.transport.command = Some("node.exe".into());
        saved(&st, program);
        let words = match connect(&st, &FakeBrowser, "prog") {
            Err(ConnectFail::Bad(w)) => w,
            other => panic!("{other:?}"),
        };
        assert!(words.contains("Only a server at an address"), "{words}");

        let mut off = svc("https://mcp.example.test/mcp");
        off.id = "off".into();
        off.enabled = false;
        saved(&st, off);
        let words = match connect(&st, &FakeBrowser, "off") {
            Err(ConnectFail::Bad(w)) => w,
            other => panic!("{other:?}"),
        };
        assert!(words.contains("Turn the server on first"), "{words}");

        // A server with no browser sign-in set up is refused too, and so is one that publishes
        // nothing readable.
        let mut plain = svc("https://mcp.example.test/mcp");
        plain.id = "plain".into();
        plain.transport.oauth = None;
        saved(&st, plain);
        let words = match connect(&st, &FakeBrowser, "plain") {
            Err(ConnectFail::Bad(w)) => w,
            other => panic!("{other:?}"),
        };
        assert!(words.contains("not set up"), "{words}");

        let silent = Fake::start(Cfg {
            no_prm: true,
            oidc_only: true,
            hide_as_doc: true,
            ..Cfg::default()
        });
        let mut ghost = svc(&silent.resource_url());
        ghost.id = "ghost".into();
        saved(&st, ghost);
        let words = match connect(&st, &FakeBrowser, "ghost") {
            Err(ConnectFail::Bad(w)) => w,
            other => panic!("{other:?}"),
        };
        assert!(words.contains("did not say"), "{words}");
    }

    #[test]
    fn a_refused_sign_in_says_so_without_repeating_anything_secret() {
        // The server expects a pre-entered client that answers with a secret we never store:
        // the token call is refused, and the words stay clean.
        let fake = Fake::start(Cfg {
            expect_client: Some(("pre-id".into(), "pre-secret".into())),
            no_register: true,
            ..Cfg::default()
        });
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let mut s = svc(&fake.resource_url());
        s.transport.oauth = Some(OAuthConfig {
            client_id: Some("pre-id".into()),
            ..Default::default()
        });
        saved(&st, s);
        let words = match connect(&st, &FakeBrowser, "svc") {
            Err(ConnectFail::Bad(w)) => w,
            other => panic!("{other:?}"),
        };
        assert!(words.contains("refused"), "{words}");
        for leak in ["pre-secret", "fake-access", "fake-refresh"] {
            assert!(!words.contains(leak), "{leak} in {words}");
        }
        // Nothing was kept, and status still says not connected.
        assert!(!status_of(&st, "svc").connected);
    }
}
