//! The speaker chats: the house's turns into them, and their answers back out to be spoken.
//!
//! **In.** [`HouseSpeaker`] takes each turn the hub forwards and puts it into the chat of that
//! turn's talk ([`crate::speaker::talk_chat`]), one chat per talk on one speaker, titled
//! `HA speaker <room> voice <date>`. It is stored through the chats crate's host send with origin
//! `speaker`, so it never counts as consent and an approval, plan or trust prompt is never
//! answered by it. Its submission id is fixed per turn id, so a turn delivered twice is stored
//! once, and it is acknowledged to the hub only once it is stored.
//!
//! **Out.** While a turn runs, a mirror task watches its chat and sends the working line when
//! the chat is thinking and once when it is waiting for something only Helm's window can answer.
//! The turn's answer goes up as soon as its end-of-turn row is in the transcript, cleaned and cut
//! for the ear ([`crate::speakable`]), as a `voice_final` carrying `"lane": "speaker"` so the hub
//! speaks it from the speaker that asked. A turn that stops at an approval, plan or trust prompt
//! says so once, in a sentence, and the answer follows when the turn ends: those are only ever
//! answered in Helm's window. A turn has exactly one answer; an answer the line will not take is
//! offered again for a while, then dropped with a line in the log.
//!
//! **When a talk ends.** The hub says a speaker has gone quiet ([`SpeakerSink::talk_closed`]):
//! that talk's chat is archived, and what was last said there is kept short so the next talk on
//! that speaker can be told about it while it is still recent.
//!
//! Settings → Hub's speaker section reads and writes this profile's policy through
//! [`speaker_view`], [`save`], and [`reset`]; the policy is re-read on every delivery, so a change
//! applies to the next turn.

use crate::client::{BoxFut, LineHandle, SpeakerSink, SpeakerTurn};
use crate::exec::AppExecutor;
use crate::sink::{max_row, plain_path, profile_name, snapshot, RealPort, Snap};
use crate::speaker::{self, Paths, PolicyFile, Talk, SPEAKER_ORIGIN};
use crate::telegram::cut_chars;
use crate::voice::{Mode, Reply};
use crate::voice_sink::{NOTHING_TO_SAY, NOT_STARTED, THINKING, WAITING_STATUS};
use helm_chats::submit::{host_send, HostSend};
use helm_chats::types::ChatStatus;
use helm_chats::ChatRuntime;
use helm_core::db::Database;
use helm_hosttools::OpInvoker;
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often a mirror looks at its chat.
const POLL: Duration = Duration::from_millis(500);
/// The speaker chats mirrored at once: a new talk leaves the oldest to finish its answer.
const MAX_MIRRORS: usize = 3;
/// A turn that has not produced an answer this long gets a sentence saying so, not silence.
const ANSWER_WAIT: Duration = Duration::from_secs(100);
/// How long an answer the line will not take is offered again before it is dropped.
const REPLY_RETRY: Duration = Duration::from_secs(20);
/// How long a delivery retries a chat that is stopping or full before leaving it to the hub.
const BUSY_RETRY: Duration = Duration::from_secs(20);
/// The chat must answer within this of the turn, or the hub has already given up on it.
const STARTING: &str = "Helm is still starting; the message will be delivered when Helm is ready.";

/// Where the speaker lane's working line and answers go: the line in the app, a recorder in tests.
pub trait SpeakerUpstream: Send + Sync {
    fn status(&self, person: &str, turn: &str, text: &str);
    /// `false` when the reply was not sent (the line is down); the mirror then offers it again.
    fn reply(&self, person: &str, reply: &Reply) -> bool;
}

impl SpeakerUpstream for LineHandle {
    fn status(&self, person: &str, turn: &str, text: &str) {
        self.speaker_status(person, turn, text)
    }
    fn reply(&self, person: &str, reply: &Reply) -> bool {
        self.speaker_reply(person, reply)
    }
}

/// What a delivery came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Placed {
    /// Stored as a `speaker`-origin submission, with the row its answer is read after.
    Sent { chat: String, baseline: i64 },
    /// The hub sent a turn this PC already stored and answered; nothing more to do.
    Already,
}

/// One profile's speaker world: what it needs to deliver a turn and to mirror its chats.
#[derive(Clone)]
pub struct Ctx {
    pub rt: ChatRuntime,
    pub db: Arc<Database>,
    pub ops: Arc<dyn OpInvoker>,
    pub handle: tokio::runtime::Handle,
    /// The local profile this exchange belongs to.
    pub profile_id: String,
    /// The hub person of that profile, or `""` when it is not linked yet.
    pub person: String,
    pub paths: Paths,
    /// This profile's speaker policy, re-read on every delivery.
    pub file: PolicyFile,
    /// The folder a new speaker chat starts in, which is what makes it a speaker chat.
    pub folder: String,
}

