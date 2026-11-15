//! Tokens: the exchange of a sign-in code for tokens, refresh, and where tokens live.
//!
//! Tokens are kept only through the credential store: sealed envelopes bound to this profile
//! and to the entry's identity. There is no plaintext anywhere, and none of these values ever
//! appears in a `Debug`, a `Display` or an error string.

use super::discovery::AuthServer;
use super::loopback::{form_encode, form_encode_value};
use super::{endpoint_allowed, OauthError};
use base64::Engine as _;
use helm_core::credentials::CredentialError;
use helm_core::credentials::CredentialStore;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub token_type: String,
    /// Until when the access token stands, as seconds since the unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Sent back only for the server that issued it (Google reads the address from it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
}

impl TokenSet {
    /// True when the access token will stand for no longer than `margin` from now.
    pub fn expires_within(&self, margin: Duration) -> bool {
        let Some(at) = self.expires_at else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        at <= now + margin.as_secs()
    }
}

/// No token value is ever printed.
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<set>")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<set>"),
            )
            .field("scope", &self.scope)
            .field("id_token", &self.id_token.as_ref().map(|_| "<set>"))
            .finish()
    }
}

/// What is kept for one sign-in: the client that was used and the tokens it holds.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAuth {
    pub client: super::register::Client,
    pub tokens: TokenSet,
    /// The protected resource the tokens were asked for, if any (they are only valid there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// The sign-in service the tokens came from, as discovery found it at sign-in. A refresh goes
    /// only to its token address, never to one found later from the server's current address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_server: Option<AuthServer>,
    /// The redirect address a handed-out client id was registered with (none for a client id the
    /// person entered), so a later sign-in can come back on the same one and reuse the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_redirect: Option<String>,
    /// When the sign-in was stored, as seconds since the unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_at: Option<u64>,
}

impl std::fmt::Debug for StoredAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredAuth")
            .field("client", &self.client)
            .field("tokens", &self.tokens)
            .field("resource", &self.resource)
            .field("auth_server", &self.auth_server)
            .field("registered_redirect", &self.registered_redirect)
            .field("stored_at", &self.stored_at)
            .finish()
    }
}

/// Tokens live only in the credential store. `name` picks the entry file (plain characters
/// only, as the store requires), `identity` binds the envelope to this entry, so a file copied
/// elsewhere on this profile cannot be opened.
pub struct TokenStore;

impl TokenStore {
    pub fn save(
        creds: &CredentialStore,
        name: &str,
        identity: &str,
        auth: &StoredAuth,
    ) -> Result<(), OauthError> {
        let stored_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .ok();
        let auth = StoredAuth {
            stored_at,
            ..auth.clone()
        };
        let json = serde_json::to_vec(&auth).map_err(|_| OauthError::Storage)?;
        creds
            .write(name, identity, &json)
            .map_err(|e| OauthError::StorageWords(store_words(&e)))
    }

