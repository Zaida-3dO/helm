//! The voice chat's side of the line: its tag, its limits and the words it is given.
//!
//! The phone's voice screen talks to one chat, the voice chat, found through a small tag file in
//! the profile (`relay/voice-chat.json`). It differs from the Telegram chat in what makes it
//! fresh: besides age and tokens, a chat that has heard nothing from the phone for the idle time
//! is left behind, so a question asked the next morning does not drag yesterday along.
//!
//! - **Origin.** A voice message enters its chat with [`VOICE_ORIGIN`], never as typed, so it
//!   never counts as consent and no approval, plan or trust prompt is answered by it.
//! - **The policy** ([`PolicyFile`], `relay/voice-policy.json`) is read again on every delivery.
//! - **The words.** How the answer is heard comes from the chat's system prompt (the `channel`
//!   section, [`crate::channel`]); every message carries a one-line note naming its [`Mode`],
//!   which the transcript hides.
//! - **The reply** ([`Reply`]) is what goes up the line, once per answer, with a fixed id so a
//!   resend is recognised.

use crate::telegram::{
    civil_date, clean_title_name, cut_chars, write_private, ChatFacts, ChatPort,
};
use helm_chats::types::SubmissionOrigin;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The origin every voice message is stored with.
pub const VOICE_ORIGIN: SubmissionOrigin = SubmissionOrigin::Voice;

const HOUR_MS: i64 = 60 * 60 * 1000;
const MINUTE_MS: i64 = 60 * 1000;

/// The limits Settings accepts.
pub const IDLE_MINUTES: std::ops::RangeInclusive<i64> = 15..=1440;
pub const AGE_HOURS: std::ops::RangeInclusive<i64> = 1..=720;
pub const TOKENS: std::ops::RangeInclusive<u64> = 10_000..=5_000_000;
/// The roll-over sizes Settings accepts (on top of 0, which turns rolling off). They stand apart
/// from the global roll-over rule because a small local model reads a long talk badly long before
/// that rule's floor.
pub const ROLL_AT: std::ops::RangeInclusive<u64> = 4_000..=200_000;

/// How much room above the chat's first context reading a roll-over size may sit, so a chat whose
/// system prompt alone is near the size cannot roll on every turn.
pub const ROLL_FLOOR_ROOM: u64 = 4_000;
/// The most to quote of the last exchanges in a carry-over note.
pub const MAX_CARRY_CHARS: usize = 1_500;

/// The longest answer sent for reading in chat mode.
pub const MAX_CHAT_CHARS: usize = 30_000;
/// The longest typed or transcribed message taken from the hub.
pub const MAX_TEXT_CHARS: usize = 8_000;

/// How the answer is used: spoken aloud, or read on the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Conversation,
    Chat,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Conversation => "conversation",
            Mode::Chat => "chat",
        }
    }
    /// `chat` is chat mode; anything else is the default, conversation.
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("chat") {
            Mode::Chat
        } else {
            Mode::Conversation
        }
    }
}

/// Which lane a turn asked for: a quick one on the small model saved for quick turns, or a deep
/// one on the model saved for new voice chats. An absent or unknown urgency means deep, so an
/// older phone behaves exactly as before. The two lanes keep separate chats so switching between
/// them never reloads a GPU mid-conversation or trips a model change refused as too large.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lane {
    Quick,
    #[default]
    Deep,
}

impl Lane {
    /// `quick` is the quick lane; anything else (including nothing) is deep.
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(str::trim) {
            Some(t) if t.eq_ignore_ascii_case("quick") => Lane::Quick,
            _ => Lane::Deep,
        }
    }
}

/// What the attention watch wakes the voice chat for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchLevel {
    #[serde(rename = "off")]
    Off,
    #[default]
    #[serde(rename = "flags")]
    Flags,
    #[serde(rename = "flags+done")]
    FlagsDone,
}