/// Put one turn into its talk's chat. Runs on a blocking thread: it reads the talk list and the
/// transcript and waits out a chat that is stopping.
pub fn place(c: &Ctx, turn: &SpeakerTurn, now: i64) -> Result<Placed, String> {
    let sid = speaker::submission_id(&turn.id);
    // Already stored (the hub's resend of a turn it has not seen acknowledged): its chat, whether
    // a turn runs there now, and the row its answer starts after.
    let stored: Option<(String, Option<String>, Option<i64>)> =
        c.db.snapshot(|tx| {
            tx.query_row(
                "SELECT s.chat_id, c.status,
                    (SELECT MAX(m.id) FROM messages m
                     WHERE m.chat_id = s.chat_id AND m.source_key = 'u:' || s.id)
             FROM submissions s LEFT JOIN chats c ON c.id = s.chat_id WHERE s.id = ?1",
                [&sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })
        .ok()
        .flatten();
    if let Some((chat, status, row)) = stored {
        let busy = status.is_some_and(|s| ChatStatus::parse(&s).is_busy());
        return Ok(if busy {
            // Watch it anyway: after a restart between storing it and the hub's acknowledgement,
            // nothing else would tell the speaker how that turn goes.
            let baseline = row.map_or_else(|| max_row(&c.db, &chat), |r| r - 1);
            Placed::Sent { chat, baseline }
        } else {
            Placed::Already
        });
    }
    let (model, effort) = c.file.new_chat();
    let port = RealPort {
        db: &c.db,
        rt: &c.rt,
        ops: &*c.ops,
        handle: &c.handle,
        folder: &c.folder,
        label: "speaker",
        new_chat_model: model.map(str::to_owned),
        new_chat_effort: effort.map(str::to_owned),
        ignored: std::sync::Mutex::new(None),
        profile_id: Some(c.profile_id.clone()),
    };
    let who = profile_name(&c.db, &c.profile_id);
    let (talk, _made) = speaker::talk_chat(
        &c.paths.chats,
        &port,
        &turn.session,
        turn.room.as_deref(),
        who.as_deref(),
        now,
    )?;
    let ignored = port
        .ignored
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    let baseline = max_row(&c.db, &talk.chat_id);
    let (said, _) = cut_chars(&turn.text, speaker::MAX_TEXT_CHARS);
    // A new talk may be told which talk came before it, unless this is that same talk.
    let previous = speaker::load_talks(&c.paths.chats)
        .previous
        .as_ref()
        .filter(|p| p.chat_id != talk.chat_id && speaker::previous_usable(p, &c.file, now))
        .map(speaker::previous_pointer);
    let mut text = speaker::delivered_text(
        turn.room.as_deref(),
        turn.area.as_deref(),
        previous.as_deref(),
        &said,
    );
    // The chat opened without the model saved for it: say so in the chat, where the agent and the
    // user can see it.
    if let Some(why) = &ignored {
        text.push_str(&format!("\n\n[Helm] {why}"));
    }
    let started = Instant::now();
    loop {
        let r = host_send(
            &c.rt,
            HostSend {
                chat_id: &talk.chat_id,
                submission_id: &sid,
                text: &text,
                origin: SPEAKER_ORIGIN,
                page: None,
            },
        );
        match r {
            Ok(_) => break,
            Err(e)
                if matches!(e.code.as_str(), "busy" | "capacity")
                    && started.elapsed() < BUSY_RETRY =>
            {
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => return Err(e.message),
        }
    }
    Ok(Placed::Sent {
        chat: talk.chat_id,
        baseline,
    })
}

/// What Settings → Hub's speaker section shows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerView {
    /// The talks open on this PC, oldest first.
    pub talks: Vec<TalkView>,
    /// The model and effort a new speaker chat opens with; `None`: the same as any other new chat.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Whether this PC takes its person's room speaker (the switch, or its default: on for the
    /// home profile, off for the others).
    pub take_speaker: bool,
    /// How long a finished talk may be named to the next one.
    pub previous_hours: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TalkView {
    /// The hub's id for this talk.
    pub session: String,
    pub chat_id: String,
    pub title: String,
    pub started_at: i64,
    pub room: Option<String>,
}

pub fn speaker_view(db: &Database, profile_dir: &std::path::Path, profile_id: &str) -> SpeakerView {
    let paths = speaker::paths(profile_dir, profile_id);
    let file = speaker::load_file(&paths.policy);
    let talks = db
        .snapshot(|tx| {
            Ok::<_, rusqlite::Error>(
                speaker::load_talks(&paths.chats)
                    .talks
                    .into_iter()
                    .map(|t| {
                        let title: Option<String> = tx
                            .query_row("SELECT title FROM chats WHERE id = ?1", [&t.chat_id], |r| {
                                r.get(0)
                            })
                            .optional()
                            .unwrap_or(None)
                            .flatten();
                        TalkView {
                            session: t.session,
                            title: title.unwrap_or_else(|| "Speaker".into()),
                            chat_id: t.chat_id,
                            started_at: t.started_at,
                            room: t.room,
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
    let (model, effort) = file.new_chat();
    let is_home = db
        .snapshot(|tx| helm_profiles::store::home(tx))
        .ok()
        .flatten()
        .is_some_and(|h| h.id == profile_id);
    SpeakerView {
        talks,
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        take_speaker: speaker::takes_speaker(&file, is_home),
        previous_hours: file.previous_hours,
    }
}

/// Save the speaker settings for a profile.
pub fn save(profile_dir: &std::path::Path, profile_id: &str, f: &PolicyFile) -> Result<(), String> {
    f.check()?;
    speaker::save_policy(&speaker::paths(profile_dir, profile_id).policy, f)
        .map_err(|_| "The speaker settings could not be saved.".to_owned())
}

/// The app's speaker sink. The executor's wiring gives it the chat runtime and database; the op
/// dispatcher ([`HouseSpeaker::attach_ops`]) and the line ([`HouseSpeaker::attach_line`]) arrive
/// at start.
pub struct HouseSpeaker {
    exec: Arc<AppExecutor>,
    ops: OnceLock<Arc<dyn OpInvoker>>,
    up: OnceLock<Arc<dyn SpeakerUpstream>>,
    profile_dir: PathBuf,
    /// One at a time: two turns naming a talk with no chat yet must not start two chats.
    serial: tokio::sync::Mutex<()>,
    /// The chats being mirrored, oldest first, at most [`MAX_MIRRORS`].
    watching: Mutex<Vec<(String, tokio::task::JoinHandle<()>)>>,
}

impl HouseSpeaker {
    pub fn new(exec: Arc<AppExecutor>, profile_dir: PathBuf) -> Self {
        HouseSpeaker {
            exec,
            ops: OnceLock::new(),
            up: OnceLock::new(),
            profile_dir,
            serial: tokio::sync::Mutex::new(()),
            watching: Mutex::new(Vec::new()),
        }
    }

    pub fn attach_ops(&self, ops: Arc<dyn OpInvoker>) {
        let _ = self.ops.set(ops);
    }

    /// Give the sink its way up the line.
    pub fn attach_line(&self, up: Arc<dyn SpeakerUpstream>) {
        let _ = self.up.set(up);
    }

    fn ctx(&self, person: Option<&str>) -> Result<Ctx, String> {
        let w = self.exec.wiring().ok_or(STARTING)?;
        let ops = self.ops.get().cloned().ok_or(STARTING)?;
        let rt = w
            .services
            .get::<ChatRuntime>()
            .ok_or("Helm's chats are not available.")?;
        let db = w
            .services
            .get::<Database>()
            .ok_or("Helm's database is not available.")?;
        crate::people::migrate(&self.profile_dir, &db);
        let profile = crate::people::profile_for(&db, person)?;
        Ok(self.ctx_of(&rt, &db, &ops, profile))
    }

    /// The context of a named profile id, for a talk already known to be this PC's.
    fn ctx_for_profile(&self, profile_id: &str) -> Result<Ctx, String> {
        let w = self.exec.wiring().ok_or(STARTING)?;
        let ops = self.ops.get().cloned().ok_or(STARTING)?;
        let rt = w
            .services
            .get::<ChatRuntime>()
            .ok_or("Helm's chats are not available.")?;
        let db = w
            .services
            .get::<Database>()
            .ok_or("Helm's database is not available.")?;
        crate::people::migrate(&self.profile_dir, &db);
        let profile = db
            .snapshot(|tx| helm_profiles::store::get(tx, profile_id))
            .ok()
            .flatten()
            .ok_or("This PC has no such profile.")?;
        Ok(self.ctx_of(&rt, &db, &ops, profile))
    }

    fn ctx_of(
        &self,
        rt: &Arc<ChatRuntime>,
        db: &Arc<Database>,
        ops: &Arc<dyn OpInvoker>,
        profile: helm_profiles::store::Profile,
    ) -> Ctx {
        let paths = speaker::paths(&self.profile_dir, &profile.id);
        let file = speaker::load_file(&paths.policy);
        Ctx {
            rt: (**rt).clone(),
            db: db.clone(),
            ops: ops.clone(),
            handle: tokio::runtime::Handle::current(),
            person: profile.hub_person_id.unwrap_or_default(),
            profile_id: profile.id,
            folder: plain_path(&paths.folder),
            paths,
            file,
        }
    }

    /// The profiles that could hold this talk: the person's, when the hub named one, else every
    /// profile of this PC. A close frame carries no person.
    fn candidate_profiles(&self, db: &Arc<Database>, person: Option<&str>) -> Vec<String> {
        match person {
            Some(p) if !p.is_empty() => db
                .snapshot(|tx| helm_profiles::store::profile_for_person(tx, p))
                .ok()
                .flatten()
                .map(|p| vec![p.id])
                .unwrap_or_default(),
            _ => db
                .snapshot(|tx| helm_profiles::store::list(tx))
                .ok()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.id)
                .collect(),
        }
    }

    /// Stop mirroring a chat: its talk is over, so no answer of its turn goes out anymore.
    fn stop_mirror(&self, chat: &str) {
        let mut w = self.watching.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(at) = w.iter().position(|(c, _)| c == chat) {
            w.remove(at).1.abort();
        }
    }

    /// Start (or carry on) the mirror of one talk's chat.
    fn watch(&self, c: &Ctx, chat: &str, baseline: i64, turn: &str) {
        let Some(up) = self.up.get().cloned() else {
            return;
        };
        let mut w = self.watching.lock().unwrap_or_else(|p| p.into_inner());
        w.retain(|(_, t)| !t.is_finished());
        if let Some(at) = w.iter().position(|(c, _)| c == chat) {
            w.remove(at).1.abort();
        }
        while w.len() >= MAX_MIRRORS {
            w.remove(0).1.abort();
        }
        let task = c.handle.spawn(mirror(
            c.rt.clone(),
            c.db.clone(),
            up,
            c.person.clone(),
            chat.to_owned(),
            baseline,
            turn.to_owned(),
        ));
        w.push((chat.to_owned(), task));
    }

    /// Start over, as Settings' speaker section asks: every talk this profile holds ends, its
    /// chat is put away, and its mirror stops. Returns how many talks ended.
    pub async fn reset(&self, profile_id: &str) -> Result<usize, String> {
        let c = self.ctx_for_profile(profile_id)?;
        let talks = speaker::take_all(&c.paths.chats);
        let n = talks.len();
        for t in talks {
            self.stop_mirror(&t.chat_id);
            let _ = c
                .ops
                .invoke("chat_archive", json!({ "chatId": t.chat_id }))
                .await;
        }
        Ok(n)
    }
}

impl SpeakerSink for HouseSpeaker {
    fn deliver<'a>(&'a self, turn: SpeakerTurn) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            let _guard = self.serial.lock().await;
            let c = self.ctx(turn.person.as_deref())?;
            let c2 = c.clone();
            let t2 = turn.clone();
            let placed = tokio::task::spawn_blocking(move || place(&c2, &t2, helm_core::now_ms()))
                .await
                .map_err(|_| "The delivery did not finish.".to_owned())??;
            match placed {
                Placed::Sent { chat, baseline } => {
                    // The chat is a speaker chat through its folder alone: that is what gives it
                    // the prompt that answers to be spoken.
                    self.watch(&c, &chat, baseline, &turn.id);
                    Ok(())
                }
                Placed::Already => Ok(()),
            }
        })
    }

    fn talk_closed<'a>(&'a self, person: Option<String>, session: String) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let Ok(w) = self.exec.wiring().ok_or(STARTING) else {
                return;
            };
            let Some(db) = w.services.get::<Database>() else {
                return;
            };
            for pid in self.candidate_profiles(&db, person.as_deref()) {
                let session = session.clone();
                let Ok(c) = self.ctx_for_profile(&pid) else {
                    continue;
                };
                let now = helm_core::now_ms();
                let db2 = c.db.clone();
                let chats = c.paths.chats.clone();
                let ended = tokio::task::spawn_blocking(move || -> Option<Talk> {
                    let talk = speaker::find(&chats, &session)?;
                    let (title, gist) = last_said(&db2, &talk.chat_id);
                    let keep = !gist.trim().is_empty();
                    speaker::end_talk(&chats, &session, now, &title, &gist, keep)
                })
                .await
                .unwrap_or(None);
                let Some(talk) = ended else { continue };
                // Its answer is no longer wanted: the talk is over, whatever the mirror was about
                // to say, and the chat is put away so the next talk starts a fresh one.
                self.stop_mirror(&talk.chat_id);
                let _ = c
                    .ops
                    .invoke("chat_archive", json!({ "chatId": talk.chat_id }))
                    .await;
                return;
            }
        })
    }
}