    /// The stored sign-in, or `None` when there is none.
    pub fn load(
        creds: &CredentialStore,
        name: &str,
        identity: &str,
    ) -> Result<Option<StoredAuth>, OauthError> {
        match creds.read(name, identity) {
            Ok(json) => serde_json::from_slice(&json)
                .map(Some)
                .map_err(|_| OauthError::Storage),
            Err(CredentialError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(OauthError::StorageWords(store_words(&e))),
        }
    }

    /// Forget a sign-in: remove the entry here. Nothing is revoked at the server.
    pub fn forget(creds: &CredentialStore, name: &str) -> Result<(), OauthError> {
        creds
            .forget(name)
            .map_err(|e| OauthError::StorageWords(store_words(&e)))
    }
}

fn store_words(e: &CredentialError) -> String {
    e.to_string()
}

/// Trade a sign-in code for tokens. The `verifier` is the one from `begin`; `resource` is the
/// protected server the tokens are meant for, when discovery named one.
pub(crate) fn exchange(
    agent: &ureq::Agent,
    server: &AuthServer,
    client: &super::register::Client,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    resource: Option<&str>,
) -> Result<TokenSet, OauthError> {
    let mut pairs: Vec<(&str, String)> = vec![
        ("grant_type", "authorization_code".into()),
        ("code", code.into()),
        ("redirect_uri", redirect_uri.into()),
        ("client_id", client.client_id.clone()),
        ("code_verifier", verifier.into()),
    ];
    if let Some(r) = resource {
        pairs.push(("resource", r.to_owned()));
    }
    token_request(agent, server, client, &pairs)
}

/// Ask the server for a new access token with the refresh token.
pub fn refresh(
    agent: &ureq::Agent,
    server: &AuthServer,
    client: &super::register::Client,
    refresh_token: &str,
    resource: Option<&str>,
) -> Result<TokenSet, OauthError> {
    let mut pairs: Vec<(&str, String)> = vec![
        ("grant_type", "refresh_token".into()),
        ("refresh_token", refresh_token.into()),
        ("client_id", client.client_id.clone()),
    ];
    if let Some(r) = resource {
        pairs.push(("resource", r.to_owned()));
    }
    let mut tokens = token_request(agent, server, client, &pairs)?;
    // A server that does not rotate refresh tokens leaves the field out of its answer: the one
    // used keeps working, so it is kept rather than lost.
    if tokens.refresh_token.as_deref().unwrap_or("").is_empty() {
        tokens.refresh_token = Some(refresh_token.to_owned());
    }
    Ok(tokens)
}

fn token_request(
    agent: &ureq::Agent,
    server: &AuthServer,
    client: &super::register::Client,
    pairs: &[(&str, String)],
) -> Result<TokenSet, OauthError> {
    let Ok(url) = Url::parse(&server.token_endpoint) else {
        return Err(OauthError::BadUrl);
    };
    if !endpoint_allowed(&url) {
        return Err(OauthError::InsecureUrl);
    }
    let owned: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = form_encode(&owned);
    let mut req = agent
        .post(url.as_str())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json");
    if let Some(secret) = &client.client_secret {
        // Both halves are form-encoded before they are joined (RFC 6749 section 2.3.1).
        let pair = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            form_encode_value(&client.client_id),
            form_encode_value(secret)
        ));
        req = req.header("Authorization", format!("Basic {pair}"));
    }
    // A refusal is an answer with a body here: its standard error word is read from it.
    let mut res = match req.config().http_status_as_error(false).build().send(body) {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(code)) => return Err(token_refusal(code, None)),
        Err(_) => return Err(OauthError::Connection),
    };
    let status = res.status().as_u16();
    let body = res
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .map_err(|_| OauthError::BadMetadata)?;
    // Only the standard machine word of an error is kept; a server's own description could
    // carry anything, and the body itself is never repeated.
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        access_token: String,
        #[serde(default)]
        token_type: String,
        #[serde(default)]
        expires_in: Option<u64>,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        id_token: Option<String>,
        #[serde(default)]
        error: Option<String>,
    }
    let parsed: Resp = serde_json::from_str(&body).map_err(|_| {
        if (200..300).contains(&status) {
            OauthError::BadMetadata
        } else {
            OauthError::Refused(status)
        }
    })?;
    if !((200..300).contains(&status) && !parsed.access_token.is_empty()) {
        return Err(token_refusal(status, parsed.error.as_deref()));
    }
    let expires_at = parsed.expires_in.map(|secs| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() + secs)
            .unwrap_or(0)
    });
    Ok(TokenSet {
        access_token: parsed.access_token,
        token_type: if parsed.token_type.is_empty() {
            "Bearer".to_owned()
        } else {
            parsed.token_type
        },
        expires_at,
        refresh_token: parsed.refresh_token,
        scope: parsed.scope.filter(|s| !s.is_empty()),
        id_token: parsed.id_token.filter(|s| !s.is_empty()),
    })
}

/// A refusal in plain words: the status and, when the server gave one, its standard error word.
fn token_refusal(status: u16, reason: Option<&str>) -> OauthError {
    let word = reason.filter(|r| {
        !r.is_empty() && r.len() <= 40 && r.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    });
    match word {
        Some(w) => OauthError::RefusedWord(status, w.to_owned()),
        None => OauthError::Refused(status),
    }
}
