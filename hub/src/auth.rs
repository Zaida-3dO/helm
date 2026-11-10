//! Clients, scopes and tokens.
//!
//! A *client* is one machine (one Helm install, or one outside caller) that belongs to exactly one
//! person. Each has a bearer token and a set of scopes:
//!
//! | scope | reaches |
//! |---|---|
//! | `memory:ro` | the memory read tools over `/mcp` |
//! | `memory:rw` | the memory tools, writes included |
//! | `relay` | `/relay`: this machine's own line to Helm |
//! | `app` | the Helm tools forwarded down the caller's own person's lines |
//! | `peer` | the household calls (`/peer/*`) |
//! | `telegram` | `POST /telegram/update` and nothing else (a forwarder for a shared bot) |
//! | `voice` | `/voice/*`, the phone's voice line to its owner's Helm, and nothing else |
//!
//! New scopes are a new [`Scope`] variant, a [`Scope::parse`] arm and a [`Scope::label`] arm;
//! routes check the scope they need with [`Client::has`], so a client without it is refused with
//! no further change.
//!
//! Clients live in the registry (`registry.rs`). `HUB_TOKENS=client:token[:scope]` still works: its
//! entries become legacy clients of one person, parsed here. The scope field is `rw` (the default),
//! `ro`, `relay`, `app`, `peer`, `telegram` or `voice`, or several joined with `+` (`rw+app`,
//! `rw+relay+app+peer`); `memory:rw` and `memory:ro` are also accepted.
//!
//! Only SHA-256 digests of tokens are kept, and a presented token is compared against every
//! active digest in constant time, so neither a timing difference nor a memory dump gives a token
//! away.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    ReadOnly,
    ReadWrite,
    /// Helm's outbound line to the hub.
    Relay,
    /// Posts Telegram updates to the bot; never anything else.
    Telegram,
    /// Sees and may call the Helm tools forwarded down the caller's own person's lines.
    App,
    /// May use the household calls.
    Peer,
    /// The phone's voice line (`/voice/*`); never anything else.
    Voice,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::ReadOnly => "memory:ro",
            Scope::ReadWrite => "memory:rw",
            Scope::Relay => "relay",
            Scope::Telegram => "telegram",
            Scope::App => "app",
            Scope::Peer => "peer",
            Scope::Voice => "voice",
        }
    }

    pub fn parse(s: &str) -> Option<Scope> {
        Some(match s.trim() {
            "ro" | "memory:ro" => Scope::ReadOnly,
            "rw" | "memory:rw" => Scope::ReadWrite,
            "relay" => Scope::Relay,
            "telegram" => Scope::Telegram,
            "app" => Scope::App,
            "peer" => Scope::Peer,
            "voice" => Scope::Voice,
            _ => return None,
        })
    }
}

/// A client's scopes: sorted, without repeats, and `memory:rw` replaces `memory:ro`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scopes(Vec<Scope>);

/// What a pairing code and `client add` grant when no scopes are named.
pub const DEFAULT_PAIRED_SCOPES: &str = "memory:rw,relay,app,peer";

impl Scopes {
    pub fn new(list: impl IntoIterator<Item = Scope>) -> Self {
        let mut v: Vec<Scope> = list.into_iter().collect();
        if v.contains(&Scope::ReadWrite) {
            v.retain(|s| *s != Scope::ReadOnly);
        }
        v.sort();
        v.dedup();
        Scopes(v)
    }

    pub fn has(&self, s: Scope) -> bool {
        self.0.contains(&s)
    }

    pub fn can_read_memory(&self) -> bool {
        self.has(Scope::ReadOnly) || self.has(Scope::ReadWrite)
    }

    pub fn can_write_memory(&self) -> bool {
        self.has(Scope::ReadWrite)
    }