/// The voice chat's limits and what new voice chats open with, as stored and as Settings shows
/// them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyFile {
    /// The model a new voice chat opens with; blank: the app's usual new-chat model.
    #[serde(default = "default_model")]
    pub model: String,
    /// The effort a new voice chat opens with; blank: the app's usual new-chat effort.
    #[serde(default = "default_effort")]
    pub effort: String,
    /// A message arriving this long after the last one opens a new chat.
    #[serde(default = "default_idle")]
    pub idle_minutes: i64,
    #[serde(default = "default_age")]
    pub max_age_hours: i64,
    #[serde(default = "default_tokens")]
    pub max_tokens: u64,
    /// What the attention watch wakes the chat for.
    #[serde(default)]
    pub watch: WatchLevel,
    /// Whether this PC takes its person's voice ("Take my voice on this PC"). `None`: the
    /// default — the home profile takes it, every other profile starts off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_voice: Option<bool>,
    /// The model a quick turn opens its chat with; blank: no quick lane, quick turns open like
    /// deep ones.
    #[serde(default)]
    pub quick_model: String,
    /// The effort a quick turn opens its chat with (the thinking it does on a local model comes
    /// from this); blank: the app's usual new-chat effort.
    #[serde(default = "default_quick_effort")]
    pub quick_effort: String,
    /// A quick-lane chat whose last context reading reaches this size is rolled over when the next
    /// turn arrives; 0 never rolls.
    #[serde(default = "default_roll_quick")]
    pub quick_roll_at: u64,
    /// The same for a deep-lane chat.
    #[serde(default = "default_roll_deep")]
    pub deep_roll_at: u64,
}

fn default_model() -> String {
    "claude-sonnet-5-5".to_owned()
}
fn default_effort() -> String {
    "low".to_owned()
}
fn default_idle() -> i64 {
    240
}
fn default_age() -> i64 {
    24
}
fn default_tokens() -> u64 {
    300_000
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
            idle_minutes: default_idle(),
            max_age_hours: default_age(),
            max_tokens: default_tokens(),
            watch: WatchLevel::default(),
            take_voice: None,
            quick_model: String::new(),
            quick_effort: default_quick_effort(),
            quick_roll_at: default_roll_quick(),
            deep_roll_at: default_roll_deep(),
        }
    }
}

/// When the tag drops on its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagPolicy {
    pub idle_ms: i64,
    pub max_age_ms: i64,
    pub max_tokens: u64,
}

impl Default for TagPolicy {
    fn default() -> Self {
        PolicyFile::default()
            .check()
            .expect("the defaults are valid")
    }
}

impl PolicyFile {
    /// Refuses a value outside the ranges Settings offers.
    pub fn check(&self) -> Result<TagPolicy, String> {
        if !IDLE_MINUTES.contains(&self.idle_minutes) {
            return Err("The idle time must be 15 to 1440 minutes.".into());
        }
        if !AGE_HOURS.contains(&self.max_age_hours) {
            return Err("The age limit must be 1 to 720 hours.".into());
        }
        if !TOKENS.contains(&self.max_tokens) {
            return Err("The token limit must be 10,000 to 5,000,000.".into());
        }
        if self.model.chars().count() > 120 || self.model.chars().any(char::is_control) {
            return Err("The model must be a model id from the model picker.".into());
        }
        if self.quick_model.chars().count() > 120 || self.quick_model.chars().any(char::is_control)
        {
            return Err("The quick-turn model must be a model id from the model picker.".into());
        }
        if !self.effort.trim().is_empty() && !helm_chats::catalogue::is_effort(self.effort.trim()) {
            return Err("The effort must be low, medium, high, xhigh or max.".into());
        }
        if !self.quick_effort.trim().is_empty()
            && !helm_chats::catalogue::is_effort(self.quick_effort.trim())
        {
            return Err("The quick-turn effort must be low, medium, high, xhigh or max.".into());
        }
        for size in [self.quick_roll_at, self.deep_roll_at] {
            if size != 0 && !ROLL_AT.contains(&size) {
                return Err(
                    "A roll-over size must be 4,000 to 200,000 tokens, or 0 to leave it off."
                        .into(),
                );
            }
        }
        Ok(TagPolicy {
            idle_ms: self.idle_minutes * MINUTE_MS,
            max_age_ms: self.max_age_hours * HOUR_MS,
            max_tokens: self.max_tokens,
        })
    }

    /// The model and effort to hand a new voice chat of `lane`, or `None` for each when blank.
    /// A quick turn with no saved quick model opens exactly like a deep one: there is no quick
    /// lane until the user picks its model.
    pub fn new_chat(&self, lane: Lane) -> (Option<&str>, Option<&str>) {
        fn kept(s: &str) -> Option<&str> {
            Some(s.trim()).filter(|t| !t.is_empty())
        }
        if lane == Lane::Quick {
            match kept(&self.quick_model) {
                Some(m) => return (Some(m), kept(&self.quick_effort)),
                None => return (kept(&self.model), kept(&self.effort)),
            }
        }
        (kept(&self.model), kept(&self.effort))
    }

