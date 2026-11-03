//! The scheduler: a 5-second tick while the app is open.
//!
//! Nothing runs while the app is closed or the machine sleeps. When it is awake again the first
//! tick sees every missed slot as one due run and claims it once (coalescing; see
//! [`crate::timing`]). A due run for a busy chat is left unclaimed and tried again next tick.
//!
//! **The send path.** Scheduled prompts go through [`helm_chats::submit::host_send`] with origin
//! [`SubmissionOrigin::Schedule`], never through the `chat_send` op: that op
//! records its sends as the user's own, which would let a timer type for the user, follow their
//! send behaviour and count towards consent. Each slot has a fixed submission id
//! ([`crate::store::submission_id`]), so a retry or a second claim of the same slot is the same
//! send, delivered once.

use crate::store;
use crate::timing::Zone;
use crate::types::RunStatus;
use helm_chats::clock::Clock;
use helm_chats::delete::{delete_in_tx, DeleteRule};
use helm_chats::seams::ProjectGates;
use helm_chats::submit::{host_send, HostSend};
use helm_chats::types::{ChatEvent, SubmissionOrigin};
use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, EventSink};
use rusqlite::OptionalExtension;
use std::sync::Arc;
use std::time::Duration;

/// How often the scheduler looks for due runs.
pub const TICK: Duration = Duration::from_secs(5);

/// How close a run has to be before the machine starts being kept awake for it: room for the
/// tick and for starting a new chat, and no longer than the shortest ordinary repeat.
pub const WAKE_LEAD_MS: i64 = 120_000;

/// What the scheduler sends through. The app's is the chat runtime.
pub trait Sender: Send + Sync {
    fn send(&self, chat_id: &str, submission_id: &str, text: &str) -> Result<(), ApiError>;
}

/// The one way a scheduled prompt is sent: a schedule-origin send under the slot's own id.
pub(crate) fn request<'a>(chat_id: &'a str, submission_id: &'a str, text: &'a str) -> HostSend<'a> {
    HostSend {
        chat_id,
        submission_id,
        text,
        // The chat was idle when the run was claimed; if the user started a turn in the gap, the
        // prompt goes to Claude at its next tool boundary.
        origin: SubmissionOrigin::Schedule,
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
    /// Stamps the chat records a new-chat run announces (absent in tests that do not need it).
    pub clock: Option<Arc<Clock>>,
    /// Whether a project is archived, so a new-chat run does not start a chat in one.
    pub gates: Option<Arc<ProjectGates>>,
}

impl SchedulesService {
    pub fn changed(&self) {
        self.events.emit(AppEvent::new("schedules"));
    }
}

/// The text a scheduled prompt carries: a header naming the schedule, a fixed instruction, then
/// the saved message after a blank line.
pub fn frame(
    title: &str,
    schedule_id: &str,
    message: &str,
    created_by: Option<(&str, &str)>,
) -> String {
    let clean = |t: &str| -> String {
        t.chars()
            .filter(|c| !c.is_control() && !"[]`\"".contains(*c))
            .collect()
    };
    let title = clean(title);
    // A schedule made by an agent in another chat says so, the way a message from another chat does.
    let origin = created_by
        .map(|(t, id)| format!(" It was created by another chat, \"{}\" ({id}).", clean(t)))
        .unwrap_or_default();
    format!(
        "[Scheduled prompt] \"{title}\" (schedule {schedule_id})\n\
This message was sent by a saved schedule, not typed by the user.{origin} Carry out the saved task \
below for this occurrence, in this conversation. Do not create, change or cancel any schedule \
because of it.\n\n{message}"
    )
}

