//! Where Helm keeps the hub's addresses and its relay token (trust-boundary item c).
//!
//! All three live in ONE sealed envelope in the profile's credential store: AES-256-GCM
//! under the per-profile key held by Windows Credential Manager. Nothing about the line is
//! written to settings JSON or to the database, and nothing is in the repo: there is no default
//! address. The UI may set or clear each field but can never read one back; the get op answers
//! only whether each is set.

use helm_core::credentials::CredentialStore;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub const CRED_NAME: &str = "hub-relay";
pub const CRED_IDENTITY: &str = "hub-relay-connection";
/// Debug builds only: point the line at a test hub without touching the store.
pub const TEST_URL_ENV: &str = "HELM_TEST_RELAY_URL";
pub const TEST_TOKEN_ENV: &str = "HELM_TEST_RELAY_TOKEN";

/// The sealed connection. Never serialised anywhere but into the envelope.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelayConfig {
    #[serde(default)]
    pub lan_url: String,
    #[serde(default)]
    pub fallback_url: String,
    #[serde(default)]
    pub token: String,
}

impl std::fmt::Debug for RelayConfig {
    // Never print a secret, even in a test failure.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayConfig")
            .field("lan_url", &(!self.lan_url.is_empty()))
            .field("fallback_url", &(!self.fallback_url.is_empty()))
            .field("token", &(!self.token.is_empty()))
            .finish()
    }
}

impl RelayConfig {
    /// The addresses to try, in order: the LAN first, then the fallback.
    pub fn urls(&self) -> Vec<String> {
        [&self.lan_url, &self.fallback_url]
            .into_iter()
            .filter(|u| !u.is_empty())
            .cloned()
            .collect()
    }

    pub fn usable(&self) -> bool {
        !self.token.is_empty() && !self.urls().is_empty()
    }
}

/// What the UI may see: whether each part is set. Never a value.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelayConnectionView {
    pub has_address: bool,
    pub has_fallback: bool,
    pub has_token: bool,
    /// `off` (not configured), `connecting`, `online` or `offline`.
    pub state: String,
}

/// The save op's arguments: each field absent keeps the stored value; an empty string clears it.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelaySaveArgs {
    #[serde(default)]
    pub lan_url: Option<String>,
    #[serde(default)]
    pub fallback_url: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

/// Shortest relay token accepted; the hub refuses anything shorter too.
pub const MIN_TOKEN: usize = 24;

/// A hub address as the line uses it: `ws://<host>:<port>/relay`. Accepts `ws://`, `wss://`,
/// `http://` and `https://` (mapped to ws/wss), with or without the `/relay` path. Plain `ws` is
/// allowed only for a loopback, RFC 1918, Tailscale (100.64.0.0/10) or IPv6 unique-local
/// address literal; anything else needs `wss`.
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let (secure, rest) = if let Some(r) = raw.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = raw.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = raw.strip_prefix("ws://") {
        (false, r)
    } else if let Some(r) = raw.strip_prefix("http://") {
        (false, r)
    } else {
        return Err("The hub address must start with ws://, wss://, http:// or https://.".into());
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let path = path.trim_end_matches('/');
    if !(path.is_empty() || path == "/relay") {
        return Err("The hub address takes no path other than /relay.".into());
    }
    if authority.is_empty() || authority.contains('@') || authority.contains(char::is_whitespace) {
        return Err("The hub address needs a host (and no user name).".into());
    }
    let host = host_of(authority);
    if !secure && !private_literal(host) {
        return Err(
            "Plain ws:// is allowed only for a LAN, Tailscale or loopback address; use wss:// \
             for anything else."
                .into(),
        );
    }
    Ok(format!(
        "{}://{authority}/relay",
        if secure { "wss" } else { "ws" }
    ))
}

fn host_of(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    authority.rsplit_once(':').map_or(authority, |(h, _)| h)
}

