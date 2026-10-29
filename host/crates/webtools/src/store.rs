//! What Helm keeps for web search: the Brave key (sealed in the credential store, never read back
//! by anything but the search call) and a small settings record.

use helm_core::credentials::CredentialStore;
use rusqlite::{OptionalExtension, Transaction};
use serde_json::{json, Value};
use std::path::Path;

pub const CRED_NAME: &str = "brave-search";
pub const CRED_IDENTITY: &str = "brave-search-key";

/// The most of an imported settings file Helm reads.
const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024;

/// The settings record: both fields are the user's, in Settings > Web search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// Claude chats get the search tool too (local-model chats always do, while a key is set).
    pub offer_in_claude_chats: bool,
    /// The settings file last used for "Import from OpenClaw", so it is typed once.
    pub import_path: Option<String>,
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS web_search_settings (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             value  TEXT NOT NULL
         );",
    )
}

/// Read leniently: a missing or odd field is its default.
pub fn load(tx: &Transaction<'_>) -> Settings {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM web_search_settings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let v: Value = raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Settings {
        offer_in_claude_chats: v["offerInClaudeChats"].as_bool().unwrap_or(false),
        import_path: v["importPath"]
            .as_str()
            .filter(|p| !p.is_empty())
            .map(str::to_owned),
    }
}

pub fn save(tx: &Transaction<'_>, s: &Settings) -> rusqlite::Result<()> {
    let v = json!({"offerInClaudeChats": s.offer_in_claude_chats, "importPath": s.import_path});
    tx.execute(
        "INSERT INTO web_search_settings (id, value) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET value = excluded.value",
        [v.to_string()],
    )?;
    Ok(())
}

/// Whether `raw` can be a Brave key: 8 to 200 letters, digits and `_ - . ~ + / =`. Narrow on
/// purpose: it also keeps a `${NAME}` placeholder, quotes and spaces out of a request header.
pub fn valid_key(raw: &str) -> bool {
    (8..=200).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.~+/=".contains(&b))
}

const BAD_KEY: &str = "That does not look like a Brave search key: it should be one run of \
                       letters and digits with no spaces.";

/// Seal a typed key. The error is a fixed sentence: it never carries the input.
pub fn set_key(creds: &CredentialStore, raw: &str) -> Result<(), String> {
    let key = raw.trim();
    if !valid_key(key) {
        return Err(BAD_KEY.to_owned());
    }
    creds
        .write(CRED_NAME, CRED_IDENTITY, key.as_bytes())
        .map_err(|_| "The key could not be saved in the credential store.".to_owned())
}

pub fn forget_key(creds: &CredentialStore) -> Result<(), String> {
    creds
        .forget(CRED_NAME)
        .map_err(|_| "The key could not be removed from the credential store.".to_owned())
}

/// The stored key. A store failure reads as no key; it is never turned into plaintext.
pub fn read_key(creds: &CredentialStore) -> Option<String> {
    let bytes = creds.read(CRED_NAME, CRED_IDENTITY).ok()?;
    let key = String::from_utf8(bytes).ok()?;
    valid_key(&key).then_some(key)
}

// ---------------------------------------------------------------------------- the import

/// The settings file of the OpenClaw assistant, where its default install keeps it. The user can
/// type any other path in Settings.
pub fn default_import_path() -> String {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    match home {
        Some(h) => Path::new(&h)
            .join(".openclaw")
            .join("openclaw.json")
            .to_string_lossy()
            .into_owned(),
        None => "openclaw.json".to_owned(),
    }
}

/// `text` with `//` and `/* */` comments and trailing commas removed (the file may be written by
/// hand with both). Strings are left alone.
fn tolerant(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut in_str) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 1;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        } else if c == ',' {
            // A comma before a closing bracket (blanks and comments between) is dropped.
            let next = next_significant(chars[i + 1..].iter().copied());
            if next != Some('}') && next != Some(']') {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// The first character that is not blank and not inside a comment.
fn next_significant(chars: impl Iterator<Item = char>) -> Option<char> {
    let mut it = chars.peekable();
    while let Some(c) = it.next() {
        match c {
            c if c.is_whitespace() => {}
            '/' if it.peek() == Some(&'/') => {
                for n in it.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
            }
            '/' if it.peek() == Some(&'*') => {
                it.next();
                let mut prev = ' ';
                for n in it.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            c => return Some(c),
        }
    }
    None
}

/// Read the Brave key from an OpenClaw settings file: `plugins.entries.brave.config.webSearch
/// .apiKey`. Every error is a fixed sentence; none carries the file's text or the key.
pub fn read_openclaw_key(path: &str) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() || path.len() > 1_024 {
        return Err("Give the full path of the OpenClaw settings file.".to_owned());
    }
    let unreadable = || "That file could not be read.".to_owned();
    let meta = std::fs::metadata(path).map_err(|_| unreadable())?;
    if !meta.is_file() || meta.len() > MAX_IMPORT_BYTES {
        return Err(unreadable());
    }
    let text = String::from_utf8(std::fs::read(path).map_err(|_| unreadable())?)
        .map_err(|_| unreadable())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let doc: Value = serde_json::from_str(text)
        .or_else(|_| serde_json::from_str(&tolerant(text)))
        .map_err(|_| {
            "That file is not valid JSON, so Helm cannot read the key from it. \
                      Paste the key instead."
                .to_owned()
        })?;
    let found = &doc["plugins"]["entries"]["brave"]["config"]["webSearch"]["apiKey"];
    match found {
        Value::Null => Err("That file has no Brave search key at \
                             plugins.entries.brave.config.webSearch.apiKey."
            .to_owned()),
        Value::String(k) if valid_key(k.trim()) => Ok(k.trim().to_owned()),
        _ => Err(
            "The key in that file is not stored as plain text (it may point at an \
                  environment variable), so Helm cannot import it. Paste the key instead."
                .to_owned(),
        ),
    }
}
