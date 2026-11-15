//! A reusable OAuth 2.x client: find a server's sign-in addresses, sign a person in through
//! their browser with a loopback redirect, and keep the resulting tokens in the credential
//! store. Both the sign-in for remote MCP servers and the one for Google accounts are built
//! on this; neither carries its own copy of these rules.
//!
//! The rules that hold everywhere here:
//! - Authorization-code flow with PKCE (S256) always, and a random `state`.
//! - The redirect comes back to `127.0.0.1` on an ephemeral port, once, at `/callback`.
//! - Every address the client connects to is https; plain http answers only from this
//!   computer, and only in test builds.
//! - Access tokens, refresh tokens, sign-in codes, client secrets and the PKCE verifier
//!   never appear in a `Debug`, a `Display`, an error string or a log line. Errors say what
//!   failed in plain words.
//! - Tokens are persisted only through [`CredentialStore`](helm_core::credentials::CredentialStore),
//!   sealed and bound to this profile and entry.

pub mod discovery;
pub mod loopback;
mod pkce;
pub mod register;
pub mod tokens;

#[cfg(test)]
pub(crate) mod fake;

pub use discovery::{discover, AuthServer};
pub use register::{register, Client};
pub use tokens::{refresh, StoredAuth, TokenSet, TokenStore};

use loopback::{CallbackError, Loopback};
use url::{Host, Url};

use std::time::Duration;

/// How long a browser sign-in may take before Helm gives up on the callback.
pub const WAIT: Duration = Duration::from_secs(300);
const AGENT_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything that can go wrong, said in words a person can read. No variant carries a value
/// that came from a token exchange.
#[derive(Debug, thiserror::Error)]
pub enum OauthError {
    #[error("The address is not usable.")]
    BadUrl,
    #[error("Sign-in addresses must be https; only this computer may answer over plain http.")]
    InsecureUrl,
    #[error("Could not reach the sign-in service.")]
    Connection,
    #[error("The sign-in service answered {0}.")]
    Unreachable(u16),
    #[error("The sign-in service's document was not readable.")]
    BadMetadata,
    #[error("The sign-in service did not say where the sign-in or the tokens live.")]
    NoAuthServer,
    #[error("This server does not hand out client ids itself; enter a client id instead.")]
    NoRegistration,
    #[error("The sign-in was refused (status {0}).")]
    Refused(u16),
    #[error("The sign-in was refused: {1} (status {0}).")]
    RefusedWord(u16, String),
    #[error("Nobody finished the sign-in in the browser in time. Try connecting again.")]
    TimedOut,
    #[error("The saved sign-in could not be read.")]
    Storage,
    #[error("The sign-in could not be saved: {0}")]
    StorageWords(String),
}

/// The agent every request here goes through: never follows redirects, always times out.
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(AGENT_TIMEOUT))
        .user_agent("helm-oauth")
        .build()
        .new_agent()
}

/// https everywhere; plain http only to this computer, and only in test builds.
pub(crate) fn endpoint_allowed(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" || !cfg!(debug_assertions) {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(a)) => a.is_loopback(),
        Some(Host::Ipv6(a)) => a.is_loopback(),
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        _ => false,
    }
}

/// A sign-in in progress: the address to open in the browser, and the listener the browser
/// will come back to. [`Pending::finish`] waits for that return and trades the code it carries
/// for tokens.
pub struct Pending {
    /// Open this in the system browser.
    pub auth_url: String,
    /// The address the browser is redirected back to.
    redirect_uri: String,
    agent: ureq::Agent,
    server: AuthServer,
    client: Client,
    verifier: String,
    resource: Option<String>,
    listener: Loopback,
}

/// The verifier and the `state` are never printed; neither is the address they sit in.
impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("redirect_uri", &self.redirect_uri)
            .field("client", &self.client)
            .field("waiting", &true)
            .finish()
    }
}

