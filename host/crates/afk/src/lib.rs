//! AFK: spans, Windows auto-AFK and the "While you were out"
//! digest. A global "all chats" mode.
//!
//! | Module | What |
//! |---|---|
//! | [`settings`] | the AFK record, read leniently, written strictly |
//! | [`monitor`] | the pure auto-AFK decision function |
//! | [`store`] | the `afk_settings`, `afk_spans` and `afk_digests` tables |
//! | [`digest`] | the digest prompt and the delivery step |
//! | [`service`] | the change engine, the 30 s tick and digest delivery |
//! | [`signals`] | Windows lock, sleep/resume and idle, on a hidden window of its own |
//! | `ops` | the UI-only `afk_*` ops |
//!
//! Not built yet: one-chat mode and the workers' automatic answers
//! (per-chat AFK), the unsigned-commit authority (no wrapper on
//! Windows), box chats, and the 5 s sleep hold (nothing is published to workers yet).

pub mod digest;
pub mod monitor;
mod ops;
pub mod service;
pub mod settings;
pub mod signals;
pub mod store;

pub use ops::register_ops;
pub use service::{AfkService, AfkView, AskSource, DigestSender};

use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_ipc::{EventSink, Services};
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("afk", store::init);
}

/// Digests go through the chats crate's host-send path: origin App, admission Exempt (a digest
/// never waits for a run slot), queued behind a running turn.
struct RuntimeSender(ChatRuntime);

impl DigestSender for RuntimeSender {
    fn send(
        &self,
        chat_id: &str,
        submission_id: &str,
        text: &str,
    ) -> Result<bool, (String, String)> {
        helm_chats::submit::host_send(
            &self.0,
            helm_chats::submit::HostSend {
                chat_id,
                submission_id,
                text,
                mode: helm_chats::submit::HostSendMode::Queue,
                origin: helm_chats::types::SubmissionOrigin::App,
                gated: false,
            },
        )
        .map(|s| s.queued)
        .map_err(|e| (e.code.to_string(), e.message))
    }
}

/// Chats waiting on the user, from the attention summary's "needs you" rows.
struct AttentionAsks(ChatRuntime);

impl AskSource for AttentionAsks {
    fn waiting(&self) -> Vec<String> {
        self.0
            .attention()
            .map(|a| a.needs_you.into_iter().map(|r| r.chat_id).collect())
            .unwrap_or_default()
    }
}

/// The shared [`AfkService`]. Needs the database and the chat runtime.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let (Some(db), Some(rt)) = (services.get::<Database>(), services.get::<ChatRuntime>()) else {
        return;
    };
    let rt = (*rt).clone();
    services.insert(Arc::new(AfkService::new(
        db,
        events.clone(),
        Arc::new(RuntimeSender(rt.clone())),
        Arc::new(AttentionAsks(rt)),
    )));
}

/// Start auto-AFK: the Windows signal thread and the 30 s tick (which also delivers digests).
pub fn start(runtime: &ChatRuntime, services: &Services) {
    let Some(svc) = services.get::<AfkService>() else {
        return;
    };
    signals::start(svc.clone());
    runtime.handle().spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            every.tick().await;
            let svc = svc.clone();
            let _ = tokio::task::spawn_blocking(move || {
                svc.tick(helm_core::now_ms(), signals::idle_secs())
            })
            .await;
        }
    });
}
