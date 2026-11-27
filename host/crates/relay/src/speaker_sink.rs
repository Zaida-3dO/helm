//! The speaker chats: the house's turns into them, and their answers back out to be spoken.
//!
//! **In.** [`HouseSpeaker`] takes each turn the hub forwards and puts it into the chat of that
//! turn's talk ([`crate::speaker::talk_chat`]), one chat per talk on one speaker, titled
//! `HA speaker <room> voice <date>`. A turn says whether it wants the quick or the deep answer
//! (absent: deep): a new talk's chat opens on that lane's model, a later deep turn moves a quick
//! chat up to the deep model once, and when a talk's chat outgrows its lane the next turn arrives
//! in a fresh linked successor with a host-written carry-over note. It is stored through the chats crate's host send with origin
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
//! **Records.** An exchange the speaker answered by itself arrives as a record
//! ([`HouseSpeaker::record`]). It is written into its talk's chat as a plain transcript — the
//! question as a speaker message, a line on what was done, the spoken answer — with no model run,
//! stored once per record id and acknowledged to the hub once it is stored. Anything the record
//! says the person still has to act on becomes the chat's attention flag ("needs you"), which is
//! also what holds the chat open past the talk. A talk's first message, whether a record or a
//! turn, makes its chat; the one board setting for chats arriving from the hub (phone voice and
//! Telegram included) is where that chat is shown.
//!
//! **When a talk ends.** The hub says a speaker has gone quiet ([`SpeakerSink::talk_closed`]):
//! that talk's chat is archived, unless something still holds it open — an attention flag, a
//! pending approval, a queued message, a running turn or helper, a draft, a pin, the open window
//! ([`helm_chats::finished::keeps_open`]) — in which case it joins the profile's kept list and is
//! put away by itself once nothing holds it and it has been quiet for half an hour (checked on
//! every delivery and close, and every ten minutes). A message the person types in it makes it an
//! ordinary chat that never auto-archives. What was last said in a finished talk is kept short so
//! the next talk on that speaker can be told about it while it is still recent.
//!
//! Settings → Hub's speaker section reads and writes this profile's policy through
//! [`speaker_view`], [`save`], and [`reset`]; the policy is re-read on every delivery, so a change
//! applies to the next turn.