    pub fn iter(&self) -> impl Iterator<Item = Scope> + '_ {
        self.0.iter().copied()
    }

    /// A list separated by commas, `+` or spaces: `memory:rw,relay,app,peer`.
    pub fn parse_list(spec: &str) -> Result<Scopes, String> {
        let mut list = Vec::new();
        for part in spec
            .split(|c: char| c == ',' || c == '+' || c.is_whitespace())
            .filter(|p| !p.is_empty())
        {
            list.push(Scope::parse(part).ok_or_else(|| {
                format!(
                    "{part:?} is not a scope; use memory:ro, memory:rw, relay, app, peer, telegram \
                     or voice"
                )
            })?);
        }
        let scopes = Scopes::new(list);
        if scopes.0.is_empty() {
            return Err("no scopes given".into());
        }
        // A forwarder's token posts updates and nothing else.
        if scopes.has(Scope::Telegram) && scopes.0.len() > 1 {
            return Err("the telegram scope cannot be combined with another".into());
        }
        // So is a phone's: a voice token reaches the voice line and nothing else.
        if scopes.has(Scope::Voice) && scopes.0.len() > 1 {
            return Err("the voice scope cannot be combined with another".into());
        }
        Ok(scopes)
    }

    /// The form stored in the registry and shown by `client list`.
    pub fn to_csv(&self) -> String {
        self.0
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Who a presented token belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    /// The registry id of this machine. One line, and one set of sharing switches, per id.
    pub id: i64,
    /// A label for logs and for the `client` field Helm sees on a forwarded call: a legacy
    /// client's configured name, otherwise the machine name.
    pub name: String,
    pub person_id: i64,
    pub person: String,
    pub machine: String,
    pub scopes: Scopes,
}

impl Client {
    pub fn has(&self, scope: Scope) -> bool {
        self.scopes.has(scope)
    }
}

/// One `HUB_TOKENS` entry.
#[derive(Debug, Clone)]
pub struct LegacyToken {
    pub name: String,
    pub digest: [u8; 32],
    pub scopes: Scopes,
}

/// The parsed `HUB_TOKENS`.
#[derive(Debug, Clone, Default)]
pub struct Tokens {
    entries: Vec<LegacyToken>,
}

/// Shortest token accepted, so a typo cannot configure a guessable one.
pub const MIN_TOKEN_LEN: usize = 24;

impl Tokens {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut entries: Vec<LegacyToken> = Vec::new();
        for (i, part) in spec
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .enumerate()
        {
            // The scope field may hold colons of its own (`memory:rw+relay`), so it is the rest.
            let mut fields = part.splitn(3, ':');
            let (Some(name), Some(token)) = (fields.next(), fields.next()) else {
                return Err(format!("token entry {} is not client:token[:scope]", i + 1));
            };
            let scopes = match fields.next() {
                None => Scopes::new([Scope::ReadWrite]),
                Some(s) => {
                    Scopes::parse_list(s).map_err(|e| format!("token entry {}: {e}", i + 1))?
                }
            };
            let name = name.trim();
            if name.is_empty() {
                return Err(format!("token entry {} has no client name", i + 1));
            }
            if token.len() < MIN_TOKEN_LEN {
                return Err(format!(
                    "the token for client {name:?} is shorter than {MIN_TOKEN_LEN} characters"
                ));
            }
            if entries.iter().any(|c| c.name == name) {
                return Err(format!("client {name:?} is listed twice"));
            }
            let digest = digest(token);
            if entries.iter().any(|c| c.digest == digest) {
                return Err(format!("client {name:?} reuses another client's token"));
            }
            entries.push(LegacyToken {
                name: name.to_owned(),
                digest,
                scopes,
            });
        }
        if entries.is_empty() {
            return Err("no client tokens configured".into());
        }
        Ok(Tokens { entries })
    }

    pub fn entries(&self) -> &[LegacyToken] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The token after `Bearer `, or `None` for any other header.
pub fn bearer(header: &str) -> Option<&str> {
    header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .map(str::trim)
}