    /// The size at which a chat of `lane` rolls over when the next turn arrives; 0 leaves it off.
    pub fn roll_at(&self, lane: Lane) -> u64 {
        match lane {
            Lane::Quick => self.quick_roll_at,
            Lane::Deep => self.deep_roll_at,
        }
    }
}

/// Whether a turn arriving for a chat whose last context reading is `context` (first reading
/// `first_context`) should open a successor instead: at or over the lane's size, with room above
/// the chat's first reading so a long system prompt cannot make every turn roll. `None` readings
/// mean nothing has been measured — no roll.
pub fn roll_due(context: Option<u64>, first_context: Option<u64>, roll_at: u64) -> bool {
    if roll_at == 0 {
        return false;
    }
    let Some(size) = context else {
        return false;
    };
    let floor = match first_context {
        Some(first) => roll_at.max(first.saturating_add(ROLL_FLOOR_ROOM)),
        None => roll_at,
    };
    size >= floor
}

/// The bracketed note a rolled-over channel chat's first message carries: the old chat's title and
/// its last few exchanges in one hidden line. Written by Helm, so no model turn spends time or
/// tokens on the hand-off. Quoted words lose their brackets and line breaks, as every one-line
/// note does.
pub fn carry_note(title: &str, exchanges: &[(&str, &str)]) -> String {
    let flat = |s: &str| -> String {
        s.replace(['[', ']'], "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let body = cut_chars(
        &exchanges
            .iter()
            .map(|(who, said)| format!("{who}: {}", flat(said)))
            .collect::<Vec<_>>()
            .join(" · "),
        MAX_CARRY_CHARS,
    )
    .0;
    let title = cut_chars(&flat(title), 200).0;
    if body.is_empty() {
        format!("[Carried over from \"{title}\": nothing was said before.]")
    } else {
        format!("[Carried over from \"{title}\": {body}]")
    }
}

pub fn policy_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("relay").join("voice-policy.json")
}

/// One profile's voice files: its tag and policy, the store of what the phone heard, and the
/// folder its chats start in.
#[derive(Clone)]
pub struct Paths {
    /// The deep lane's tag file (the one voice chats had before lanes).
    pub tag: PathBuf,
    /// The quick lane's tag file; its own chat, limits and age.
    pub tag_quick: PathBuf,
    pub policy: PathBuf,
    pub heard: PathBuf,
    pub folder: PathBuf,
}

/// Whether this PC takes its person's voice: what the switch says, or the default — on for
/// the home profile, off for every other.
pub fn takes_voice(f: &PolicyFile, is_home: bool) -> bool {
    f.take_voice
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

/// The voice chat, as stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceTag {
    pub chat_id: String,
    pub started_at: i64,
    /// When the last message from the phone was placed; the idle time counts from here.
    pub last_turn_at: i64,
    /// The chat's token count when it was tagged, so the limit counts voice use only.
    pub tokens_at_start: u64,
    /// An opaque id for this conversation, carried by every reply; it changes when a new chat
    /// starts. Never the chat's own id.
    pub conversation: String,
    /// Older builds put a primer in the first message; it is now in the system prompt. Kept so
    /// saved tags still read.
    #[serde(default)]
    pub primed: bool,
}

pub fn tag_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("relay").join("voice-chat.json")
}

/// The folder voice chats start in.
pub fn folder(profile_dir: &Path) -> PathBuf {
    profile_dir.join("voice")
}

pub fn load_tag(path: &Path) -> Option<VoiceTag> {
    let bytes = helm_core::privfs::read_capped(path, 64 * 1024).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_tag(path: &Path, tag: Option<&VoiceTag>) -> std::io::Result<()> {
    match tag {
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Some(t) => write_private(path, t),
    }
}

/// Why the current tag no longer holds, or `None` when it does.
pub fn expired(
    tag: &VoiceTag,
    facts: Option<ChatFacts>,
    policy: &TagPolicy,
    now: i64,
) -> Option<&'static str> {
    let Some(f) = facts else {
        return Some("the chat is gone");
    };
    if f.archived {
        return Some("the chat was archived");
    }
    if now - tag.last_turn_at >= policy.idle_ms {
        return Some("it was idle too long");
    }
    if now - tag.started_at >= policy.max_age_ms {
        return Some("the time limit passed");
    }
    if f.tokens.saturating_sub(tag.tokens_at_start) >= policy.max_tokens {
        return Some("the token limit passed");
    }
    None
}

