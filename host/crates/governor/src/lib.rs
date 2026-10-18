//! The usage governor. It reads plan usage **only** through
//! helm-usage's registered [`helm_usage::UsageService`] (its tables are private), sorts it into a
//! band against local thresholds kept in Helm settings, and near the limit tells running agents to
//! wrap up or stops them.
//!
//! | Module | What |
//! |---|---|
//! | [`bands`] | settings (lenient read, strict write) and the band of a usage reading |
//! | [`store`] | the `governor_settings` table |
//! | [`service`] | the 30 s loop and what each band does |
//! | `ops` | `governor_state`, `governor_settings` (UI-only) and the `governor` event |
//!
//! Not built: the selective band's effect on automated sends (the run-slot hook waits on the
//! chats forward) and proposing backlog items in surplus.

pub mod bands;
pub mod service;
pub mod store;

pub use bands::{Band, GovernorSettings};
pub use service::{ChatControl, Governor, GovernorView, UsageRead};

use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_ipc::{ApiError, EventSink, OpClass, OpCtx, OpRegistry, Services};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("governor", store::init);
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("governor_state", OpClass::Blocking, |ctx, _| {
        to_value(&svc(ctx)?.view(helm_core::now_ms()))
    })
    .blocking("governor_settings", OpClass::Blocking, |ctx, args| {
        let settings = args
            .get("settings")
            .cloned()
            .ok_or_else(|| ApiError::bad_args("governor_settings takes {settings}."))?;
        to_value(&svc(ctx)?.save(&settings, helm_core::now_ms())?)
    });
}

fn svc(ctx: &OpCtx) -> Result<Arc<Governor>, ApiError> {
    ctx.services.need::<Governor>()
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

/// The shared [`Governor`]. Register after helm-usage, whose service it reads.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let (Some(db), Some(usage)) = (
        services.get::<Database>(),
        services.get::<helm_usage::UsageService>(),
    ) else {
        return;
    };
    services.insert(Arc::new(Governor::new(
        db,
        events.clone(),
        Arc::new(service::UsageHandle(usage)),
    )));
}

/// The app's chat controls: wrap-up notes through `host_send` (origin App, steered, never gated)
/// and stops through the `chat_stop` op.
struct RuntimeControl {
    rt: ChatRuntime,
    dispatcher: Arc<helm_ipc::Dispatcher>,
}

impl ChatControl for RuntimeControl {
    fn running(&self) -> Vec<String> {
        self.rt
            .db()
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT id FROM chats WHERE status IN ('starting', 'running') AND archived = 0",
                )?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap_or_default()
    }

    fn wrap_up(&self, chat_id: &str, submission_id: &str, text: &str) -> Result<(), String> {
        // Only into a running turn: an idle chat would take the note as a new prompt.
        if !self.running().iter().any(|c| c == chat_id) {
            return Err("the chat is no longer running".into());
        }
        helm_chats::submit::host_send(
            &self.rt,
            helm_chats::submit::HostSend {
                chat_id,
                submission_id,
                text,
                mode: helm_chats::submit::HostSendMode::Steer,
                origin: helm_chats::types::SubmissionOrigin::App,
                gated: false,
            },
        )
        .map(|_| ())
        .map_err(|e| e.message)
    }

    fn stop(&self, chat_id: &str) {
        let d = self.dispatcher.clone();
        let id = chat_id.to_owned();
        self.rt.handle().spawn(async move {
            if let Err(e) = d
                .call_host("chat_stop", json!({"chatId": id, "continueQueue": false}))
                .await
            {
                log::warn!("usage governor: stop of {id} failed: {}", e.message);
            }
        });
    }
}

/// Start the 30 s loop once the op dispatcher exists.
pub fn start(runtime: &ChatRuntime, dispatcher: &Arc<helm_ipc::Dispatcher>) {
    let Some(gov) = dispatcher.ctx().services.get::<Governor>() else {
        return;
    };
    gov.attach(Arc::new(RuntimeControl {
        rt: runtime.clone(),
        dispatcher: dispatcher.clone(),
    }));
    runtime.handle().spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            every.tick().await;
            let g = gov.clone();
            let _ = tokio::task::spawn_blocking(move || g.tick(helm_core::now_ms())).await;
        }
    });
}
