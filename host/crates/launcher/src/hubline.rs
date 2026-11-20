//! The hub's addresses and token, read from the same sealed envelope the relay line saves.
//!
//! The launcher reads three fields and nothing else, with its own small struct: it does not
//! depend on the relay crate. The token is never printed, logged or put in an error; `Debug`
//! shows only whether each field is set.

use helm_core::credentials::{CredentialError, CredentialStore};
use serde::Deserialize;

/// The envelope's name and identity in the profile's credential store (the relay line's own).
pub const CRED_NAME: &str = "hub-relay";
pub const CRED_IDENTITY: &str = "hub-relay-connection";
/// Debug builds only: the same test override the relay line honours.
pub const TEST_URL_ENV: &str = "HELM_TEST_RELAY_URL";
pub const TEST_TOKEN_ENV: &str = "HELM_TEST_RELAY_TOKEN";

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HubLine {
    #[serde(default)]
    pub lan_url: String,
    #[serde(default)]
    pub fallback_url: String,
    #[serde(default)]
    pub token: String,
}

impl std::fmt::Debug for HubLine {
    // Never print a secret, even in a test failure.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubLine")
            .field("lan_url", &(!self.lan_url.is_empty()))
            .field("fallback_url", &(!self.fallback_url.is_empty()))
            .field("token", &(!self.token.is_empty()))
            .finish()
    }
}

impl HubLine {
    /// A token and at least one address: the PC is paired with a hub.
    pub fn usable(&self) -> bool {
        !self.token.is_empty() && !(self.lan_url.is_empty() && self.fallback_url.is_empty())
    }

    /// The launcher's own addresses, LAN first: each saved `/relay` address with its path
    /// swapped for `/launcher`.
    pub fn launcher_urls(&self) -> Vec<String> {
        [&self.lan_url, &self.fallback_url]
            .into_iter()
            .filter(|u| !u.is_empty())
            .map(|u| launcher_url(u))
            .collect()
    }
}

/// `ws://host:port/relay` (or with no path) → `ws://host:port/launcher`.
pub fn launcher_url(relay: &str) -> String {
    let base = relay.trim().trim_end_matches('/');
    let base = base.strip_suffix("/relay").unwrap_or(base);
    format!("{base}/launcher")
}

/// The saved connection, or nothing set when there is none or it cannot be opened. A store
/// failure is never turned into plaintext, and never carries a value.
pub fn load(creds: &CredentialStore) -> HubLine {
    match creds.read(CRED_NAME, CRED_IDENTITY) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(CredentialError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            HubLine::default()
        }
        Err(_) => HubLine::default(),
    }
}

/// The connection to use: the debug-only test override, else the store.
pub fn effective(creds: &CredentialStore) -> HubLine {
    if cfg!(debug_assertions) {
        let url = std::env::var(TEST_URL_ENV)
            .ok()
            .filter(|u| !u.trim().is_empty());
        let token = std::env::var(TEST_TOKEN_ENV).ok().filter(|t| !t.is_empty());
        if let (Some(url), Some(token)) = (url, token) {
            return HubLine {
                lan_url: url,
                token,
                ..Default::default()
            };
        }
    }
    load(creds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_platform::keychain::MemoryKeychain;
    use std::sync::Arc;

    const TOKEN: &str = "launcher-test-token-0123456789abcdef";

    #[test]
    fn reads_what_the_relay_line_saves_and_never_shows_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let creds = CredentialStore::new(
            &dir.path().join("credentials"),
            "profilehash",
            Arc::new(MemoryKeychain::default()),
        );
        // Nothing saved: not paired.
        assert!(!load(&creds).usable());
        // The names agree with the relay crate's, so the two can never drift apart.
        assert_eq!(CRED_NAME, helm_relay::config::CRED_NAME);
        assert_eq!(CRED_IDENTITY, helm_relay::config::CRED_IDENTITY);
        assert_eq!(TEST_URL_ENV, helm_relay::config::TEST_URL_ENV);
        assert_eq!(TEST_TOKEN_ENV, helm_relay::config::TEST_TOKEN_ENV);
        helm_relay::config::save(
            &creds,
            helm_relay::config::RelaySaveArgs {
                lan_url: Some("http://192.168.0.10:8000".into()),
                fallback_url: Some("ws://100.100.1.2:8000/relay".into()),
                token: Some(TOKEN.into()),
            },
        )
        .unwrap();
        let line = load(&creds);
        assert!(line.usable());
        assert_eq!(line.token, TOKEN);
        assert_eq!(
            line.launcher_urls(),
            vec![
                "ws://192.168.0.10:8000/launcher".to_owned(),
                "ws://100.100.1.2:8000/launcher".to_owned()
            ]
        );
        let shown = format!("{line:?}");
        assert!(
            !shown.contains(TOKEN) && !shown.contains("192.168"),
            "{shown}"
        );
        // The test address shape the e2e checks use maps the same way.
        assert_eq!(
            launcher_url("ws://127.0.0.1:9000/relay"),
            "ws://127.0.0.1:9000/launcher"
        );
        assert_eq!(
            launcher_url("ws://127.0.0.1:9000/"),
            "ws://127.0.0.1:9000/launcher"
        );
    }
}
