//! The dictation record, in this crate's own `dictation_settings` table, and the address rule.
//!
//! **Where audio may go.** A recording is sent to one address, the user's speech-to-text server,
//! and only over plain http: to this computer, or, when the user has ticked "Allow an address on
//! my network", to a numeric address on their own network (a home LAN, Tailscale or a unique-local
//! IPv6 address). A name other than `localhost` is refused because it cannot be checked, and a
//! public address is refused always. [`check_stt_url`] is the one check: it runs on save and again
//! before every request.

use rusqlite::{OptionalExtension, Transaction};
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub const DEFAULT_MODEL: &str = "large-v3-turbo";
pub const DEFAULT_LANGUAGE: &str = "en";
/// The language setting that leaves the choice to the server.
pub const AUTO_LANGUAGE: &str = "auto";

const MAX_MODEL: usize = 100;

/// The saved dictation settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The server's address as the user typed it, once it has passed [`check_stt_url`].
    pub stt_url: Option<String>,
    /// The address may be on the user's own network, not only on this computer.
    pub allow_private_network: bool,
    pub model: String,
    /// A language code like `en`, or [`AUTO_LANGUAGE`].
    pub language: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stt_url: None,
            allow_private_network: false,
            model: DEFAULT_MODEL.to_owned(),
            language: DEFAULT_LANGUAGE.to_owned(),
        }
    }
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS dictation_settings (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             value  TEXT NOT NULL
         );",
    )
}

/// Read leniently: a missing or odd field is its default.
pub fn load(tx: &Transaction<'_>) -> Settings {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM dictation_settings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let v: Value = raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let d = Settings::default();
    Settings {
        stt_url: v["sttUrl"]
            .as_str()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_owned),
        allow_private_network: v["allowPrivateNetwork"].as_bool().unwrap_or(false),
        model: v["model"]
            .as_str()
            .filter(|m| model_ok(m))
            .map_or(d.model, str::to_owned),
        language: v["language"]
            .as_str()
            .filter(|l| language_ok(l))
            .map_or(d.language, str::to_owned),
    }
}

pub fn save(tx: &Transaction<'_>, s: &Settings) -> rusqlite::Result<()> {
    let v = json!({
        "sttUrl": s.stt_url,
        "allowPrivateNetwork": s.allow_private_network,
        "model": s.model,
        "language": s.language,
    });
    tx.execute(
        "INSERT INTO dictation_settings (id, value) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET value = excluded.value",
        [v.to_string()],
    )?;
    Ok(())
}

/// A model name as a server takes it: letters, digits and `. _ - : /`, one line.
pub fn model_ok(m: &str) -> bool {
    !m.is_empty()
        && m.len() <= MAX_MODEL
        && m.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/".contains(&b))
}

/// `auto`, or a code like `en`, `fr` or `pt-BR`.
pub fn language_ok(l: &str) -> bool {
    if l == AUTO_LANGUAGE {
        return true;
    }
    let (lang, region) = match l.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (l, None),
    };
    (2..=3).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_lowercase())
        && region.is_none_or(|r| {
            (2..=4).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_alphabetic())
        })
}

// ---- the address rule -----------------------------------------------------------------------

/// How far from this computer an address is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    /// This computer.
    Here,
    /// The user's own network.
    Private,
    /// Anywhere else.
    Public,
}

fn reach_v4(ip: Ipv4Addr) -> Reach {
    let o = ip.octets();
    if ip.is_loopback() {
        Reach::Here
    } else if ip.is_private() || ip.is_link_local() || (o[0] == 100 && (o[1] & 0xC0) == 64) {
        Reach::Private
    } else {
        Reach::Public
    }
}

fn reach_v6(ip: Ipv6Addr) -> Reach {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return reach_v4(v4);
    }
    let first = ip.segments()[0];
    if ip.is_loopback() {
        Reach::Here
    } else if (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 {
        Reach::Private
    } else {
        Reach::Public
    }
}

/// The server's address in its plain form (`http://127.0.0.1:8000`, no path), or why it is
/// refused. A trailing `/v1` is dropped: the call adds it.
pub fn check_stt_url(raw: &str, allow_private: bool) -> Result<String, String> {
    let raw = raw.trim();
    let Some(rest) = raw
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("http://"))
        .map(|_| &raw[7..])
    else {
        return Err(
            "That address does not start with http://. Speech-to-text is reached over \
                    plain http, on this computer or on your own network."
                .to_owned(),
        );
    };
    if rest.contains(['@', '?', '#', '\\', ' ']) {
        return Err("That address has parts a speech-to-text server's does not.".to_owned());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !matches!(path, "" | "/" | "/v1" | "/v1/") {
        return Err(
            "That address has a path. Use just the server's address, like http://127.0.0.1:8000."
                .to_owned(),
        );
    }
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let Some((h, after)) = inner.split_once(']') else {
            return Err("That address is not valid.".to_owned());
        };
        (h.to_owned(), after.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p)),
            None => (authority.to_owned(), None),
        }
    };
    let host = host.to_ascii_lowercase();
    let reach = if host == "localhost" {
        Reach::Here
    } else {
        match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => reach_v4(ip),
            Ok(IpAddr::V6(ip)) => reach_v6(ip),
            // A name cannot be checked: it could point anywhere.
            Err(_) => Reach::Public,
        }
    };
    match reach {
        Reach::Here => {}
        Reach::Private if allow_private => {}
        Reach::Private => {
            return Err(
                "That address is on your network, not on this computer. Tick \"Allow an \
                        address on my network\" to use it."
                    .to_owned(),
            )
        }
        Reach::Public => {
            return Err(
                "That address is not on this computer or your own network. Use \
                        127.0.0.1, or the numeric address (like 192.168.1.20) of a computer at \
                        home."
                    .to_owned(),
            )
        }
    }
    let port = match port {
        None => 80,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => n,
            _ => return Err("That port is not valid.".to_owned()),
        },
    };
    Ok(if host.contains(':') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    })
}