/// The chat a voice tag names while it holds, following a roll-over link out of an archived chat:
/// `expired` reads the current facts through the port, which answers gone for an archived chat the
/// roll-over moved on. An archived chat with no live successor still means start fresh.
pub fn current(path: &Path, port: &dyn ChatPort, policy: &TagPolicy, now: i64) -> Option<VoiceTag> {
    let mut tag = load_tag(path)?;
    if follow_successor(port, &mut tag.chat_id, &mut tag.tokens_at_start) {
        // The tag file moves to the live successor once, so later looks find it there.
        let _ = save_tag(path, Some(&tag));
    }
    expired(&tag, port.facts(&tag.chat_id), policy, now)
        .is_none()
        .then_some(tag)
}

/// Walk a roll-over chain out of an archived chat: while the tagged chat is archived and points at
/// a successor, take it, up to [`MAX_FOLLOW_HOPS`]. Returns whether the tag moved. The token count
/// restarts at the successor's own — the limit counts that chat's use, as for a fresh chat.
pub(crate) const MAX_FOLLOW_HOPS: usize = 8;

pub(crate) fn follow_successor(
    port: &dyn ChatPort,
    tag_chat: &mut String,
    tokens: &mut u64,
) -> bool {
    let mut moved = false;
    for _ in 0..MAX_FOLLOW_HOPS {
        match port.facts(tag_chat) {
            Some(f) if f.archived => {}
            _ => break,
        }
        let Some(next) = port.successor(tag_chat) else {
            break;
        };
        match port.facts(&next) {
            Some(f) => {
                *tag_chat = next;
                *tokens = f.tokens;
                moved = true;
            }
            None => break,
        }
    }
    moved
}

/// The chat a message from the phone goes to: the tagged one while it holds, else a new one
/// that takes the tag. `profile` is the display name of the profile that owns the channel; a
/// new chat carries it in its title. Returns the tag as it now stands and whether the chat was
/// just created. The tag's last-message time moves to `now`.
pub fn active_chat(
    path: &Path,
    port: &dyn ChatPort,
    policy: &TagPolicy,
    profile: Option<&str>,
    now: i64,
) -> Result<(VoiceTag, bool), String> {
    let (mut tag, created) = match current(path, port, policy, now) {
        Some(tag) => (tag, false),
        None => {
            let id = port.create(&chat_title(profile, now))?;
            let tokens_at_start = port.facts(&id).map(|f| f.tokens).unwrap_or(0);
            let tag = VoiceTag {
                chat_id: id,
                started_at: now,
                last_turn_at: now,
                tokens_at_start,
                conversation: new_conversation(),
                primed: false,
            };
            (tag, true)
        }
    };
    tag.last_turn_at = now;
    save_tag(path, Some(&tag)).map_err(|_| "The voice chat tag could not be saved.".to_owned())?;
    Ok((tag, created))
}

/// `Phone voice <profile name> <YYYY-MM-DD>` (UTC date); no name part when the name is unknown.
/// The first word names the channel the words came in on, the same shape the other channels use
/// (`Telegram …`, `HA speaker …`), so a chat list shows at once where a talk happened.
pub fn chat_title(profile: Option<&str>, now: i64) -> String {
    let name = clean_title_name(profile);
    let (y, m, d) = civil_date(now.div_euclid(86_400_000));
    match name.as_deref() {
        Some(n) => format!("Phone voice {n} {y:04}-{m:02}-{d:02}"),
        None => format!("Phone voice {y:04}-{m:02}-{d:02}"),
    }
}

/// A random 16-hex conversation id.
pub fn new_conversation() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}

/// The submission id for a turn id from the phone: the same turn delivered twice maps to the
/// same submission. A UUID, as workers require (version 8, from a SHA-256 of the turn id).
pub fn submission_id(turn: &str) -> String {
    hashed_id(&format!("helm-voice/{turn}"))
}

