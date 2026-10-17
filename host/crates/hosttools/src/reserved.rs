//! The reserved-name file, `host/crates/hosttools/reserved.json`. The worker
//! reads the same file; a test on each side fails if they drift.

use serde::Deserialize;
use std::sync::OnceLock;

pub const RESERVED_JSON: &str = include_str!("../reserved.json");

#[derive(Deserialize)]
struct File {
    names: Vec<String>,
    dropped: Vec<String>,
}

fn file() -> &'static File {
    static F: OnceLock<File> = OnceLock::new();
    F.get_or_init(|| serde_json::from_str(RESERVED_JSON).expect("reserved.json parses"))
}

/// Every name a host tool may take.
pub fn names() -> &'static [String] {
    &file().names
}

/// Names that must never be registered.
pub fn dropped() -> &'static [String] {
    &file().dropped
}

pub fn is_reserved(name: &str) -> bool {
    names().iter().any(|n| n == name)
}

pub fn is_dropped(name: &str) -> bool {
    dropped().iter().any(|n| n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sorted_unique_prefixed_and_disjoint() {
        for list in [names(), dropped()] {
            let mut sorted = list.to_vec();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted, list, "keep each list sorted and unique");
            for n in list {
                assert!(n.starts_with(crate::spec::PREFIX), "{n}");
            }
        }
        for n in dropped() {
            assert!(!is_reserved(n), "{n} is both reserved and dropped");
        }
    }

    /// No reserved name matches a dropped-tool pattern.
    #[test]
    fn no_box_mac_pi_or_handback_name_is_reserved() {
        for n in names() {
            for bad in [
                "box",
                "mac_",
                "subagent",
                "handback",
                "pair_chat",
                "pr_host",
            ] {
                assert!(!n.contains(bad), "{n} looks like a dropped tool ({bad})");
            }
        }
        assert!(is_dropped("app_handback_result"));
        assert!(is_dropped("app_subagent"));
    }
}
