//! The schedule store and scheduler against a real database with the chats tables, and the send
//! through a real chat runtime.

use crate::scheduler::{self, SchedulesService, Sender};
use crate::store::{self, NewSchedule, Update};
use crate::timing::{FixedZone, Zone};
use crate::types::*;
use helm_core::db::{Database, MigrationRegistry};
use helm_ipc::{ApiError, RecordingSink};
use std::sync::{Arc, Mutex};

const T0: i64 = 1_800_000_000_000;
const MIN: i64 = 60_000;
const ZONE: FixedZone = FixedZone(0);

fn registry() -> MigrationRegistry {
    let mut reg = MigrationRegistry::new();
    helm_chats::register_migrations(&mut reg);
    crate::register_migrations(&mut reg);
    reg
}

fn temp() -> (tempfile::TempDir, Arc<Database>) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("t.sqlite3"), &registry()).unwrap();
    (dir, Arc::new(db))
}

fn chat(db: &Database) -> String {
    db.write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, 1))
        .unwrap()
}

fn sql(db: &Database, q: &str, p: &[&dyn rusqlite::ToSql]) {
    db.write(|tx| tx.execute(q, p).map(|_| ())).unwrap();
}

fn interval(secs: u64) -> Timing {
    Timing::Interval {
        seconds: secs,
        start_at: None,
    }
}

fn new(request: &str, chat: &str, timing: Timing) -> NewSchedule {
    NewSchedule {
        request_id: request.into(),
        definition: Definition {
            title: " Check-in ".into(),
            message: "How is it going?".into(),
            timing,
            kind: ScheduleKind::Prompt,
            destination: Destination::Chat,
            chat_id: chat.into(),
            project_id: None,
            created_by_chat: None,
            template: None,
        },
    }
}

fn make(db: &Database, chat: &str, timing: Timing) -> Schedule {
    let rid = uuid::Uuid::new_v4().to_string();
    db.write(|tx| store::create(tx, &ZONE, T0, new(&rid, chat, timing)))
        .unwrap()
}

fn claim(db: &Database, now: i64) -> (Vec<store::Claim>, usize) {
    db.write(|tx| store::claim_due(tx, &ZONE, now)).unwrap()
}

fn get(db: &Database, id: &str) -> Schedule {
    db.snapshot(|tx| store::list(tx, true))
        .unwrap()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
}

fn runs(db: &Database, id: &str) -> Vec<Run> {
    db.snapshot(|tx| store::runs(tx, id)).unwrap()
}

#[test]
fn create_trims_the_title_and_sets_the_first_run() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(1800));
    assert_eq!(s.definition.title, "Check-in");
    assert_eq!(s.state, ScheduleState::Active);
    assert_eq!(s.next_run_at, Some(T0 + 30 * MIN));
    assert_eq!(s.revision, 1);
}