/// Start a browser sign-in. `scopes` are asked for as one scope string; `resource` names the
/// protected server the tokens should be valid at, when discovery found one.
pub fn begin(
    agent: &ureq::Agent,
    server: &AuthServer,
    client: &Client,
    scopes: &[String],
    resource: Option<&str>,
) -> Result<Pending, OauthError> {
    let state = pkce::random_state();
    let listener = Loopback::bind(&state, WAIT).map_err(|_| OauthError::Connection)?;
    let redirect_uri = listener.redirect_uri();
    let verifier = pkce::random_verifier();
    let Ok(mut url) = Url::parse(&server.authorization_endpoint) else {
        return Err(OauthError::BadUrl);
    };
    if !endpoint_allowed(&url) {
        return Err(OauthError::InsecureUrl);
    }
    let challenge = pkce::challenge(&verifier);
    let scope = scopes.join(" ");
    {
        let mut q = url.query_pairs_mut();
        q.extend_pairs([
            ("response_type", "code"),
            ("client_id", client.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        if !scope.is_empty() {
            q.append_pair("scope", &scope);
        }
        if let Some(r) = resource {
            q.append_pair("resource", r);
        }
    }
    Ok(Pending {
        auth_url: url.to_string(),
        redirect_uri,
        agent: agent.clone(),
        server: server.clone(),
        client: client.clone(),
        verifier,
        resource: resource.map(str::to_owned),
        listener,
    })
}

impl Pending {
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the browser's return and exchange the code. Gives up after [`WAIT`].
    pub fn finish(&self) -> Result<TokenSet, OauthError> {
        self.finish_within(WAIT)
    }

    pub(crate) fn finish_within(&self, budget: Duration) -> Result<TokenSet, OauthError> {
        let code = match self.listener.wait(budget) {
            Ok(code) => code,
            Err(CallbackError::TimedOut) | Err(CallbackError::Closed) => {
                return Err(OauthError::TimedOut)
            }
        };
        tokens::exchange(
            &self.agent,
            &self.server,
            &self.client,
            &code,
            &self.redirect_uri,
            &self.verifier,
            self.resource.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::fake::{Cfg, Fake};
    use helm_core::credentials::CredentialStore;
    use helm_platform::keychain::MemoryKeychain;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;

    /// A deliberately plain browser: one GET, no cookies, no redirects followed.
    fn raw_get(url: &str) -> (u16, HashMap<String, String>, String) {
        let u = Url::parse(url).unwrap();
        let host = u.host_str().unwrap();
        let port = u.port_or_known_default().unwrap();
        let mut s = TcpStream::connect((host, port)).unwrap();
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
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
        let mut lines = head.lines();
        let status: u16 = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
            }
        }
        (status, headers, body.to_owned())
    }

    /// Walk the browser half: open the sign-in address, follow its redirect to Helm's
    /// callback. Returns the callback page's body.
    fn open_in_browser(auth_url: &str) -> String {
        let (status, headers, _) = raw_get(auth_url);
        assert_eq!(status, 302, "the fake server redirected the browser");
        let location = headers.get("location").expect("a redirect address").clone();
        raw_get(&location).2
    }

    fn sample_auth() -> StoredAuth {
        StoredAuth {
            client: Client::pre_registered("id-1", Some("s-1")),
            tokens: TokenSet {
                access_token: "fake-access-1".into(),
                token_type: "Bearer".into(),
                expires_at: Some(1_800_000_000),
                refresh_token: Some("fake-refresh-1".into()),
                scope: Some("read".into()),
                id_token: None,
            },
            resource: Some("https://mcp.example/mcp".into()),
            stored_at: None,
        }
    }

    #[test]
    fn a_full_sign_in_with_a_handed_out_client_id() {
        let fake = Fake::start(Cfg {
            require_resource: true,
            ..Cfg::default()
        });
        let agent = http_agent();
        let server = discover(&agent, &fake.resource_url(), None).unwrap();
        assert_eq!(server.token_endpoint, format!("{}/token", fake.base));
        assert!(server.registration_endpoint.is_some());
        let client = register(&agent, &server, "http://127.0.0.1:1/callback").unwrap();
        assert_eq!(client.client_id, "fake-client-1");
        assert_eq!(client.client_secret.as_deref(), Some("fake-secret-9"));

        let scopes = ["read".to_owned(), "send".to_owned()];
        let pending = begin(
            &agent,
            &server,
            &client,
            &scopes,
            Some(&fake.resource_url()),
        )
        .unwrap();
        assert!(pending.auth_url.contains("code_challenge="));
        assert!(pending.auth_url.contains("code_challenge_method=S256"));
        assert!(pending.auth_url.contains("scope=read+send"));

        let page = open_in_browser(&pending.auth_url);
        assert!(page.contains("close this tab"), "{page}");
        let tokens = pending.finish_within(Duration::from_secs(5)).unwrap();
        assert_eq!(tokens.access_token, "fake-access-1");
        assert_eq!(tokens.refresh_token.as_deref(), Some("fake-refresh-1"));
        assert_eq!(tokens.token_type, "Bearer");
        assert_eq!(tokens.scope.as_deref(), Some("read send"));
        assert!(tokens.expires_at.is_some());
        assert!(!tokens.expires_within(Duration::from_secs(60)));

        // A refresh gives a new access token for the same client.
        let refreshed = refresh(
            &agent,
            &server,
            &client,
            tokens.refresh_token.as_deref().unwrap(),
            Some(&fake.resource_url()),
        )
        .unwrap();
        assert_eq!(refreshed.access_token, "fake-access-2");
        assert_eq!(
            refreshed.refresh_token.as_deref(),
            Some("fake-refresh-1"),
            "the same refresh token keeps working"
        );
    }

    #[test]
    fn a_client_id_entered_once_works_without_registration() {
        let fake = Fake::start(Cfg {
            expect_client: Some(("pre-id".into(), "pre-secret".into())),
            no_register: true,
            ..Cfg::default()
        });
        let agent = http_agent();
        let server = discover(&agent, &fake.resource_url(), None).unwrap();
        assert!(server.registration_endpoint.is_none());
        assert!(matches!(
            register(&agent, &server, "http://127.0.0.1:1/callback"),
            Err(OauthError::NoRegistration)
        ));
        let client = Client::pre_registered("pre-id", Some("pre-secret"));
        let pending = begin(&agent, &server, &client, &[], None).unwrap();
        open_in_browser(&pending.auth_url);
        let tokens = pending.finish_within(Duration::from_secs(5)).unwrap();
        assert_eq!(tokens.access_token, "fake-access-1");

        // A wrong secret never gets far: the token call is refused, and the words stay clean.
        let impostor = Client::pre_registered("pre-id", Some("wrong"));
        let pending = begin(&agent, &server, &impostor, &[], None).unwrap();
        open_in_browser(&pending.auth_url);
        let why = pending.finish_within(Duration::from_secs(5)).unwrap_err();
        let said = format!("{why}");
        assert!(said.contains("refused"), "{said}");
        assert!(
            !said.contains("wrong") && !said.contains("pre-secret"),
            "{said}"
        );
    }

    #[test]
    fn the_callback_is_only_belonging_to_this_sign_in() {
        let fake = Fake::start(Cfg::default());
        let agent = http_agent();
        let server = discover(&agent, &fake.resource_url(), None).unwrap();
        let client = Client::pre_registered("c", None);
        let pending = begin(&agent, &server, &client, &[], None).unwrap();
        // A callback carrying another sign-in's state is refused by the page, and nothing is
        // exchanged: the wait runs out.
        let (status, headers, _) = raw_get(&pending.auth_url);
        assert_eq!(status, 302);
        let location = headers.get("location").unwrap().clone();
        let tampered = location.replace("state=", "state=notthe");
        let page = raw_get(&tampered).2;
        assert!(page.contains("did not finish"), "{page}");
        // A different path on the same address is refused too.
        let port = pending
            .redirect_uri()
            .rsplit(':')
            .next()
            .unwrap()
            .to_owned();
        let page = raw_get(&format!("http://127.0.0.1:{port}/nope")).2;
        assert!(page.contains("finished"), "{page}");
        assert!(matches!(
            pending.finish_within(Duration::from_millis(400)),
            Err(OauthError::TimedOut)
        ));
    }

    #[test]
    fn a_code_that_cannot_be_claimed_is_refused_plainly() {
        // The fake trades its code only to a client that proves the PKCE challenge; with the
        // proof broken on the server side, the exchange simply fails, in plain words.
        let fake = Fake::start(Cfg {
            break_pkce: true,
            ..Cfg::default()
        });
        let agent = http_agent();
        let server = discover(&agent, &fake.resource_url(), None).unwrap();
        let client = Client::pre_registered("c", None);
        let pending = begin(&agent, &server, &client, &[], None).unwrap();
        let challenge = pending
            .auth_url
            .split("code_challenge=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned();
        open_in_browser(&pending.auth_url);
        let why = pending.finish_within(Duration::from_secs(5)).unwrap_err();
        let said = format!("{why}");
        assert!(said.contains("refused"), "{said}");
        assert!(
            !said.contains("fake-") && !said.contains(&challenge),
            "{said}"
        );
    }

    #[test]
    fn discovery_walks_every_published_way() {
        let agent = http_agent();
        // 1. The hint from a 401 header, 2. the well-known resource document,
        // 3. no resource document at all, 4. an OpenID-style server.
        let f1 = Fake::start(Cfg::default());
        let f2 = Fake::start(Cfg::default());
        let f3 = Fake::start(Cfg {
            no_prm: true,
            ..Cfg::default()
        });
        let f4 = Fake::start(Cfg {
            oidc_only: true,
            no_prm: true,
            ..Cfg::default()
        });
        let hint = format!(
            "Bearer realm=\"x\", error=\"invalid_token\", resource_metadata=\"{}/prm\"",
            f1.base
        );
        let found = [
            discover(&agent, &f1.resource_url(), Some(&hint)).unwrap(),
            discover(&agent, &f2.resource_url(), None).unwrap(),
            discover(&agent, &f3.resource_url(), None).unwrap(),
            discover(&agent, &f4.resource_url(), None).unwrap(),
        ];
        for (s, f) in found.iter().zip([&f1, &f2, &f3, &f4]) {
            assert_eq!(s.authorization_endpoint, format!("{}/authorize", f.base));
            assert_eq!(s.token_endpoint, format!("{}/token", f.base));
        }
    }

    #[test]
    fn garbled_or_oversized_documents_are_a_clean_refusal() {
        let agent = http_agent();
        let fake = Fake::start(Cfg {
            garble_metadata: true,
            ..Cfg::default()
        });
        let why = discover(&agent, &fake.resource_url(), None).unwrap_err();
        assert!(format!("{why}").contains("not readable"), "{why}");
        let fake = Fake::start(Cfg {
            huge_metadata: true,
            ..Cfg::default()
        });
        assert!(discover(&agent, &fake.resource_url(), None).is_err());
        // Nothing published at all: a plain refusal too.
        let fake = Fake::start(Cfg {
            no_prm: true,
            oidc_only: true,
            hide_as_doc: true,
            ..Cfg::default()
        });
        let why = discover(&agent, &fake.resource_url(), None).unwrap_err();
        assert!(format!("{why}").contains("did not say"), "{why}");
    }

    #[test]
    fn tokens_survive_only_in_the_credential_store() {
        let d = tempfile::tempdir().unwrap();
        let creds = CredentialStore::new(
            &d.path().join("auth"),
            "profile-1",
            Arc::new(MemoryKeychain::default()),
        );
        let auth = sample_auth();
        TokenStore::save(&creds, "server-a", "mcp:server-a", &auth).unwrap();
        let loaded = TokenStore::load(&creds, "server-a", "mcp:server-a").unwrap();
        // The load matches apart from the storage time the save stamped in.
        let got = loaded.unwrap();
        assert_eq!(got.client, auth.client);
        assert_eq!(got.tokens, auth.tokens);
        assert_eq!(got.resource, auth.resource);
        assert!(got.stored_at.is_some());
        // Nothing plaintext on disk.
        let raw = std::fs::read(d.path().join("auth").join("server-a.cred")).unwrap();
        assert!(!raw.windows(12).any(|w| w == b"fake-access-"));
        // Forget: gone, and a missing entry is simply not connected.
        TokenStore::forget(&creds, "server-a").unwrap();
        assert_eq!(
            TokenStore::load(&creds, "server-a", "mcp:server-a").unwrap(),
            None
        );
        // An envelope written under one identity cannot be opened under another.
        TokenStore::save(&creds, "server-b", "mcp:server-b", &auth).unwrap();
        assert!(TokenStore::load(&creds, "server-b", "mcp:other").is_err());
    }

    #[test]
    fn nothing_secret_ever_prints() {
        let auth = sample_auth();
        let said = format!("{auth:?}");
        for leak in ["fake-access-1", "fake-refresh-1", "s-1"] {
            assert!(!said.contains(leak), "{leak} printed in {said}");
        }
        let agent = http_agent();
        let fake = Fake::start(Cfg::default());
        let server = discover(&agent, &fake.resource_url(), None).unwrap();
        let pending = begin(&agent, &server, &auth.client, &[], None).unwrap();
        let said = format!("{pending:?}");
        assert!(
            !said.contains("code_challenge"),
            "the sign-in address: {said}"
        );
        assert!(!said.contains("state="), "the state value: {said}");
        // A token call's refusal prints no body either.
        let why = refresh(&agent, &server, &auth.client, "not-a-token", None).unwrap_err();
        assert!(!format!("{why}").contains("fake-"));
    }
}