/// The talk's chat, as a new one may be told about it: its title and what was last said there.
fn last_said(db: &Database, chat: &str) -> (String, String) {
    db.snapshot(|tx| {
        let title: Option<String> = tx
            .query_row("SELECT title FROM chats WHERE id = ?1", [chat], |r| {
                r.get(0)
            })
            .optional()
            .unwrap_or(None)
            .flatten();
        let said: Option<String> = tx
            .query_row(
                "SELECT text FROM messages WHERE chat_id = ?1 AND role = 'assistant'
                 AND active = 1 AND text != '' AND status != 'error'
                 ORDER BY id DESC LIMIT 1",
                [chat],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        Ok::<_, rusqlite::Error>((title.unwrap_or_default(), said.unwrap_or_default()))
    })
    .unwrap_or_default()
}

/// Watch one speaker chat until its turn has an answer, and send it up once.
async fn mirror(
    rt: ChatRuntime,
    db: Arc<Database>,
    up: Arc<dyn SpeakerUpstream>,
    person: String,
    chat: String,
    baseline: i64,
    turn: String,
) {
    let began = Instant::now();
    let mut said_thinking = false;
    let mut said_waiting = false;
    let mut idle_seen = 0;
    while began.elapsed() < ANSWER_WAIT {
        let s = snapshot(&rt, &db, &chat, baseline, true);
        if s.blocked.is_some() {
            // Only Helm's window can answer an approval, a plan or a trust prompt: the speaker
            // is told that once, and the answer follows when the turn ends.
            if !said_waiting {
                up.status(&person, &turn, WAITING_STATUS);
                said_waiting = true;
            }
        } else if s.busy && !said_thinking {
            up.status(&person, &turn, THINKING);
            said_thinking = true;
        }
        let settled = s.turn_done.is_some() || s.stopped.is_some() || s.final_text.is_some();
        if settled {
            // The chat stays busy while a message the user queued opens the next turn, and its
            // last streamed row is only in the transcript a moment after it stopped: two idle
            // samples, then read it.
            if s.busy {
                idle_seen = 0;
            } else {
                idle_seen += 1;
            }
            if idle_seen >= 2 {
                let words = answer_of(&s);
                send(&up, &person, &turn, &words).await;
                return;
            }
        }
        tokio::time::sleep(POLL).await;
    }
    // The hub stops waiting on a turn long before this: say so in words rather than silence.
    send(&up, &person, &turn, NOT_STARTED).await;
}

/// What to speak for a settled exchange: where it stopped on an error, that in words — never the
/// half-answer the error cut short, and never the error row itself.
pub fn answer_of(s: &Snap) -> String {
    if let Some(stop) = &s.stopped {
        return stop.last_word();
    }
    if let Some((_, text)) = &s.turn_done {
        if !text.trim().is_empty() {
            return text.clone();
        }
    }
    if let Some(text) = &s.final_text {
        if !text.trim().is_empty() {
            return text.clone();
        }
    }
    NOTHING_TO_SAY.to_owned()
}

/// The one answer for this turn, cleaned for the ear, offered again while the line is down.
async fn send(up: &Arc<dyn SpeakerUpstream>, person: &str, turn: &str, words: &str) {
    let reply = Reply::from_text(
        speaker::submission_id(turn),
        Some(turn.to_owned()),
        Mode::Conversation,
        words,
        turn.to_owned(),
        helm_core::now_ms(),
    );
    let began = Instant::now();
    let mut sent = up.reply(person, &reply);
    while !sent && began.elapsed() < REPLY_RETRY {
        tokio::time::sleep(POLL).await;
        sent = up.reply(person, &reply);
    }
    if !sent {
        log::warn!("relay: a speaker answer could not be sent: the line is down");
    }
}