#[test]
fn create_is_idempotent_and_refuses_a_reused_request_id() {
    let (_d, db) = temp();
    let c = chat(&db);
    let a = db
        .write(|tx| store::create(tx, &ZONE, T0, new("r1", &c, interval(1800))))
        .unwrap();
    let b = db
        .write(|tx| store::create(tx, &ZONE, T0 + 5, new("r1", &c, interval(1800))))
        .unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(db.snapshot(|tx| store::list(tx, true)).unwrap().len(), 1);
    let other = db
        .write(|tx| store::create(tx, &ZONE, T0, new("r1", &c, interval(3600))))
        .unwrap_err();
    match other {
        store::SErr::Api(e) => assert_eq!(e.code, "conflict"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn create_refuses_bad_input_a_missing_or_archived_chat_and_a_past_time() {
    let (_d, db) = temp();
    let c = chat(&db);
    let try_make = |n: NewSchedule| db.write(|tx| store::create(tx, &ZONE, T0, n));
    assert!(try_make(new("a", &c, interval(59))).is_err());
    assert!(try_make(new("b", "no-such-chat", interval(60))).is_err());
    assert!(try_make(new("c", &c, Timing::Once { at: T0 - 1 })).is_err());
    let mut empty = new("d", &c, interval(60));
    empty.definition.message = "  ".into();
    assert!(try_make(empty).is_err());
    sql(&db, "UPDATE chats SET archived = 1 WHERE id = ?1", &[&c]);
    assert!(try_make(new("e", &c, interval(60))).is_err());
}

#[test]
fn there_can_be_at_most_256_live_schedules() {
    let (_d, db) = temp();
    let c = chat(&db);
    for i in 0..256 {
        sql(
            &db,
            "INSERT INTO schedules(id, request_key, definition, state, next_run_at, revision, created_at) \
             VALUES (?1, ?1, '{}', 'paused', NULL, 1, 1)",
            &[&format!("s{i}")],
        );
    }
    let e = db
        .write(|tx| store::create(tx, &ZONE, T0, new("z", &c, interval(60))))
        .unwrap_err();
    assert!(format!("{e:?}").contains("256"));
    // A cancelled one frees a place.
    sql(
        &db,
        "UPDATE schedules SET state = 'cancelled' WHERE id = 's0'",
        &[],
    );
    assert!(db
        .write(|tx| store::create(tx, &ZONE, T0, new("z", &c, interval(60))))
        .is_ok());
}

#[test]
fn an_update_needs_the_current_revision_and_bumps_it() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(1800));
    let stale = db.write(|tx| {
        store::update(
            tx,
            &ZONE,
            T0,
            &s.id,
            99,
            Update {
                title: Some("x".into()),
                ..Default::default()
            },
        )
    });
    assert!(stale.is_err());
    let ok = db
        .write(|tx| {
            store::update(
                tx,
                &ZONE,
                T0,
                &s.id,
                1,
                Update {
                    title: Some("New".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!((ok.definition.title.as_str(), ok.revision), ("New", 2));
}

#[test]
fn pause_resume_and_cancel() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(1800));
    let p = db
        .write(|tx| {
            store::update(
                tx,
                &ZONE,
                T0,
                &s.id,
                1,
                Update {
                    paused: Some(true),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(p.state, ScheduleState::Paused);
    // A paused schedule is not claimed, however overdue.
    assert!(claim(&db, T0 + 10 * 60 * MIN).0.is_empty());
    // Resuming after the due time moves it on, without replaying what passed.
    let later = T0 + 5 * 60 * MIN;
    let r = db
        .write(|tx| {
            store::update(
                tx,
                &ZONE,
                later,
                &s.id,
                2,
                Update {
                    paused: Some(false),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(r.state, ScheduleState::Active);
    assert!(r.next_run_at.unwrap() > later);
    let c1 = db.write(|tx| store::cancel(tx, &s.id, 3)).unwrap();
    assert_eq!((c1.state, c1.next_run_at), (ScheduleState::Cancelled, None));
    // Cancelling again is fine, changing is not.
    assert_eq!(
        db.write(|tx| store::cancel(tx, &s.id, 3)).unwrap().state,
        ScheduleState::Cancelled
    );
    assert!(db
        .write(|tx| store::update(
            tx,
            &ZONE,
            T0,
            &s.id,
            4,
            Update {
                paused: Some(false),
                ..Default::default()
            }
        ))
        .is_err());
}

#[test]
fn nothing_is_claimed_before_it_is_due() {
    let (_d, db) = temp();
    let c = chat(&db);
    make(&db, &c, interval(1800));
    assert!(claim(&db, T0 + 30 * MIN - 1).0.is_empty());
}

#[test]
fn missed_runs_coalesce_into_one_claim_and_the_next_slot_is_in_the_future() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(1800));
    let due = T0 + 30 * MIN;
    // The app was away for 5 slots and a bit.
    let now = due + 160 * MIN;
    let (claims, n) = claim(&db, now);
    assert_eq!((claims.len(), n), (1, 1));
    assert_eq!(claims[0].scheduled_at, due);
    let after = get(&db, &s.id);
    assert_eq!(after.next_run_at, Some(due + 180 * MIN));
    assert_eq!(runs(&db, &s.id).len(), 1);
    // A second tick in the same moment finds nothing.
    assert!(claim(&db, now + 1).0.is_empty());
}

#[test]
fn a_busy_chat_makes_the_run_wait_and_it_fires_once_when_idle() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(1800));
    let due = T0 + 30 * MIN;
    sql(
        &db,
        "UPDATE chats SET status = 'running' WHERE id = ?1",
        &[&c],
    );
    // Three ticks and many slots later, still busy: nothing claimed, nothing recorded.
    for k in 0..3 {
        assert!(claim(&db, due + k * 45 * MIN).0.is_empty());
    }
    assert!(runs(&db, &s.id).is_empty());
    assert_eq!(get(&db, &s.id).next_run_at, Some(due));
    // The turn ends: one run, for the slot that was waiting, and the schedule moves past now.
    sql(&db, "UPDATE chats SET status = 'idle' WHERE id = ?1", &[&c]);
    let now = due + 200 * MIN;
    let (claims, _) = claim(&db, now);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].scheduled_at, due);
    assert!(get(&db, &s.id).next_run_at.unwrap() > now);
}

#[test]
fn undelivered_submissions_also_make_the_chat_busy() {
    let (_d, db) = temp();
    let c = chat(&db);
    make(&db, &c, interval(60));
    sql(
        &db,
        "INSERT INTO submissions(id, chat_id, text, mode, state, created_at) VALUES ('u1', ?1, 'x', 'prompt', 'saved', 1)",
        &[&c],
    );
    assert!(claim(&db, T0 + 10 * MIN).0.is_empty());
    sql(
        &db,
        "UPDATE submissions SET state = 'delivered' WHERE id = 'u1'",
        &[],
    );
    assert_eq!(claim(&db, T0 + 10 * MIN).0.len(), 1);
}

#[test]
fn a_missing_or_archived_chat_blocks_the_run_and_pauses_the_schedule() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(60));
    sql(&db, "UPDATE chats SET archived = 1 WHERE id = ?1", &[&c]);
    let (claims, n) = claim(&db, T0 + 2 * MIN);
    assert_eq!((claims.len(), n), (0, 1));
    let r = runs(&db, &s.id);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].status, RunStatus::Blocked);
    assert_eq!(get(&db, &s.id).state, ScheduleState::Paused);
    // Gone entirely.
    let c2 = chat(&db);
    let s2 = make(&db, &c2, interval(60));
    sql(&db, "DELETE FROM chats WHERE id = ?1", &[&c2]);
    let _ = claim(&db, T0 + 4 * MIN);
    assert_eq!(runs(&db, &s2.id)[0].status, RunStatus::Blocked);
}

#[test]
fn a_slot_is_claimed_once_even_if_the_schedule_is_wound_back() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(60));
    let due = T0 + MIN;
    assert_eq!(claim(&db, due).0.len(), 1);
    // Something rewinds the schedule onto the slot it already claimed.
    sql(
        &db,
        "UPDATE schedules SET next_run_at = ?2 WHERE id = ?1",
        &[&s.id, &due],
    );
    let (claims, n) = claim(&db, due + 1);
    assert!(
        claims.is_empty(),
        "the slot already has a receipt, so nothing is sent again"
    );
    assert_eq!(n, 1);
    assert_eq!(runs(&db, &s.id).len(), 1);
    assert!(get(&db, &s.id).next_run_at.unwrap() > due);
}

