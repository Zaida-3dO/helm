//! The room speaker's side of the line: its switch, its limits, its talks and the words it is
//! given.
//!
//! A speaker is a Home Assistant voice satellite in a room. The hub hears the audio and sends
//! the words here as a turn, marked with the room and with the hub's id for one talk on that
//! speaker (`session`). Each talk is its own chat, found through a small tag file in the profile
//! (`relay/people/<profileId>/speaker-chats.json`), and the hub says when a talk ends, which
//! archives that chat: what the speaker asks next starts a new one. This differs from the
//! phone's voice chat, which is one long chat that ages out on its own.
//!
//! - **Origin.** A speaker message enters its chat with [`SPEAKER_ORIGIN`], never as typed, so
//!   it never counts as consent and no approval, plan or trust prompt is answered by it.
//! - **The policy** ([`PolicyFile`], `relay/people/<profileId>/speaker-policy.json`) is read
//!   again on every delivery: the model and effort a new speaker chat opens with (the sink hands
//!   them at the first send), whether this PC takes its person's speaker at all, the board a new
//!   speaker chat is shown on, and whether speaker chats run lean (a thin prompt and few tools).
//! - **Records.** An exchange the speaker answered by itself arrives as a record. It is written
//!   into its talk's chat as a plain transcript (origin [`SPEAKER_ORIGIN`]) with no model run,
//!   and a talk that left something open ([`Talk::leftover`]) is not archived when it ends.
//! - **The words.** How the answer is heard comes from the chat's system prompt (the `channel`
//!   section, [`crate::channel`]); every message carries a one-line note naming the room it came
//!   from, which the transcript hides, and a new talk may be told which talk came before it.
//! - **The reply** is the phone voice lane's [`crate::voice::Reply`] frame with
//!   `"lane": "speaker"` added, so the hub answers the speaker that asked and never the phone.

use crate::telegram::{civil_date, clean_title_name, write_private, ChatPort};
use helm_chats::types::SubmissionOrigin;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The origin every speaker message is stored with.
pub const SPEAKER_ORIGIN: SubmissionOrigin = SubmissionOrigin::Speaker;

const MINUTE_MS: i64 = 60 * 1000;
#[cfg(test)]
const HOUR_MS: i64 = 60 * MINUTE_MS;

/// The settings ranges Settings accepts.
pub const PREVIOUS_HOURS: std::ops::RangeInclusive<i64> = 0..=72;
/// Talks remembered at once: a hub that stopped talking to this PC leaves its talks tagged, and
/// the oldest are dropped with their chats left as they are.
pub const TALKS_KEPT: usize = 32;

/// The longest message taken from the hub.
pub const MAX_TEXT_CHARS: usize = 8_000;
/// How much of the talk before is quoted to a new one.
pub const PREVIOUS_NOTE_MAX_CHARS: usize = 300;
/// The longest lean-prompt replacement Settings accepts, in characters.
pub const MAX_LEAN_PROMPT_CHARS: usize = 6_000;
/// The most entries in a lean tool list, and the longest one entry may be.
pub const MAX_LEAN_TOOLS: usize = 24;
pub const MAX_LEAN_TOOL_CHARS: usize = 120;
/// How much of what the speaker said just before a handed-over turn is kept.
pub const MAX_CONTEXT_CHARS: usize = 2_048;

/// The model a new speaker chat opens with, until the person picks another in Settings: a small
/// local model, because a spoken answer has to come back in seconds.
pub const DEFAULT_MODEL: &str = "local/strata/qwen3.8-flash-next-iq3_s";

/// The speaker's limits and what new speaker chats open with, as stored and as Settings shows
/// them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyFile {
    /// The model a new speaker chat opens with; blank: the app's usual new-chat model.
    #[serde(default = "default_model")]
    pub model: String,
    /// The effort a new speaker chat opens with; blank: the app's usual new-chat effort.
    #[serde(default = "default_effort")]
    pub effort: String,
    /// Whether this PC takes its person's room speaker ("Take the house speaker on this PC").
    /// `None`: the default — the home profile takes it, every other profile starts off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_speaker: Option<bool>,
    /// How long a talk that just ended may be named to the next one.
    #[serde(default = "default_previous")]
    pub previous_hours: i64,
    /// Lean speaker chats: a short prompt of its own and a small tool set (see
    /// [`crate::lean`]). A chat started under another setting keeps what it started with.
    #[serde(default)]
    pub lean: bool,
    /// The system prompt a lean speaker chat gets; blank: the built-in speaker prompt. Lean mode
    /// only; the ordinary channel prompt still shapes non-lean speaker chats.
    #[serde(default)]
    pub lean_prompt: String,
    /// What a lean speaker chat may use: host-tool names and MCP server names or ids (the Home
    /// Assistant server is matched by its preset). `None`: the built-in default set; an empty
    /// list: nothing beyond the tools every chat keeps for finding more. Lean mode only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lean_tools: Option<Vec<String>>,
    /// The board a new speaker chat is shown on; `None` leaves it to the app (the board over the
    /// speaker folder, else the default board). One setting for every chat that arrives from the
    /// hub — phone voice, room speakers and Telegram alike.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<String>,
    /// The model a quick turn opens its talk's chat with; blank: no quick lane, quick turns open
    /// like deep ones.
    #[serde(default)]
    pub quick_model: String,
    /// The effort a quick turn opens its talk's chat with; blank: the app's usual new-chat effort.
    #[serde(default = "default_quick_effort")]
    pub quick_effort: String,
    /// A quick-lane talk whose chat reaches this context size is rolled over when the next turn
    /// arrives; 0 never rolls.
    #[serde(default = "default_roll_quick")]
    pub quick_roll_at: u64,
    /// The same for a deep-lane talk.
    #[serde(default = "default_roll_deep")]
    pub deep_roll_at: u64,
}