/// The submission id for a note from the attention watch, in a namespace of its own, so a note's
/// key can never stand for a turn from the phone.
pub fn note_submission_id(key: &str) -> String {
    hashed_id(&format!("helm-voice-note/{key}"))
}

fn hashed_id(name: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(name);
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Uuid::new_v8(b).to_string()
}

// ---- the words the chat is given ----------------------------------------------------------

/// The note on every voice message. It names the mode and, for a message that was spoken as a
/// continuation of the one before it, says so. One bracketed line.
pub fn mode_note(mode: Mode, continues: bool) -> String {
    let how = match mode {
        Mode::Conversation => "conversation: speak it, short, no markup",
        Mode::Chat => "chat: read on screen, formatting fine",
    };
    if continues {
        format!("[Voice · {how} · continuing what they just said]")
    } else {
        format!("[Voice · {how}]")
    }
}

/// What a voice message is stored with: the mode note, a pointer to the other lane's live chat if
/// this one has just started (so "and the bedroom" still resolves across lanes), any notes of
/// replies the user did not hear (bracketed single lines), a line saying how late a queued message
/// is, then the message.
pub fn delivered_text(
    mode: Mode,
    continues: bool,
    age_minutes: Option<i64>,
    heard_notes: &[String],
    pointer: Option<&str>,
    text: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&mode_note(mode, continues));
    out.push('\n');
    if let Some(p) = pointer.filter(|p| !p.trim().is_empty()) {
        let flat = p.replace(['[', ']'], "").trim().to_owned();
        out.push_str(&format!("[The phone's other voice chat — {flat}]\n"));
    }
    for note in heard_notes {
        out.push_str(note);
        out.push('\n');
    }
    if let Some(n) = age_minutes.filter(|n| *n >= 1) {
        out.push_str(&format!(
            "(sent {n} min ago, queued while Helm was offline)\n"
        ));
    }
    out.push_str(text.trim());
    out
}

// ---- what goes up --------------------------------------------------------------------------

/// One answer going up to the phone: a reply to a turn, or unprompted news.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    /// Made on this PC and fixed for the answer, so a resend is the same reply.
    pub id: String,
    /// The phone's turn it answers; `None` for news.
    pub turn: Option<String>,
    pub mode: Mode,
    /// Conversation mode: the sentences to speak. Chat mode: empty.
    pub sentences: Vec<String>,
    /// Conversation mode: the sentences joined. Chat mode: the answer as written.
    pub text: String,
    pub truncated: bool,
    pub conversation: String,
    pub at: i64,
    /// How many attachments went up just before this.
    pub attachments: u8,
    /// For news: the title of the chat it is about.
    pub about: Option<String>,
}

impl Reply {
    /// An answer made for `mode` from the agent's text: conversation mode is cleaned and cut for
    /// speech ([`crate::speakable`]), chat mode keeps the text up to [`MAX_CHAT_CHARS`].
    pub fn from_text(
        id: String,
        turn: Option<String>,
        mode: Mode,
        raw: &str,
        conversation: String,
        at: i64,
    ) -> Reply {
        let (sentences, text, truncated) = match mode {
            Mode::Conversation => {
                let s = crate::speakable::speakable(raw);
                let mut sentences = s.sentences;
                if sentences.is_empty() {
                    sentences.push("Done.".to_owned());
                }
                let text = sentences.join(" ");
                (sentences, text, s.truncated)
            }
            Mode::Chat => {
                let (text, cut) = cut_chars(raw.trim(), MAX_CHAT_CHARS);
                (vec![], text, cut)
            }
        };
        Reply {
            id,
            turn,
            mode,
            sentences,
            text,
            truncated,
            conversation,
            at,
            attachments: 0,
            about: None,
        }
    }