#[test]
fn at_most_eight_occurrences_are_claimed_per_tick() {
    let (_d, db) = temp();
    for _ in 0..10 {
        let c = chat(&db);
        make(&db, &c, interval(60));
    }
    assert_eq!(claim(&db, T0 + 2 * MIN).0.len(), 8);
    assert_eq!(claim(&db, T0 + 2 * MIN).0.len(), 2);
}

#[test]
fn a_one_time_schedule_completes_after_its_run() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, Timing::Once { at: T0 + MIN });
    assert_eq!(claim(&db, T0 + MIN).0.len(), 1);
    let after = get(&db, &s.id);
    assert_eq!(
        (after.state, after.next_run_at),
        (ScheduleState::Completed, None)
    );
    assert!(claim(&db, T0 + 10 * MIN).0.is_empty());
}

#[test]
fn at_most_100_receipts_are_kept_per_schedule() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(60));
    for k in 1..=105 {
        assert_eq!(claim(&db, T0 + k * MIN).0.len(), 1, "run {k}");
    }
    let n: i64 = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT count(*) FROM schedule_runs WHERE schedule_id = ?1",
                [&s.id],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(n, store::KEEP_RUNS);
}

#[test]
fn recovery_settles_a_send_the_app_was_closed_in_the_middle_of() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(60));
    let landed = store::submission_id(&s.id, 1);
    let lost = store::submission_id(&s.id, 2);
    for (slot, sub) in [(1i64, &landed), (2, &lost)] {
        sql(
            &db,
            "INSERT INTO schedule_runs(schedule_id, scheduled_at, started_at, status, chat_id, submission_id) \
             VALUES (?1, ?2, 1, 'dispatching', ?3, ?4)",
            &[&s.id, &slot, &c, sub],
        );
    }
    sql(
        &db,
        "INSERT INTO submissions(id, chat_id, text, mode, state, created_at) VALUES (?1, ?2, 'x', 'prompt', 'delivered', 1)",
        &[&landed, &c],
    );
    assert_eq!(db.write(store::recover).unwrap(), 2);
    let r = runs(&db, &s.id);
    let status = |slot: i64| r.iter().find(|x| x.scheduled_at == slot).unwrap().status;
    assert_eq!(status(1), RunStatus::Started);
    assert_eq!(status(2), RunStatus::Uncertain);
}

