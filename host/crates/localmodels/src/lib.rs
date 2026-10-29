//! Local model servers on this machine (Strata, Ollama): their settings and keys, the model
//! catalogue, and what happens before and after each turn of a chat that runs on one.
//!
//! | Module | What |
//! |---|---|
//! | [`settings`] | the `local_model_settings` record, the loopback address rule, strict saves |
//! | [`net`] | requests to a server: loopback only, no proxy, short timeouts |
//! | [`servers`] | what Helm asks of Ollama and of Strata |
//! | [`gpu`] | free graphics memory from `nvidia-smi` |
//! | [`service`] | the service: keys, catalogue, test, the check and loading before a turn |
//! | `ops` | the `local_*` ops for Settings > Local models |
//! | [`ext`] | the settings chats may change through `app_update_settings` |
//! | [`types`], [`bindings`] | the shapes the UI sees |
//!
//! **Helm never reaches a local server on its own.** There is no probe at start-up and no timer:
//! a server is asked only for a catalogue refresh, a Test, or a local chat's turn. In debug builds
//! `HELM_TEST_LOCAL_SERVERS` is `off` (no server is ever contacted) or a loopback address that
//! every server uses instead (a fake); release builds ignore it.
//!
//! A server's address must be on this machine, and its key lives in the credential store and goes
//! to nothing but that server and the chat worker it launches.

pub mod bindings;
pub mod ext;
pub mod gpu;
pub mod net;
mod ops;
pub mod servers;
pub mod service;
pub mod settings;
#[cfg(test)]
mod tests;
pub mod types;

pub use ops::register_ops;
pub use service::LocalService;

use helm_chats::ChatRuntime;
use helm_core::credentials::CredentialStore;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{EventSink, Services};
use helm_platform::Platform;
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("local-models", settings::init);
}

/// The settings chats may change, for [`helm_hosttools::SettingsExtensions`].
pub fn settings_extension() -> Arc<dyn helm_hosttools::SettingsExtension> {
    ext::extension()
}

/// The shared [`LocalService`], installed into the chat runtime so chats can use it. Needs the
/// database, profile, platform and chat runtime to be in place. Contacts no server.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let (Some(db), Some(profile), Some(platform)) = (
        services.get::<Database>(),
        services.get::<Profile>(),
        services.get::<Platform>(),
    ) else {
        log::warn!("local models: no database, profile or platform; local models stay off");
        return;
    };
    let creds = CredentialStore::new(
        &profile.path("credentials"),
        &profile.hash,
        platform.keychain.clone(),
    );
    let svc = Arc::new(LocalService::new(
        db,
        creds,
        events.clone(),
        Arc::new(gpu::NvidiaSmi),
        profile.path("catalogue").join("local.json"),
    ));
    if let Some(rt) = services.get::<ChatRuntime>() {
        rt.set_local_models(svc.clone());
    }
    // The cost report prices local tokens against the model chosen here.
    let chosen = svc.clone();
    services.insert(Arc::new(helm_usage::CompareChoice(Arc::new(move || {
        chosen.settings().compare_with.key().to_owned()
    }))));
    services.insert(svc);
}
