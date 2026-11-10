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
//! - **The words.** A fresh conversation's first message carries the [`PRIMER`]; every message
//!   carries a one-line note naming its [`Mode`]. The transcript hides both.
//! - **The reply** ([`Reply`]) is what goes up the line, once per answer, with a fixed id so a
//!   resend is recognised.

use crate::telegram::{civil_date, cut_chars, write_private, ChatFacts, ChatPort};
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

impl Default for PolicyFile {
    fn default() -> Self {
        PolicyFile {
            model: default_model(),
            effort: default_effort(),
            idle_minutes: default_idle(),
            max_age_hours: default_age(),
            max_tokens: default_tokens(),
            watch: WatchLevel::default(),
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
        if !self.effort.trim().is_empty() && !helm_chats::catalogue::is_effort(self.effort.trim()) {
            return Err("The effort must be low, medium, high, xhigh or max.".into());
        }
        Ok(TagPolicy {
            idle_ms: self.idle_minutes * MINUTE_MS,
            max_age_ms: self.max_age_hours * HOUR_MS,
            max_tokens: self.max_tokens,
        })
    }

    /// The model and effort to hand a new voice chat, or `None` for each when blank.
    pub fn new_chat(&self) -> (Option<&str>, Option<&str>) {
        fn kept(s: &str) -> Option<&str> {
            Some(s.trim()).filter(|t| !t.is_empty())
        }
        (kept(&self.model), kept(&self.effort))
    }
}

pub fn policy_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("relay").join("voice-policy.json")
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
    /// The primer has been delivered to this chat.
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

/// The tag while it holds, `None` once it has expired or when there is none.
pub fn current(path: &Path, port: &dyn ChatPort, policy: &TagPolicy, now: i64) -> Option<VoiceTag> {
    let tag = load_tag(path)?;
    expired(&tag, port.facts(&tag.chat_id), policy, now)
        .is_none()
        .then_some(tag)
}

/// The chat a message from the phone goes to: the tagged one while it holds, else a new one
/// that takes the tag. Returns the tag as it now stands and whether the chat was just created.
/// The tag's last-message time moves to `now`.
pub fn active_chat(
    path: &Path,
    port: &dyn ChatPort,
    policy: &TagPolicy,
    now: i64,
) -> Result<(VoiceTag, bool), String> {
    let (mut tag, created) = match current(path, port, policy, now) {
        Some(tag) => (tag, false),
        None => {
            let id = port.create(&chat_title(now))?;
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

/// `Voice <YYYY-MM-DD>` (UTC date).
pub fn chat_title(now: i64) -> String {
    let (y, m, d) = civil_date(now.div_euclid(86_400_000));
    format!("Voice {y:04}-{m:02}-{d:02}")
}

/// A random 16-hex conversation id.
pub fn new_conversation() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}

/// The submission id for a turn id from the phone: the same turn delivered twice maps to the
/// same submission. A UUID, as workers require (version 8, from a SHA-256 of the turn id).
pub fn submission_id(turn: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("helm-voice/{turn}"));
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Uuid::new_v8(b).to_string()
}

// ---- the words the chat is given ----------------------------------------------------------

/// What a fresh conversation's first message starts with and ends with, so the transcript can
/// hide the whole primer however its middle is worded.
pub const PRIMER_START: &str = "[Voice chat: you are the user's voice assistant in Helm";
pub const PRIMER_END: &str = "If not, answer exactly [quiet].]";

/// Put in front of the first message of each fresh conversation.
pub const PRIMER: &str = "[Voice chat: you are the user's voice assistant in Helm, heard through the voice screen of their phone app. Each message says its mode.
CONVERSATION mode (heard, not read): answer in short spoken sentences. Lead with the answer. Aim for about 30 seconds (75 words) and never more than a minute. Offer more instead of giving it (\"Want the details?\"). No markdown, lists, tables, code, URLs, ids, hashes or file paths: say \"a link\", \"a file\", \"the build chat\". Summarise long results from other chats; the full text stays in that chat.
Show something only when hearing it would be worse (a table of numbers, a picture, a link to open): call app_voice_show, and always say in a sentence what you put on screen.
CHAT mode (read, not heard): normal formatting, links and longer answers are fine.
You can reach other chats (app_list_chats, app_read_chat, app_send_message). When you hand work to a chat, declare app_wait_for chat_idle on it so you are woken when it finishes.
When a message starts with [Wait finished] or [Attention], it is news, not the user: decide whether it is worth telling them. If yes, open with it (\"By the way, I just heard back from the build chat: it passed.\"). If not, answer exactly [quiet].]";

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

/// What a voice message is stored with: the primer (first message of a fresh conversation), the
/// mode note, any notes of replies the user did not hear (bracketed single lines), a line saying
/// how late a queued message is, then the message.
pub fn delivered_text(
    primer: bool,
    mode: Mode,
    continues: bool,
    age_minutes: Option<i64>,
    heard_notes: &[String],
    text: &str,
) -> String {
    let mut out = String::new();
    if primer {
        out.push_str(PRIMER);
        out.push('\n');
    }
    out.push_str(&mode_note(mode, continues));
    out.push('\n');
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
    }

    impl ChatPort for Port {
        fn facts(&self, id: &str) -> Option<ChatFacts> {
            self.chats.borrow().get(id).copied()
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

        let (tag, created) = active_chat(&path, &port, &policy, T0).unwrap();
        assert!(created && !tag.primed && tag.conversation.len() == 16);
        assert_eq!(port.created.borrow()[0], "Voice 2027-01-15");

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
            let (tag, created) = active_chat(&path, &port, &policy, now).unwrap();
            assert_eq!(
                created, fresh,
                "gap {gap}, tokens {tokens}, archived {archived}"
            );
            assert_eq!(tag.last_turn_at, now);
            assert_eq!(tag.chat_id != before.chat_id, fresh);
            assert_eq!(tag.conversation != before.conversation, fresh);
        }

        // Age: a chat in steady use still ends at the age limit.
        let policy = PolicyFile {
            max_age_hours: 1,
            ..PolicyFile::default()
        }
        .check()
        .unwrap();
        let start = load_tag(&path).unwrap();
        let (tag, created) = active_chat(&path, &port, &policy, now + 61 * MIN).unwrap();
        assert!(created && tag.chat_id != start.chat_id);
        // Dropping the tag by hand starts a new chat too; `current` reads without starting one.
        assert!(current(&path, &port, &policy, now + 62 * MIN).is_some());
        save_tag(&path, None).unwrap();
        assert!(current(&path, &port, &policy, now + 62 * MIN).is_none());
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
        let f = PolicyFile {
            model: String::new(),
            effort: "high".into(),
            idle_minutes: 60,
            watch: WatchLevel::FlagsDone,
            ..Default::default()
        };
        save_policy(&path, &f).unwrap();
        assert_eq!(load_file(&path), f);
        assert_eq!(f.new_chat(), (None, Some("high")));
        assert_eq!(
            PolicyFile::default().new_chat(),
            (Some("claude-sonnet-5-5"), Some("low"))
        );
        // Partial files fill in the defaults; unknown fields are refused.
        let part: PolicyFile = serde_json::from_str(r#"{"watch":"flags+done"}"#).unwrap();
        assert_eq!(part.watch, WatchLevel::FlagsDone);
        assert_eq!(part.idle_minutes, 240);
        assert!(serde_json::from_str::<PolicyFile>(r#"{"loud":true}"#).is_err());
    }

    #[test]
    fn messages_carry_their_notes_and_replies_their_frames() {
        let msg = delivered_text(
            true,
            Mode::Conversation,
            true,
            Some(7),
            &["[Not heard by the user: x]".to_owned()],
            "  hello  ",
        );
        assert!(msg.starts_with(PRIMER_START) && msg.contains(PRIMER_END));
        let after = msg.split(PRIMER_END).nth(1).unwrap();
        assert_eq!(
            after.trim_start_matches('\n'),
            "[Voice · conversation: speak it, short, no markup · continuing what they just said]\n[Not heard by the user: x]\n(sent 7 min ago, queued while Helm was offline)\nhello"
        );
        assert_eq!(
            delivered_text(false, Mode::Chat, false, Some(0), &[], "hi"),
            "[Voice · chat: read on screen, formatting fine]\nhi"
        );
        assert_eq!(submission_id("t1"), submission_id("t1"));
        assert_ne!(submission_id("t1"), submission_id("t2"));
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
