//! Requests Helm applies itself when the user approves.
//!
//! An applying kind's request is staged by its own tool, with a host-built target and card lines,
//! and is never spent by the agent. [`decide_and_apply`] is the one way any surface answers a
//! request: it records the decision (one compare-and-set out of `pending`, so the first answer
//! wins), and for an approved applying kind starts the apply on its own thread so the click never
//! waits. The apply spends the grant first ([`store::spend_for_apply`], `allowed` → `used`, its
//! own transaction), then runs the kind's [`Applier`] under the app's maintenance lock, then
//! writes the outcome. The agent hears the outcome as the usual note from upkeep.
//!
//! A crash before the spend leaves the request `allowed`: it expires and is reported as not
//! applied. A crash after it leaves the request `used` with no outcome: after ten minutes upkeep
//! says Helm stopped while applying.

use crate::kinds::AllowKinds;
use crate::store;
use crate::types::{ConsentDecideArgs, ConsentRequest, Decision};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, EventSink, Services};
use serde_json::Value;
use std::sync::{Arc, OnceLock};

/// Where the user may answer from. Each surface that calls [`decide_and_apply`] names itself
/// here: `app` for this PC's main window, `device:<name>` for another device of the household
/// answering a card of a chat that runs here, `telegram` for a tap on the Approve button of an
/// ask the relay sent to the owner's own Telegram account.
pub const SURFACES: &[&str] = &["app", "device:", "telegram"];

/// Whether `via` names a surface that may answer.
fn surface_ok(via: &str) -> bool {
    SURFACES
        .iter()
        .any(|s| via.starts_with(s) && (s.ends_with(':') || via == *s))
}

/// One approved request, as its applier sees it.
pub struct ApplyJob<'a> {
    pub services: &'a Arc<Services>,
    pub events: &'a Arc<dyn EventSink>,
    pub request_id: &'a str,
    pub chat_id: &'a str,
    /// The canonical target the staging tool built (and the card was rendered from).
    pub target: &'a Value,
}

/// Applies one kind's approved requests.
pub trait Applier: Send + Sync {
    /// Apply the request and say what happened, in a sentence or two the agent and the user can
    /// read. Runs on a plain thread (not inside the async runtime) under the maintenance lock.
    fn apply(&self, job: &ApplyJob<'_>) -> String;
    /// The host tool that stages this kind, named when `app_request_allow` refuses it.
    fn staged_with(&self) -> &'static str;
    /// Whether applying holds the app's maintenance lock. A kind that changes Helm's own records
    /// does; a kind that only hands the approval on to something outside Helm and waits for its
    /// answer does not, so other chats' changes never queue behind that wait.
    fn holds_maintenance_lock(&self) -> bool {
        true
    }
}

/// The app's maintenance lock (the one maintenance ops and mutating host tools hold), as a
/// service applying can take. Set once the op dispatcher exists.
#[derive(Default)]
pub struct MaintenanceLock(OnceLock<Arc<tokio::sync::Mutex<()>>>);

impl MaintenanceLock {
    pub fn set(&self, lock: Arc<tokio::sync::Mutex<()>>) {
        let _ = self.0.set(lock);
    }

    pub fn get(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
        self.0.get().cloned()
    }
}

/// A decision, and the apply it started (tests join it; the app lets it run).
pub struct Decided {
    pub request: ConsentRequest,
    pub applying: Option<std::thread::JoinHandle<()>>,
}

fn signal(services: &Services, events: &Arc<dyn EventSink>, chat_id: &str) {
    // A signal only: the card is rebuilt from consent_list, never from an event payload.
    events.emit(AppEvent::new("consent").with("chatId", chat_id.to_owned()));
    // The decision is owed to the agent: wake the delivery loop.
    if let Some(w) = services.get::<crate::ConsentWake>() {
        w.0.notify_one();
    }
}

/// Record the user's answer to a request from surface `via`, and apply it when it is an approved
/// applying kind. `conflict` when the request is no longer pending.
pub fn decide_and_apply(
    services: &Arc<Services>,
    events: &Arc<dyn EventSink>,
    args: &ConsentDecideArgs,
    via: &str,
) -> Result<Decided, ApiError> {
    if !surface_ok(via) {
        return Err(ApiError::bad_args("Unknown place to answer from."));
    }
    let db = services.need::<Database>()?;
    let kinds = services
        .get::<AllowKinds>()
        .unwrap_or_else(|| Arc::new(AllowKinds::new()));
    let v = store::decide_via(
        &db,
        &kinds,
        &args.request_id,
        args.decision,
        args.grant_minutes,
        args.note.as_deref(),
        via,
        helm_core::now_ms(),
    )?;
    signal(services, events, &v.chat_id);
    let applying = match (args.decision, kinds.applier(&v.kind)) {
        (Decision::Allow, Some(applier)) => {
            let (services, events, id) = (services.clone(), events.clone(), v.id.clone());
            let started = std::thread::Builder::new()
                .name("consent-apply".into())
                .spawn(move || apply(&services, &events, &db, &kinds, &*applier, &id));
            match started {
                Ok(h) => Some(h),
                Err(e) => {
                    // Nothing was spent: the grant lapses and the agent hears it was not applied.
                    log::warn!("consent: could not start applying: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    Ok(Decided {
        request: v,
        applying,
    })
}

fn apply(
    services: &Arc<Services>,
    events: &Arc<dyn EventSink>,
    db: &Database,
    kinds: &AllowKinds,
    applier: &dyn Applier,
    request_id: &str,
) {
    let approved = match store::spend_for_apply(db, kinds, request_id, helm_core::now_ms()) {
        Ok(Some(a)) => a,
        Ok(None) => return log::warn!("consent: an approved request was no longer spendable"),
        Err(e) => return log::warn!("consent: spending an approval failed: {}", e.message),
    };
    let lock = applier
        .holds_maintenance_lock()
        .then(|| services.get::<MaintenanceLock>().and_then(|m| m.get()))
        .flatten();
    // A plain thread, never the runtime's: blocking_lock is safe here.
    let guard = lock.as_ref().map(|l| l.blocking_lock());
    let job = ApplyJob {
        services,
        events,
        request_id,
        chat_id: &approved.chat_id,
        target: &approved.target,
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| applier.apply(&job)))
        .unwrap_or_else(|_| {
            "Helm hit an internal error while applying; check with app_get_settings.".into()
        });
    drop(guard);
    if let Err(e) = store::set_outcome(db, request_id, &outcome) {
        log::warn!("consent: the outcome was not saved: {}", e.message);
    }
    signal(services, events, &approved.chat_id);
}
