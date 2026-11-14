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
//! | [`starter`] | starting a Strata install that is not running, when its settings allow it, and stopping it on an unload |
//! | [`lock`] | the graphics-card lock: while it is on, nothing loads a local model |
//! | [`games`] | watching for a game from the user's list, which sets the lock while it runs |
//! | [`task`] | ending and starting the user's restart task around an unload |
//! | `ops` | the `local_*` ops for Settings > Local models and the top bar's unload control |
//! | [`ext`] | the settings chats may change through `app_update_settings` |
//! | [`types`], [`bindings`] | the shapes the UI sees |
//!
//! **Helm never reaches a local server on its own.** There is no probe at start-up and no timer:
//! a server is asked only for a catalogue refresh, a Test, a local chat's turn, or the user's
//! unload of local models — and a Strata that is set to be started when needed is launched only
//! for a refresh, a Test or a turn, never at start-up. The one exception is deliberate: when Helm
//! starts with local models unloaded, it unloads them once more, because a restart task may have
//! relaunched a server at boot. The game watch reads only the list of process names. In debug
//! builds `HELM_TEST_LOCAL_SERVERS` is `off` (no server is ever contacted) or a loopback address
//! that every server uses instead (a fake); release builds ignore it, and with it in effect no
//! real Strata is ever launched or stopped, no scheduled task is touched, and the start-up unload
//! does not run.
//!
//! A server's address must be on this machine, and its key lives in the credential store and goes
//! to nothing but that server and the chat worker it launches.

pub mod bindings;
pub mod ext;
pub mod games;
pub mod gpu;
pub mod lock;
pub mod net;
mod ops;
pub mod servers;
pub mod service;
pub mod settings;
pub mod starter;
pub mod task;
#[cfg(test)]
mod tests;
pub mod types;
mod variants;

pub use lock::LockState;
pub use ops::register_ops;
pub use service::{ChatControl, LocalService};

use helm_chats::ChatRuntime;
use helm_core::credentials::CredentialStore;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{EventSink, Services};
use helm_platform::Platform;
use serde_json::json;
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("local-models", |tx| {
        settings::init(tx)?;
        variants::init(tx)?;
        lock::init(tx)
    });
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
    let svc = Arc::new(
        LocalService::new(
            db,
            creds,
            events.clone(),
            Arc::new(gpu::NvidiaSmi),
            profile.path("catalogue").join("local.json"),
        )
        .with_starter(std::sync::Arc::new(starter::ProcessStarter))
        .with_task(Arc::new(task::Schtasks)),
    );
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

/// Stops busy local chats for an unload: a line in the chat, then the `chat_stop_no_resend` op,
/// so the line comes first and messages Claude had not read are not sent into the refusal.
struct RuntimeChats {
    rt: ChatRuntime,
    dispatcher: Arc<helm_ipc::Dispatcher>,
}

impl ChatControl for RuntimeChats {
    fn stop(&self, chat_id: &str, note: &str) {
        let (d, rt) = (self.dispatcher.clone(), self.rt.clone());
        let (id, note) = (chat_id.to_owned(), note.to_owned());
        self.rt.handle().spawn(async move {
            rt.add_context_row(&id, helm_chats::types::ContextKind::Notice, &note);
            if let Err(e) = d
                .call_host("chat_stop_no_resend", json!({"chatId": id}))
                .await
            {
                log::warn!("unload: stop of chat {id} failed: {}", e.message);
            }
        });
    }

    fn stop_limit(&self) -> std::time::Duration {
        self.rt.limits().stop + std::time::Duration::from_secs(5)
    }
}

/// Once the op dispatcher exists: let an unload stop chats, unload again when Helm starts with
/// local models unloaded, and start the game watch.
pub fn start(runtime: &ChatRuntime, dispatcher: &Arc<helm_ipc::Dispatcher>) {
    let Some(svc) = dispatcher.ctx().services.get::<LocalService>() else {
        return;
    };
    svc.attach_chats(Arc::new(RuntimeChats {
        rt: runtime.clone(),
        dispatcher: dispatcher.clone(),
    }));
    svc.start_watch(Arc::new(games::SystemProcesses));
}