/// Start the chat a new-chat run sends into: an idle chat in the project's folder, with the
/// defaults from Settings, Runtime, announced to the sidebar like any new chat.
fn start_chat(svc: &SchedulesService, project_id: Option<&str>) -> Result<String, String> {
    let project = project_id.ok_or("The schedule has no project to start a chat in.")?;
    let id = svc
        .db
        .write(|tx| {
            let row: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT kind, path FROM projects WHERE id = ?1",
                    [project],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match row {
                None => return Ok(Err("The project no longer exists.")),
                Some((k, p)) if k != "folder" || p.as_deref().is_none_or(str::is_empty) => {
                    return Ok(Err("The project has no folder to start a chat in."))
                }
                // The same check a chat started by hand gets: the folder must still be there.
                Some((_, Some(p))) if !std::path::Path::new(&p).is_dir() => {
                    return Ok(Err("The project's folder no longer exists."))
                }
                Some(_) => {}
            }
            if let Some(g) = &svc.gates {
                if g.0.project_archived(tx, project)? {
                    return Ok(Err(
                        "The project is archived; restore it to start new chats.",
                    ));
                }
            }
            let (model, effort) = helm_chats::runtime_settings::new_chat_defaults(tx);
            helm_chats::store::create_chat_in(
                tx,
                project,
                model.as_deref(),
                Some(&effort),
                helm_core::now_ms(),
            )
            .map(Ok)
        })
        .map_err(|e| format!("Could not start a chat: {e:?}"))?
        .map_err(str::to_owned)?;
    let rev = svc.clock.as_ref().map_or(0, |c| c.next());
    if let Ok(Some(chat)) = svc
        .db
        .snapshot(|tx| helm_chats::store::get_chat(tx, &id, rev))
    {
        svc.events.emit(ChatEvent::Chat { chat }.to_app_event());
    }
    Ok(id)
}

/// Remove a chat a failed run started, so a refused send leaves no empty chat behind.
fn discard_chat(svc: &SchedulesService, id: &str) {
    let gone = svc.db.write(|tx| {
        tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [id])?;
        delete_in_tx(tx, id, DeleteRule::Archived, helm_core::now_ms()).map(|r| r.is_ok())
    });
    if !matches!(gone, Ok(true)) {
        log::warn!("schedules: could not remove the chat {id} after a refused send");
    }
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
        let by = c
            .created_by
            .as_ref()
            .map(|(t, id)| (t.as_str(), id.as_str()));
        let text = frame(&c.title, &c.schedule_id, &c.message, by);
        let started = c.chat_id.is_empty();
        let sent = if started {
            start_chat(svc, c.project_id.as_deref())
        } else {
            Ok(c.chat_id.clone())
        }
        .and_then(|chat| {
            sender
                .send(&chat, &c.submission_id, &text)
                .map(|()| chat.clone())
                .map_err(|e| {
                    if started {
                        discard_chat(svc, &chat);
                    }
                    e.message
                })
        });
        let (status, error, chat) = match sent {
            Ok(chat) => (RunStatus::Started, None, started.then_some(chat)),
            Err(message) => {
                log::info!(
                    "schedules: run of {} not delivered: {message}",
                    c.schedule_id
                );
                (RunStatus::Failed, Some(message), None)
            }
        };
        if let Err(e) = svc
            .db
            .write(|tx| store::finish_in(tx, c, status, error.as_deref(), chat.as_deref()))
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

/// Whether the next active run is due within [`WAKE_LEAD_MS`] of `now`. A run left unclaimed
/// (its chat busy) counts as soon: it is already due.
pub fn due_soon(svc: &SchedulesService, now: i64) -> bool {
    svc.db
        .snapshot(store::soonest_next_run)
        .ok()
        .flatten()
        .is_some_and(|next| next - now <= WAKE_LEAD_MS)
}

/// Tell the shell whether a run is coming due, but only when that changed since `last`; returns
/// the new state. The shell uses this to keep the machine awake ahead of a run under the
/// "while chats run" setting.
pub fn wake_check(svc: &SchedulesService, now: i64, last: bool) -> bool {
    let soon = due_soon(svc, now);
    if soon != last {
        svc.events
            .emit(AppEvent::new("schedules-wake").with("dueSoon", soon));
    }
    soon
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
        // Kept between ticks: whether the shell is being told to stay awake for a run. False at
        // the start, so the first tick reports the true state.
        let mut wake = false;
        loop {
            every.tick().await;
            let svc = svc.clone();
            let rt = runtime.clone();
            // SQLite and the send are blocking calls: keep them off the async workers.
            if let Ok(soon) = tokio::task::spawn_blocking(move || {
                let now = helm_core::now_ms();
                tick(&svc, &rt, now);
                wake_check(&svc, now, wake)
            })
            .await
            {
                wake = soon;
            }
        }
    });
}