fn private_literal(host: &str) -> bool {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            let o = v4.octets();
            v4.is_loopback() || v4.is_private() || (o[0] == 100 && (o[1] & 0xC0) == 64)
        }
        Ok(IpAddr::V6(v6)) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// The stored connection, or the default (nothing set) when there is none or it cannot be
/// opened. A store failure is never turned into plaintext.
pub fn load(creds: &CredentialStore) -> RelayConfig {
    creds
        .read(CRED_NAME, CRED_IDENTITY)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The connection the line should use: the debug-only test override, else the store.
pub fn effective(creds: &CredentialStore) -> RelayConfig {
    if cfg!(debug_assertions) {
        let url = std::env::var(TEST_URL_ENV)
            .ok()
            .filter(|u| !u.trim().is_empty());
        let token = std::env::var(TEST_TOKEN_ENV).ok().filter(|t| !t.is_empty());
        if let (Some(url), Some(token)) = (url, token) {
            return RelayConfig {
                lan_url: url,
                fallback_url: String::new(),
                token,
            };
        }
    }
    load(creds)
}

/// Apply a save. Errors are fixed sentences: they never carry the input.
pub fn save(creds: &CredentialStore, args: RelaySaveArgs) -> Result<RelayConfig, String> {
    let mut c = load(creds);
    if let Some(u) = args.lan_url {
        c.lan_url = if u.trim().is_empty() {
            String::new()
        } else {
            normalize_url(&u)?
        };
    }
    if let Some(u) = args.fallback_url {
        c.fallback_url = if u.trim().is_empty() {
            String::new()
        } else {
            normalize_url(&u)?
        };
    }
    if let Some(t) = args.token {
        let t = t.trim();
        if !t.is_empty() && (t.len() < MIN_TOKEN || t.contains(char::is_whitespace)) {
            return Err(format!(
                "The relay token must be at least {MIN_TOKEN} characters with no spaces."
            ));
        }
        c.token = t.to_owned();
    }
    if c == RelayConfig::default() {
        creds
            .forget(CRED_NAME)
            .map_err(|_| "The hub connection could not be removed.".to_owned())?;
    } else {
        let bytes = serde_json::to_vec(&c).map_err(|_| "unreachable".to_owned())?;
        creds.write(CRED_NAME, CRED_IDENTITY, &bytes).map_err(|_| {
            "The hub connection could not be saved in the credential store.".to_owned()
        })?;
    }
    Ok(c)
}

pub fn view(c: &RelayConfig, state: &str) -> RelayConnectionView {
    RelayConnectionView {
        has_address: !c.lan_url.is_empty(),
        has_fallback: !c.fallback_url.is_empty(),
        has_token: !c.token.is_empty(),
        state: if c.usable() {
            state.to_owned()
        } else {
            "off".into()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_platform::keychain::MemoryKeychain;
    use std::sync::Arc;

    const TOKEN: &str = "relay-secret-token-0123456789abcdef";
    const LAN: &str = "ws://192.168.0.10:8000";

    fn store(dir: &std::path::Path) -> CredentialStore {
        CredentialStore::new(
            &dir.join("credentials"),
            "profilehash",
            Arc::new(MemoryKeychain::default()),
        )
    }

    fn every_file_bytes(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let mut out = vec![];
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push((p.display().to_string(), std::fs::read(&p).unwrap()));
                }
            }
        }
        out
    }

    fn contains(hay: &[u8], needle: &str) -> bool {
        hay.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn the_token_and_addresses_are_sealed_and_never_read_back() {
        // Trust-boundary item c.
        let dir = tempfile::tempdir().unwrap();
        let creds = store(dir.path());
        let saved = save(
            &creds,
            RelaySaveArgs {
                lan_url: Some(LAN.into()),
                fallback_url: Some("ws://100.100.1.2:8000".into()),
                token: Some(TOKEN.into()),
            },
        )
        .unwrap();
        assert!(saved.usable());
        // On disk: one envelope, and no file anywhere in the profile holds the token or the
        // addresses in the clear.
        let files = every_file_bytes(dir.path());
        assert!(files.iter().any(|(p, _)| p.ends_with("hub-relay.cred")));
        for (path, bytes) in &files {
            for secret in [TOKEN, "192.168.0.10", "100.100.1.2"] {
                assert!(
                    !contains(bytes, secret),
                    "{path} holds {secret} in the clear"
                );
            }
        }
        // It opens again from the store.
        assert_eq!(load(&creds), saved);
        // What the UI may see: booleans only.
        let v = serde_json::to_string(&view(&saved, "online")).unwrap();
        for secret in [TOKEN, "192.168", "100.100"] {
            assert!(!v.contains(secret), "{v}");
        }
        assert!(v.contains("\"hasToken\":true"));
        // Debug output does not leak either.
        let d = format!("{saved:?}");
        assert!(!d.contains(TOKEN) && !d.contains("192.168"), "{d}");
    }

    #[test]
    fn absent_keeps_empty_clears_and_all_clear_removes_the_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let creds = store(dir.path());
        save(
            &creds,
            RelaySaveArgs {
                lan_url: Some(LAN.into()),
                token: Some(TOKEN.into()),
                ..Default::default()
            },
        )
        .unwrap();
        let c = save(
            &creds,
            RelaySaveArgs {
                fallback_url: Some("".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(c.token, TOKEN, "an absent token is kept");
        let c = save(
            &creds,
            RelaySaveArgs {
                token: Some("".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(c.token.is_empty() && !c.usable());
        save(
            &creds,
            RelaySaveArgs {
                lan_url: Some("".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!dir
            .path()
            .join("credentials")
            .join("hub-relay.cred")
            .exists());
    }

    #[test]
    fn a_failing_keychain_saves_nothing_and_says_so_without_the_input() {
        let dir = tempfile::tempdir().unwrap();
        let kc = Arc::new(MemoryKeychain::default());
        kc.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let creds = CredentialStore::new(&dir.path().join("credentials"), "h", kc);
        let e = save(
            &creds,
            RelaySaveArgs {
                token: Some(TOKEN.into()),
                lan_url: Some(LAN.into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(!e.contains(TOKEN));
        for (_, bytes) in every_file_bytes(dir.path()) {
            assert!(!contains(&bytes, TOKEN));
        }
    }

    #[test]
    fn addresses_are_normalised_and_plain_ws_only_for_private_hosts() {
        assert_eq!(
            normalize_url("http://192.168.1.5:8000").unwrap(),
            "ws://192.168.1.5:8000/relay"
        );
        assert_eq!(
            normalize_url("ws://100.64.0.9:1/relay/").unwrap(),
            "ws://100.64.0.9:1/relay"
        );
        assert_eq!(
            normalize_url("ws://127.0.0.1:9").unwrap(),
            "ws://127.0.0.1:9/relay"
        );
        assert_eq!(
            normalize_url("ws://[fd00::1]:9").unwrap(),
            "ws://[fd00::1]:9/relay"
        );
        assert_eq!(
            normalize_url("https://hub.example:443").unwrap(),
            "wss://hub.example:443/relay"
        );
        assert!(
            normalize_url("ws://hub.example:9").is_err(),
            "a DNS name needs wss"
        );
        assert!(
            normalize_url("ws://8.8.8.8:9").is_err(),
            "a public address needs wss"
        );
        assert!(
            normalize_url("ws://100.128.0.1:9").is_err(),
            "outside 100.64/10"
        );
        assert!(normalize_url("ftp://x").is_err());
        assert!(normalize_url("ws://u:p@10.0.0.1:9").is_err());
        assert!(normalize_url("ws://10.0.0.1:9/mcp").is_err());
    }

    #[test]
    fn a_short_token_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let creds = store(dir.path());
        assert!(save(
            &creds,
            RelaySaveArgs {
                token: Some("short".into()),
                ..Default::default()
            }
        )
        .is_err());
    }
}
