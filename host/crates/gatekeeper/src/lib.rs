//! The question gatekeeper: it decides whether a chat's question really needs the user.
//!
//! | Module | What |
//! |---|---|
//! | [`settings`] | the per-person record (switch, how-often-to-ask, policy words, model), read leniently and written strictly |
//! | [`route`] | the pure decision table: what is forced to the user whatever a model answers, the hard-to-undo word list, and the limits |
//! | [`scrub`] | the host's pass over the write-up: ids stripped, fields capped, a plain fallback built from the card |
//! | [`store`] | the `gate_asks` audit trail and `gate_settings` tables |
//! | [`service`] | the gate: holds each question card, judges it through the short-lived helper, and answers the card through the chat runtime — or hands it back |
//! | [`stoproute`] | the stop judge's pure decision table: which turn ends are never looked at, what the model's answer is, and what Helm does about it |
//! | [`stops`] | the stop judge: holds a clean turn end while the working mode is run to completion, judges it through the same helper, and nudges the chat on or lets it stop |
//! | [`workmode`] | the working mode (run to completion, or check in with me): the app's mode, a board's own, the launch text and the line a turn carries after a change |
//! | [`wmode_ext`] | the working mode as the `workmode` settings key: chats read it, and only change it through a card the user approves |
//! | [`ops`] | the UI-only `gate_*` and `workmode_*` ops: history, review, change answer, settings, and the working mode |
//! | `bindings` | the TypeScript for the shapes the UI reads |
//!
//! Every card the gate looks at leaves one audit row, including cards it let through for a
//! structural reason. The safety line is checked in the chat actor and again here: permission
//! cards, folder trust and secret inputs are never judged. The answer the gate gives is only
//! ever one of the card's own offered choices, and a gate answer is never the user's consent:
//! it arrives as a tool result or an Answer-origin message, never as typed words.

pub mod bindings;
mod ops;
pub mod route;
pub mod scrub;
pub mod service;
pub mod settings;
pub mod settings_key;
pub mod stoproute;
pub mod stops;
pub mod store;
pub mod wmode_ext;
pub mod workmode;

pub use ops::register_ops;
pub use service::GateService;

use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_ipc::{EventSink, Services};
use std::sync::Arc;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("gatekeeper", store::init)
        .one_time("gatekeeper-asks-stop-channel", store::allow_stop_channel);
}

/// The shared [`GateService`], installed as the chat runtime's question gate. Needs the
/// database and the chat runtime.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let (Some(db), Some(rt)) = (services.get::<Database>(), services.get::<ChatRuntime>()) else {
        return;
    };
    let svc = GateService::new(db.clone(), events.clone(), (*rt).clone());
    rt.set_question_gate(svc.clone());
    services.insert(svc);
    // The stop judge: it holds a clean turn end while chats run to completion. It reads plan
    // usage from the meter, which is registered before this.
    let usage = services
        .get::<helm_usage::UsageService>()
        .map(|u| Arc::new(stops::UsageMeter(u)) as Arc<dyn stops::UsageProbe>);
    rt.set_stop_judge(stops::StopGate::new(
        db.clone(),
        events.clone(),
        (*rt).clone(),
        usage,
    ));
    start_upkeep(&(*rt).clone());
}

/// The upkeep tick, every five minutes: decided rows nobody reviewed leave the review list
/// after seven days, records older than half a year are pruned, and a card left "checking"
/// by a crash settles as failed.
fn start_upkeep(rt: &ChatRuntime) {
    let db = rt.db().clone();
    rt.handle().spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            every.tick().await;
            let db = db.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let now = helm_core::now_ms();
                let _: Result<(), rusqlite::Error> = db.write(|tx| {
                    store::settle_orphans(tx, now)?;
                    store::mark_kept(tx, now)?;
                    store::prune(tx, now)?;
                    Ok(())
                });
            })
            .await;
        }
    });
}