fn default_model() -> String {
    DEFAULT_MODEL.to_owned()
}
fn default_effort() -> String {
    "low".to_owned()
}
fn default_previous() -> i64 {
    12
}
fn default_quick_effort() -> String {
    "medium".to_owned()
}
fn default_roll_quick() -> u64 {
    16_000
}
fn default_roll_deep() -> u64 {
    48_000
}

impl Default for PolicyFile {
    fn default() -> Self {
        PolicyFile {
            model: default_model(),
            effort: default_effort(),
            take_speaker: None,
            previous_hours: default_previous(),
            lean: false,
            lean_prompt: String::new(),
            lean_tools: None,
            board_id: None,
            quick_model: String::new(),
            quick_effort: default_quick_effort(),
            quick_roll_at: default_roll_quick(),
            deep_roll_at: default_roll_deep(),
        }
    }
}

impl PolicyFile {
    /// Refuses a value outside the ranges Settings offers.
    pub fn check(&self) -> Result<(), String> {
        if self.model.chars().count() > 120 || self.model.chars().any(char::is_control) {
            return Err("The model must be a model id from the model picker.".into());
        }
        if !self.effort.trim().is_empty() && !helm_chats::catalogue::is_effort(self.effort.trim()) {
            return Err("The effort must be low, medium, high, xhigh or max.".into());
        }
        if !PREVIOUS_HOURS.contains(&self.previous_hours) {
            return Err("How long a finished talk may be named must be 0 to 72 hours.".into());
        }
        if self.board_id.as_deref().is_some_and(|b| {
            b.trim().is_empty() || b.chars().count() > 120 || b.chars().any(char::is_control)
        }) {
            return Err("The board must be a board id, or left unset.".into());
        }
        if self.lean_prompt.chars().count() > MAX_LEAN_PROMPT_CHARS
            || self
                .lean_prompt
                .chars()
                .any(|c| c.is_control() && c != '\n')
        {
            return Err(format!(
                "The lean prompt must be plain text of at most {MAX_LEAN_PROMPT_CHARS} characters."
            ));
        }
        if let Some(list) = &self.lean_tools {
            if list.len() > MAX_LEAN_TOOLS
                || list.iter().any(|t| {
                    t.trim().is_empty()
                        || t.chars().count() > MAX_LEAN_TOOL_CHARS
                        || t.chars().any(char::is_control)
                })
            {
                return Err(format!(
                    "The lean tool list must be at most {MAX_LEAN_TOOLS} names of up to \
                     {MAX_LEAN_TOOL_CHARS} characters each."
                ));
            }
        }
        if self.quick_model.chars().count() > 120 || self.quick_model.chars().any(char::is_control)
        {
            return Err("The quick-turn model must be a model id from the model picker.".into());
        }
        if !self.quick_effort.trim().is_empty()
            && !helm_chats::catalogue::is_effort(self.quick_effort.trim())
        {
            return Err("The quick-turn effort must be low, medium, high, xhigh or max.".into());
        }
        for size in [self.quick_roll_at, self.deep_roll_at] {
            if size != 0 && !crate::voice::ROLL_AT.contains(&size) {
                return Err(
                    "A roll-over size must be 4,000 to 200,000 tokens, or 0 to leave it off."
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// The board chats that arrive from the hub are shown on, if one is chosen. One setting for
    /// every channel: phone voice, room speakers and Telegram alike.
    pub fn board(&self) -> Option<&str> {
        self.board_id
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
    }

    /// The model and effort to hand a new talk's chat for `lane`, or `None` for each when blank.
    /// A quick turn with no saved quick model opens exactly like a deep one.
    pub fn new_chat(&self, lane: crate::voice::Lane) -> (Option<&str>, Option<&str>) {
        fn kept(s: &str) -> Option<&str> {
            Some(s.trim()).filter(|t| !t.is_empty())
        }
        if lane == crate::voice::Lane::Quick {
            match kept(&self.quick_model) {
                Some(m) => return (Some(m), kept(&self.quick_effort)),
                None => return (kept(&self.model), kept(&self.effort)),
            }
        }
        (kept(&self.model), kept(&self.effort))
    }

    /// The size at which a talk's chat rolls over when the next turn arrives; 0 leaves it off.
    pub fn roll_at(&self, lane: crate::voice::Lane) -> u64 {
        match lane {
            crate::voice::Lane::Quick => self.quick_roll_at,
            crate::voice::Lane::Deep => self.deep_roll_at,
        }
    }

    /// Whether the quick lane is a real second model rather than the deep one under another name.
    pub fn has_quick_lane(&self) -> bool {
        !self.quick_model.trim().is_empty()
    }
}

/// One profile's speaker files: its policy and its talk list, and the folder its chats start in
/// (`speaker/<profileId>`, which is how a chat there is known for a speaker chat of that profile).
#[derive(Clone)]
pub struct Paths {
    pub policy: PathBuf,
    pub chats: PathBuf,
    pub folder: PathBuf,
}

/// Whether this PC takes its person's room speaker: what the switch says, or the default — on
/// for the home profile, off for every other.
pub fn takes_speaker(f: &PolicyFile, is_home: bool) -> bool {
    f.take_speaker
        .unwrap_or_else(|| helm_profiles::store::channel_default(is_home))
}

/// The saved settings, or the defaults when none are saved (or the file is unreadable).
pub fn load_file(path: &Path) -> PolicyFile {
    // The cap fits a full lean prompt and tool list with room to spare.
    helm_core::privfs::read_capped(path, 32 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice::<PolicyFile>(&b).ok())
        .filter(|f| f.check().is_ok())
        .unwrap_or_default()
}

/// Save settings that passed [`PolicyFile::check`].
pub fn save_policy(path: &Path, f: &PolicyFile) -> std::io::Result<()> {
    write_private(path, f)
}

/// One talk on one speaker: the chat it is working in, and when it started.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Talk {
    /// The hub's id for this talk; it never changes during one.
    pub session: String,
    pub chat_id: String,
    pub started_at: i64,
    /// What the speaker called its room, for the chat's title and for each message's note.
    pub room: Option<String>,
    /// What the speaker called itself (its device name or token id): the pointer to this talk
    /// belongs to that speaker alone, so two speakers in one room never inherit each other's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The speaker left something open in this talk (a record said so): the chat is kept, not
    /// archived, when the talk ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub leftover: bool,
    /// The talk's chat was opened on the quick lane's model. A later deep turn moves that one chat
    /// up to the deep model (upgrade only); once moved, this goes off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quick: bool,
}

/// A talk whose chat outlived the talk because something was still held in it: it waits to be put
/// away on its own once nothing holds it and it has been quiet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kept {
    pub chat_id: String,
    /// When the talk ended (epoch milliseconds); the quiet clock starts here.
    pub closed_at: i64,
}

/// The talks this profile has open, oldest first, plus the one that ended last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Talks {
    pub talks: Vec<Talk>,
    /// The talk that ended most recently, so the next one can be told about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Previous>,
    /// Talk chats kept open past their talk (something was held in them). They are swept and put
    /// away once nothing holds them and they have been quiet for a while.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<Kept>,
}

/// The last finished talk, as a new one may be told about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Previous {
    pub title: String,
    pub chat_id: String,
    pub ended_at: i64,
    /// The last thing said there, kept short.
    pub gist: String,
    /// The room the talk ran in, as the speaker named it; a new talk is only pointed at this one
    /// when it comes from that same room (or either side never named one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// The speaker that ended, as it named itself; the pointer belongs to that speaker alone
    /// (or either side never named a device), so a second speaker's new talk is never pointed
    /// at the first one's chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// The speaker files of one profile.
pub fn paths(profile_dir: &Path, profile_id: &str) -> Paths {
    let dir = crate::people::person_dir(profile_dir, profile_id);
    Paths {
        policy: dir.join("speaker-policy.json"),
        chats: dir.join("speaker-chats.json"),
        folder: crate::channel::speaker_folder(profile_dir).join(profile_id),
    }
}

pub fn load_talks(path: &Path) -> Talks {
    helm_core::privfs::read_capped(path, 64 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice::<Talks>(&b).ok())
        .unwrap_or_default()
}

pub fn save_talks(path: &Path, t: &Talks) -> std::io::Result<()> {
    write_private(path, t)
}

/// The chat a speaker turn goes to: the tagged one for its talk while that chat is there and
/// not archived, else a new chat that takes the tag. `profile` is the display name of the
/// profile that owns the channel, used in the title when the speaker named no room. Returns the
/// talk and whether its chat was just made.
#[allow(clippy::too_many_arguments)]
pub fn talk_chat(
    path: &Path,
    port: &dyn ChatPort,
    session: &str,
    room: Option<&str>,
    device: Option<&str>,
    profile: Option<&str>,
    now: i64,
    quick: bool,
) -> Result<(Talk, bool), String> {
    let mut talks = load_talks(path);
    if let Some(i) = talks.talks.iter().position(|t| t.session == session) {
        // A chat archived by a roll-over whose successor is live is not gone: the talk moves to
        // it. A chat archived because the person put it away (no successor) still starts a new one.
        let mut tokens = 0;
        if crate::voice::follow_successor(port, &mut talks.talks[i].chat_id, &mut tokens) {
            save_talks(path, &talks)
                .map_err(|_| "The speaker talk list could not be saved.".to_owned())?;
        }
        if port
            .facts(&talks.talks[i].chat_id)
            .is_some_and(|f| !f.archived)
        {
            return Ok((talks.talks[i].clone(), false));
        }
        talks.talks.retain(|t| t.session != session);
    }
    let chat_id = port.create(&chat_title(room, profile, now))?;
    let talk = Talk {
        session: session.to_owned(),
        chat_id,
        started_at: now,
        room: room.map(str::to_owned),
        device: device.map(str::to_owned),
        leftover: false,
        quick,
    };
    talks.talks.push(talk.clone());
    while talks.talks.len() > TALKS_KEPT {
        talks.talks.remove(0);
    }
    save_talks(path, &talks).map_err(|_| "The speaker talk list could not be saved.".to_owned())?;
    Ok((talk, true))
}

/// The talk the hub says has ended: it leaves the list, and becomes the one the next talk may be
/// told about. Returns its chat, which the caller archives. `None` when this PC never made that
/// talk.
pub fn end_talk(
    path: &Path,
    session: &str,
    now: i64,
    title: &str,
    gist: &str,
    keep_previous: bool,
) -> Option<Talk> {
    let mut talks = load_talks(path);
    let at = talks.talks.iter().position(|t| t.session == session)?;
    let talk = talks.talks.remove(at);
    if keep_previous && !gist.trim().is_empty() {
        talks.previous = Some(Previous {
            title: title.to_owned(),
            chat_id: talk.chat_id.clone(),
            ended_at: now,
            gist: gist.trim().chars().take(PREVIOUS_NOTE_MAX_CHARS).collect(),
            room: talk.room.clone(),
            device: talk.device.clone(),
        });
    }
    let _ = save_talks(path, &talks);
    Some(talk)
}

/// Note that the talk left something open, so its chat outlives the talk. `false` when this PC
/// has no such talk.
pub fn mark_leftover(path: &Path, session: &str) -> bool {
    let mut talks = load_talks(path);
    let Some(t) = talks.talks.iter_mut().find(|t| t.session == session) else {
        return false;
    };
    t.leftover = true;
    save_talks(path, &talks).is_ok()
}

/// A talk's chat was rolled over into `new_chat` (an early roll-over the hub-side channel does):
/// the talk follows its own link, keeping the talk on one live chat. `quick` restates which lane
/// the new chat opened on, so a talk that changed lanes keeps its record in step with its chat.
pub fn retalk(path: &Path, session: &str, new_chat: &str, quick: bool) -> bool {
    let mut talks = load_talks(path);
    let Some(t) = talks.talks.iter_mut().find(|t| t.session == session) else {
        return false;
    };
    t.chat_id = new_chat.to_owned();
    t.quick = quick;
    save_talks(path, &talks).is_ok()
}

/// A deep turn arrived for a talk whose chat opened on the quick model: once its chat is moved up
/// to the deep model (or refused), this marks that done, so the move happens once per talk.
pub fn mark_deep(path: &Path, session: &str) -> bool {
    let mut talks = load_talks(path);
    let Some(t) = talks.talks.iter_mut().find(|t| t.session == session) else {
        return false;
    };
    if !t.quick {
        return true;
    }
    t.quick = false;
    save_talks(path, &talks).is_ok()
}

/// A talk's chat is kept past the talk because something was held in it.
pub fn keep_chat(path: &Path, chat_id: &str, closed_at: i64) {
    let mut talks = load_talks(path);
    if talks.kept.iter().any(|k| k.chat_id == chat_id) {
        return;
    }
    talks.kept.push(Kept {
        chat_id: chat_id.to_owned(),
        closed_at,
    });
    while talks.kept.len() > TALKS_KEPT {
        talks.kept.remove(0);
    }
    let _ = save_talks(path, &talks);
}

/// A kept chat is dealt with (put away, or handed to the person because they wrote in it): it
/// leaves the list.
pub fn unkeep_chat(path: &Path, chat_id: &str) {
    let mut talks = load_talks(path);
    if talks.kept.iter().any(|k| k.chat_id == chat_id) {
        talks.kept.retain(|k| k.chat_id != chat_id);
        let _ = save_talks(path, &talks);
    }
}

/// The talk tagged for `session`, if this PC has one.
pub fn find(path: &Path, session: &str) -> Option<Talk> {
    load_talks(path)
        .talks
        .into_iter()
        .find(|t| t.session == session)
}

/// Every chat this profile's speaker talks are working in, so a reset or a settings change can
/// act on them and forget the list. The kept chats (talks that ended with something held) empty too.
pub fn take_all(path: &Path) -> Vec<Talk> {
    let t = load_talks(path);
    let _ = save_talks(path, &Talks::default());
    t.talks
}

/// Every chat kept past its talk, with when its talk ended — a reset puts these away as well.
pub fn take_kept(path: &Path) -> Vec<Kept> {
    let mut t = load_talks(path);
    let out = std::mem::take(&mut t.kept);
    let _ = save_talks(path, &t);
    out
}

/// The talk that ended last, when it is recent enough to be named to the next one.
/// How long after a talk closes its pointer may ride to the next one on that speaker: the talk
/// idle-closes in minutes, and past half an hour the reference has lost its point. The setting's
/// `previousHours` only turns the naming off (0); any other value lets the 30-minute window stand.
const PREVIOUS_WINDOW_MS: i64 = 30 * MINUTE_MS;

pub fn previous_usable(p: &Previous, policy: &PolicyFile, now: i64) -> bool {
    policy.previous_hours > 0 && now - p.ended_at <= PREVIOUS_WINDOW_MS
}

/// `HA speaker <room> voice <YYYY-MM-DD>` (UTC date), or the profile's name where the speaker
/// named no room.
pub fn chat_title(room: Option<&str>, profile: Option<&str>, now: i64) -> String {
    let named = clean_title_name(room).or_else(|| clean_title_name(profile));
    let (y, m, d) = civil_date(now.div_euclid(86_400_000));
    match named.as_deref() {
        Some(n) => format!("HA speaker {n} voice {y:04}-{m:02}-{d:02}"),
        None => format!("HA speaker voice {y:04}-{m:02}-{d:02}"),
    }
}

/// The submission id for a turn id from the hub: the same turn delivered twice maps to the same
/// submission. A UUID, as workers require (version 8, from a SHA-256 of the turn id).
pub fn submission_id(turn: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("helm-speaker/{turn}"));
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Uuid::new_v8(b).to_string()
}

