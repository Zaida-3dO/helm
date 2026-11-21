//! The long-lived Claude token that chats sign in with.
//!
//! The person makes it once with `claude setup-token` and gives it to Helm, which keeps it sealed
//! in the profile's credential store and hands it to the processes that run Claude for a chat
//! (workers and the title, suggestion, triage and gatekeeper helpers). Those processes only use
//! the token, so none of them refreshes a sign-in. The usage meter does not use it: reading usage
//! needs the normal sign-in.
//!
//! What is stored is one sealed record holding the token and the day it was saved. Everything that
//! leaves this module for display ([`Status`]) holds the date only; the token itself is reached
//! through [`read`], whose result prints as `Token(..)`. Nothing here logs, and no error carries
//! the input.

use crate::credentials::CredentialStore;
use crate::profile::Profile;
use helm_platform::Platform;
use serde::{Deserialize, Serialize};

const CRED_NAME: &str = "claude-chat-token";
const CRED_IDENTITY: &str = "claude-chat-token";

/// A Claude token is a short run of letters, digits, `-` and `_` after this prefix.
const PREFIX: &str = "sk-ant-";
/// Keys for the pay-as-you-go API start like this; they are not what `setup-token` makes.
const API_KEY_PREFIX: &str = "sk-ant-api";
const MIN_LEN: usize = 20;
const MAX_LEN: usize = 400;

const BAD_TOKEN: &str = "That does not look like a Claude token. Run `claude setup-token`, then \
                         paste the whole token it prints (one run of letters and digits, no spaces).";

/// The profile's credential store, where the token is sealed (beside the other saved secrets).
pub fn credentials(profile: &Profile, platform: &Platform) -> CredentialStore {
    CredentialStore::new(
        &profile.path("credentials"),
        &profile.hash,
        platform.keychain.clone(),
    )
}

/// A saved token. It prints without its text, so a stray `{:?}` never writes it to a log.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// The token's text, for the one place that hands it to a child process.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    token: String,
    #[serde(rename = "savedAt")]
    saved_at: u64,
}

/// What Settings shows: whether a token is saved and when. Never the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub saved: bool,
    /// Unix ms of the day it was saved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<u64>,
}