/// A zone whose offset changes at an instant.
struct Switch(i64);
impl Zone for Switch {
    fn offset_secs(&self, utc_ms: i64) -> i32 {
        if utc_ms < self.0 {
            0
        } else {
            3600
        }
    }
}

#[test]
fn a_weekly_schedule_follows_a_clock_change() {
    let (_d, db) = temp();
    let c = chat(&db);
    let weekly = Timing::Weekly {
        hour: 12,
        minute: 0,
        days: vec![0, 1, 2, 3, 4, 5, 6],
    };
    // Created in winter time (+0). Daylight-saving starts a few hours from now (+1h).
    let now = T0;
    let flip = now + 3 * 3_600_000;
    let before = Switch(i64::MAX);
    let s = db
        .write(|tx| store::create(tx, &before, now, new("w", &c, weekly)))
        .unwrap();
    let old_next = s.next_run_at.unwrap();
    assert_eq!(
        old_next % 86_400_000,
        12 * 3_600_000,
        "12:00 at +0 is 12:00 UTC"
    );
    // Nothing moves while the offset at the next run is unchanged.
    assert_eq!(
        db.write(|tx| store::refresh_calendar(tx, &before, now))
            .unwrap(),
        0
    );
    // After the flip the same wall time is an hour earlier in UTC.
    let after = Switch(flip.min(old_next - 1));
    assert_eq!(
        db.write(|tx| store::refresh_calendar(tx, &after, now))
            .unwrap(),
        1
    );
    let moved = get(&db, &s.id).next_run_at.unwrap();
    assert_eq!(
        moved % 86_400_000,
        11 * 3_600_000,
        "12:00 at +1 is 11:00 UTC"
    );
    // Intervals are never re-timed by the clock.
    assert_eq!(
        db.write(|tx| store::refresh_calendar(tx, &after, now))
            .unwrap(),
        0
    );
}

#[test]
fn the_submission_id_is_fixed_per_slot_and_shaped_like_a_uuid() {
    let a = store::submission_id("s1", 100);
    assert_eq!(a, store::submission_id("s1", 100));
    assert_ne!(a, store::submission_id("s1", 101));
    assert_ne!(a, store::submission_id("s2", 100));
    let u = uuid::Uuid::parse_str(&a).unwrap();
    assert_eq!(u.get_version_num(), 5);
}

