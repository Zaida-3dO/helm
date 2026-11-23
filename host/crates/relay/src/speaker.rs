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
//!   them at the first send), and whether this PC takes its person's speaker at all.
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

impl Default for PolicyFile {
    fn default() -> Self {
        PolicyFile {
            model: default_model(),
            effort: default_effort(),
            take_speaker: None,
            previous_hours: default_previous(),
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
        Ok(())
    }

    /// The model and effort to hand a new speaker chat, or `None` for each when blank.
    pub fn new_chat(&self) -> (Option<&str>, Option<&str>) {
        fn kept(s: &str) -> Option<&str> {
            Some(s.trim()).filter(|t| !t.is_empty())
        }
        (kept(&self.model), kept(&self.effort))
    }
}

/// One profile's speaker files: its policy and its talk list, and the folder its chats start in.
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
    helm_core::privfs::read_capped(path, 4 * 1024)
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
}

/// The talks this profile has open, oldest first, plus the one that ended last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Talks {
    pub talks: Vec<Talk>,
    /// The talk that ended most recently, so the next one can be told about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Previous>,
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
}

/// The speaker files of one profile.
pub fn paths(profile_dir: &Path, profile_id: &str) -> Paths {
    let dir = crate::people::person_dir(profile_dir, profile_id);
    Paths {
        policy: dir.join("speaker-policy.json"),
        chats: dir.join("speaker-chats.json"),
        folder: crate::channel::speaker_folder(profile_dir),
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
pub fn talk_chat(
    path: &Path,
    port: &dyn ChatPort,
    session: &str,
    room: Option<&str>,
    profile: Option<&str>,
    now: i64,
) -> Result<(Talk, bool), String> {
    let mut talks = load_talks(path);
    if let Some(t) = talks.talks.iter().find(|t| t.session == session) {
        if port.facts(&t.chat_id).is_some_and(|f| !f.archived) {
            return Ok((t.clone(), false));
        }
        talks.talks.retain(|t| t.session != session);
    }
    let chat_id = port.create(&chat_title(room, profile, now))?;
    let talk = Talk {
        session: session.to_owned(),
        chat_id,
        started_at: now,
        room: room.map(str::to_owned),
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
        });
    }
    let _ = save_talks(path, &talks);
    Some(talk)
}

/// The talk tagged for `session`, if this PC has one.
pub fn find(path: &Path, session: &str) -> Option<Talk> {
    load_talks(path)
        .talks
        .into_iter()
        .find(|t| t.session == session)
}

/// Every chat this profile's speaker talks are working in, so a reset or a settings change can
/// act on them and forget the list.
pub fn take_all(path: &Path) -> Vec<Talk> {
    let t = load_talks(path);
    let _ = save_talks(path, &Talks::default());
    t.talks
}

/// The talk that ended last, when it is recent enough to be named to the next one.
pub fn previous_usable(p: &Previous, policy: &PolicyFile, now: i64) -> bool {
    policy.previous_hours > 0 && now - p.ended_at <= policy.previous_hours * HOUR_MS
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

/// The note on every speaker message: which speaker and room it came from, and how the answer is
/// used. One bracketed line, hidden from the transcript.
pub fn channel_note(room: Option<&str>, area: Option<&str>) -> String {
    let where_ = match (room, area) {
        (Some(room), Some(area)) if room != area => format!("{room} / {area}"),
        (Some(one), _) | (_, Some(one)) => one.to_owned(),
        (None, None) => "the house".to_owned(),
    };
    format!("[Speaker · {where_} · speak it, short, no markup]")
}

/// What a speaker message is stored with: the channel note, a pointer to the talk that just
/// ended, then the words.
pub fn delivered_text(
    room: Option<&str>,
    area: Option<&str>,
    previous: Option<&str>,
    text: &str,
) -> String {
    let mut out = channel_note(room, area);
    out.push('\n');
    if let Some(p) = previous {
        out.push_str(&format!("[Previous conversation ended: {p}]\n"));
    }
    out.push_str(text.trim());
    out
}

/// The line that names the talk before: its title, and what was last said there.
pub fn previous_pointer(p: &Previous) -> String {
    format!("{} — {}", p.title, p.gist.trim())
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
    }
    impl ChatPort for Port {
        fn facts(&self, id: &str) -> Option<crate::telegram::ChatFacts> {
            (!id.starts_with("gone")).then_some(crate::telegram::ChatFacts {
                archived: false,
                tokens: 10,
            })
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
        assert_eq!(
            delivered_text(Some("study"), None, None, "  What time is it?  "),
            "[Speaker · study · speak it, short, no markup]\nWhat time is it?"
        );
        assert!(
            delivered_text(None, None, Some("Kitchen voice — 7:20"), "go on")
                .contains("[Previous conversation ended: Kitchen voice — 7:20]")
        );
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
        let (first, made) =
            talk_chat(&file, &port, "s-1", Some("kitchen"), Some("Alex"), T0).unwrap();
        assert!(made);
        assert_eq!(first.chat_id, "chat-1");
        // The same talk keeps its chat; another talk on another speaker gets its own.
        let (again, made) = talk_chat(
            &file,
            &port,
            "s-1",
            Some("kitchen"),
            Some("Alex"),
            T0 + 60_000,
        )
        .unwrap();
        assert!(!made);
        assert_eq!(again.chat_id, "chat-1");
        let (other, made) =
            talk_chat(&file, &port, "s-2", None, Some("Alex"), T0 + 61_000).unwrap();
        assert!(made && other.chat_id != first.chat_id);
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
        let policy = PolicyFile::default();
        assert!(previous_usable(&prev, &policy, T0 + 8 * 60 * MINUTE_MS));
        assert!(!previous_usable(&prev, &policy, T0 + 20 * HOUR_MS));
        assert_eq!(
            previous_pointer(&prev),
            "HA speaker kitchen voice 2027-01-15 — It is 7:20."
        );
        let off = PolicyFile {
            previous_hours: 0,
            ..PolicyFile::default()
        };
        assert!(!previous_usable(&prev, &off, T0 + 60 * MINUTE_MS));
        // Reset empties the list and hands back the chats to archive.
        let all = take_all(&file);
        assert_eq!(all.len(), 1);
        assert!(load_talks(&file).talks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_are_checked_and_default_to_the_small_local_model() {
        let f = PolicyFile::default();
        assert_eq!(f.model, DEFAULT_MODEL);
        assert_eq!(f.new_chat(), (Some(DEFAULT_MODEL), Some("low")));
        assert!(f.check().is_ok());
        assert!(takes_speaker(&f, true) && !takes_speaker(&f, false));
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
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
    }
}