/// The submission id of a record from the hub: its own namespace, so a record and a turn that
/// share an id never share a submission, and the same record delivered twice is stored once.
pub fn record_submission_id(record: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("helm-speaker-record/{record}"));
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Uuid::new_v8(b).to_string()
}

/// The room/area a speaker device names itself by, fit for the one-line note: cleaned like a
/// chat title (control characters out, capped short) and with the bracket characters out too —
/// the note is one line inside its own brackets, and a `]` from a device would close it early.
fn clean_place(raw: Option<&str>) -> Option<String> {
    let bare = raw?.replace(['[', ']'], "");
    clean_title_name(Some(&bare))
}

/// The note on every speaker message: which speaker and room it came from, and how the answer is
/// used. One bracketed line, hidden from the transcript.
pub fn channel_note(room: Option<&str>, area: Option<&str>) -> String {
    let (room, area) = (clean_place(room), clean_place(area));
    let where_ = match (room.as_deref(), area.as_deref()) {
        (Some(room), Some(area)) if room != area => format!("{room} / {area}"),
        (Some(one), _) | (_, Some(one)) => one.to_owned(),
        (None, None) => "the house".to_owned(),
    };
    format!("[Speaker · {where_} · speak it, short, no markup]")
}

/// What the speaker said just before a turn it handed over, as one bracketed line: line breaks
/// and brackets out, so a `]` inside it cannot close the note early.
pub fn context_line(context: &str) -> Option<String> {
    let flat = context
        .replace(['[', ']'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let flat: String = flat.chars().take(MAX_CONTEXT_CHARS).collect();
    (!flat.is_empty()).then(|| format!("[Said on the speaker just before: {flat}]"))
}

/// What a speaker message is stored with: the channel note, a pointer to the talk that just
/// ended, what was said just before (for a turn the speaker handed over), then the words.
pub fn delivered_text(
    room: Option<&str>,
    area: Option<&str>,
    previous: Option<&str>,
    context: Option<&str>,
    text: &str,
) -> String {
    let mut out = channel_note(room, area);
    out.push('\n');
    if let Some(p) = previous {
        out.push_str(&format!("[Previous conversation ended: {p}]\n"));
    }
    if let Some(line) = context.and_then(context_line) {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(text.trim());
    out
}

/// The line between a recorded question and answer: what the speaker did to answer, what it
/// passed on or could not do, and what it left open. `None` when there is nothing to say.
pub fn record_notice(rec: &crate::client::SpeakerRecord) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if !rec.tools.is_empty() {
        let used: Vec<String> = rec
            .tools
            .iter()
            .map(|t| {
                let what = if t.summary.trim().is_empty() {
                    t.name.as_str()
                } else {
                    t.summary.trim()
                };
                if t.ok {
                    what.to_owned()
                } else {
                    format!("{what} (did not work)")
                }
            })
            .collect();
        parts.push(format!("The speaker used: {}.", used.join("; ")));
    }
    match rec.kind.as_str() {
        "escalated" => parts.push("The speaker passed this on to Helm.".to_owned()),
        "error" => parts.push("The speaker could not answer this.".to_owned()),
        _ => {}
    }
    if !rec.leftover.is_empty() {
        parts.push(format!("Left open: {}.", rec.leftover.join("; ")));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The line that names the talk before: its title, its chat id (so a new chat can pull up what
/// was said there with `app_read_chat`), and what was last said.
pub fn previous_pointer(p: &Previous) -> String {
    format!("{} — {} (chat {})", p.title, p.gist.trim(), p.chat_id)
}

/// Whether a new turn belongs to the room of the talk it might be pointed at: both named one and
/// they differ, no; either never named one, yes. Compared like titles are cleaned, case-free.
pub fn same_room(prev: Option<&str>, next: Option<&str>) -> bool {
    match (prev.map(str::trim), next.map(str::trim)) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => a.eq_ignore_ascii_case(b),
        _ => true,
    }
}

/// Whether a new turn comes from the speaker that ended the talk it might be pointed at: both
/// named themselves and differ, no; either never named a device, yes. Room alone is not enough —
/// two speakers in one room are two people's conversations.
pub fn same_speaker(prev: Option<&str>, next: Option<&str>) -> bool {
    match (prev.map(str::trim), next.map(str::trim)) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => a.eq_ignore_ascii_case(b),
        _ => true,
    }
}

/// The talk id and room of a `speaker_session_closed` frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Closed {
    pub session: String,
    /// When the talk ran out, as the hub's wall clock (epoch milliseconds).
    pub closed_at: i64,
}

pub fn parse_closed(frame: &Value) -> Option<Closed> {
    Some(Closed {
        session: frame.get("sessionId")?.as_str()?.to_owned(),
        closed_at: frame.get("closedAt").and_then(Value::as_i64).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000_000; // 2027-01-15

    #[derive(Default)]
    struct Port {
        made: std::cell::RefCell<Vec<String>>,
        archived: std::cell::RefCell<std::collections::HashSet<String>>,
        successors: std::cell::RefCell<std::collections::HashMap<String, String>>,
    }
    impl ChatPort for Port {
        fn facts(&self, id: &str) -> Option<crate::telegram::ChatFacts> {
            (!id.starts_with("gone")).then_some(crate::telegram::ChatFacts {
                archived: self.archived.borrow().contains(id),
                tokens: 10,
            })
        }
        fn successor(&self, id: &str) -> Option<String> {
            self.successors.borrow().get(id).cloned()
        }
        fn create(&self, title: &str) -> Result<String, String> {
            self.made.borrow_mut().push(title.to_owned());
            Ok(format!("chat-{}", self.made.borrow().len()))
        }
    }

    #[test]
    fn titles_notes_and_ids() {
        assert_eq!(
            chat_title(Some("bedroom"), Some("Alex"), T0),
            "HA speaker bedroom voice 2027-01-15"
        );
        assert_eq!(
            chat_title(None, Some("Alex"), T0),
            "HA speaker Alex voice 2027-01-15"
        );
        assert_eq!(chat_title(None, None, T0), "HA speaker voice 2027-01-15");
        assert_eq!(
            channel_note(Some("study"), Some("Study")),
            "[Speaker · study / Study · speak it, short, no markup]"
        );
        assert_eq!(
            channel_note(None, None),
            "[Speaker · the house · speak it, short, no markup]"
        );
        // Room and area arrive from the speaker device: brackets and line breaks in them never
        // break the one-line note.
        assert_eq!(
            channel_note(Some("bed]\nroom [x"), Some("bed]")),
            "[Speaker · bedroom x / bed · speak it, short, no markup]"
        );
        assert_eq!(
            channel_note(Some("] ]"), None),
            "[Speaker · the house · speak it, short, no markup]",
            "a name left empty after cleaning falls back to the house"
        );
        assert_eq!(
            delivered_text(Some("study"), None, None, None, "  What time is it?  "),
            "[Speaker · study · speak it, short, no markup]\nWhat time is it?"
        );
        assert!(
            delivered_text(None, None, Some("Kitchen voice — 7:20"), None, "go on")
                .contains("[Previous conversation ended: Kitchen voice — 7:20]")
        );
        // What the speaker said just before a handed-over turn rides in one line, whatever it
        // held; a blank context adds nothing.
        assert_eq!(
            delivered_text(
                None,
                None,
                None,
                Some("Lamp on.\n[Done]\n  Thanks"),
                "and the bedroom"
            ),
            "[Speaker · the house · speak it, short, no markup]\n[Said on the speaker just before: Lamp on. Done Thanks]\nand the bedroom"
        );
        assert_eq!(context_line(" \n "), None);
        // A record is stored under its own id, never a turn's.
        assert_eq!(record_submission_id("r-1"), record_submission_id("r-1"));
        assert_ne!(record_submission_id("r-1"), submission_id("r-1"));
        let rec = |kind: &str, ok: bool, leftover: &[&str]| crate::client::SpeakerRecord {
            id: "r-1".into(),
            device: None,
            room: None,
            area: None,
            session: "s-1".into(),
            started_at: 1,
            text: "Lights".into(),
            answer: "Done.".into(),
            kind: kind.into(),
            tools: vec![
                crate::client::RecordedTool {
                    name: "light_on".into(),
                    summary: "bedroom lamp".into(),
                    ok: true,
                },
                crate::client::RecordedTool {
                    name: "heat".into(),
                    summary: String::new(),
                    ok,
                },
            ],
            leftover: leftover.iter().map(|s| (*s).to_owned()).collect(),
            model: None,
            person: None,
        };
        assert_eq!(
            record_notice(&rec("answer", true, &[])).as_deref(),
            Some("The speaker used: bedroom lamp; heat.")
        );
        assert_eq!(
            record_notice(&rec("escalated", false, &["buy bulbs", "call Sam"])).as_deref(),
            Some("The speaker used: bedroom lamp; heat (did not work). The speaker passed this on to Helm. Left open: buy bulbs; call Sam.")
        );
        let bare = crate::client::SpeakerRecord {
            tools: vec![],
            ..rec("error", true, &[])
        };
        assert_eq!(
            record_notice(&bare).as_deref(),
            Some("The speaker could not answer this.")
        );
        let plain = crate::client::SpeakerRecord {
            kind: "answer".into(),
            ..bare
        };
        assert_eq!(record_notice(&plain), None);
        // A turn id always maps to the same submission, and never to the phone's turn with the
        // same id.
        assert_eq!(submission_id("t-1"), submission_id("t-1"));
        assert_ne!(submission_id("t-1"), crate::voice::submission_id("t-1"));
    }

    #[test]
    fn talks_are_tagged_one_per_talk_and_end_into_the_next() {
        let dir = std::env::temp_dir().join(format!("helm-speaker-talks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("speaker-chats.json");
        let port = Port::default();
        let (first, made) = talk_chat(
            &file,
            &port,
            "s-1",
            Some("kitchen"),
            Some("kitchen display"),
            Some("Alex"),
            T0,
            false,
        )
        .unwrap();
        assert!(made);
        assert_eq!(first.chat_id, "chat-1");
        // The same talk keeps its chat; another talk on another speaker gets its own.
        let (again, made) = talk_chat(
            &file,
            &port,
            "s-1",
            Some("kitchen"),
            Some("kitchen display"),
            Some("Alex"),
            T0 + 60_000,
            false,
        )
        .unwrap();
        assert!(!made);
        assert_eq!(again.chat_id, "chat-1");
        let (other, made) = talk_chat(
            &file,
            &port,
            "s-2",
            None,
            None,
            Some("Alex"),
            T0 + 61_000,
            true,
        )
        .unwrap();
        assert!(made && other.chat_id != first.chat_id);
        assert!(
            !first.quick && find(&file, "s-2").unwrap().quick,
            "the lane is recorded"
        );
        // A quick turn's chat opened on the quick model; a deep talk upgrade marks it moved once.
        assert!(mark_deep(&file, "s-2") && !find(&file, "s-2").unwrap().quick);
        assert!(
            !mark_deep(&file, "s-9"),
            "a talk this PC never made is not marked"
        );
        // The hub says talk one ended: it leaves the list, and the next talk can be told about
        // it. Ending a talk this PC never made changes nothing.
        let ended = end_talk(
            &file,
            "s-1",
            T0 + 700_000,
            "HA speaker kitchen voice 2027-01-15",
            "It is 7:20.",
            true,
        );
        assert_eq!(ended.map(|t| t.chat_id), Some("chat-1".to_owned()));
        assert_eq!(find(&file, "s-1"), None);
        let talks = load_talks(&file);
        let prev = talks.previous.clone().unwrap();
        assert_eq!(prev.gist, "It is 7:20.");
        assert_eq!(
            prev.device.as_deref(),
            Some("kitchen display"),
            "the speaker who ended is recorded beside the room"
        );
        let policy = PolicyFile::default();
        // The pointer rides for half an hour after the close, not the whole previousHours span:
        // +5 and +25 minutes name it, +45 does not, and a far-off 12 hours certainly not.
        assert!(previous_usable(
            &prev,
            &policy,
            T0 + 700_000 + 5 * MINUTE_MS
        ));
        assert!(previous_usable(
            &prev,
            &policy,
            T0 + 700_000 + 25 * MINUTE_MS
        ));
        assert!(!previous_usable(
            &prev,
            &policy,
            T0 + 700_000 + 45 * MINUTE_MS
        ));
        assert!(!previous_usable(
            &prev,
            &policy,
            T0 + 700_000 + 12 * HOUR_MS
        ));
        assert_eq!(
            previous_pointer(&prev),
            "HA speaker kitchen voice 2027-01-15 — It is 7:20. (chat chat-1)"
        );
        // The pointer belongs to the room its talk ran in, and a new turn's room decides whether
        // it rides along: same room yes, another no, either unnamed still yes.
        assert_eq!(prev.room.as_deref(), Some("kitchen"));
        assert!(same_room(prev.room.as_deref(), Some("Kitchen")));
        assert!(!same_room(prev.room.as_deref(), Some("study")));
        assert!(same_room(prev.room.as_deref(), None) && same_room(None, Some("study")));
        // And it belongs to the speaker who ended, not just the room: a second speaker in the
        // same kitchen keeps its own pointer; an unnamed device stays lenient (older hubs).
        assert!(same_speaker(
            prev.device.as_deref(),
            Some("Kitchen Display")
        ));
        assert!(!same_speaker(prev.device.as_deref(), Some("bedroom phone")));
        assert!(
            same_speaker(prev.device.as_deref(), None) && same_speaker(None, Some("study speaker"))
        );
        let off = PolicyFile {
            previous_hours: 0,
            ..PolicyFile::default()
        };
        assert!(!previous_usable(&prev, &off, T0 + 700_000 + 5 * MINUTE_MS));
        // A talk that left something open says so, and keeps saying so; one this PC never made
        // cannot be marked.
        assert!(mark_leftover(&file, "s-2") && !mark_leftover(&file, "s-9"));
        assert!(find(&file, "s-2").unwrap().leftover);
        // An archived talk chat with a live successor is not gone: the talk follows the link
        // (the roll-over case) instead of opening another chat.
        port.archived.borrow_mut().insert("chat-2".to_owned());
        port.successors
            .borrow_mut()
            .insert("chat-2".to_owned(), "succ-9".to_owned());
        let (followed, made) = talk_chat(
            &file,
            &port,
            "s-2",
            None,
            None,
            Some("Alex"),
            T0 + 62_000,
            true,
        )
        .unwrap();
        assert!(!made && followed.chat_id == "succ-9");
        // No successor: hand-archived still means start fresh.
        port.archived.borrow_mut().insert("succ-9".to_owned());
        let (fresh, made) = talk_chat(
            &file,
            &port,
            "s-2",
            None,
            None,
            Some("Alex"),
            T0 + 63_000,
            false,
        )
        .unwrap();
        assert!(made && fresh.chat_id != first.chat_id && fresh.chat_id != "succ-9");
        // Kept chats are listed, kept once, and leave the list when dealt with.
        keep_chat(&file, "chat-1", T0 + 700_000);
        keep_chat(&file, "chat-1", T0 + 700_000);
        assert_eq!(load_talks(&file).kept.len(), 1);
        unkeep_chat(&file, "chat-1");
        assert!(load_talks(&file).kept.is_empty());
        keep_chat(&file, "kept-1", T0 + 700_000);
        let kept = take_kept(&file);
        assert_eq!(
            kept.iter().map(|k| k.chat_id.as_str()).collect::<Vec<_>>(),
            ["kept-1"]
        );
        // Reset empties the list and hands back the chats to archive. The one talk left is the
        // fresh one for s-2 (its chat was hand-archived with no successor, so the leftover flag
        // went with it): nothing is open on it.
        let all = take_all(&file);
        assert_eq!(all.len(), 1);
        assert!(!all[0].leftover);
        assert!(load_talks(&file).talks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_are_checked_and_default_to_the_small_local_model() {
        let f = PolicyFile::default();
        assert_eq!(f.model, DEFAULT_MODEL);
        assert_eq!(
            f.new_chat(crate::voice::Lane::Deep),
            (Some(DEFAULT_MODEL), Some("low"))
        );
        assert!(f.check().is_ok());
        assert!(takes_speaker(&f, true) && !takes_speaker(&f, false));
        // The quick lane defaults to blank — no second model until the user picks one — with
        // thinking on by default and small roll-over sizes.
        assert!(f.quick_model.is_empty() && f.quick_effort == "medium");
        assert_eq!((f.quick_roll_at, f.deep_roll_at), (16_000, 48_000));
        assert!(!f.has_quick_lane());
        let lanes = PolicyFile {
            quick_model: "local/small/quick-one".into(),
            ..PolicyFile::default()
        };
        assert!(lanes.has_quick_lane());
        assert_eq!(
            lanes.new_chat(crate::voice::Lane::Quick),
            (Some("local/small/quick-one"), Some("medium"))
        );
        assert_eq!(
            f.new_chat(crate::voice::Lane::Quick),
            (Some(DEFAULT_MODEL), Some("low"))
        );
        // An older settings file reads with the lane fields defaulted, and they round-trip.
        let with_lane: PolicyFile = serde_json::from_str(
            r#"{"model":"m","effort":"low","quickModel":"q","quickEffort":"high","deepRollAt":0}"#,
        )
        .unwrap();
        assert!(with_lane.check().is_ok());
        assert_eq!(
            with_lane.new_chat(crate::voice::Lane::Quick),
            (Some("q"), Some("high"))
        );
        assert_eq!(with_lane.roll_at(crate::voice::Lane::Deep), 0);
        // Lean is off and no board is chosen until Settings says so; a chosen board is trimmed.
        // Lean's own prompt and tool list start unset: blank prompt means the built-in one, and
        // an absent list means the app's default set.
        assert!(!f.lean && f.board().is_none());
        assert!(f.lean_prompt.is_empty() && f.lean_tools.is_none());
        let chosen = PolicyFile {
            lean: true,
            board_id: Some(" board-1 ".into()),
            ..PolicyFile::default()
        };
        assert!(chosen.check().is_ok() && chosen.board() == Some("board-1"));
        // An older settings file with neither field still loads, and writes neither back.
        let old: PolicyFile = serde_json::from_str(r#"{"model":"m","effort":"low"}"#).unwrap();
        assert!(!old.lean && old.board_id.is_none());
        assert!(old.lean_prompt.is_empty() && old.lean_tools.is_none());
        assert!(!serde_json::to_string(&old).unwrap().contains("boardId"));
        assert!(!serde_json::to_string(&old).unwrap().contains("leanTools"));
        // A chosen lean prompt and tool list round-trip through the file's camelCase names; an
        // empty list is a real choice ("nothing but the finding tools"), distinct from no list.
        let chosen_lean = PolicyFile {
            lean: true,
            lean_prompt: "Answer in one line.".into(),
            lean_tools: Some(vec!["home assistant".into(), "memory_search".into()]),
            ..PolicyFile::default()
        };
        assert!(chosen_lean.check().is_ok());
        let json = serde_json::to_string(&chosen_lean).unwrap();
        assert!(json.contains(r#""leanPrompt":"Answer in one line.""#));
        assert!(json.contains(r#""leanTools":["home assistant","memory_search"]"#));
        let back: PolicyFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, chosen_lean);
        assert!(PolicyFile {
            lean_tools: Some(vec![]),
            ..PolicyFile::default()
        }
        .check()
        .is_ok());
        let switched = PolicyFile {
            take_speaker: Some(false),
            ..PolicyFile::default()
        };
        assert!(!takes_speaker(&switched, true));
        for bad in [
            PolicyFile {
                model: "a".repeat(140),
                ..PolicyFile::default()
            },
            PolicyFile {
                effort: "sometimes".into(),
                ..PolicyFile::default()
            },
            PolicyFile {
                previous_hours: 73,
                ..PolicyFile::default()
            },
            PolicyFile {
                board_id: Some("  ".into()),
                ..PolicyFile::default()
            },
            PolicyFile {
                board_id: Some("b\nad".into()),
                ..PolicyFile::default()
            },
            PolicyFile {
                lean_prompt: "x".repeat(MAX_LEAN_PROMPT_CHARS + 1),
                ..PolicyFile::default()
            },
            PolicyFile {
                lean_prompt: "one\there".into(),
                ..PolicyFile::default()
            },
            PolicyFile {
                lean_tools: Some((0..=MAX_LEAN_TOOLS).map(|i| format!("t{i}")).collect()),
                ..PolicyFile::default()
            },
            PolicyFile {
                lean_tools: Some(vec!["  ".into()]),
                ..PolicyFile::default()
            },
            PolicyFile {
                lean_tools: Some(vec!["x".repeat(MAX_LEAN_TOOL_CHARS + 1)]),
                ..PolicyFile::default()
            },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
    }
}
