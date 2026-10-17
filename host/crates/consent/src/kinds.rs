//! The allow-kind registry and canonical targets.
//!
//! A kind says what one Allow grant permits. The **host** builds the canonical target and the
//! card's one-line summary from its own records; the agent's target is only the key it looks
//! records up by. The MCP crate registers `mcp_repo_server` and, in tests and debug builds
//! only, [`test_kind`].

use rusqlite::Transaction;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Longest host summary on a card (≤ 240 characters).
pub const MAX_SUMMARY: usize = 240;
/// Longest serialised canonical target.
pub const MAX_TARGET: usize = 20_000;

/// What the host built for one target.
#[derive(Clone, Debug, PartialEq)]
pub struct Canonical {
    /// The canonical target; hashed with sorted keys and no whitespace.
    pub target: Value,
    /// The card title, rendered by the host (never the agent's reason).
    pub summary: String,
    /// The fixed-form sentence the user's own words must contain, e.g. `Approve the MCP server x.`
    pub fixed_form: String,
}

/// Builds the canonical target from the agent's target and the host's records. An `Err` is the
/// refusal text (the target names nothing the host knows, or is malformed).
pub type CanonicalFn = fn(&Transaction<'_>, &str, &Value) -> Result<Canonical, String>;

#[derive(Clone, Copy)]
pub struct AllowKind {
    /// 1–32 ASCII letters, digits or `_`: the `action` an agent names.
    pub id: &'static str,
    /// The card's action label, e.g. "Approve a repository MCP server".
    pub label: &'static str,
    /// Whether the user's own typed words can stand in for a click.
    pub accepts_words: bool,
    pub canonical: CanonicalFn,
}

/// Every registered kind. Built once at startup (`registrations::allow_kinds`) and shared as a
/// service.
#[derive(Clone, Default)]
pub struct AllowKinds {
    kinds: Vec<AllowKind>,
}

impl AllowKinds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a kind. A bad or duplicate id panics (a wiring bug, caught by the first test that
    /// builds the list).
    pub fn register(&mut self, k: AllowKind) -> &mut Self {
        let ok = (1..=32).contains(&k.id.len())
            && k.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        assert!(ok, "bad allow kind id: {}", k.id);
        assert!(
            self.get(k.id).is_none(),
            "allow kind registered twice: {}",
            k.id
        );
        self.kinds.push(k);
        self
    }

    pub fn get(&self, id: &str) -> Option<&AllowKind> {
        self.kinds.iter().find(|k| k.id == id)
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.kinds.iter().map(|k| k.id).collect()
    }
}

/// Canonical JSON: object keys sorted at every depth, no whitespace.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The SHA-256 (64 lowercase hex) of the canonical JSON.
pub fn target_hash(v: &Value) -> String {
    hex::encode(Sha256::digest(canonical_json(v).as_bytes()))
}

/// The first field (sorted order) where two canonical targets differ, for a refusal that names it.
pub fn first_difference(a: &Value, b: &Value) -> String {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            keys.into_iter()
                .find(|k| x.get(*k) != y.get(*k))
                .cloned()
                .unwrap_or_else(|| "target".into())
        }
        _ => "target".into(),
    }
}

/// A kind for tests and debug probes (the e2e Allow-card flow): target `{"name": <1–64 chars>}`.
/// Never registered in a release build.
pub fn test_kind() -> AllowKind {
    fn canonical(_tx: &Transaction<'_>, _chat: &str, t: &Value) -> Result<Canonical, String> {
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| (1..=64).contains(&n.chars().count()))
            .ok_or("The target needs a name of 1-64 characters.")?;
        Ok(Canonical {
            target: serde_json::json!({ "name": name }),
            summary: format!("Run the test action {name}"),
            fixed_form: format!("Run the test action {name}."),
        })
    }
    AllowKind {
        id: "test_action",
        label: "Run a test action",
        accepts_words: true,
        canonical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let a = json!({"b": 1, "a": {"y": [1, {"d": 2, "c": 3}], "x": "s"}});
        assert_eq!(
            canonical_json(&a),
            r#"{"a":{"x":"s","y":[1,{"c":3,"d":2}]},"b":1}"#
        );
        let b = json!({"a": {"x": "s", "y": [1, {"c": 3, "d": 2}]}, "b": 1});
        assert_eq!(target_hash(&a), target_hash(&b));
        assert_eq!(target_hash(&a).len(), 64);
        assert_ne!(target_hash(&a), target_hash(&json!({"b": 2})));
    }

    #[test]
    fn first_difference_names_the_field() {
        let a = json!({"file": ".mcp.json", "name": "x", "sha256": "aa"});
        let b = json!({"file": ".mcp.json", "name": "y", "sha256": "bb"});
        assert_eq!(first_difference(&a, &b), "name");
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_kinds_panic() {
        let mut k = AllowKinds::new();
        k.register(test_kind()).register(test_kind());
    }

    #[test]
    #[should_panic(expected = "bad allow kind id")]
    fn bad_ids_panic() {
        let mut k = AllowKinds::new();
        k.register(AllowKind {
            id: "has space",
            ..test_kind()
        });
    }
}