use crate::client::{BoxFut, LineHandle, SpeakerRecord, SpeakerSink, SpeakerTurn};
use crate::exec::AppExecutor;
use crate::sink::{max_row, plain_path, profile_name, snapshot, RealPort, RetryHold, Snap};
use crate::speaker::{self, Paths, PolicyFile, Talk, SPEAKER_ORIGIN};
use crate::telegram::{cut_chars, ChatPort};
use crate::voice::{Mode, Reply};
use crate::voice_sink::{NOTHING_TO_SAY, NOT_STARTED, THINKING, WAITING_STATUS};
use helm_chats::submit::{append_exchange, host_send, Exchange, HostSend};
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
    let lane = crate::voice::Lane::parse(turn.urgency.as_deref());
    let quick_lane = lane == crate::voice::Lane::Quick && c.file.has_quick_lane();
    let (model, effort) = c.file.new_chat(lane);
    let mut port = RealPort {
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
        board_id: c.file.board().map(str::to_owned),
    };
    let who = profile_name(&c.db, &c.profile_id);
    let (mut talk, made) = speaker::talk_chat(
        &c.paths.chats,
        &port,
        &turn.session,
        turn.room.as_deref(),
        who.as_deref(),
        now,
        quick_lane,
    )?;
    let ignored = port
        .ignored
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    // A deep turn on a chat that opened on the quick model moves that one chat up to the deep
    // model (upgrade only, once per talk). A refused change is logged, not fatal: the turn goes
    // to the chat as it stands.
    if !made && lane == crate::voice::Lane::Deep && talk.quick {
        let mut args = json!({ "chatId": talk.chat_id });
        if let Some(m) = &model {
            args["model"] = json!(m);
        }
        if let Some(e) = &effort {
            args["effort"] = json!(e);
        }
        if args.get("model").is_some() || args.get("effort").is_some() {
            match c.handle.block_on(c.ops.invoke("chat_set_model", args)) {
                Ok(_) => {
                    speaker::mark_deep(&c.paths.chats, &turn.session);
                    talk.quick = false;
                }
                Err(e) => log::warn!(
                    "relay: the speaker chat could not move to its deep model: {}",
                    e.message
                ),
            }
        }
    }
    // The talk's chat has outgrown its lane: a fresh chat in the same lane, linked as its
    // successor, with this turn's words carried over in a host-written note. No model turn, so
    // there is no race with the next utterance — but only when the chat is idle: a roll archives
    // it and stops what runs there, so an answer in flight (or an ask waiting on it) would be
    // dropped mid-turn; that turn goes to the old chat and the next quiet moment rolls. The
    // talk's own lane decides the size and the fresh chat, not this turn: a quick utterance
    // landing in a deep talk must not size its chat against the quick limit, nor open its
    // successor on the quick model.
    let mut carry = None;
    if !made {
        let old = talk.chat_id.clone();
        let talk_lane = if talk.quick {
            crate::voice::Lane::Quick
        } else {
            crate::voice::Lane::Deep
        };
        let size =
            c.db.snapshot(|tx| {
                Ok::<_, rusqlite::Error>(helm_chats::turn_usage::context_reported_at(tx, &old))
            })
            .ok()
            .flatten();
        let first = crate::sink::first_context(&c.db, &old);
        // A failed read counts as "holds": no roll without proof the chat is idle.
        let idle =
            c.db.snapshot(|tx| {
                Ok::<_, rusqlite::Error>(
                    helm_chats::finished::keeps_open(tx, &old, None)?.is_none(),
                )
            })
            .unwrap_or(false);
        if crate::voice::roll_due(size.map(|(n, _)| n), first, c.file.roll_at(talk_lane)) && idle {
            let (m2, e2) = c.file.new_chat(talk_lane);
            port.new_chat_model = m2.map(str::to_owned);
            port.new_chat_effort = e2.map(str::to_owned);
            let title =
                c.db.snapshot(|tx| {
                    Ok::<String, rusqlite::Error>(
                        tx.query_row("SELECT title FROM chats WHERE id = ?1", [&old], |r| {
                            r.get::<_, String>(0)
                        })
                        .optional()
                        .unwrap_or_default()
                        .unwrap_or_default(),
                    )
                })
                .unwrap_or_default();
            let exchanges = crate::sink::last_exchanges(&c.db, &old);
            let pairs: Vec<(&str, &str)> =
                exchanges.iter().map(|(r, t)| (*r, t.as_str())).collect();
            match port.create(&speaker::chat_title(
                turn.room.as_deref(),
                who.as_deref(),
                now,
            )) {
                Ok(new_id) => {
                    if let Err(e) = helm_chats::rollover::link_successor(&c.rt, &old, &new_id) {
                        log::warn!("relay: the speaker chat roll-over could not link {old}: {e}");
                    } else {
                        carry = Some(crate::voice::carry_note(&title, &pairs));
                        talk.chat_id = new_id.clone();
                        if !speaker::retalk(&c.paths.chats, &turn.session, &new_id, talk.quick) {
                            log::warn!(
                                "relay: the speaker talk could not follow its own roll-over"
                            );
                        }
                    }
                }
                Err(e) => log::warn!("relay: the speaker roll-over started no fresh chat: {e}"),
            }
        }
    }
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
        turn.context.as_deref(),
        &said,
    );
    if let Some(note) = &carry {
        // What the rolled-over chat was saying, in front of the new words.
        text = format!("{note}\n{text}");
    }
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

                kind: None,
                wait: None,
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

