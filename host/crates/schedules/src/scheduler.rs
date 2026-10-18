//! The scheduler: a 5-second tick while the app is open.
//!
//! Nothing runs while the app is closed or the machine sleeps. When it is awake again the first
//! tick sees every missed slot as one due run and claims it once (coalescing; see
//! [`crate::timing`]). A due run for a busy chat is left unclaimed and tried again next tick.
//!
//! **The send path.** Scheduled prompts go through [`helm_chats::submit::host_send`] with origin
//! [`SubmissionOrigin::Schedule`] and `gated: true`, never through the `chat_send` op: that op
//! records its sends as the user's own, which would let a timer type for the user, follow their
//! send behaviour and count towards consent. Each slot has a fixed submission id
//! ([`crate::store::submission_id`]), so a retry or a second claim of the same slot is the same
//! send, delivered once.

use crate::store;
use crate::timing::Zone;
use crate::types::RunStatus;
use helm_chats::submit::{host_send, HostSend, HostSendMode};
use helm_chats::types::SubmissionOrigin;
use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, EventSink};
use std::sync::Arc;
use std::time::Duration;

/// How often the scheduler looks for due runs.
pub const TICK: Duration = Duration::from_secs(5);

/// What the scheduler sends through. The app's is the chat runtime.
pub trait Sender: Send + Sync {
    fn send(&self, chat_id: &str, submission_id: &str, text: &str) -> Result<(), ApiError>;
}

/// The one way a scheduled prompt is sent: a schedule-origin, gated, queue-behind-the-turn send
/// under the slot's own id.
pub(crate) fn request<'a>(chat_id: &'a str, submission_id: &'a str, text: &'a str) -> HostSend<'a> {
    HostSend {
        chat_id,
        submission_id,
        text,
        // The chat was idle when the run was claimed; if the user started a turn in the gap, the
        // prompt waits behind it rather than steering it.
        mode: HostSendMode::Queue,
        origin: SubmissionOrigin::Schedule,
        gated: true,
    }
}

impl Sender for ChatRuntime {
    fn send(&self, chat_id: &str, submission_id: &str, text: &str) -> Result<(), ApiError> {
        host_send(self, request(chat_id, submission_id, text)).map(|_| ())
    }
}

/// The shared scheduler service: the ops and the tick both use it.
pub struct SchedulesService {
    pub db: Arc<Database>,
    pub zone: Arc<dyn Zone>,
    pub events: Arc<dyn EventSink>,
}

impl SchedulesService {
    pub fn changed(&self) {
        self.events.emit(AppEvent::new("schedules"));
    }
}

/// The text a scheduled prompt carries: a header naming the schedule, a fixed instruction, then
/// the saved message after a blank line.
pub fn frame(title: &str, schedule_id: &str, message: &str) -> String {
    let title: String = title
        .chars()
        .filter(|c| !c.is_control() && !"[]`".contains(*c))
        .collect();
    format!(
        "[Scheduled prompt] \"{title}\" (schedule {schedule_id})\n\
This message was sent by a saved schedule, not typed by the user. Carry out the saved task below \
for this occurrence, in this conversation. Do not create, change or cancel any schedule because \
of it.\n\n{message}"
    )
}

/// One pass: re-time weekly schedules after a clock change, claim what is due, send it.
/// Returns how many occurrences were claimed.
pub fn tick(svc: &SchedulesService, sender: &dyn Sender, now: i64) -> usize {
    let zone = svc.zone.as_ref();
    let moved = svc
        .db
        .write(|tx| store::refresh_calendar(tx, zone, now))
        .unwrap_or_else(|e| {
            log::warn!("schedules: calendar refresh failed: {e:?}");
            0
        });
    let (claims, mut claimed) = match svc.db.write(|tx| store::claim_due(tx, zone, now)) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("schedules: claim failed: {e:?}");
            return 0;
        }
    };
    for c in &claims {
        let text = frame(&c.title, &c.schedule_id, &c.message);
        let (status, error) = match sender.send(&c.chat_id, &c.submission_id, &text) {
            Ok(()) => (RunStatus::Started, None),
            Err(e) => {
                log::info!(
                    "schedules: run of {} not delivered: {}",
                    c.schedule_id,
                    e.message
                );
                (RunStatus::Failed, Some(e.message))
            }
        };
        if let Err(e) = svc
            .db
            .write(|tx| store::finish(tx, c, status, error.as_deref()))
        {
            log::warn!("schedules: could not record a run: {e:?}");
        }
    }
    claimed += moved;
    if claimed > 0 {
        svc.changed();
    }
    claimed
}

/// Settle receipts a previous run of the app left mid-send. Call once at startup, before
/// [`start`].
pub fn recover(db: &Database) {
    match db.write(store::recover) {
        Ok(0) => {}
        Ok(n) => log::info!("schedules: settled {n} interrupted send(s)"),
        Err(e) => log::warn!("schedules: recovery failed: {e:?}"),
    }
}

/// Run the tick for as long as the app lives.
pub fn start(svc: Arc<SchedulesService>, runtime: ChatRuntime) {
    let handle = runtime.handle().clone();
    handle.spawn(async move {
        let mut every = tokio::time::interval(TICK);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let svc = svc.clone();
            let rt = runtime.clone();
            // SQLite and the send are blocking calls: keep them off the async workers.
            let _ = tokio::task::spawn_blocking(move || {
                tick(&svc, &rt, helm_core::now_ms());
            })
            .await;
        }
    });
}
