//! Encrypted credential store.
//!
//! Credentials are AES-256-GCM envelopes in owner-only files. The 256-bit key is per profile,
//! created once under a file lock and held in the OS credential store (Windows Credential
//! Manager) under the profile's hashed account. If the store fails, credential access stops:
//! there is no plaintext fallback.
//!
//! Envelope layout (all fields fixed-size except the ciphertext):
//!
//! | bytes | field |
//! |---|---|
//! | 4 | magic `HCE1` |
//! | 1 | version (1) |
//! | 12 | random 96-bit nonce |
//! | n + 16 | ciphertext and 128-bit tag |
//!
//! The associated data binds each envelope to its profile and identity (for example an MCP
//! server's id), so an envelope copied to another profile or entry fails to open.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use helm_platform::{Keychain, KeychainError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const MAGIC: &[u8; 4] = b"HCE1";
pub const VERSION: u8 = 1;
pub const MAX_ENVELOPE: usize = 1024 * 1024;
pub const MAX_PLAINTEXT: usize = 512 * 1024;
const HEADER: usize = 4 + 1 + 12;

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("credential store unavailable: {0}")]
    Keychain(#[from] KeychainError),
    #[error("credential is larger than 512 KiB")]
    PlaintextTooLarge,
    #[error("credential envelope is larger than 1 MiB")]
    EnvelopeTooLarge,
    #[error("credential envelope is malformed or for another profile or entry")]
    Rejected,
    #[error("credential name is not allowed")]
    BadName,
    #[error("credential file: {0}")]
    Io(#[from] std::io::Error),
}

pub struct CredentialStore {
    dir: PathBuf,
    /// The profile's hashed id: the keychain account and part of every envelope's AAD.
    account: String,
    keychain: Arc<dyn Keychain>,
    key: Mutex<Option<[u8; 32]>>,
}

impl CredentialStore {
    /// `dir` is an owner-only folder in the profile (for example `mcp-auth/`); `profile_hash` is
    /// [`Profile::hash`](crate::profile::Profile).
    pub fn new(dir: &Path, profile_hash: &str, keychain: Arc<dyn Keychain>) -> Self {
        Self {
            dir: dir.to_owned(),
            account: profile_hash.to_owned(),
            keychain,
            key: Mutex::new(None),
        }
    }

    /// The profile key: from the cache, else the keychain, else created once under `key.lock`.
    fn key(&self) -> Result<[u8; 32], CredentialError> {
        let mut cached = self.key.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(k) = *cached {
            return Ok(k);
        }
        crate::privfs::create_private_dir_all(&self.dir)?;
        let lock_path = self.dir.join("key.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock.lock()?; // OS file lock: released on drop or crash
        let key = match self.keychain.get_key(&self.account)? {
            Some(k) => k,
            None => {
                let mut k = [0u8; 32];
                getrandom::fill(&mut k).map_err(|e| std::io::Error::other(e.to_string()))?;
                self.keychain.set_key(&self.account, &k)?;
                // Read back, so a store that silently drops writes is caught now.
                self.keychain
                    .get_key(&self.account)?
                    .filter(|r| *r == k)
                    .ok_or(CredentialError::Keychain(KeychainError::Store(
                        "key did not persist".into(),
                    )))?
            }
        };
        drop(lock);
        *cached = Some(key);
        Ok(key)
    }

    fn aad(&self, identity: &str) -> Vec<u8> {
        let mut aad = b"helm-credential\0".to_vec();
        aad.extend_from_slice(self.account.as_bytes());
        aad.push(0);
        aad.extend_from_slice(identity.as_bytes());
        aad
    }

    pub fn seal(&self, identity: &str, plaintext: &[u8]) -> Result<Vec<u8>, CredentialError> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(CredentialError::PlaintextTooLarge);
        }
        let cipher = Aes256Gcm::new_from_slice(&self.key()?).expect("32-byte key");
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
        let aad = self.aad(identity);
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| CredentialError::Rejected)?;
        let mut out = Vec::with_capacity(HEADER + ct.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        if out.len() > MAX_ENVELOPE {
            return Err(CredentialError::EnvelopeTooLarge);
        }
        Ok(out)
    }

    pub fn open(&self, identity: &str, envelope: &[u8]) -> Result<Vec<u8>, CredentialError> {
        if envelope.len() > MAX_ENVELOPE {
            return Err(CredentialError::EnvelopeTooLarge);
        }
        if envelope.len() < HEADER + 16 || &envelope[..4] != MAGIC || envelope[4] != VERSION {
            return Err(CredentialError::Rejected);
        }
        let cipher = Aes256Gcm::new_from_slice(&self.key()?).expect("32-byte key");
        let aad = self.aad(identity);
        let pt = cipher
            .decrypt(
                Nonce::from_slice(&envelope[5..HEADER]),
                Payload {
                    msg: &envelope[HEADER..],
                    aad: &aad,
                },
            )
            .map_err(|_| CredentialError::Rejected)?;
        if pt.len() > MAX_PLAINTEXT {
            return Err(CredentialError::PlaintextTooLarge);
        }
        Ok(pt)
    }

    fn file(&self, name: &str) -> Result<PathBuf, CredentialError> {
        let ok = !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !ok {
            return Err(CredentialError::BadName);
        }
        Ok(self.dir.join(format!("{name}.cred")))
    }

    /// Seal and atomically write `plaintext` as the owner-only file `<name>.cred`.
    pub fn write(
        &self,
        name: &str,
        identity: &str,
        plaintext: &[u8],
    ) -> Result<(), CredentialError> {
        let path = self.file(name)?;
        let envelope = self.seal(identity, plaintext)?;
        crate::privfs::create_private_dir_all(&self.dir)?;
        crate::privfs::write_atomic(&path, &envelope)?;
        Ok(())
    }

    /// Read `<name>.cred` without following links, capped at 1 MiB, and open it.
    pub fn read(&self, name: &str, identity: &str) -> Result<Vec<u8>, CredentialError> {
        let path = self.file(name)?;
        let envelope = crate::privfs::read_capped(&path, MAX_ENVELOPE as u64).map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                CredentialError::EnvelopeTooLarge
            } else {
                CredentialError::Io(e)
            }
        })?;
        self.open(identity, &envelope)
    }

    /// "Forget": remove the local file only; never revokes anything remotely.
    pub fn forget(&self, name: &str) -> Result<(), CredentialError> {
        match std::fs::remove_file(self.file(name)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_platform::keychain::MemoryKeychain;
    use std::sync::atomic::Ordering;

    fn store(dir: &Path, profile: &str, kc: Arc<MemoryKeychain>) -> CredentialStore {
        CredentialStore::new(&dir.join("mcp-auth"), profile, kc)
    }

    #[test]
    fn round_trip_through_a_file() {
        let d = tempfile::tempdir().unwrap();
        let kc = Arc::new(MemoryKeychain::default());
        let s = store(d.path(), "p1", kc.clone());
        s.write("server-a", "mcp:server-a", b"token").unwrap();
        assert_eq!(s.read("server-a", "mcp:server-a").unwrap(), b"token");
        let raw = std::fs::read(d.path().join("mcp-auth").join("server-a.cred")).unwrap();
        assert_eq!(&raw[..4], MAGIC);
        assert!(
            !raw.windows(5).any(|w| w == b"token"),
            "no plaintext on disk"
        );
        assert!(
            helm_platform::fs::is_owner_only(&d.path().join("mcp-auth").join("server-a.cred"))
                .unwrap()
        );
        // A second store on the same profile and keychain reuses the key.
        let again = store(d.path(), "p1", kc);
        assert_eq!(again.read("server-a", "mcp:server-a").unwrap(), b"token");
    }

    #[test]
    fn an_envelope_cannot_move_between_identities_or_profiles() {
        let d = tempfile::tempdir().unwrap();
        let kc = Arc::new(MemoryKeychain::default());
        let s = store(d.path(), "p1", kc.clone());
        let env = s.seal("mcp:a", b"secret").unwrap();
        assert!(matches!(
            s.open("mcp:b", &env),
            Err(CredentialError::Rejected)
        ));
        // Same key, different profile account in the AAD.
        let other = store(d.path(), "p2", kc.clone());
        let k = kc.get_key("p1").unwrap().unwrap();
        kc.set_key("p2", &k).unwrap();
        assert!(matches!(
            other.open("mcp:a", &env),
            Err(CredentialError::Rejected)
        ));
    }

    #[test]
    fn tampering_is_detected() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path(), "p1", Arc::new(MemoryKeychain::default()));
        let mut env = s.seal("id", b"secret").unwrap();
        let last = env.len() - 1;
        env[last] ^= 1;
        assert!(matches!(s.open("id", &env), Err(CredentialError::Rejected)));
        let mut wrong_version = s.seal("id", b"secret").unwrap();
        wrong_version[4] = 2;
        assert!(matches!(
            s.open("id", &wrong_version),
            Err(CredentialError::Rejected)
        ));
    }

    #[test]
    fn size_limits() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path(), "p1", Arc::new(MemoryKeychain::default()));
        assert!(s.seal("id", &vec![0; MAX_PLAINTEXT]).is_ok());
        assert!(matches!(
            s.seal("id", &vec![0; MAX_PLAINTEXT + 1]),
            Err(CredentialError::PlaintextTooLarge)
        ));
        assert!(matches!(
            s.open("id", &vec![0; MAX_ENVELOPE + 1]),
            Err(CredentialError::EnvelopeTooLarge)
        ));
        std::fs::create_dir_all(d.path().join("mcp-auth")).unwrap();
        std::fs::write(
            d.path().join("mcp-auth").join("big.cred"),
            vec![0; MAX_ENVELOPE + 1],
        )
        .unwrap();
        assert!(matches!(
            s.read("big", "id"),
            Err(CredentialError::EnvelopeTooLarge)
        ));
    }

    #[test]
    fn keychain_failure_stops_access_with_no_plaintext_fallback() {
        let d = tempfile::tempdir().unwrap();
        let kc = Arc::new(MemoryKeychain::default());
        kc.fail.store(true, Ordering::SeqCst);
        let s = store(d.path(), "p1", kc);
        assert!(matches!(
            s.write("a", "id", b"secret"),
            Err(CredentialError::Keychain(_))
        ));
        assert!(!d.path().join("mcp-auth").join("a.cred").exists());
    }

    #[test]
    fn names_are_restricted() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path(), "p1", Arc::new(MemoryKeychain::default()));
        for bad in ["", "..", "a/b", "a\\b", "con.txt"] {
            assert!(
                matches!(s.write(bad, "id", b"x"), Err(CredentialError::BadName)),
                "{bad}"
            );
        }
    }
}
