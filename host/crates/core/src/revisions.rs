//! Revisions and compare-and-set.
//!
//! - The **global revision counter** (`counters` key `revision`) bumps on mutations the UI watches.
//! - **Stored revisions** are integers kept with a setting or row. A change carries the revision it
//!   was based on; a stale one is refused. Capped at 2^53 − 1 so JavaScript reads it exactly.
//! - **Keyed revisions** are for settings with no stored counter: HMAC-SHA-256 over the canonical
//!   JSON of the user-editable projection, keyed with 32 random bytes made fresh at each launch and
//!   held only in memory, truncated to 16 hex characters.

use hmac::{Hmac, Mac};
use rusqlite::Transaction;
use serde_json::Value;
use sha2::Sha256;

pub const GLOBAL_REVISION_KEY: &str = "revision";
pub const MAX_REVISION: i64 = (1i64 << 53) - 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RevisionError {
    #[error("stale revision: expected {expected}, current is {current}")]
    Stale { expected: String, current: String },
    #[error("revision counter exhausted")]
    Exhausted,
}

/// Bump and return the global revision counter, inside the caller's write transaction.
pub fn bump_global(tx: &Transaction<'_>) -> rusqlite::Result<i64> {
    tx.query_row(
        "INSERT INTO counters(key, value) VALUES (?1, 1) \
         ON CONFLICT(key) DO UPDATE SET value = value + 1 RETURNING value",
        [GLOBAL_REVISION_KEY],
        |r| r.get(0),
    )
}

/// Compare-and-set for a stored revision: `Ok(next)` when `expected` is current.
pub fn check_stored(current: i64, expected: i64) -> Result<i64, RevisionError> {
    if current != expected {
        return Err(RevisionError::Stale {
            expected: expected.to_string(),
            current: current.to_string(),
        });
    }
    if current >= MAX_REVISION {
        return Err(RevisionError::Exhausted);
    }
    Ok(current + 1)
}

/// Canonical JSON: object keys sorted at every depth, no whitespace.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string serializes"));
                out.push(':');
                write_canonical(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).expect("scalar serializes")),
    }
}

/// The per-launch key for keyed revisions. Create one at startup; never persist it.
pub struct KeyedRevisions {
    key: [u8; 32],
}

impl KeyedRevisions {
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("OS random source");
        Self { key }
    }

    #[cfg(test)]
    fn with_key(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// The 16-hex revision of a setting's user-editable projection.
    pub fn of(&self, projection: &Value) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("any key length");
        mac.update(canonical_json(projection).as_bytes());
        hex::encode(mac.finalize().into_bytes())[..16].to_owned()
    }

    /// Refuse a change based on a revision that no longer matches.
    pub fn check(&self, projection: &Value, expected: &str) -> Result<(), RevisionError> {
        let current = self.of(projection);
        if current == expected {
            Ok(())
        } else {
            Err(RevisionError::Stale {
                expected: expected.to_owned(),
                current,
            })
        }
    }
}

impl Default for KeyedRevisions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{tests::temp_db, MigrationRegistry};
    use serde_json::json;

    #[test]
    fn global_counter_counts_up() {
        let (_d, db) = temp_db(&MigrationRegistry::new());
        assert_eq!(db.write(bump_global).unwrap(), 1);
        assert_eq!(db.write(bump_global).unwrap(), 2);
    }

    #[test]
    fn stored_cas_refuses_stale() {
        assert_eq!(check_stored(4, 4), Ok(5));
        assert!(matches!(
            check_stored(5, 4),
            Err(RevisionError::Stale { .. })
        ));
        assert_eq!(
            check_stored(MAX_REVISION, MAX_REVISION),
            Err(RevisionError::Exhausted)
        );
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let v = json!({"b": 1, "a": {"d": [1, {"z": 0, "y": "s"}], "c": null}});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"c":null,"d":[1,{"y":"s","z":0}]},"b":1}"#
        );
    }

    #[test]
    fn keyed_revision_is_16_hex_and_ignores_key_order() {
        let k = KeyedRevisions::with_key([1; 32]);
        let a = k.of(&json!({"x": 1, "y": [true]}));
        let b = k.of(&json!({"y": [true], "x": 1}));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, k.of(&json!({"x": 2, "y": [true]})));
    }

    #[test]
    fn keyed_revision_is_the_truncated_hmac() {
        // Independent recomputation: HMAC-SHA-256(key = 32 x 0x01, msg = canonical JSON).
        let k = KeyedRevisions::with_key([1; 32]);
        let mut mac = Hmac::<Sha256>::new_from_slice(&[1u8; 32]).unwrap();
        mac.update(br#"{"a":1}"#);
        let full = hex::encode(mac.finalize().into_bytes());
        assert_eq!(k.of(&json!({"a": 1})), full[..16]);
    }

    #[test]
    fn keyed_revisions_change_per_launch() {
        let v = json!({"a": 1});
        assert_ne!(KeyedRevisions::new().of(&v), KeyedRevisions::new().of(&v));
    }

    #[test]
    fn keyed_check_refuses_stale() {
        let k = KeyedRevisions::with_key([2; 32]);
        let v = json!({"a": 1});
        let rev = k.of(&v);
        assert!(k.check(&v, &rev).is_ok());
        assert!(k.check(&json!({"a": 2}), &rev).is_err());
    }
}