pub fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Equal-length byte strings compared without an early exit.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccc";

    fn scopes_of(t: &Tokens, name: &str) -> String {
        t.entries()
            .iter()
            .find(|e| e.name == name)
            .unwrap()
            .scopes
            .to_csv()
    }

    #[test]
    fn legacy_scopes_parse_to_scope_sets() {
        let t = Tokens::parse(&format!(
            "helm:{A}, remote:{B}:ro+app, line:{C}:relay, fwd:{A}x:telegram, \
             full:{B}x:rw+relay+app+peer, phone:{C}x:voice"
        ))
        .unwrap();
        assert_eq!(scopes_of(&t, "helm"), "memory:rw");
        assert_eq!(scopes_of(&t, "remote"), "memory:ro,app");
        assert_eq!(scopes_of(&t, "line"), "relay");
        assert_eq!(scopes_of(&t, "fwd"), "telegram");
        assert_eq!(scopes_of(&t, "full"), "memory:rw,relay,app,peer");
        assert_eq!(scopes_of(&t, "phone"), "voice");
        assert!(
            Scopes::parse_list("voice,rw").is_err(),
            "a voice token stands alone"
        );
        assert!(Tokens::parse(&format!("phone:{A}:voice+app")).is_err());
        // rw wins over ro; a forwarder token stands alone; unknown scopes fail.
        assert_eq!(Scopes::parse_list("ro,rw").unwrap().to_csv(), "memory:rw");
        assert!(Scopes::parse_list("telegram,app").is_err());
        assert!(Scopes::parse_list("admin").is_err());
        assert!(Scopes::parse_list("").is_err());
        assert!(Tokens::parse(&format!("fwd:{A}:telegram+app")).is_err());
        assert!(Tokens::parse(&format!("helm:{A}:admin")).is_err());
    }

    #[test]
    fn scope_helpers() {
        let s = Scopes::parse_list("memory:ro,relay").unwrap();
        assert!(s.can_read_memory() && !s.can_write_memory());
        assert!(s.has(Scope::Relay) && !s.has(Scope::App));
        let s = Scopes::parse_list(DEFAULT_PAIRED_SCOPES).unwrap();
        assert!(s.can_write_memory() && s.has(Scope::Peer) && s.has(Scope::App));
        assert_eq!(Scope::parse(Scope::Peer.label()), Some(Scope::Peer));
        assert_eq!(Scope::parse(Scope::Voice.label()), Some(Scope::Voice));
        let v = Scopes::parse_list("voice").unwrap();
        assert!(v.has(Scope::Voice) && !v.can_read_memory() && !v.has(Scope::Relay));
    }

    #[test]
    fn only_digests_are_held() {
        // Trust boundary (b): the configured token never stays in memory as text.
        let t = Tokens::parse(&format!("line:{A}:relay")).unwrap();
        let dump = format!("{t:?}");
        assert!(!dump.contains(A), "{dump}");
        assert!(!dump.contains("aaaa"), "{dump}");
        assert_eq!(t.entries()[0].digest, digest(A));
    }

    #[test]
    fn bearer_and_ct_eq() {
        assert_eq!(bearer(&format!("Bearer {A}")), Some(A));
        assert_eq!(bearer(&format!("bearer {A}")), Some(A));
        assert_eq!(bearer(A), None, "no Bearer prefix");
        assert_eq!(bearer(&format!("Basic {A}")), None);
        // A digest differing only in its first or its last byte is refused. Timing cannot be
        // asserted in a unit test; ct_eq folds every byte with no early return.
        let a = digest(A);
        let mut b = a;
        b[31] ^= 1;
        assert!(!ct_eq(&a, &b));
        let mut c = a;
        c[0] ^= 1;
        assert!(!ct_eq(&a, &c));
        assert!(ct_eq(&a, &a));
        assert!(!ct_eq(&a, &a[..31]));
    }

    #[test]
    fn refuses_bad_configuration() {
        assert!(Tokens::parse("").is_err());
        assert!(Tokens::parse("helm:short").is_err());
        assert!(Tokens::parse(&format!("helm:{A},helm:{B}")).is_err());
        assert!(Tokens::parse(&format!("helm:{A},remote:{A}")).is_err());
        assert!(Tokens::parse(&format!(":{A}")).is_err());
        assert!(Tokens::parse("helm").is_err());
        assert!(Tokens::parse(&format!("helm:{A}:rw:extra")).is_err());
    }
}