/// Write one recorded exchange into its talk's chat. Runs on a blocking thread, like [`place`].
/// The same record twice (the hub sends it again until it is acknowledged) writes nothing more.
pub fn place_record(c: &Ctx, rec: &SpeakerRecord, now: i64) -> Result<(), String> {
    // A record carries no urgency: a talk that starts with one opens on the deep lane; an
    // existing talk's chat (quick or deep) is joined as it stands.
    let (model, effort) = c.file.new_chat(crate::voice::Lane::Deep);
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
        board_id: c.file.board().map(str::to_owned),
    };
    let who = profile_name(&c.db, &c.profile_id);
    let (talk, _made) = speaker::talk_chat(
        &c.paths.chats,
        &port,
        &rec.session,
        rec.room.as_deref(),
        who.as_deref(),
        now,
        false,
    )?;
    if !rec.leftover.is_empty() {
        speaker::mark_leftover(&c.paths.chats, &rec.session);
        // The items the speaker could not finish become the chat's flag, so they show up under
        // "needs you" and hold the chat open when its talk ends (an agent or a later record may
        // already have filed a flag; this one replaces it).
        let reason = cut_chars(&rec.leftover.join("; "), 500).0;
        let question = "Left over from the speaker";
        let filed =
            c.db.write(|tx| {
                helm_chats::flags::file(tx, &talk.chat_id, question, &reason, "normal", now)
            })
            .ok()
            .flatten();
        if filed.is_some_and(|f| f != helm_chats::flags::Filed::Duplicate) {
            c.rt.publish_attention();
        }
    }
    let (asked, _) = cut_chars(&rec.text, speaker::MAX_TEXT_CHARS);
    let (said, _) = cut_chars(&rec.answer, speaker::MAX_TEXT_CHARS);
    let text =
        speaker::delivered_text(rec.room.as_deref(), rec.area.as_deref(), None, None, &asked);
    let notice = speaker::record_notice(rec);
    let sid = speaker::record_submission_id(&rec.id);
    // The exchange keeps the speaker's own time when it is sane, never one in the future.
    let at = if rec.started_at > 0 {
        rec.started_at.min(now)
    } else {
        now
    };
    append_exchange(
        &c.rt,
        Exchange {
            chat_id: &talk.chat_id,
            submission_id: &sid,
            text: &text,
            origin: SPEAKER_ORIGIN,
            notice: notice.as_deref(),
            reply: &said,
            at,
        },
    )
    .map_err(|e| e.message)?;
    Ok(())
}

/// How long a kept chat must have been quiet, with nothing holding it, before it is put away.
const KEEP_QUIET_MS: i64 = 30 * 60_000;
/// The interval of the sweep over kept chats (deliveries and closes also run it).
const KEEP_SWEEP: Duration = Duration::from_secs(600);
/// How long a close waits for the talk's last turn to settle before deciding keep-or-archive.
const CLOSE_SETTLE: Duration = Duration::from_secs(4);

/// Whether the person typed into this chat after `since` — a message of their own, as opposed to
/// another channel delivery or a card answer.
fn person_typed(db: &Database, chat: &str, since: i64) -> bool {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM submissions s
             JOIN submission_meta m ON m.submission_id = s.id
             WHERE s.chat_id = ?1 AND s.created_at > ?2 AND m.origin IN ('typed', 'user'))",
            rusqlite::params![chat, since],
            |r| r.get::<_, bool>(0),
        )
        .or(Ok::<bool, rusqlite::Error>(false))
    })
    .unwrap_or(false)
}

/// The chat the person has open in the window (an open chat is never swept away).
fn window_open_chat(tx: &rusqlite::Transaction<'_>) -> Option<String> {
    helm_core::prefs::read(tx, "selection")?
        .get("chatId")?
        .as_str()
        .map(str::to_owned)
}

