//! Item ids: a full UUID, or a prefix of 8 or more hex characters. Stored as lowercase hex
//! with the hyphens removed, so `A1B2C3D4-E5F6` and `a1b2c3d4e5f6` are one id, and a prefix and the
//! full id of the same item are recognised as the same item.

pub const MIN_LEN: usize = 8;
pub const MAX_LEN: usize = 32;

/// The stored form of `raw`, or `None` when it is not a UUID or an 8+ hex-character prefix of one.
pub fn normalise(raw: &str) -> Option<String> {
    let s: String = raw
        .trim()
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if s.len() < MIN_LEN || s.len() > MAX_LEN || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(s)
}

/// One of them is a prefix of the other: the same item, named at different lengths.
pub fn same_item(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The short label shown for an item without a title: its first 8 hex characters.
pub fn prefix(id: &str) -> &str {
    &id[..id.len().min(MIN_LEN)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_uuids_and_prefixes_normalise_alike() {
        assert_eq!(
            normalise("A1B2C3D4-E5F6-4A7B-8C9D-0E1F2A3B4C5D").as_deref(),
            Some("a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d")
        );
        assert_eq!(normalise(" a1b2c3d4 ").as_deref(), Some("a1b2c3d4"));
        assert_eq!(normalise("a1b2c3d4-e5f6").as_deref(), Some("a1b2c3d4e5f6"));
    }

    #[test]
    fn short_long_and_non_hex_ids_are_refused() {
        assert_eq!(normalise("a1b2c3d"), None, "7 characters is too short");
        assert_eq!(normalise("a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d0"), None);
        assert_eq!(normalise("a1b2c3dg"), None);
        assert_eq!(normalise("../../etc"), None);
        assert_eq!(normalise(""), None);
    }

    #[test]
    fn a_prefix_and_the_full_id_are_the_same_item() {
        assert!(same_item("a1b2c3d4", "a1b2c3d4e5f64a7b"));
        assert!(same_item("a1b2c3d4e5f64a7b", "a1b2c3d4"));
        assert!(!same_item("a1b2c3d4", "a1b2c3d5"));
        assert_eq!(prefix("a1b2c3d4e5f64a7b"), "a1b2c3d4");
    }
}