/// Whether `raw` can be a Claude token made by `claude setup-token`.
pub fn valid_token(raw: &str) -> bool {
    (MIN_LEN..=MAX_LEN).contains(&raw.len())
        && raw.starts_with(PREFIX)
        && !raw.starts_with(API_KEY_PREFIX)
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The first Claude token in `text` (a terminal's output, say): the run that starts at the prefix
/// and stops at the first character a token cannot hold. `None` when there is none.
pub fn find_token(text: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(at) = text[from..].find(PREFIX) {
        let start = from + at;
        let end = text[start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .map_or(text.len(), |n| start + n);
        if valid_token(&text[start..end]) {
            return Some(&text[start..end]);
        }
        from = start + PREFIX.len();
    }
    None
}

/// Seal a typed or captured token. The error is a fixed sentence: it never carries the input.
pub fn save(creds: &CredentialStore, raw: &str, now_ms: u64) -> Result<Status, String> {
    let token = raw.trim();
    if !valid_token(token) {
        return Err(BAD_TOKEN.to_owned());
    }
    let record = Record {
        token: token.to_owned(),
        saved_at: now_ms,
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|_| "The token could not be saved in the credential store.".to_owned())?;
    creds
        .write(CRED_NAME, CRED_IDENTITY, &bytes)
        .map_err(|_| "The token could not be saved in the credential store.".to_owned())?;
    Ok(Status {
        saved: true,
        saved_at: Some(now_ms),
    })
}

/// Remove the saved token from this PC. It does not revoke the token anywhere.
pub fn forget(creds: &CredentialStore) -> Result<Status, String> {
    creds
        .forget(CRED_NAME)
        .map_err(|_| "The token could not be removed from the credential store.".to_owned())?;
    Ok(Status {
        saved: false,
        saved_at: None,
    })
}

fn load(creds: &CredentialStore) -> Option<Record> {
    let bytes = creds.read(CRED_NAME, CRED_IDENTITY).ok()?;
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    valid_token(&record.token).then_some(record)
}

/// The saved token. A store failure reads as no token; it is never turned into plaintext.
pub fn read(creds: &CredentialStore) -> Option<Token> {
    load(creds).map(|r| Token(r.token))
}

pub fn status(creds: &CredentialStore) -> Status {
    match load(creds) {
        Some(r) => Status {
            saved: true,
            saved_at: Some(r.saved_at),
        },
        None => Status {
            saved: false,
            saved_at: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_platform::keychain::MemoryKeychain;
    use std::sync::Arc;

    const GOOD: &str = "sk-ant-oat01-AbC123_def-GHI456jklMNO789pqrSTU";

    fn store(dir: &std::path::Path, kc: Arc<MemoryKeychain>) -> CredentialStore {
        CredentialStore::new(&dir.join("credentials"), "p1", kc)
    }

    #[test]
    fn saves_reads_reports_and_forgets_without_leaking() {
        let d = tempfile::tempdir().unwrap();
        let kc = Arc::new(MemoryKeychain::default());
        let s = store(d.path(), kc.clone());
        assert_eq!(
            status(&s),
            Status {
                saved: false,
                saved_at: None
            }
        );
        assert!(read(&s).is_none());

        // Spaces around a paste are dropped; the date is the one given.
        let st = save(&s, &format!("  {GOOD}\n"), 1_700_000_000_000).unwrap();
        assert_eq!(st.saved_at, Some(1_700_000_000_000));
        let token = read(&s).unwrap();
        assert_eq!(token.expose(), GOOD);
        assert_eq!(format!("{token:?}"), "Token(..)");
        let shown = serde_json::to_string(&status(&s)).unwrap();
        assert_eq!(shown, r#"{"saved":true,"savedAt":1700000000000}"#);

        // Sealed on disk (no plaintext), owner-only, and a second store on the same keychain
        // opens it.
        let file = d.path().join("credentials").join("claude-chat-token.cred");
        let raw = std::fs::read(&file).unwrap();
        assert!(
            !raw.windows(8).any(|w| w == b"sk-ant-o"),
            "no plaintext on disk"
        );
        assert!(helm_platform::fs::is_owner_only(&file).unwrap());
        assert_eq!(read(&store(d.path(), kc)).unwrap().expose(), GOOD);

        // Saving again replaces it and moves the date.
        save(&s, GOOD, 1_800_000_000_000).unwrap();
        assert_eq!(status(&s).saved_at, Some(1_800_000_000_000));

        assert!(!forget(&s).unwrap().saved);
        assert!(read(&s).is_none());
        assert!(forget(&s).is_ok(), "forgetting nothing is fine");
    }

    #[test]
    fn what_counts_as_a_token_and_how_one_is_found_in_output() {
        for bad in [
            "",
            "hello",
            "sk-ant-short",
            "sk-ant-api03-AbC123_def-GHI456jklMNO789pqrSTU",
            "sk-ant-oat01-AbC123 def-GHI456jklMNO789pqrSTU",
            "sk-ant-oat01-AbC123$def-GHI456jklMNO789pqrSTU",
            "xx-ant-oat01-AbC123_def-GHI456jklMNO789pqrSTU",
        ] {
            assert!(!valid_token(bad), "{bad}");
        }
        assert!(valid_token(GOOD));
        let err = save(
            &store(
                tempfile::tempdir().unwrap().path(),
                Arc::new(MemoryKeychain::default()),
            ),
            "sk-ant-nope but with the secret-looking text",
            1,
        )
        .unwrap_err();
        assert!(!err.contains("nope"), "the error never echoes the input");

        let out =
            format!("Open the page...\r\nYour token:\r\n\x1b[1m{GOOD}\x1b[0m\r\nStore it safely.");
        assert_eq!(find_token(&out), Some(GOOD));
        assert_eq!(
            find_token(&format!("{GOOD}\nsk-ant-oat01-ZZZZZZZZZZZZZZZZZZZZZZZZ")),
            Some(GOOD)
        );
        assert_eq!(find_token("sk-ant-short then sk-ant-"), None);
        assert_eq!(find_token("nothing here"), None);
    }

    #[test]
    fn a_broken_store_reads_as_no_token() {
        let d = tempfile::tempdir().unwrap();
        let kc = Arc::new(MemoryKeychain::default());
        let s = store(d.path(), kc.clone());
        save(&s, GOOD, 5).unwrap();
        kc.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let locked = store(d.path(), kc);
        assert!(read(&locked).is_none() && !status(&locked).saved);
        assert!(save(&locked, GOOD, 6).is_err());
    }
}