#[test]
fn the_prompt_names_the_schedule_and_carries_the_message() {
    let t = scheduler::frame("Morning [x]`", "abc", "Do the thing.", None);
    assert!(t.starts_with("[Scheduled prompt] \"Morning x\" (schedule abc)\n"));
    assert!(t.contains("not typed by the user"));
    assert!(!t.contains("another chat"));
    let from = scheduler::frame("T", "abc", "m", Some(("Planner [x]", "c9")));
    assert!(from.contains("created by another chat, \"Planner x\" (c9)"));
    assert!(t.ends_with("\n\nDo the thing."));
}

#[test]
fn the_send_is_schedule_origin_gated_and_never_steers() {
    let r = scheduler::request("c", "s", "t");
    assert_eq!(r.origin, helm_chats::types::SubmissionOrigin::Schedule);
    assert!(!r.origin.is_users_own(), "a timer never types for the user");
    assert!(r.gated);
    assert_eq!(r.mode, helm_chats::submit::HostSendMode::Queue);
    assert_eq!((r.chat_id, r.submission_id, r.text), ("c", "s", "t"));
}

struct Recording(
    Mutex<Vec<(String, String, String)>>,
    Mutex<Option<ApiError>>,
);
impl Sender for Recording {
    fn send(&self, chat: &str, sub: &str, text: &str) -> Result<(), ApiError> {
        self.0
            .lock()
            .unwrap()
            .push((chat.into(), sub.into(), text.into()));
        match self.1.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

fn svc(db: &Arc<Database>) -> (SchedulesService, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    (
        SchedulesService {
            db: db.clone(),
            zone: Arc::new(ZONE),
            events: sink.clone(),
            clock: None,
            gates: None,
        },
        sink,
    )
}

#[test]
fn a_tick_sends_the_framed_prompt_once_records_it_and_tells_the_pane() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(60));
    let (svc, sink) = svc(&db);
    let send = Recording(Mutex::new(vec![]), Mutex::new(None));
    assert_eq!(scheduler::tick(&svc, &send, T0 + MIN), 1);
    let sent = send.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, c);
    assert_eq!(sent[0].1, store::submission_id(&s.id, T0 + MIN));
    assert!(sent[0].2.contains("How is it going?"));
    assert_eq!(runs(&db, &s.id)[0].status, RunStatus::Started);
    assert_eq!(sink.kinds(), ["schedules"]);
    // Nothing due: nothing sent, no event.
    assert_eq!(scheduler::tick(&svc, &send, T0 + MIN + 5_000), 0);
    assert_eq!(send.0.lock().unwrap().len(), 1);
    assert_eq!(sink.kinds().len(), 1);
}

#[test]
fn a_refused_send_is_recorded_as_failed_with_a_short_error_and_not_retried() {
    let (_d, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(60));
    let (svc, _sink) = svc(&db);
    let send = Recording(
        Mutex::new(vec![]),
        Mutex::new(Some(ApiError::new("capacity", "x".repeat(500)))),
    );
    scheduler::tick(&svc, &send, T0 + MIN);
    let r = runs(&db, &s.id);
    assert_eq!(r[0].status, RunStatus::Failed);
    assert_eq!(r[0].error.as_ref().unwrap().chars().count(), 240);
    scheduler::tick(&svc, &send, T0 + MIN + 5_000);
    assert_eq!(send.0.lock().unwrap().len(), 1);
}

fn project_of(db: &Database, chat: &str) -> String {
    db.snapshot(|tx| {
        tx.query_row("SELECT project_id FROM chats WHERE id = ?1", [chat], |r| {
            r.get(0)
        })
    })
    .unwrap()
}

fn chat_count(db: &Database) -> i64 {
    db.snapshot(|tx| tx.query_row("SELECT count(*) FROM chats", [], |r| r.get(0)))
        .unwrap()
}

fn kinds(c: &str, kind: ScheduleKind, dest: Destination, project: Option<&str>) -> NewSchedule {
    let mut n = new(&uuid::Uuid::new_v4().to_string(), c, interval(60));
    n.definition.kind = kind;
    n.definition.destination = dest;
    n.definition.project_id = project.map(str::to_owned);
    n
}

