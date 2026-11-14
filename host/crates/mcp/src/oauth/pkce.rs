//! Random values for a sign-in: the PKCE verifier and challenge, and the `state` value.
//!
//! The verifier is never printed and never stored: only its SHA-256 challenge goes to the
//! server, and the verifier itself goes to the token endpoint once, at the exchange.

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// 48 random bytes as base64url without padding: 64 characters, inside the 43–128 the
/// exchange requires.
pub fn random_verifier() -> String {
    let mut bytes = [0u8; 48];
    fill(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    let sum = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sum)
}

/// A 128-bit `state` value, as hex.
pub fn random_state() -> String {
    let mut bytes = [0u8; 16];
    fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fill(buf: &mut [u8]) {
    // The system random source: if it fails there is no usable sign-in at all.
    getrandom::fill(buf).expect("the system random source failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_random_and_shapes_are_right() {
        let v1 = random_verifier();
        let v2 = random_verifier();
        assert_ne!(v1, v2);
        assert_eq!(v1.len(), 64);
        assert!(v1
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        let c1 = challenge(&v1);
        assert_ne!(c1, v1);
        assert!(c1
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert!(!c1.contains('='));
        let s1 = random_state();
        assert_eq!(s1.len(), 32);
        assert_ne!(s1, random_state());
        // A known S256 answer from the specification's example.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
