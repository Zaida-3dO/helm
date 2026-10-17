//! Per-client bearer tokens.
//!
//! Configured as `HUB_TOKENS=client:token[:scope],client:token[:scope]`, where
//! scope is `rw` (the default) or `ro`. Only SHA-256 digests of the tokens are
//! kept in memory, and a presented token is compared against every digest in
//! constant time, so neither a timing difference nor a memory dump gives a
//! token away.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub name: String,
    pub scope: Scope,
}

#[derive(Debug, Clone)]
pub struct Tokens {
    entries: Vec<([u8; 32], Client)>,
}

/// Shortest token accepted, so a typo cannot configure a guessable one.
pub const MIN_TOKEN_LEN: usize = 24;

impl Tokens {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut entries: Vec<([u8; 32], Client)> = Vec::new();
        for (i, part) in spec
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .enumerate()
        {
            let mut fields = part.split(':');
            let (Some(name), Some(token)) = (fields.next(), fields.next()) else {
                return Err(format!("token entry {} is not client:token[:scope]", i + 1));
            };
            let scope = match fields.next() {
                None | Some("rw") => Scope::ReadWrite,
                Some("ro") => Scope::ReadOnly,
                Some(_) => {
                    return Err(format!(
                        "token entry {} has a scope other than rw or ro",
                        i + 1
                    ))
                }
            };
            if fields.next().is_some() {
                return Err(format!("token entry {} has too many fields", i + 1));
            }
            let name = name.trim();
            if name.is_empty() {
                return Err(format!("token entry {} has no client name", i + 1));
            }
            if token.len() < MIN_TOKEN_LEN {
                return Err(format!(
                    "the token for client {name:?} is shorter than {MIN_TOKEN_LEN} characters"
                ));
            }
            if entries.iter().any(|(_, c)| c.name == name) {
                return Err(format!("client {name:?} is listed twice"));
            }
            let digest = digest(token);
            if entries.iter().any(|(d, _)| *d == digest) {
                return Err(format!("client {name:?} reuses another client's token"));
            }
            entries.push((
                digest,
                Client {
                    name: name.to_owned(),
                    scope,
                },
            ));
        }
        if entries.is_empty() {
            return Err("no client tokens configured".into());
        }
        Ok(Tokens { entries })
    }

    /// The client a presented `Authorization` header value belongs to.
    pub fn check(&self, header: Option<&str>) -> Option<&Client> {
        let token = header?
            .strip_prefix("Bearer ")
            .or_else(|| header?.strip_prefix("bearer "))?;
        let presented = digest(token.trim());
        let mut found = None;
        // Visit every entry so the time taken does not depend on which matched.
        for (d, client) in &self.entries {
            if ct_eq(d, &presented) {
                found = Some(client);
            }
        }
        found
    }

    pub fn clients(&self) -> impl Iterator<Item = &Client> {
        self.entries.iter().map(|(_, c)| c)
    }
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn parses_clients_and_scopes() {
        let t = Tokens::parse(&format!("helm:{A}, remote:{B}:ro")).unwrap();
        let c = t.check(Some(&format!("Bearer {A}"))).unwrap();
        assert_eq!((c.name.as_str(), c.scope), ("helm", Scope::ReadWrite));
        let c = t.check(Some(&format!("Bearer {B}"))).unwrap();
        assert_eq!((c.name.as_str(), c.scope), ("remote", Scope::ReadOnly));
    }

    #[test]
    fn rejects_wrong_missing_and_malformed_credentials() {
        let t = Tokens::parse(&format!("helm:{A}")).unwrap();
        assert!(t.check(None).is_none());
        assert!(t.check(Some(A)).is_none(), "no Bearer prefix");
        assert!(t.check(Some(&format!("Basic {A}"))).is_none());
        assert!(t.check(Some(&format!("Bearer {B}"))).is_none());
        assert!(t.check(Some(&format!("Bearer {}", &A[1..]))).is_none());
        assert!(t.check(Some("Bearer ")).is_none());
    }

    #[test]
    fn refuses_bad_configuration() {
        assert!(Tokens::parse("").is_err());
        assert!(Tokens::parse("helm:short").is_err());
        assert!(Tokens::parse(&format!("helm:{A}:admin")).is_err());
        assert!(Tokens::parse(&format!("helm:{A},helm:{B}")).is_err());
        assert!(Tokens::parse(&format!("helm:{A},remote:{A}")).is_err());
        assert!(Tokens::parse(&format!(":{A}")).is_err());
        assert!(Tokens::parse("helm").is_err());
    }
}
