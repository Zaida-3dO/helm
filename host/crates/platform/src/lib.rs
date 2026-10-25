//! Platform seams for the Helm host.
//!
//! Every OS-specific capability sits behind a small trait. Windows gets the real
//! implementations, Linux gets cheap ones or no-op stubs, and macOS is not a target.
//!
//! What is used for real: the credential key store ([`keychain`]), owner-only file security and
//! atomic replace ([`fs`]), single-instance signalling ([`instance`]) and the quit confirmation
//! ([`dialogs`]). Power, taskbar badge and sound are no-op stubs until they are implemented.

pub mod audio;
pub mod dialogs;
pub mod external;
pub mod fs;
pub mod instance;
pub mod keychain;
pub mod pathgate;
pub mod power;
pub mod private;
pub mod stubs;

use std::sync::Arc;

pub use audio::{AudioInputs, CaptureDevice};
pub use dialogs::Dialogs;
pub use external::OpenExternal;
pub use keychain::{Keychain, KeychainError};
pub use power::PowerAssertions;
pub use stubs::{SoundOut, TaskbarBadge};

/// Every platform capability the host holds, chosen once at startup.
#[derive(Clone)]
pub struct Platform {
    pub keychain: Arc<dyn Keychain>,
    pub dialogs: Arc<dyn Dialogs>,
    pub power: Arc<dyn PowerAssertions>,
    pub audio: Arc<dyn AudioInputs>,
    pub external: Arc<dyn OpenExternal>,
    pub badge: Arc<dyn TaskbarBadge>,
    pub sound: Arc<dyn SoundOut>,
}

impl Platform {
    /// The real implementations for the running OS.
    pub fn native() -> Self {
        Self {
            keychain: Arc::new(keychain::NativeKeychain::new()),
            dialogs: Arc::new(dialogs::NativeDialogs),
            power: Arc::new(power::NativePower::new()),
            audio: Arc::new(audio::NativeAudio),
            external: Arc::new(external::NativeOpen),
            badge: Arc::new(stubs::NoopBadge),
            sound: Arc::new(stubs::NoopSound),
        }
    }
}
