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
//! refuses the header the chat connected with, the worker asks once for a current one, and this
//! module answers it (refreshing first when the token is old, expired or just refused).
//!
//! The rules:
//! - Tokens live only in the credential store, one entry per server, bound to the server's id.
//! - Nothing here returns, logs or prints a token: op results say only that the server is
//!   connected and until when the token stands; refusals are plain words.
//! - A connect is one whole flow on the calling op's thread: discover, take or get a client id,
//!   open the system browser, wait on the loopback callback, keep the tokens. It gives up with
//!   the client module's five-minute wait.

use crate::config::McpStore;
use crate::oauth::{self, StoredAuth, TokenStore};
use crate::types::{McpServer, OAuthStatus, TransportKind};
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

/// The host tool the worker asks for a current `Authorization` header. Never listed to agents.
pub const HEADER_TOOL: &str = "app_mcp_oauth_header";

/// An access token with less than this left is refreshed before use.
const REFRESH_MARGIN: Duration = Duration::from_secs(300);

/// The words a not-connected server is reported with, in the chat's server list as well.
pub const NOT_CONNECTED: &str = "Not connected: open Settings → MCP and click Connect.";

/// The credential entry for one server's sign-in, and the identity its envelope is bound to.
fn entry_name(server_id: &str) -> String {
    format!("mcp-oauth-{server_id}")
}

fn entry_identity(server_id: &str) -> String {
    format!("mcp-oauth:{server_id}")
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

/// What is kept about a sign-in, without any token material.
fn status_of(store: &McpStore, id: &str) -> OAuthStatus {
    let loaded = TokenStore::load(store.creds(), &entry_name(id), &entry_identity(id));
    match loaded.ok().flatten() {
        Some(auth) => OAuthStatus {
            connected: true,
            expires_at: auth.tokens.expires_at,
        },
        None => OAuthStatus {
            connected: false,
            expires_at: None,
        },
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
    let agent = oauth::http_agent();

    // Where the sign-in lives, as the server publishes it.
    let as_info = oauth::discover(&agent, &url, None).map_err(|e| Bad(e.to_string()))?;
    // A client id entered once, or one the server hands out. A handed-out id is asked for with
    // the loopback shape the sign-in will actually come back on; the browser callback always
    // lands on this computer.
    let client = match server
        .transport
        .oauth
        .as_ref()
        .and_then(|o| o.client_id.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(cid) => oauth::Client::pre_registered(cid, None),
        None => oauth::register(&agent, &as_info, "http://127.0.0.1:1/callback")
            .map_err(|e| Bad(e.to_string()))?,
    };
    let scopes: Vec<String> = server
        .transport
        .oauth
        .as_ref()
        .and_then(|o| o.scope.as_deref())
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let pending = oauth::begin(&agent, &as_info, &client, &scopes, Some(&url))
        .map_err(|e| Bad(e.to_string()))?;
    if let Err(e) = open.open_url(&pending.auth_url) {
        return Err(Internal(format!(
            "The system browser could not be opened: {e}"
        )));
    }
    // The wait is the client module's: five minutes, one callback, this computer only.
    let tokens = pending.finish().map_err(|e| Bad(e.to_string()))?;
    let auth = StoredAuth {
        client,
        tokens,
        resource: Some(url),
        stored_at: None,
    };
    TokenStore::save(store.creds(), &entry_name(id), &entry_identity(id), &auth)
        .map_err(|e| Internal(e.to_string()))?;
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

/// The access token to use for `server` now: the stored one, refreshed first when `force` is
/// set or when it has under [`REFRESH_MARGIN`] left. A refresh that the sign-in service refuses
/// falls back to the stored token and lets the server itself decide.
///
/// This reaches the network only when a refresh is due; `Err` words never carry token material.
pub(crate) fn current_access(
    store: &McpStore,
    server: &McpServer,
    force: bool,
) -> Result<String, String> {
    let auth = load_auth(store, &server.id)?.ok_or_else(|| NOT_CONNECTED.to_owned())?;
    let refreshable = auth.tokens.refresh_token.as_deref().unwrap_or("");
    let due = force || auth.tokens.expires_within(REFRESH_MARGIN);
    if due && !refreshable.is_empty() {
        let url = server.transport.url.as_deref().unwrap_or("");
        let agent = oauth::http_agent();
        if let Ok(as_info) = oauth::discover(&agent, url, None) {
            let resource = auth.resource.clone().or_else(|| Some(url.to_owned()));
            let refresh = oauth::refresh(
                &agent,
                &as_info,
                &auth.client,
                refreshable,
                resource.as_deref(),
            );
            if let Ok(tokens) = refresh {
                let saved = StoredAuth {
                    client: auth.client.clone(),
                    tokens: tokens.clone(),
                    resource,
                    stored_at: None,
                };
                // A token that could not be saved still works for now; the next connect or
                // refresh will notice.
                let _ = TokenStore::save(
                    store.creds(),
                    &entry_name(&server.id),
                    &entry_identity(&server.id),
                    &saved,
                );
                return Ok(tokens.access_token);
            }
        }
    }
    Ok(auth.tokens.access_token)
}

/// The header the worker's retry sends, as the host tool answers it. `force` asks for a fresh
/// token even when the stored one still looks valid: the server has just refused it.
pub(crate) fn current_header(store: &McpStore, server: &McpServer) -> Result<String, String> {
    let access = current_access(store, server, true)?;
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

        // The worker's one-time ask refreshes without waiting for the expiry.
        let server = st.load().unwrap().remove(0);
        let header = current_header(&st, &server).unwrap();
        assert_eq!(header, "Bearer fake-access-2");
        // The refreshed set is what is kept, and resolving again reuses it.
        let kept = TokenStore::load(st.creds(), "mcp-oauth-svc", "mcp-oauth:svc")
            .unwrap()
            .unwrap();
        assert_eq!(kept.tokens.access_token, "fake-access-2");
        let (_, r) = st.resolve_enabled(&|_| None).unwrap();
        assert_eq!(r[0].headers["Authorization"], "Bearer fake-access-2");

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

    #[test]
    fn every_way_a_server_cannot_be_signed_in_to_is_said_in_words() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());

        // Nothing kept yet: the resolve says connect, and no chat tries without a credential.
        saved(&st, svc("https://mcp.example.test/mcp"));
        let (_, r) = st.resolve_enabled(&|_| None).unwrap();
        assert_eq!(r[0].error.as_deref(), Some(NOT_CONNECTED));
        assert_eq!(
            current_header(&st, &st.load().unwrap()[0]).unwrap_err(),
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