/// Decide what to do now with the chats this profile's ended talks left behind: unkeep the ones
/// that are gone or that the person took over (a typed message makes an ordinary chat, never
/// auto-archived), and name the ones to put away — nothing holds them any more
/// ([`helm_chats::finished::keeps_open`]) and they have been quiet past [`KEEP_QUIET_MS`] since
/// their talk closed. The caller archives the named chats and unkeeps each once that lands, so a
/// refusal is simply tried again next time.
fn kept_ready(c: &Ctx, now: i64) -> Vec<String> {
    let talks = speaker::load_talks(&c.paths.chats);
    if talks.kept.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for k in &talks.kept {
        let gone =
            c.db.snapshot(|tx| {
                tx.query_row(
                    "SELECT archived FROM chats WHERE id = ?1",
                    [&k.chat_id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
            })
            .map(|found| found.is_none_or(|a| a != 0))
            .unwrap_or(true);
        if gone || person_typed(&c.db, &k.chat_id, k.closed_at) {
            speaker::unkeep_chat(&c.paths.chats, &k.chat_id);
            continue;
        }
        let holding =
            c.db.snapshot(|tx| {
                let open = window_open_chat(tx);
                helm_chats::finished::keeps_open(tx, &k.chat_id, open.as_deref())
            })
            .ok()
            .flatten();
        if holding.is_some() {
            continue;
        }
        let stamp =
            c.db.snapshot(|tx| {
                tx.query_row(
                    "SELECT MAX(c.updated_at, COALESCE(
                        (SELECT a.last_activity_at FROM chat_activity a WHERE a.chat_id = c.id), 0))
                     FROM chats c WHERE c.id = ?1",
                    [&k.chat_id],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap_or(now);
        if now - stamp.max(k.closed_at) < KEEP_QUIET_MS {
            continue;
        }
        out.push(k.chat_id.clone());
    }
    out
}

/// What Settings → Hub's speaker section shows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerView {
    /// The talks open on this PC, oldest first.
    pub talks: Vec<TalkView>,
    /// The model and effort a new speaker chat opens with; blank: the same as any other new
    /// chat. Plain strings so the settings form can hand them straight back on the next save.
    pub model: String,
    pub effort: String,
    /// Whether this PC takes its person's room speaker (the switch, or its default: on for the
    /// home profile, off for the others).
    pub take_speaker: bool,
    /// How long a finished talk may be named to the next one.
    pub previous_hours: i64,
    /// Whether speaker chats run lean (a thin prompt, few tools).
    pub lean: bool,
    /// The board chats that arrive from the hub are shown on; blank: the app's own choice. One
    /// setting for every channel — phone voice, room speakers and Telegram alike.
    pub board_id: String,
    /// The model a quick turn opens its talk's chat with; blank: no quick lane — quick turns open
    /// like deep ones.
    pub quick_model: String,
    /// The effort a quick turn opens its talk's chat with; blank: the same as any other new chat.
    pub quick_effort: String,
    /// The context size at which each lane's chat rolls over when the next turn arrives; 0 is off.
    pub quick_roll_at: u64,
    pub deep_roll_at: u64,
    /// Talks kept open past their close because something was held in them.
    pub kept: Vec<KeptView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptView {
    pub chat_id: String,
    pub title: String,
    /// When the talk ended (epoch milliseconds).
    pub closed_at: i64,
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
    let is_home = db
        .snapshot(|tx| helm_profiles::store::home(tx))
        .ok()
        .flatten()
        .is_some_and(|h| h.id == profile_id);
    let talks_file = speaker::load_talks(&paths.chats);
    let kept = db
        .snapshot(|tx| {
            Ok::<_, rusqlite::Error>(
                talks_file
                    .kept
                    .iter()
                    .map(|k| {
                        let title: Option<String> = tx
                            .query_row("SELECT title FROM chats WHERE id = ?1", [&k.chat_id], |r| {
                                r.get(0)
                            })
                            .optional()
                            .unwrap_or(None)
                            .flatten();
                        KeptView {
                            chat_id: k.chat_id.clone(),
                            title: title.unwrap_or_else(|| "Speaker".into()),
                            closed_at: k.closed_at,
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
    SpeakerView {
        talks,
        model: file.model.trim().to_owned(),
        effort: file.effort.trim().to_owned(),
        take_speaker: speaker::takes_speaker(&file, is_home),
        previous_hours: file.previous_hours,
        lean: file.lean,
        board_id: file.board().unwrap_or_default().to_owned(),
        quick_model: file.quick_model.trim().to_owned(),
        quick_effort: file.quick_effort.trim().to_owned(),
        quick_roll_at: file.quick_roll_at,
        deep_roll_at: file.deep_roll_at,
        kept,
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
    /// Whether the kept-chat interval task has been started (lazily, on the first delivery or
    /// close, once ops and a runtime are in place).
    sweeper: std::sync::atomic::AtomicBool,
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
            sweeper: std::sync::atomic::AtomicBool::new(false),
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
        build_ctx(rt, db, ops, &self.profile_dir, profile)
    }

    /// Sweep one profile's kept chats (the ones ended talks left behind) right now: those with
    /// nothing holding them and quiet past [`KEEP_QUIET_MS`] are archived here; a refusal is
    /// tried again at the next tick. Returns how many went away.
    async fn sweep_kept(&self, c: &Ctx) -> usize {
        let now = helm_core::now_ms();
        let cc = c.clone();
        let ready = tokio::task::spawn_blocking(move || kept_ready(&cc, now))
            .await
            .unwrap_or_default();
        let mut done = 0;
        for id in ready {
            match c.ops.invoke("chat_archive", json!({ "chatId": &id })).await {
                Ok(_) => {
                    speaker::unkeep_chat(&c.paths.chats, &id);
                    done += 1;
                }
                Err(e) => log::warn!(
                    "relay: a kept speaker chat could not be put away: {}",
                    e.message
                ),
            }
        }
        done
    }

    /// Start the interval watch over kept chats, once, when ops and a runtime are in place.
    fn start_sweeper(&self) {
        if self.sweeper.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let Some(ops) = self.ops.get().cloned() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.sweeper.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        handle.spawn(sweeper_loop(
            self.exec.clone(),
            self.profile_dir.clone(),
            ops,
        ));
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
    /// chat is put away, and its mirror stops — including the chats older talks left behind.
    /// Returns how many talks ended.
    pub async fn reset(&self, profile_id: &str) -> Result<usize, String> {
        let c = self.ctx_for_profile(profile_id)?;
        // The kept list first: `take_all` clears the whole file.
        let kept = speaker::take_kept(&c.paths.chats);
        let talks = speaker::take_all(&c.paths.chats);
        let n = talks.len();
        for k in kept {
            let _ = c
                .ops
                .invoke("chat_archive", json!({ "chatId": k.chat_id }))
                .await;
        }
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

/// The per-profile context, outside `HouseSpeaker` so the interval sweep can build it too.
fn build_ctx(
    rt: &Arc<ChatRuntime>,
    db: &Arc<Database>,
    ops: &Arc<dyn OpInvoker>,
    profile_dir: &std::path::Path,
    profile: helm_profiles::store::Profile,
) -> Ctx {
    let paths = speaker::paths(profile_dir, &profile.id);
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

/// The kept-chat watch: every [`KEEP_SWEEP`], for every profile on this PC, put away the chats
/// ended talks left behind once nothing holds them and they have gone quiet.
async fn sweeper_loop(exec: Arc<AppExecutor>, profile_dir: PathBuf, ops: Arc<dyn OpInvoker>) {
    let mut every = tokio::time::interval(KEEP_SWEEP);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let Some(w) = exec.wiring() else { continue };
        let (Some(rt), Some(db)) = (
            w.services.get::<ChatRuntime>(),
            w.services.get::<Database>(),
        ) else {
            continue;
        };
        let profiles = db
            .snapshot(|tx| helm_profiles::store::list(tx))
            .ok()
            .unwrap_or_default();
        for p in profiles {
            let c = build_ctx(&rt, &db, &ops, &profile_dir, p);
            if speaker::load_talks(&c.paths.chats).kept.is_empty() {
                continue;
            }
            let now = helm_core::now_ms();
            let cc = c.clone();
            let ready = match tokio::task::spawn_blocking(move || kept_ready(&cc, now)).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            for id in ready {
                if ops
                    .invoke("chat_archive", json!({ "chatId": &id }))
                    .await
                    .is_ok()
                {
                    speaker::unkeep_chat(&c.paths.chats, &id);
                }
            }
        }
    }
}

impl SpeakerSink for HouseSpeaker {
    fn deliver<'a>(&'a self, turn: SpeakerTurn) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            let _guard = self.serial.lock().await;
            self.start_sweeper();
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
                }
                Placed::Already => {}
            }
            // The same look as the interval task: a kept chat whose holds have cleared usually
            // goes away on the next delivery rather than at the next tick.
            self.sweep_kept(&c).await;
            Ok(())
        })
    }

    fn record<'a>(&'a self, rec: SpeakerRecord) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            let _guard = self.serial.lock().await;
            self.start_sweeper();
            let c = self.ctx(rec.person.as_deref())?;
            let cc = c.clone();
            tokio::task::spawn_blocking(move || place_record(&cc, &rec, helm_core::now_ms()))
                .await
                .map_err(|_| "The delivery did not finish.".to_owned())??;
            self.sweep_kept(&c).await;
            Ok(())
        })
    }

    fn talk_closed<'a>(&'a self, person: Option<String>, session: String) -> BoxFut<'a, ()> {
        Box::pin(async move {
            self.start_sweeper();
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
                // to say. The chat is put away so the next talk starts a fresh one — unless
                // something still holds it ([`helm_chats::finished::keeps_open`]), in which case
                // it joins the kept list and its own quiet sweep puts it away later.
                self.stop_mirror(&talk.chat_id);
                let db3 = c.db.clone();
                let chat = talk.chat_id.clone();
                let started = talk.started_at;
                let (holding, typed) = tokio::task::spawn_blocking(move || {
                    let ask = || {
                        db3.snapshot(|tx| {
                            let open = window_open_chat(tx);
                            helm_chats::finished::keeps_open(tx, &chat, open.as_deref())
                        })
                        .ok()
                        .flatten()
                    };
                    let mut holding = ask();
                    // A talk that closes right after its last turn: give a running turn or a
                    // message still on its way a few seconds to settle, so the common close
                    // just after the final answer still archives. Anything deeper-holding is a
                    // real leftover and waits in the kept list instead.
                    let began = Instant::now();
                    while matches!(
                        holding,
                        Some("a turn still running") | Some("a message not delivered yet")
                    ) && began.elapsed() < CLOSE_SETTLE
                    {
                        std::thread::sleep(Duration::from_millis(150));
                        holding = ask();
                    }
                    // Did the person type into this chat during the talk? Then it is theirs —
                    // an ordinary chat, not one the channel may archive or sweep.
                    let typed = person_typed(&db3, &chat, started);
                    (holding, typed)
                })
                .await
                .unwrap_or((None, false));
                if holding.is_some() || talk.leftover {
                    speaker::keep_chat(&c.paths.chats, &talk.chat_id, now);
                } else if typed {
                    log::debug!(
                        "relay: a speaker chat the person typed into stays an ordinary chat"
                    );
                } else {
                    let _ = c
                        .ops
                        .invoke("chat_archive", json!({ "chatId": talk.chat_id }))
                        .await;
                }
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
    // Only this exchange's own rows prove the turn is under way. A chat left `interrupted` by
    // its previous turn (the local server down, say) carries that stop until the new turn
    // starts: counting it as this turn's would speak the old error and never the answer.
    let mut started = false;
    let mut retry = RetryHold::default();
    let mut said_retrying = false;
    while began.elapsed() < ANSWER_WAIT {
        let s = snapshot(&rt, &db, &chat, baseline, true);
        if s.busy {
            started = true;
            retry.reset();
            said_retrying = false;
        }
        let begun_turn = started || s.max_id > baseline;
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
        if !s.busy && begun_turn && retry.holds(&s, true, helm_core::now_ms()) {
            // Helm carries a stopped turn on by itself: say the retry line once and keep
            // waiting for the continued turn's answer, instead of speaking the stop as final.
            if !said_retrying {
                if let Some(line) = s.stopped.as_ref().and_then(|st| st.status()) {
                    up.status(&person, &turn, line);
                }
                said_retrying = true;
            }
            tokio::time::sleep(POLL).await;
            continue;
        }
        // Settled once this exchange has something to say: its answer, or — once its own rows
        // are in — its stop.
        let settled =
            s.turn_done.is_some() || s.final_text.is_some() || (begun_turn && s.stopped.is_some());
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

/// What to speak for a settled exchange: the turn's answer if its transcript row already has
/// one, then the last streamed words, and only then where it stopped on an error — an answer
/// already written goes up before a stop, as the phone mirror sends its unsent answers first.
/// Never the half-answer the error cut short, and never the error row itself.
pub fn answer_of(s: &Snap) -> String {
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
    if let Some(stop) = &s.stopped {
        return stop.last_word();
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
