//! Helm host core: the durable profile and everything stored in it.
//!
//! | Module | What |
//! |---|---|
//! | [`profile`] | the profile directory and `app.lock` |
//! | [`privfs`] | owner-only atomic writes, no-follow reads |
//! | [`db`] | WAL, single writer, snapshot readers, migrations |
//! | [`revisions`] | CAS and keyed revisions |
//! | [`drafts`] | `draft_revision` compare-and-set |
//! | [`prefs`] | the preference registry and its classes |
//! | [`credentials`] | AES-256-GCM envelopes; key in Credential Manager |
//! | [`recovery`] | startup recovery of interrupted work |
//! | [`lifecycle`] | quit |
//! | [`ops`] | the `api` ops this crate serves |

pub mod credentials;
pub mod db;
pub mod drafts;
pub mod lifecycle;
pub mod ops;
pub mod prefs;
pub mod privfs;
pub mod profile;
pub mod recovery;
pub mod revisions;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}