    /// The frame for the line: `voice_final` for an answer, `voice_notify` for news.
    pub fn frame(&self) -> Value {
        let numbered: Vec<Value> = self
            .sentences
            .iter()
            .enumerate()
            .map(|(i, s)| json!({ "n": i + 1, "text": s }))
            .collect();
        let mut f = json!({
            "type": if self.turn.is_some() { "voice_final" } else { "voice_notify" },
            "id": self.id,
            "turn": self.turn,
            "mode": self.mode.as_str(),
            "sentences": numbered,
            "text": self.text,
            "truncated": self.truncated,
            "conversation": self.conversation,
            "at": self.at,
            "attachments": self.attachments,
        });
        if self.turn.is_none() {
            f["about"] = match &self.about {
                Some(chat) => json!({ "chat": chat }),
                None => Value::Null,
            };
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const T0: i64 = 1_800_000_000_000; // 2027-01-15
    const MIN: i64 = MINUTE_MS;

    #[derive(Default)]
    struct Port {
        chats: RefCell<HashMap<String, ChatFacts>>,
        created: RefCell<Vec<String>>,
        /// Roll-over links the fake database carries, so a tag can follow them.
        successors: RefCell<HashMap<String, String>>,
    }

    impl ChatPort for Port {
        fn facts(&self, id: &str) -> Option<ChatFacts> {
            self.chats.borrow().get(id).copied()
        }
        fn successor(&self, chat_id: &str) -> Option<String> {
            self.successors.borrow().get(chat_id).cloned()
        }
        fn create(&self, title: &str) -> Result<String, String> {
            let id = format!("chat-{}", self.created.borrow().len() + 1);
            self.created.borrow_mut().push(title.to_owned());
            self.chats.borrow_mut().insert(
                id.clone(),
                ChatFacts {
                    archived: false,
                    tokens: 1_000,
                },
            );
            Ok(id)
        }
    }

    #[test]
    fn the_tag_holds_until_idle_age_tokens_or_the_chat_goes() {
        let dir = tempfile::tempdir().unwrap();
        let path = tag_path(dir.path());
        let policy = PolicyFile::default().check().unwrap();
        let port = Port::default();

        let (tag, created) = active_chat(&path, &port, &policy, Some("Alex"), T0).unwrap();
        assert!(created && !tag.primed && tag.conversation.len() == 16);
        assert_eq!(port.created.borrow()[0], "Phone voice Alex 2027-01-15");

        // (minutes since the last message, tokens now, archived, expected new chat)
        let rows = [
            (30, 1_000, false, false),
            (239, 1_500, false, false),
            (241, 1_500, false, true), // idle: more than 240 minutes since the last message
            (10, 301_000, false, true), // tokens used since the tag: 300,000
            (10, 2_000, true, true),   // archived
        ];
        let mut now = T0;
        for (gap, tokens, archived, fresh) in rows {
            let before = load_tag(&path).unwrap();
            now += gap * MIN;
            port.chats
                .borrow_mut()
                .insert(before.chat_id.clone(), ChatFacts { archived, tokens });
            let (tag, created) = active_chat(&path, &port, &policy, None, now).unwrap();
            assert_eq!(
                created, fresh,
                "gap {gap}, tokens {tokens}, archived {archived}"
            );
            assert_eq!(tag.last_turn_at, now);
            assert_eq!(tag.chat_id != before.chat_id, fresh);
            assert_eq!(tag.conversation != before.conversation, fresh);
        }

        // An archived chat whose roll-over link names a live successor is not gone: the tag moves
        // to it, token counting restarts there, and no new chat starts. A hand-archived chat with
        // no successor still means start fresh (the row above).
        let before = load_tag(&path).unwrap();
        port.chats.borrow_mut().insert(
            before.chat_id.clone(),
            ChatFacts {
                archived: true,
                tokens: 5_000,
            },
        );
        port.successors
            .borrow_mut()
            .insert(before.chat_id.clone(), "succ-1".into());
        port.chats.borrow_mut().insert(
            "succ-1".to_owned(),
            ChatFacts {
                archived: false,
                tokens: 2_500,
            },
        );
        now += MIN;
        let (tag, created) = active_chat(&path, &port, &policy, None, now).unwrap();
        assert!(
            !created,
            "the tag followed the roll-over link instead of starting a chat"
        );
        assert_eq!(tag.chat_id, "succ-1");
        assert_eq!(
            tag.tokens_at_start, 2_500,
            "token use counts from the successor"
        );
        assert_eq!(
            tag.conversation, before.conversation,
            "the conversation carries on"
        );
        // The saved tag itself now names the successor.
        assert_eq!(load_tag(&path).unwrap().chat_id, "succ-1");

        // A chat started without a name carries none: chat 2 came from the idle row, passed None.
        // (Two more names went in since: one per fresh chat above.)
        assert_eq!(port.created.borrow()[1], "Phone voice 2027-01-15");
        // Age: a chat in steady use still ends at the age limit.
        let policy = PolicyFile {
            max_age_hours: 1,
            ..PolicyFile::default()
        }
        .check()
        .unwrap();
        let start = load_tag(&path).unwrap();
        let (tag, created) =
            active_chat(&path, &port, &policy, Some("Alex"), now + 61 * MIN).unwrap();
        assert!(created && tag.chat_id != start.chat_id);
        // Dropping the tag by hand starts a new chat too; `current` reads without starting one.
        assert!(current(&path, &port, &policy, now + 62 * MIN).is_some());
        save_tag(&path, None).unwrap();
        assert!(current(&path, &port, &policy, now + 62 * MIN).is_none());
    }

    #[test]
    fn the_title_names_the_person() {
        // (name, expected title)
        let rows = [
            (Some("Alex"), "Phone voice Alex 2027-01-15"),
            (None, "Phone voice 2027-01-15"),
            (Some("   "), "Phone voice 2027-01-15"),
        ];
        for (name, want) in rows {
            assert_eq!(chat_title(name, T0), want, "name {name:?}");
        }
    }

    #[test]
    fn the_policy_is_checked_and_defaults_when_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = policy_path(dir.path());
        assert_eq!(load_file(&path), PolicyFile::default());
        let bad: [(PolicyFile, &str); 5] = [
            (
                PolicyFile {
                    idle_minutes: 5,
                    ..Default::default()
                },
                "idle",
            ),
            (
                PolicyFile {
                    max_age_hours: 0,
                    ..Default::default()
                },
                "age",
            ),
            (
                PolicyFile {
                    max_tokens: 5,
                    ..Default::default()
                },
                "token",
            ),
            (
                PolicyFile {
                    effort: "huge".into(),
                    ..Default::default()
                },
                "effort",
            ),
            (
                PolicyFile {
                    model: "a\nb".into(),
                    ..Default::default()
                },
                "model",
            ),
        ];
        for (f, word) in bad {
            let e = f.check().unwrap_err();
            assert!(e.contains(word), "{e}");
        }
        // The lane fields refuse a bad quick effort and sizes outside their own range (0 is off).
        let too_big = PolicyFile {
            deep_roll_at: 300_000,
            ..PolicyFile::default()
        };
        assert!(too_big.check().unwrap_err().contains("roll-over size"));
        let bad_effort = PolicyFile {
            quick_effort: "sometimes".into(),
            ..PolicyFile::default()
        };
        assert!(bad_effort
            .check()
            .unwrap_err()
            .contains("quick-turn effort"));
        let off = PolicyFile {
            quick_roll_at: 0,
            deep_roll_at: 0,
            ..PolicyFile::default()
        };
        assert!(off.check().is_ok());
        let f = PolicyFile {
            model: String::new(),
            effort: "high".into(),
            idle_minutes: 60,
            watch: WatchLevel::FlagsDone,
            quick_model: "local/small/quick-one".into(),
            ..Default::default()
        };
        save_policy(&path, &f).unwrap();
        assert_eq!(load_file(&path), f);
        assert_eq!(f.new_chat(Lane::Deep), (None, Some("high")));
        assert_eq!(
            f.new_chat(Lane::Quick),
            (Some("local/small/quick-one"), Some("medium"))
        );
        // No saved quick model: a quick turn opens exactly like a deep one.
        let no_lane = PolicyFile {
            quick_model: String::new(),
            ..PolicyFile::default()
        };
        assert_eq!(
            no_lane.new_chat(Lane::Quick),
            (Some("claude-sonnet-5-5"), Some("low"))
        );
        assert_eq!(
            PolicyFile::default().new_chat(Lane::Deep),
            (Some("claude-sonnet-5-5"), Some("low"))
        );
        assert_eq!(PolicyFile::default().roll_at(Lane::Quick), 16_000);
        assert_eq!(PolicyFile::default().roll_at(Lane::Deep), 48_000);
        assert_eq!(Lane::parse(Some("QUICK ")), Lane::Quick);
        assert_eq!(Lane::parse(None), Lane::Deep);
        assert_eq!(Lane::parse(Some("deeper")), Lane::Deep);
        // Partial files fill in the defaults; unknown fields are refused.
        let part: PolicyFile = serde_json::from_str(r#"{"watch":"flags+done"}"#).unwrap();
        assert_eq!(part.watch, WatchLevel::FlagsDone);
        assert_eq!(part.idle_minutes, 240);
        assert!(serde_json::from_str::<PolicyFile>(r#"{"loud":true}"#).is_err());
    }

    #[test]
    fn messages_carry_their_notes_and_replies_their_frames() {
        let msg = delivered_text(
            Mode::Conversation,
            true,
            Some(7),
            &["[Not heard by the user: x]".to_owned()],
            None,
            "  hello  ",
        );
        assert_eq!(
            msg,
            "[Voice · conversation: speak it, short, no markup · continuing what they just said]\n[Not heard by the user: x]\n(sent 7 min ago, queued while Helm was offline)\nhello"
        );
        assert_eq!(
            delivered_text(Mode::Chat, false, Some(0), &[], None, "hi"),
            "[Voice · chat: read on screen, formatting fine]\nhi"
        );
        // A pointer to the other lane's live chat rides on the first message of a new one, and
        // brackets in it never break its own line.
        assert_eq!(
            delivered_text(Mode::Conversation, false, None, &[], Some("Kitchen voice [x] — Done."), "and the lamp"),
            "[Voice · conversation: speak it, short, no markup]\n[The phone's other voice chat — Kitchen voice x — Done.]\nand the lamp"
        );
        assert_eq!(
            delivered_text(Mode::Conversation, false, None, &[], Some("   "), "hi"),
            "[Voice · conversation: speak it, short, no markup]\nhi",
            "an empty pointer adds nothing"
        );
        // The carry-over note quotes the last exchanges on one hidden line.
        let note = carry_note(
            "Phone voice Alex 2027-01-15",
            &[("They", "lamp off\n[done]"), ("Helm", "Done.")],
        );
        assert_eq!(
            note,
            "[Carried over from \"Phone voice Alex 2027-01-15\": They: lamp off done · Helm: Done.]"
        );
        // Roll-over sizes: on at the size, off below it and when switched off, with the floor of
        // first reading + room holding a big system prompt steady.
        assert!(roll_due(Some(16_000), Some(500), 16_000));
        assert!(!roll_due(Some(15_999), Some(500), 16_000));
        assert!(!roll_due(Some(90_000), Some(500), 0));
        assert!(!roll_due(None, None, 16_000));
        // A chat whose first reading was already near the size cannot roll on every turn.
        assert!(!roll_due(Some(14_900), Some(12_000), 13_000));
        assert!(roll_due(Some(16_000), Some(12_000), 13_000));
        assert_eq!(submission_id("t1"), submission_id("t1"));
        assert_ne!(submission_id("t1"), submission_id("t2"));
        assert_ne!(submission_id("k1"), note_submission_id("k1"));
        assert_eq!(Mode::parse("chat"), Mode::Chat);
        assert_eq!(Mode::parse("anything"), Mode::Conversation);

        let r = Reply::from_text(
            "r1".into(),
            Some("t1".into()),
            Mode::Conversation,
            "Hello there. See https://example.com/a",
            "c1".into(),
            5,
        );
        let f = r.frame();
        assert_eq!(f["type"], "voice_final");
        assert_eq!(f["turn"], "t1");
        assert_eq!(
            f["sentences"],
            json!([{ "n": 1, "text": "Hello there." }, { "n": 2, "text": "See a link." }])
        );
        assert_eq!(f["text"], "Hello there. See a link.");
        assert_eq!(f["attachments"], 0);
        assert!(f.get("about").is_none());
        // Chat mode keeps the text as written; news has no turn and says what it is about.
        let mut n = Reply::from_text(
            "r2".into(),
            None,
            Mode::Chat,
            "# Heading\n\n| a |\n|---|\n| 1 |",
            "c1".into(),
            6,
        );
        n.about = Some("Build".into());
        let f = n.frame();
        assert_eq!(f["type"], "voice_notify");
        assert_eq!(f["turn"], Value::Null);
        assert_eq!(f["sentences"], json!([]));
        assert_eq!(f["text"], "# Heading\n\n| a |\n|---|\n| 1 |");
        assert_eq!(f["about"], json!({ "chat": "Build" }));
        // Nothing to say in conversation mode still says something.
        let r = Reply::from_text("r3".into(), None, Mode::Conversation, "   ", "c".into(), 1);
        assert_eq!(r.sentences, vec!["Done.".to_owned()]);
    }
}
