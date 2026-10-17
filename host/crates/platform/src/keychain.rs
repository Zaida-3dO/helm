//! The OS store for each profile's credential encryption key.
//!
//! The key lives in Windows Credential Manager (DPAPI-protected, per user),
//! or on Linux in the Secret Service (GNOME Keyring, KWallet) over D-Bus, both reached through the
//! `keyring` crate. There is deliberately no plaintext fallback: when the store fails or is
//! missing, credential access stops.

use std::collections::HashMap;
use std::sync::Mutex;

/// Service name under which every profile key is filed. The account is the profile's hashed id.
pub const KEY_SERVICE: &str = "Helm credential key";

#[derive(Debug, thiserror::Error)]
pub enum KeychainError {
    #[error("the credential store is not available on this platform")]
    Unavailable,
    #[error("the stored credential key is malformed")]
    Malformed,
    #[error("credential store error: {0}")]
    Store(String),
}

/// One 32-byte key per account. `account` is always a 64-hex hash, never a path or a name.
pub trait Keychain: Send + Sync {
    fn get_key(&self, account: &str) -> Result<Option<[u8; 32]>, KeychainError>;
    fn set_key(&self, account: &str, key: &[u8; 32]) -> Result<(), KeychainError>;
    fn delete_key(&self, account: &str) -> Result<(), KeychainError>;
}

pub struct NativeKeychain {
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    service: String,
}

impl NativeKeychain {
    pub fn new() -> Self {
        Self::with_service(KEY_SERVICE)
    }

    /// A store filed under another service name; tests use this to stay out of the real one.
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_owned(),
        }
    }
}

impl Default for NativeKeychain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(windows, target_os = "linux"))]
impl Keychain for NativeKeychain {
    fn get_key(&self, account: &str) -> Result<Option<[u8; 32]>, KeychainError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|e| KeychainError::Store(e.to_string()))?;
        match entry.get_secret() {
            Ok(bytes) => {
                let key: [u8; 32] = bytes.try_into().map_err(|_| KeychainError::Malformed)?;
                Ok(Some(key))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(KeychainError::Store(e.to_string())),
        }
    }

    fn set_key(&self, account: &str, key: &[u8; 32]) -> Result<(), KeychainError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|e| KeychainError::Store(e.to_string()))?;
        entry
            .set_secret(key)
            .map_err(|e| KeychainError::Store(e.to_string()))
    }

    fn delete_key(&self, account: &str) -> Result<(), KeychainError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|e| KeychainError::Store(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeychainError::Store(e.to_string())),
        }
    }
}

/// Other platforms: no store, so credentials are unavailable (never plaintext).
#[cfg(not(any(windows, target_os = "linux")))]
impl Keychain for NativeKeychain {
    fn get_key(&self, _account: &str) -> Result<Option<[u8; 32]>, KeychainError> {
        Err(KeychainError::Unavailable)
    }
    fn set_key(&self, _account: &str, _key: &[u8; 32]) -> Result<(), KeychainError> {
        Err(KeychainError::Unavailable)
    }
    fn delete_key(&self, _account: &str) -> Result<(), KeychainError> {
        Err(KeychainError::Unavailable)
    }
}

/// An in-memory store for tests. Never used by the app itself.
#[derive(Default)]
pub struct MemoryKeychain {
    keys: Mutex<HashMap<String, [u8; 32]>>,
    /// When set, every call fails, to prove there is no plaintext fallback.
    pub fail: std::sync::atomic::AtomicBool,
}

impl MemoryKeychain {
    fn check(&self) -> Result<(), KeychainError> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            Err(KeychainError::Store("injected failure".into()))
        } else {
            Ok(())
        }
    }
}

impl Keychain for MemoryKeychain {
    fn get_key(&self, account: &str) -> Result<Option<[u8; 32]>, KeychainError> {
        self.check()?;
        Ok(self.keys.lock().unwrap().get(account).copied())
    }
    fn set_key(&self, account: &str, key: &[u8; 32]) -> Result<(), KeychainError> {
        self.check()?;
        self.keys.lock().unwrap().insert(account.to_owned(), *key);
        Ok(())
    }
    fn delete_key(&self, account: &str) -> Result<(), KeychainError> {
        self.check()?;
        self.keys.lock().unwrap().remove(account);
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Round-trips a key through the real Windows Credential Manager under a throwaway service
    /// name, then deletes it.
    #[test]
    fn credential_manager_round_trip() {
        let service = format!("helm-test-{}", uuid::Uuid::new_v4());
        let store = NativeKeychain::with_service(&service);
        let account = "a".repeat(64);
        assert_eq!(store.get_key(&account).unwrap(), None);
        let key = [7u8; 32];
        store.set_key(&account, &key).unwrap();
        assert_eq!(store.get_key(&account).unwrap(), Some(key));
        store.delete_key(&account).unwrap();
        assert_eq!(store.get_key(&account).unwrap(), None);
    }
}