#[test]
fn reminders_post_a_note_and_new_chat_runs_start_their_own_chat() {
    let (_d, db) = temp();
    let c = chat(&db);
    let project = project_of(&db, &c);
    let create = |n: NewSchedule| db.write(|tx| store::create(tx, &ZONE, T0, n));

    // Validation: a reminder may name no chat but not a missing one; a new-chat prompt needs a
    // folder project, and drops any chat it was given.
    assert!(create(kinds("", ScheduleKind::Reminder, Destination::Chat, None)).is_ok());
    assert!(create(kinds(
        "gone",
        ScheduleKind::Reminder,
        Destination::Chat,
        None
    ))
    .is_err());
    assert!(create(kinds("", ScheduleKind::Prompt, Destination::NewChat, None)).is_err());
    assert!(create(kinds(
        "",
        ScheduleKind::Prompt,
        Destination::NewChat,
        Some("nope")
    ))
    .is_err());
    assert!(create(kinds("", ScheduleKind::Prompt, Destination::Chat, None)).is_err());
    sql(
        &db,
        "INSERT INTO projects(id, kind, name, created_at) VALUES ('group', 'board', 'G', 1)",
        &[],
    );
    assert!(create(kinds(
        "",
        ScheduleKind::Prompt,
        Destination::NewChat,
        Some("group")
    ))
    .is_err());

    let note = create(kinds(
        &c,
        ScheduleKind::Reminder,
        Destination::NewChat,
        Some(&project),
    ))
    .unwrap();
    assert_eq!(
        note.definition.destination,
        Destination::Chat,
        "a reminder has no destination"
    );
    assert_eq!(note.definition.project_id, None);
    let mut made = kinds(
        &c,
        ScheduleKind::Prompt,
        Destination::NewChat,
        Some(&project),
    );
    made.definition.created_by_chat = Some(c.clone());
    let fresh = create(made).unwrap();
    assert_eq!(fresh.definition.chat_id, "");
    assert_eq!(fresh.chat_title, project_name(&db, &project));

    // Both fire at once even though the linked chat is busy: a reminder sends nothing, and a new
    // chat has no busy chat to wait for.
    sql(
        &db,
        "UPDATE chats SET status = 'running' WHERE id = ?1",
        &[&c],
    );
    let (svc, sink) = svc(&db);
    let send = Recording(Mutex::new(vec![]), Mutex::new(None));
    assert_eq!(scheduler::tick(&svc, &send, T0 + MIN), 3);
    let sent = send.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let started = &sent[0].0;
    assert_ne!(started, &c);
    assert!(
        sent[0].2.contains("created by another chat"),
        "{}",
        sent[0].2
    );
    assert_eq!(project_of(&db, started), project);
    assert_eq!(runs(&db, &fresh.id)[0].chat_id, *started);
    assert_eq!(runs(&db, &note.id)[0].status, RunStatus::Started);
    let pending = db.snapshot(store::reminders).unwrap();
    assert_eq!(
        pending.len(),
        2,
        "this one and the chat-less one from the checks above"
    );
    let linked = pending.iter().find(|r| r.chat_id.is_some()).unwrap();
    assert_eq!(
        (linked.title.as_str(), linked.chat_id.as_deref()),
        ("Check-in", Some(c.as_str()))
    );
    assert!(sink.kinds().contains(&"schedules".to_string()));

    // A refused send leaves no empty chat behind; reminders can be dismissed one by one or all.
    let before = chat_count(&db);
    *send.1.lock().unwrap() = Some(ApiError::new("capacity", "busy"));
    scheduler::tick(&svc, &send, T0 + 2 * MIN);
    assert_eq!(chat_count(&db), before);
    assert_eq!(runs(&db, &fresh.id)[0].status, RunStatus::Failed);
    assert_eq!(db.snapshot(store::reminders).unwrap().len(), 4);
    assert_eq!(
        db.write(|tx| store::dismiss_reminders(tx, Some(&linked.id)))
            .unwrap(),
        1
    );
    assert_eq!(
        db.write(|tx| store::dismiss_reminders(tx, None)).unwrap(),
        3
    );
    assert!(db.snapshot(store::reminders).unwrap().is_empty());

    // Updates move a schedule between kinds, and re-check the destination only when it changes.
    let moved = db
        .write(|tx| {
            store::update(
                tx,
                &ZONE,
                T0,
                &fresh.id,
                db_revision(tx, &fresh.id),
                Update {
                    destination: Some(Destination::Chat),
                    chat_id: Some(c.clone()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(
        (moved.definition.destination, moved.definition.project_id),
        (Destination::Chat, None)
    );
}

fn project_name(db: &Database, id: &str) -> String {
    db.snapshot(|tx| {
        tx.query_row("SELECT name FROM projects WHERE id = ?1", [id], |r| {
            r.get(0)
        })
    })
    .unwrap()
}

fn db_revision(tx: &rusqlite::Transaction<'_>, id: &str) -> i64 {
    tx.query_row("SELECT revision FROM schedules WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .unwrap()
}

// The send itself, through a real chat runtime.

struct NoWorker;
impl helm_chats::runtime::Spawner for NoWorker {
    fn spawn(
        &self,
        _req: &helm_chats::runtime::SpawnRequest,
    ) -> Result<helm_chats::runtime::Spawned, String> {
        Err("no worker in this test".into())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_send_is_a_schedule_origin_submission_stored_once_per_slot() {
    let (dir, db) = temp();
    let c = chat(&db);
    let s = make(&db, &c, interval(60));
    let sink = Arc::new(RecordingSink::default());
    let rt = helm_chats::ChatRuntime::new(
        db.clone(),
        Arc::new(helm_chats::clock::Clock::new(0)),
        sink.clone(),
        Arc::new(NoWorker),
        helm_chats::RuntimeConfig {
            profile_dir: dir.path().join("prof"),
            limits: helm_chats::runtime::Limits::default(),
        },
        tokio::runtime::Handle::current(),
    );
    let service = Arc::new(SchedulesService {
        db: db.clone(),
        zone: Arc::new(ZONE),
        events: sink,
        clock: None,
        gates: None,
    });
    let slot = T0 + MIN;
    let sid = store::submission_id(&s.id, slot);
    {
        let (service, rt) = (service.clone(), rt.clone());
        tokio::task::spawn_blocking(move || scheduler::tick(&service, &rt, slot))
            .await
            .unwrap();
    }
    let row = |db: &Database| -> Vec<(String, String, String)> {
        db.snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT s.id, COALESCE(m.origin, ''), s.text FROM submissions s \
                 LEFT JOIN submission_meta m ON m.submission_id = s.id WHERE s.chat_id = ?1",
            )?;
            let rows = st
                .query_map([&c], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .unwrap()
    };
    let rows = row(&db);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, sid);
    assert_eq!(rows[0].1, "schedule", "never the user's own send");
    assert!(rows[0].2.contains("How is it going?"));
    // The same slot sent again (a retry, a rewound schedule) is the same submission.
    sql(&db, "UPDATE chats SET status = 'idle' WHERE id = ?1", &[&c]);
    sql(&db, "DELETE FROM submissions WHERE id <> ?1", &[&sid]);
    let again = rt_send(&rt, &c, &sid, "again").await;
    assert!(again.is_ok());
    let rows = row(&db);
    assert_eq!(
        rows.len(),
        1,
        "a second send of the slot's id adds no submission"
    );
    assert!(
        rows[0].2.contains("How is it going?"),
        "and keeps the first text"
    );
}

async fn rt_send(
    rt: &helm_chats::ChatRuntime,
    chat: &str,
    sid: &str,
    text: &str,
) -> Result<(), ApiError> {
    let (rt, chat, sid, text) = (rt.clone(), chat.to_owned(), sid.to_owned(), text.to_owned());
    tokio::task::spawn_blocking(move || Sender::send(&rt, &chat, &sid, &text))
        .await
        .unwrap()
}
